//! Vault-backed hosts, whichever password manager holds them: the Login
//! items of one configured folder or vault, mirrored into the host store so
//! the window, the tools, and the session pool see them as ordinary profiles
//! with local state attached.
//!
//! The item is the source of truth. Its name is the alias; its first
//! `ssh://` or `telnet://` URL is the endpoint; its username and password
//! are the login; the codex-hosts `description` and `tags` custom fields
//! carry the remark and the tags. Everything else in the profile (host-key
//! trust, privilege settings, approval, retention, jump host, advanced
//! limits) is local and survives every refresh. Each provider module reads
//! its own item format into [`HostFacts`]; this module does the rest.

use std::collections::BTreeMap;
use std::future::Future;

use codex_hosts_bw::FieldEdit;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::model::{HostProfile, Protocol, VaultHostRef, VaultProvider, normalize_tags};
use crate::ssh::RemoteFailure;
use crate::storage::HostStore;
use crate::{bitwarden, onepassword};

/// Custom field holding the host description.
pub(crate) const DESCRIPTION_FIELD: &str = "description";
/// Custom field holding the tags, comma separated.
pub(crate) const TAGS_FIELD: &str = "tags";

/// Why an item is not served as a host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ItemProblem {
    /// Not a Login item.
    NotLogin,
    /// The name is empty.
    NoName,
    /// No `ssh://` or `telnet://` URL.
    NoHostUri,
    /// Neither the item nor its URL names a user.
    NoUsername,
    /// A local host or another item already uses the name.
    NameTaken,
}

impl ItemProblem {
    /// The reason in words, for tool errors.
    pub(crate) fn reason(&self) -> &'static str {
        match self {
            Self::NotLogin => "it is no longer a Login item",
            Self::NoName => "its name is empty",
            Self::NoHostUri => "it has no ssh:// or telnet:// URI",
            Self::NoUsername => "neither the item nor its URI names a user",
            Self::NameTaken => "its name is used by another host",
        }
    }
}

/// What a Login item says about its host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HostFacts {
    pub(crate) alias: String,
    pub(crate) protocol: Protocol,
    pub(crate) address: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) description: String,
    pub(crate) tags: Vec<String>,
    pub(crate) revision: String,
}

/// Splits the tags field (commas or whitespace).
pub(crate) fn parse_tags(text: &str) -> Vec<String> {
    normalize_tags(text.split([',', ';', '\n']).flat_map(str::split_whitespace))
}

/// Joins tags for the tags field.
pub(crate) fn format_tags(tags: &[String]) -> String {
    normalize_tags(tags).join(", ")
}

/// The first line of the description field, trimmed.
pub(crate) fn description_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_owned()
}

/// One item of one password manager, as a profile refers to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ItemKey {
    pub(crate) provider: VaultProvider,
    /// Also the profile id, so Credential Manager records (sudo passwords)
    /// and pool keys stay stable even if the mirror is rebuilt.
    pub(crate) id: Uuid,
    /// The provider's own id when it is not `id`.
    pub(crate) external_id: Option<String>,
}

impl ItemKey {
    pub(crate) fn bitwarden(id: Uuid) -> Self {
        Self {
            provider: VaultProvider::Bitwarden,
            id,
            external_id: None,
        }
    }

    /// The item `host` mirrors, for a vault-backed host.
    pub(crate) fn of(host: &HostProfile) -> Option<Self> {
        host.vault().map(|vault| Self {
            provider: vault.provider,
            id: vault.item_id,
            external_id: vault.external_id.clone(),
        })
    }
}

/// What the editor wants written into a host's item, for any provider. No
/// `Debug`: the password edit is a secret.
pub(crate) struct HostWrite {
    pub(crate) provider: VaultProvider,
    /// The host's id; `None` creates the item.
    pub(crate) item_id: Option<Uuid>,
    /// The provider's own id of an existing item, when it is not `item_id`.
    pub(crate) external_id: Option<String>,
    pub(crate) name: String,
    pub(crate) host_uri: String,
    pub(crate) username: String,
    pub(crate) password: FieldEdit<Zeroizing<String>>,
    pub(crate) description: String,
    pub(crate) tags: Vec<String>,
    /// The revision the editor read; a different one in the vault means
    /// someone else changed the item meanwhile.
    pub(crate) expected_revision: String,
}

impl HostWrite {
    /// The same write without the password, kept while a job runs.
    pub(crate) fn clone_without_password(&self) -> Self {
        Self {
            provider: self.provider,
            item_id: self.item_id,
            external_id: self.external_id.clone(),
            name: self.name.clone(),
            host_uri: self.host_uri.clone(),
            username: self.username.clone(),
            password: FieldEdit::Keep,
            description: self.description.clone(),
            tags: self.tags.clone(),
            expected_revision: self.expected_revision.clone(),
        }
    }

    /// The codex-hosts fields by name; empty ones are left out.
    pub(crate) fn codex_fields(&self) -> BTreeMap<String, String> {
        let mut fields = BTreeMap::new();
        if !self.description.trim().is_empty() {
            fields.insert(
                DESCRIPTION_FIELD.to_owned(),
                self.description.trim().to_owned(),
            );
        }
        if !self.tags.is_empty() {
            fields.insert(TAGS_FIELD.to_owned(), format_tags(&self.tags));
        }
        fields
    }
}

/// Why writing a host's item did not happen.
pub(crate) enum WriteFailure {
    /// The item changed in the vault since the editor read it.
    Conflict,
    /// The written item does not describe a host after all.
    Problem(ItemProblem),
    Remote(RemoteFailure),
}

impl From<RemoteFailure> for WriteFailure {
    fn from(error: RemoteFailure) -> Self {
        Self::Remote(error)
    }
}

/// Writes the facts into a mirror profile; local state (including the SSH
/// authentication method: a vault host may log in with a local key or the
/// agent, the item password serves password logins) is untouched. A changed
/// endpoint drops the host-key trust, as the editor does, so the next
/// connection verifies the new server afresh.
pub(crate) fn apply(facts: &HostFacts, item: &ItemKey, host: &mut HostProfile) -> bool {
    let before = host.clone();
    let endpoint_changed = host.address.trim() != facts.address
        || host.port != facts.port
        || host.protocol != facts.protocol;
    host.alias = facts.alias.clone();
    host.description = facts.description.clone();
    host.tags = facts.tags.clone();
    host.address = facts.address.clone();
    host.port = facts.port;
    host.protocol = facts.protocol;
    host.username = facts.username.clone();
    host.login_password_id = None;
    host.vault = Some(VaultHostRef {
        provider: item.provider,
        item_id: item.id,
        external_id: item.external_id.clone(),
        item_name: facts.alias.clone(),
        revision_date: facts.revision.clone(),
    });
    if endpoint_changed {
        host.host_fingerprint = None;
        host.host_key_algorithm = None;
        host.host_key_first_seen_unix = None;
        host.host_key_last_verified_unix = None;
        host.verified = false;
    }
    *host != before
}

/// A fresh mirror for an item seen for the first time; the profile id is
/// the item key's id.
pub(crate) fn mirror(facts: &HostFacts, item: &ItemKey) -> HostProfile {
    let mut host = HostProfile::new(facts.alias.clone());
    host.id = item.id;
    apply(facts, item, &mut host);
    host
}

/// One item of a listing, read by its provider.
#[derive(Clone, Debug)]
pub(crate) struct Listed {
    pub(crate) key: ItemKey,
    /// The item name as the provider shows it, for problem reports.
    pub(crate) name: String,
    pub(crate) facts: Result<HostFacts, ItemProblem>,
}

/// Result of bringing the store's mirrors in line with a listing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Reconciled {
    /// Whether the store needs saving.
    pub(crate) changed: bool,
    /// Number of the provider's hosts after the pass.
    pub(crate) hosts: usize,
    /// Items that are not served, by name.
    pub(crate) problems: Vec<(String, ItemProblem)>,
}

/// Mirrors `listed` (every item of `provider`'s folder or vault) into
/// `store.hosts`: updates or creates one profile per usable Login item,
/// drops that provider's mirrors whose item is gone, and leaves local hosts
/// and other providers' hosts alone. Names are compared like aliases
/// (trimmed, ASCII case-insensitive); a clash with any other host or an
/// already-served item is reported, not resolved.
pub(crate) fn reconcile(
    store: &mut HostStore,
    provider: VaultProvider,
    mut listed: Vec<Listed>,
) -> Reconciled {
    let mut result = Reconciled::default();
    let mut taken: Vec<String> = store
        .hosts
        .iter()
        .filter(|host| host.vault_provider() != Some(provider))
        .map(|host| host.alias.trim().to_ascii_lowercase())
        .collect();
    // Items already mirrored keep their name against newcomers.
    listed.sort_by_key(|item| !store.hosts.iter().any(|host| host.id == item.key.id));
    let mut accepted: Vec<(ItemKey, HostFacts)> = Vec::new();
    for item in listed {
        match item.facts {
            Ok(facts) => {
                let key = facts.alias.to_ascii_lowercase();
                if taken.contains(&key) {
                    result.problems.push((item.name, ItemProblem::NameTaken));
                } else {
                    taken.push(key);
                    accepted.push((item.key, facts));
                }
            }
            Err(problem) => result.problems.push((item.name, problem)),
        }
    }
    let before = store.hosts.len();
    store.hosts.retain(|host| {
        host.vault_provider() != Some(provider)
            || accepted.iter().any(|(item, _)| item.id == host.id)
    });
    result.changed |= store.hosts.len() != before;
    for (item, facts) in &accepted {
        match store.hosts.iter_mut().find(|host| host.id == item.id) {
            Some(host) => result.changed |= apply(facts, item, host),
            None => {
                store.hosts.push(mirror(facts, item));
                result.changed = true;
            }
        }
    }
    result.hosts = accepted.len();
    result
}

/// Writes the refreshed facts of one item into the stored mirror and returns
/// the stored profile. A renamed item whose new name clashes with another
/// host keeps its previous alias in the store. `None` when the mirror is
/// not in the store (removed meanwhile).
pub(crate) fn persist_refreshed(
    item: &ItemKey,
    facts: &HostFacts,
) -> Result<Option<HostProfile>, String> {
    let _write = crate::storage::write_lock();
    let mut store = HostStore::load().map_err(|error| error.to_string())?;
    let clash = store
        .hosts
        .iter()
        .any(|host| host.id != item.id && host.alias.trim().eq_ignore_ascii_case(&facts.alias));
    let Some(host) = store.hosts.iter_mut().find(|host| host.id == item.id) else {
        return Ok(None);
    };
    let previous_alias = host.alias.clone();
    let mut changed = apply(facts, item, host);
    if clash {
        host.alias = previous_alias;
        changed = true;
    }
    let refreshed = host.clone();
    if changed {
        store.save().map_err(|error| error.to_string())?;
    }
    Ok(Some(refreshed))
}

/// Removes the mirror of an item that no longer exists; best effort.
pub(crate) fn drop_mirror(id: Uuid) {
    let _write = crate::storage::write_lock();
    if let Ok(mut store) = HostStore::load() {
        let before = store.hosts.len();
        store.hosts.retain(|host| host.id != id);
        if store.hosts.len() != before {
            let _ = store.save();
        }
    }
}

/// Admission for one host operation. It changes whenever the item does (and,
/// for Bitwarden, whenever the session key does), so a retained SSH session
/// is not reused after the item changed elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Admission {
    Bitwarden(bitwarden::Admission),
    OnePassword { item_id: Uuid, revision: String },
}

/// What the connection authenticates with, read by the admission so the
/// connection does not start the CLI again. No `Debug`.
pub(crate) enum VaultLogin {
    Bitwarden(bitwarden::VaultLogin),
    OnePassword(Option<Zeroizing<String>>),
}

impl VaultLogin {
    /// The password the admission read, if the item has one.
    pub(crate) fn password(&self) -> Option<&Zeroizing<String>> {
        match self {
            Self::Bitwarden(login) => login.password.as_ref(),
            Self::OnePassword(password) => password.as_ref(),
        }
    }

    pub(crate) fn take_password(&mut self) -> Option<Zeroizing<String>> {
        match self {
            Self::Bitwarden(login) => login.password.take(),
            Self::OnePassword(password) => password.take(),
        }
    }
}

/// An admitted login: the host as the item describes it right now, the
/// pool key material, and what the connection authenticates with.
pub(crate) struct AdmittedLogin {
    pub(crate) host: HostProfile,
    pub(crate) admission: Admission,
    pub(crate) login: VaultLogin,
}

fn not_a_vault_host() -> RemoteFailure {
    RemoteFailure::new(
        "PROFILE_INVALID",
        "The host is not defined in a password manager.",
    )
}

/// Admits a login operation for a vault-backed host through its password
/// manager. Every call goes through here, including one that reuses a
/// retained session.
pub(crate) async fn admit(host: &HostProfile) -> Result<AdmittedLogin, RemoteFailure> {
    match host.vault_provider() {
        Some(VaultProvider::Bitwarden) => {
            let admitted = bitwarden::admit(host).await?;
            let host = admitted.host.clone();
            let admission = Admission::Bitwarden(admitted.admission.clone());
            Ok(AdmittedLogin {
                host,
                admission,
                login: VaultLogin::Bitwarden(admitted.into_login()),
            })
        }
        Some(VaultProvider::OnePassword) => onepassword::admit(host).await,
        None => Err(not_a_vault_host()),
    }
}

/// An approved call connects only to the endpoint and login the approval
/// showed. When admission finds that the item moved meanwhile, the refreshed
/// mirror is already saved, so the next call's approval shows the new target.
pub(crate) fn keep_approved_target(
    shown: &HostProfile,
    admitted: &HostProfile,
    approved: bool,
) -> Result<(), RemoteFailure> {
    let same = shown.protocol == admitted.protocol
        && shown.address == admitted.address
        && shown.port == admitted.port
        && shown.username == admitted.username;
    if !approved || same {
        return Ok(());
    }
    Err(RemoteFailure::new(
        "APPROVAL_TARGET_CHANGED",
        format!(
            "The password manager item for {} changed its endpoint or login after the call was approved; nothing was sent. Call again to review the new target.",
            shown.alias
        ),
    ))
}

/// Reads the login password of a vault-backed host, reusing what an
/// admission handed over when it can.
pub(crate) async fn resolve_login_password(
    host: &HostProfile,
    login: Option<VaultLogin>,
) -> Result<Zeroizing<String>, RemoteFailure> {
    match host.vault_provider() {
        Some(VaultProvider::Bitwarden) => {
            let session = login.and_then(|login| match login {
                VaultLogin::Bitwarden(login) => Some(login.session),
                VaultLogin::OnePassword(_) => None,
            });
            bitwarden::resolve_login_password(host, session).await
        }
        Some(VaultProvider::OnePassword) => onepassword::resolve_login_password(host).await,
        None => Err(not_a_vault_host()),
    }
}

/// Mirrors every password manager that is set up before an alias lookup
/// fails, so a host created there moments ago is found. `Ok(false)` when
/// none ran.
pub(crate) fn discover(
    store: &HostStore,
) -> impl Future<Output = Result<bool, RemoteFailure>> + Send + use<> {
    // Only the flags cross the await: the store is not `Sync`.
    let (with_bitwarden, with_onepassword) =
        (store.bitwarden.available(), store.onepassword.available());
    async move {
        let mut ran = false;
        if with_bitwarden {
            ran |= bitwarden::discover().await?;
        }
        if with_onepassword {
            ran |= onepassword::discover().await?;
        }
        Ok(ran)
    }
}

/// Mirrors for a listing where that costs the user nothing: Bitwarden with
/// a stored key. 1Password is not listed here, because an unauthorized
/// process would make the app ask. `true` when the store changed.
pub(crate) fn refresh_quietly(store: &HostStore) -> impl Future<Output = bool> + Send + use<> {
    let with_bitwarden = store.bitwarden.available();
    async move { with_bitwarden && matches!(bitwarden::refresh_quietly().await, Ok(Some(_))) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_approved_call_refuses_a_target_the_vault_moved_after_approval() {
        let shown = HostProfile {
            alias: "web".into(),
            address: "10.0.0.5".into(),
            port: 22,
            username: "alice".into(),
            ..HostProfile::default()
        };
        assert!(keep_approved_target(&shown, &shown.clone(), true).is_ok());
        let moves = [
            HostProfile {
                username: "root".into(),
                ..shown.clone()
            },
            HostProfile {
                address: "10.0.0.6".into(),
                ..shown.clone()
            },
            HostProfile {
                port: 2222,
                ..shown.clone()
            },
            HostProfile {
                protocol: Protocol::Telnet,
                ..shown.clone()
            },
        ];
        for moved in &moves {
            let error = keep_approved_target(&shown, moved, true).unwrap_err();
            assert_eq!(error.code, "APPROVAL_TARGET_CHANGED");
            // Without an approval the vault item applies at once, as documented.
            assert!(keep_approved_target(&shown, moved, false).is_ok());
        }
    }

    fn facts(alias: &str, address: &str) -> HostFacts {
        HostFacts {
            alias: alias.to_owned(),
            protocol: Protocol::Ssh,
            address: address.to_owned(),
            port: 22,
            username: "u".to_owned(),
            description: String::new(),
            tags: Vec::new(),
            revision: "1".to_owned(),
        }
    }

    fn listed(provider: VaultProvider, id: u8, alias: &str) -> Listed {
        Listed {
            key: ItemKey {
                provider,
                id: Uuid::from_u128(u128::from(id)),
                external_id: (provider == VaultProvider::OnePassword).then(|| format!("op{id}")),
            },
            name: alias.to_owned(),
            facts: Ok(facts(alias, "h")),
        }
    }

    #[test]
    fn tags_and_descriptions_are_normalized() {
        assert_eq!(parse_tags(" a,b;c\nd  e "), ["a", "b", "c", "d", "e"]);
        assert_eq!(
            format_tags(&["b".to_owned(), "a".to_owned(), "b".to_owned()]),
            "b, a"
        );
        assert_eq!(description_line(" front door \nsecond"), "front door");
    }

    #[test]
    fn reconcile_keeps_to_its_own_provider() {
        let mut store = HostStore::default();
        store.hosts.push(HostProfile::new("local".to_owned()));
        let bitwarden = vec![listed(VaultProvider::Bitwarden, 1, "shared-name")];
        assert!(reconcile(&mut store, VaultProvider::Bitwarden, bitwarden).changed);

        let onepassword = vec![
            listed(VaultProvider::OnePassword, 2, "Shared-Name"),
            listed(VaultProvider::OnePassword, 3, "LOCAL"),
            listed(VaultProvider::OnePassword, 4, "op-only"),
        ];
        let result = reconcile(&mut store, VaultProvider::OnePassword, onepassword);
        assert_eq!(result.hosts, 1);
        assert_eq!(
            result.problems,
            vec![
                ("Shared-Name".to_owned(), ItemProblem::NameTaken),
                ("LOCAL".to_owned(), ItemProblem::NameTaken),
            ]
        );
        let op = store.find_alias("op-only").unwrap();
        assert_eq!(op.vault_provider(), Some(VaultProvider::OnePassword));
        assert_eq!(op.vault().unwrap().external_id.as_deref(), Some("op4"));

        // An empty 1Password listing removes only 1Password mirrors.
        let gone = reconcile(&mut store, VaultProvider::OnePassword, Vec::new());
        assert!(gone.changed);
        assert!(store.find_alias("shared-name").unwrap().is_bitwarden_host());
        assert!(store.find_alias("local").is_some());
        assert!(store.find_alias("op-only").is_none());
    }

    #[test]
    fn older_stores_read_as_bitwarden_and_are_written_unchanged() {
        let old = r#"{"item_id":"00000000-0000-0000-0000-000000000009","item_name":"box","revision_date":"r"}"#;
        let read: VaultHostRef = serde_json::from_str(old).unwrap();
        assert_eq!(read.provider, VaultProvider::Bitwarden);
        assert_eq!(read.external_id, None);
        assert_eq!(serde_json::to_string(&read).unwrap(), old);

        let op = VaultHostRef {
            provider: VaultProvider::OnePassword,
            external_id: Some("abc".to_owned()),
            ..read
        };
        let written = serde_json::to_string(&op).unwrap();
        assert!(written.contains(r#""provider":"onepassword""#));
        assert_eq!(serde_json::from_str::<VaultHostRef>(&written).unwrap(), op);
    }

    #[test]
    fn a_moved_endpoint_drops_the_trust_and_local_state_survives() {
        let item = ItemKey::bitwarden(Uuid::from_u128(9));
        let mut host = mirror(&facts("box", "10.0.0.5"), &item);
        assert_eq!(host.id, item.id);
        host.advanced.require_call_approval = true;
        host.host_fingerprint = Some("SHA256:old".to_owned());
        host.verified = true;
        assert!(!apply(&facts("box", "10.0.0.5"), &item, &mut host));
        assert!(host.verified);
        assert!(apply(&facts("box", "10.0.0.6"), &item, &mut host));
        assert!(!host.verified && host.host_fingerprint.is_none());
        assert!(host.advanced.require_call_approval);
    }
}
