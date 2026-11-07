//! Bitwarden as a credential store inside codex-hosts.
//!
//! The Login items of one vault folder are the vault-backed hosts
//! ([`hosts`]): each item's name, URI, username and password define a host,
//! mirrored into the host store with its local state. The items are read
//! through the official native `bw.exe` (the adapter in `codex_hosts_bw`).
//! The master password is typed only in the codex-hosts window and reaches
//! `bw` only through the child's environment; the unlocked session key is
//! kept in Credential Manager for as long as the re-authentication policy
//! allows ([`session`]); the vault cache stays in the CLI's own data
//! directory and is synced per the configured window before a host is used,
//! so a change made elsewhere is picked up. Nothing here holds an HTTP stack
//! or a vault SDK.

pub(crate) mod hosts;
pub(crate) mod prompt;
pub(crate) mod session;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod vault;

use std::time::{Duration, Instant};

use codex_hosts_bw::{BwExecutable, BwProfile, SessionKey};
use tokio::sync::Mutex;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::model::{BitwardenProfile, BitwardenSettings, HostProfile};
use crate::ssh::RemoteFailure;
use crate::storage::HostStore;
use crate::vault_hosts::{self, ItemKey, ItemProblem, Reconciled};

pub(crate) use vault::{RealRunner, Vault, VaultError};

/// `bw` rewrites its data file, so calls from one process take turns.
static CLI_GATE: Mutex<()> = Mutex::const_new(());

/// The configured account with everything a `bw` call needs.
pub(crate) struct Account {
    pub(crate) profile: BitwardenProfile,
    exe: BwExecutable,
    data: BwProfile,
}

impl Account {
    /// The configured account, or the reason a vault-backed host cannot be
    /// served right now.
    pub(crate) fn from_settings(settings: &BitwardenSettings) -> Result<Self, RemoteFailure> {
        if !settings.enabled {
            return Err(RemoteFailure::new(
                "BITWARDEN_DISABLED",
                "Bitwarden is switched off in codex-hosts; enable it under Credential services in the window.",
            ));
        }
        let (Some(profile), Some(cli_path)) = (&settings.profile, &settings.cli_path) else {
            return Err(RemoteFailure::new(
                "BITWARDEN_NOT_CONFIGURED",
                "Bitwarden is not set up in codex-hosts; finish the credential manager setup first.",
            ));
        };
        let exe = BwExecutable::new(cli_path).map_err(|error| {
            RemoteFailure::new("BITWARDEN_NOT_CONFIGURED", format!("bw.exe: {error}"))
        })?;
        let dir = session::data_dir(profile.id)
            .map_err(|error| RemoteFailure::new("BITWARDEN_UNAVAILABLE", error))?;
        let data = BwProfile::new(dir).map_err(|error| {
            RemoteFailure::new("BITWARDEN_UNAVAILABLE", format!("data directory: {error}"))
        })?;
        Ok(Self {
            profile: profile.clone(),
            exe,
            data,
        })
    }

    pub(crate) fn id(&self) -> Uuid {
        self.profile.id
    }

    /// A vault handle with the given session (none for login/status calls).
    pub(crate) fn vault(&self, session: Option<SessionKey>) -> Vault {
        Vault::new(RealRunner, self.exe.clone(), self.data.clone(), session)
    }

    /// A vault handle with the stored session, when the policy still allows
    /// it; otherwise the vault counts as locked.
    pub(crate) fn unlocked_vault(&self) -> Result<Vault, RemoteFailure> {
        let session = session::load(&self.profile)
            .map_err(|error| RemoteFailure::new("BITWARDEN_UNAVAILABLE", error))?;
        match session {
            Some(session) => Ok(self.vault(Some(session))),
            None => Err(VaultError::Locked.into_remote(&self.profile.email)),
        }
    }
}

/// Admission for one host operation: the vault is usable, its cache is
/// fresh, and the item was read at `revision`. The fingerprint changes
/// whenever the session key does and the revision whenever the item does,
/// so a retained SSH session is reused neither after a lock nor after the
/// item changed elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Admission {
    pub(crate) item_id: Uuid,
    pub(crate) revision: String,
    pub(crate) session_fingerprint: u64,
}

fn settings() -> Result<BitwardenSettings, RemoteFailure> {
    HostStore::load_recovering()
        .map(|store| store.bitwarden)
        .map_err(|error| RemoteFailure::new("BITWARDEN_UNAVAILABLE", error.to_string()))
}

fn unavailable(error: impl std::fmt::Display) -> RemoteFailure {
    RemoteFailure::new("BITWARDEN_UNAVAILABLE", error.to_string())
}

/// Syncs when the cache is older than the configured window; a failed sync
/// is reported as such rather than served from old data.
async fn ensure_fresh(
    account: &Account,
    vault: &mut Vault,
    max_age_seconds: u64,
) -> Result<(), RemoteFailure> {
    if session::sync_is_fresh(account.id(), max_age_seconds) {
        return Ok(());
    }
    match vault.sync().await {
        Ok(()) => session::mark_synced(account.id()).map_err(unavailable),
        Err(VaultError::Locked) => {
            let _ = session::forget(account.id());
            Err(VaultError::Locked.into_remote(&account.profile.email))
        }
        Err(VaultError::SignedOut) => {
            let _ = session::forget(account.id());
            Err(VaultError::SignedOut.into_remote(&account.profile.email))
        }
        Err(_) => Err(VaultError::SyncRequired.into_remote(&account.profile.email)),
    }
}

/// How long a key obtained by one step of a call (a prompt, a discovery)
/// stays available to the next step in this process, so one tool call never
/// asks for the master password twice under the per-call policy.
const RECENT_SESSION_TTL: Duration = Duration::from_secs(30);

static RECENT_SESSION: std::sync::Mutex<Option<(Zeroizing<String>, Instant)>> =
    std::sync::Mutex::new(None);

fn remember_session(key: &SessionKey) {
    if let Ok(mut slot) = RECENT_SESSION.lock() {
        *slot = Some((Zeroizing::new(key.expose().to_owned()), Instant::now()));
    }
}

fn take_recent_session() -> Option<SessionKey> {
    let mut slot = RECENT_SESSION.lock().ok()?;
    let (key, at) = slot.take()?;
    (at.elapsed() <= RECENT_SESSION_TTL).then(|| SessionKey::new(key.as_str().to_owned()))
}

/// Whether reading the vault now would ask for the master password: it is
/// set up, and neither a key from an earlier step of this call nor a stored
/// key the policy still allows is at hand. Anything unreadable counts as not
/// due; the read itself reports it.
pub(crate) fn unlock_due() -> bool {
    let recent = RECENT_SESSION
        .lock()
        .is_ok_and(|slot| matches!(&*slot, Some((_, at)) if at.elapsed() <= RECENT_SESSION_TTL));
    if recent {
        return false;
    }
    let Ok(settings) = settings() else {
        return false;
    };
    match (&settings.profile, settings.available()) {
        (Some(profile), true) => matches!(session::peek(profile), Ok(false)),
        _ => false,
    }
}

/// A vault with a usable session: the one an earlier step of this call
/// obtained, the stored one, or the one the user creates in the codex-hosts
/// window when the policy asks again.
async fn unlocked_or_prompt(account: &Account) -> Result<Vault, RemoteFailure> {
    if let Some(session) = take_recent_session() {
        return Ok(account.vault(Some(session)));
    }
    // An idle-expired key locks `bw` too before anything asks again.
    if let Ok(Some(expired)) = session::take_expired(&account.profile) {
        account.vault(Some(expired)).lock().await;
    }
    match account.unlocked_vault() {
        Ok(vault) => Ok(vault),
        Err(error) if error.code == "BITWARDEN_LOCKED" => {
            prompt::request_unlock().await?;
            account.unlocked_vault()
        }
        Err(error) => Err(error),
    }
}

fn folder_of(account: &Account) -> Result<Uuid, RemoteFailure> {
    account.profile.folder_id.ok_or_else(|| {
        RemoteFailure::new(
            "BITWARDEN_NOT_CONFIGURED",
            "No Bitwarden folder is chosen for hosts; finish the credential manager setup first.",
        )
    })
}

fn item_failure(error: VaultError, account: &Account, alias: &str) -> RemoteFailure {
    if matches!(error, VaultError::Locked | VaultError::SignedOut) {
        let _ = session::forget(account.id());
        return error.into_remote(&account.profile.email);
    }
    error.into_remote(alias)
}

fn problem_failure(alias: &str, problem: &ItemProblem) -> RemoteFailure {
    RemoteFailure::new(
        "BITWARDEN_ITEM_MISMATCH",
        format!(
            "The Bitwarden item behind {alias} no longer describes a host: {}.",
            problem.reason()
        ),
    )
}

/// An admitted login: the refreshed host, the pool key material, and the
/// session the connection will authenticate with. Under the per-call policy
/// this is the one hand-over of the key the window stored for this call.
pub(crate) struct AdmittedLogin {
    /// The host as the item describes it right now.
    pub(crate) host: HostProfile,
    pub(crate) admission: Admission,
    login: VaultLogin,
}

impl AdmittedLogin {
    pub(crate) fn into_login(self) -> VaultLogin {
        self.login
    }
}

/// What the connection authenticates with: the session the admission used
/// and, since the item was already read, its password, so the connection
/// does not start `bw` again for it. No `Debug`.
pub(crate) struct VaultLogin {
    pub(crate) session: SessionKey,
    pub(crate) password: Option<Zeroizing<String>>,
}

/// Admits a login operation for a vault-backed host: unlocks (asking the
/// user when the policy requires it), syncs per the configured window, reads
/// the item, and refreshes the mirror, so the connection that follows uses
/// the endpoint, account and password the vault holds now. Every call goes
/// through here, including one that reuses a retained session.
pub(crate) async fn admit(host: &HostProfile) -> Result<AdmittedLogin, RemoteFailure> {
    let item_key = ItemKey::of(host)
        .ok_or_else(|| RemoteFailure::new("BITWARDEN_UNAVAILABLE", "not a vault host"))?;
    let item_id = item_key.id;
    let _turn = CLI_GATE.lock().await;
    let settings = settings()?;
    let account = Account::from_settings(&settings)?;
    let mut vault = unlocked_or_prompt(&account).await?;
    ensure_fresh(&account, &mut vault, settings.sync_max_age_seconds).await?;
    let item = match vault.get_item(item_id).await {
        Ok(item) => item,
        Err(VaultError::ItemMissing) => {
            vault_hosts::drop_mirror(item_id);
            return Err(VaultError::ItemMissing.into_remote(&host.alias));
        }
        Err(error) => return Err(item_failure(error, &account, &host.alias)),
    };
    if item.is_deleted() {
        vault_hosts::drop_mirror(item_id);
        return Err(VaultError::ItemDeleted.into_remote(&host.alias));
    }
    let facts =
        hosts::facts(item.summary()).map_err(|problem| problem_failure(&host.alias, &problem))?;
    let password = item.login_password().ok().flatten();
    let refreshed = match vault_hosts::persist_refreshed(&item_key, &facts) {
        Ok(Some(stored)) => stored,
        _ => {
            let mut copy = host.clone();
            vault_hosts::apply(&facts, &item_key, &mut copy);
            copy
        }
    };
    session::touch(account.id()).map_err(unavailable)?;
    let session = vault
        .take_session()
        .ok_or_else(|| VaultError::Locked.into_remote(&account.profile.email))?;
    Ok(AdmittedLogin {
        host: refreshed,
        admission: Admission {
            item_id,
            revision: facts.revision,
            session_fingerprint: session::fingerprint(&session),
        },
        login: VaultLogin { session, password },
    })
}

/// Reads the login password of a vault-backed host from its item, with the
/// session an admission handed over or, without one, the stored session
/// (asking the user when the policy requires it).
pub(crate) async fn resolve_login_password(
    host: &HostProfile,
    session: Option<SessionKey>,
) -> Result<Zeroizing<String>, RemoteFailure> {
    let item_id = host
        .vault()
        .ok_or_else(|| RemoteFailure::new("BITWARDEN_UNAVAILABLE", "not a vault host"))?
        .item_id;
    let _turn = CLI_GATE.lock().await;
    let settings = settings()?;
    let account = Account::from_settings(&settings)?;
    let mut vault = match session {
        Some(session) => account.vault(Some(session)),
        None => {
            let mut vault = unlocked_or_prompt(&account).await?;
            ensure_fresh(&account, &mut vault, settings.sync_max_age_seconds).await?;
            vault
        }
    };
    let resolved = vault
        .resolve_password(item_id, Some(host.username.trim()))
        .await
        .map_err(|error| item_failure(error, &account, &host.alias))?;
    session::touch(account.id()).map_err(unavailable)?;
    Ok(resolved.password)
}

/// Lists the folder and mirrors it into the store (saved when something
/// changed); the listing is returned too, so a caller that shows it does
/// not start `bw` again. The caller provides an unlocked vault; the sync
/// window applies.
pub(crate) async fn refresh_store(
    account: &Account,
    vault: &mut Vault,
    max_age_seconds: u64,
) -> Result<(Reconciled, Vec<codex_hosts_bw::ItemSummary>), RemoteFailure> {
    let folder = folder_of(account)?;
    ensure_fresh(account, vault, max_age_seconds).await?;
    let items = vault
        .list_items(Some(folder))
        .await
        .map_err(|error| item_failure(error, account, &account.profile.folder_name))?;
    let _write = crate::storage::write_lock();
    let mut store = HostStore::load().map_err(unavailable)?;
    let result = hosts::reconcile(&mut store, &items, folder);
    if result.changed {
        store.save().map_err(unavailable)?;
    }
    session::touch(account.id()).map_err(unavailable)?;
    Ok((result, items))
}

/// Mirrors the folder before an alias lookup fails, so a host created in the
/// vault moments ago is found. Asks for the master password when the policy
/// requires it and keeps that key for the admission that follows. `Ok(false)`
/// when Bitwarden is off or not set up.
pub(crate) async fn discover() -> Result<bool, RemoteFailure> {
    let settings = settings()?;
    if !settings.available() {
        return Ok(false);
    }
    let _turn = CLI_GATE.lock().await;
    let account = Account::from_settings(&settings)?;
    let mut vault = unlocked_or_prompt(&account).await?;
    if let Some(key) = vault.session() {
        remember_session(key);
    }
    refresh_store(&account, &mut vault, settings.sync_max_age_seconds).await?;
    Ok(true)
}

/// Mirrors the folder for a listing when that costs the user nothing: a
/// stored key that is not spent by being read (never under "every call")
/// and no prompt. `Ok(None)` when it could not run.
pub(crate) async fn refresh_quietly() -> Result<Option<Reconciled>, RemoteFailure> {
    let settings = settings()?;
    if !settings.available() {
        return Ok(None);
    }
    let _turn = CLI_GATE.lock().await;
    let account = Account::from_settings(&settings)?;
    let Some(session) = session::load_non_consuming(&account.profile).map_err(unavailable)? else {
        return Ok(None);
    };
    let mut vault = account.vault(Some(session));
    refresh_store(&account, &mut vault, settings.sync_max_age_seconds)
        .await
        .map(|(reconciled, _)| Some(reconciled))
}
