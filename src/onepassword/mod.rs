//! 1Password as a credential store inside codex-hosts.
//!
//! The Login items of one vault are vault-backed hosts, read through the
//! official `op.exe` ([`cli`]): title = alias, first `ssh://` or `telnet://`
//! URL = endpoint, username and password = login, and the `codex-hosts`
//! section's `description` and `tags` fields. The window creates, edits and
//! archives those items like Bitwarden's. Signing in, unlocking, and
//! authorizing each process (Windows Hello) are the 1Password app's;
//! codex-hosts keeps no 1Password credential, session, or re-authentication
//! policy. Listing never reads a password: `op item list` carries no
//! secrets, so the description and tags of a listed host stay as last read
//! from its item and are refreshed whenever the host is used.

pub(crate) mod cli;

use tokio::sync::Mutex;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::model::{HostProfile, OnePasswordAccount, OnePasswordSettings, Protocol, VaultProvider};
use crate::ssh::RemoteFailure;
use crate::storage::HostStore;
use crate::vault_hosts::{
    self, Admission, AdmittedLogin, DESCRIPTION_FIELD, HostFacts, HostWrite, ItemKey, ItemProblem,
    Listed, Reconciled, TAGS_FIELD, VaultLogin, WriteFailure,
};
use cli::{OpCall, OpError, OpExecutable, OpItem, OpItemSummary, OpRunner, RealRunner};

/// One prompt at a time: calls from this process take turns, so the first
/// one's Windows Hello authorization serves the rest.
static CLI_GATE: Mutex<()> = Mutex::const_new(());

/// Namespace of the profile ids derived from 1Password item ids.
const ITEM_NAMESPACE: Uuid = Uuid::from_u128(0x6f1d_0c2e_8a54_4c7e_9b3f_2d61_a0e4_51c7);

impl ItemKey {
    /// The key of a 1Password item; the profile id is derived from the
    /// account and the item id, so it is stable across refreshes.
    pub(crate) fn onepassword(account_id: &str, item_id: &str) -> Self {
        Self {
            provider: VaultProvider::OnePassword,
            id: Uuid::new_v5(
                &ITEM_NAMESPACE,
                format!("{account_id}/{item_id}").as_bytes(),
            ),
            external_id: Some(item_id.to_owned()),
        }
    }
}

/// `op` with the configured account and vault.
pub(crate) struct Op<R: OpRunner = RealRunner> {
    runner: R,
    exe: OpExecutable,
    pub(crate) account: OnePasswordAccount,
}

impl Op {
    /// The configured account, or the reason 1Password hosts cannot be
    /// served right now.
    pub(crate) fn from_settings(settings: &OnePasswordSettings) -> Result<Self, RemoteFailure> {
        if !settings.enabled {
            return Err(RemoteFailure::new(
                "ONEPASSWORD_DISABLED",
                "1Password is switched off in codex-hosts; enable it in the credential manager.",
            ));
        }
        let (Some(account), Some(cli_path)) = (&settings.account, &settings.cli_path) else {
            return Err(RemoteFailure::new(
                "ONEPASSWORD_NOT_CONFIGURED",
                "1Password is not set up in codex-hosts; choose its vault in the credential manager first.",
            ));
        };
        let exe = OpExecutable::new(cli_path).map_err(|error| failure(error, ""))?;
        Ok(Self::new(RealRunner, exe, account.clone()))
    }
}

impl<R: OpRunner> Op<R> {
    pub(crate) fn new(runner: R, exe: OpExecutable, account: OnePasswordAccount) -> Self {
        Self {
            runner,
            exe,
            account,
        }
    }

    async fn run(
        &self,
        make: impl Fn() -> Result<OpCall, OpError>,
    ) -> Result<Zeroizing<Vec<u8>>, OpError> {
        run_once_more(&self.runner, &self.exe, make).await
    }

    pub(crate) fn item_key(&self, item_id: &str) -> ItemKey {
        ItemKey::onepassword(&self.account.account_id, item_id)
    }

    /// The vault's items; nothing secret is read.
    pub(crate) async fn list_items(&self) -> Result<Vec<OpItemSummary>, OpError> {
        let (account, vault) = (&self.account.account_id, &self.account.vault_id);
        let stdout = self.run(|| OpCall::item_list(account, vault)).await?;
        cli::parse_item_list(&stdout)
    }

    /// One whole item, password included.
    pub(crate) async fn get_item(&self, item_id: &str) -> Result<OpItem, OpError> {
        let (account, vault) = (&self.account.account_id, &self.account.vault_id);
        let stdout = self
            .run(|| OpCall::item_get(account, vault, item_id))
            .await?;
        cli::parse_item(stdout)
    }

    /// Writes the host into its item, or creates the item. An existing item
    /// whose version is not the one the editor read is left alone.
    pub(crate) async fn save(&self, write: &HostWrite) -> Result<OpItem, WriteFailure> {
        let (account, vault) = (&self.account.account_id, &self.account.vault_id);
        let mut codex = write.codex_fields();
        let section: Vec<(&str, String)> = [DESCRIPTION_FIELD, TAGS_FIELD]
            .into_iter()
            .map(|label| (label, codex.remove(label).unwrap_or_default()))
            .collect();
        let fields = cli::ItemFields {
            title: &write.name,
            host_uri: &write.host_uri,
            username: &write.username,
            password: &write.password,
            section: &section,
        };
        let alias = write.name.as_str();
        let stdout = match write.external_id.as_deref() {
            None => {
                let json = cli::new_item_json(&fields);
                self.run(|| OpCall::item_create(account, vault, json.clone()))
                    .await
                    .map_err(|error| failure(error, alias))?
            }
            Some(item_id) => {
                let current = self
                    .get_item(item_id)
                    .await
                    .map_err(|error| failure(error, alias))?;
                if !write.expected_revision.is_empty()
                    && current.summary().version.to_string() != write.expected_revision
                {
                    return Err(WriteFailure::Conflict);
                }
                let json = current
                    .patched(&fields)
                    .map_err(|error| failure(error, alias))?;
                self.run(|| OpCall::item_edit(account, vault, item_id, json.clone()))
                    .await
                    .map_err(|error| failure(error, alias))?
            }
        };
        cli::parse_item(stdout).map_err(|error| failure(error, alias).into())
    }

    /// Moves the item to the 1Password archive; an item already gone is
    /// fine.
    pub(crate) async fn archive(&self, item_id: &str) -> Result<(), OpError> {
        let (account, vault) = (&self.account.account_id, &self.account.vault_id);
        match self
            .run(|| OpCall::item_archive(account, vault, item_id))
            .await
        {
            Ok(_) | Err(OpError::ItemMissing) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// Saves a host the window edited into its 1Password item (or a new one)
/// and reports the item as the host it now describes.
pub(crate) async fn save_item(write: &HostWrite) -> Result<(ItemKey, HostFacts), WriteFailure> {
    let _turn = CLI_GATE.lock().await;
    let op = Op::from_settings(&settings()?)?;
    save_with(&op, write).await
}

async fn save_with<R: OpRunner>(
    op: &Op<R>,
    write: &HostWrite,
) -> Result<(ItemKey, HostFacts), WriteFailure> {
    let item = op.save(write).await?;
    let facts = item_facts(&item).map_err(WriteFailure::Problem)?;
    Ok((op.item_key(&item.summary().id), facts))
}

/// Archives the item behind a 1Password host.
pub(crate) async fn archive_item(host: &HostProfile) -> Result<(), RemoteFailure> {
    let _turn = CLI_GATE.lock().await;
    let op = Op::from_settings(&settings()?)?;
    let item_id = external_id(host)?;
    op.archive(&item_id)
        .await
        .map_err(|error| failure(error, &host.alias))
}

/// Creates a vault for the hosts, during setup.
pub(crate) async fn create_vault(
    exe: &OpExecutable,
    account: &str,
    name: &str,
) -> Result<cli::OpVault, OpError> {
    let _turn = CLI_GATE.lock().await;
    let stdout = run_once_more(&RealRunner, exe, || OpCall::vault_create(account, name)).await?;
    cli::parse_vault(&stdout)
}

/// Runs `call` once more when the first attempt could not reach the app.
async fn run_once_more<R: OpRunner>(
    runner: &R,
    exe: &OpExecutable,
    make: impl Fn() -> Result<OpCall, OpError>,
) -> Result<Zeroizing<Vec<u8>>, OpError> {
    let mut retried = false;
    loop {
        let output = runner.run(exe, make()?).await.map_err(cli::run_failure)?;
        match cli::outcome(output) {
            Err(error) if error.retry_once() && !retried => retried = true,
            other => return other,
        }
    }
}

/// Checks that `exe` is the 2.x CLI and lists the accounts the app shares
/// with it; none means the app's CLI integration is off. Asks nothing.
pub(crate) async fn check_cli(
    exe: &OpExecutable,
) -> Result<(String, Vec<cli::OpAccount>), OpError> {
    let version = run_once_more(&RealRunner, exe, || Ok(OpCall::version())).await?;
    let (major, minor, patch) = cli::parse_version(&version)?;
    let accounts = run_once_more(&RealRunner, exe, || Ok(OpCall::account_list())).await?;
    let accounts = cli::parse_accounts(&accounts)?;
    if accounts.is_empty() {
        return Err(OpError::IntegrationOff);
    }
    Ok((format!("{major}.{minor}.{patch}"), accounts))
}

/// The vaults of `account`; the first call of this process makes the app
/// ask the user.
pub(crate) async fn list_vaults(
    exe: &OpExecutable,
    account: &str,
) -> Result<Vec<cli::OpVault>, OpError> {
    let _turn = CLI_GATE.lock().await;
    let stdout = run_once_more(&RealRunner, exe, || OpCall::vault_list(account)).await?;
    cli::parse_vaults(&stdout)
}

/// What the item's list entry says about its host. The description and tags
/// are not in the list; `known` (the current mirror) supplies them.
pub(crate) fn listed_facts(
    summary: &OpItemSummary,
    known: Option<&HostProfile>,
) -> Result<HostFacts, ItemProblem> {
    let mut facts = base_facts(summary, summary.additional_information.clone())?;
    if let Some(known) = known {
        facts.description = known.description.clone();
        facts.tags = known.tags.clone();
    }
    Ok(facts)
}

/// What the whole item says about its host.
pub(crate) fn item_facts(item: &OpItem) -> Result<HostFacts, ItemProblem> {
    let username = item.username().map_err(|_| ItemProblem::NotLogin)?;
    let mut facts = base_facts(item.summary(), username)?;
    facts.description = vault_hosts::description_line(
        &item
            .codex_field(DESCRIPTION_FIELD)
            .ok()
            .flatten()
            .unwrap_or_default(),
    );
    facts.tags = vault_hosts::parse_tags(
        &item
            .codex_field(TAGS_FIELD)
            .ok()
            .flatten()
            .unwrap_or_default(),
    );
    Ok(facts)
}

fn base_facts(summary: &OpItemSummary, username: Option<String>) -> Result<HostFacts, ItemProblem> {
    if !summary.is_login() {
        return Err(ItemProblem::NotLogin);
    }
    let alias = summary.title.trim();
    if alias.is_empty() {
        return Err(ItemProblem::NoName);
    }
    let uri = summary.host_uri().ok_or(ItemProblem::NoHostUri)?;
    let username = username
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .or(uri.username.clone())
        .ok_or(ItemProblem::NoUsername)?;
    let protocol = match uri.scheme {
        codex_hosts_bw::UriScheme::Ssh => Protocol::Ssh,
        codex_hosts_bw::UriScheme::Telnet => Protocol::Telnet,
    };
    Ok(HostFacts {
        alias: alias.to_owned(),
        protocol,
        address: uri.host.clone(),
        port: uri.port_or_default(),
        username,
        description: String::new(),
        tags: Vec::new(),
        revision: summary.version.to_string(),
    })
}

/// Mirrors the vault's listing into `store.hosts`.
pub(crate) fn reconcile<R: OpRunner>(
    op: &Op<R>,
    store: &mut HostStore,
    items: &[OpItemSummary],
) -> Reconciled {
    let listed = items
        .iter()
        .filter(|item| item.vault.id == op.account.vault_id)
        .map(|item| {
            let key = op.item_key(&item.id);
            let known = store.hosts.iter().find(|host| host.id == key.id);
            Listed {
                facts: listed_facts(item, known),
                key,
                name: item.title.clone(),
            }
        })
        .collect();
    vault_hosts::reconcile(store, VaultProvider::OnePassword, listed)
}

/// A user-facing failure: only what the user can do about it.
pub(crate) fn failure(error: OpError, alias: &str) -> RemoteFailure {
    let message = match &error {
        OpError::NotOp => "The chosen file is not the 1Password CLI (op.exe 2.x); choose it again in the codex-hosts credential manager.".to_owned(),
        OpError::IntegrationOff => "Turn on Settings > Developer > Integrate with 1Password CLI in the 1Password app, then try again.".to_owned(),
        OpError::AppUnavailable => "Open the 1Password app, then try again.".to_owned(),
        OpError::NotAuthorized => "1Password did not allow the request: unlock the 1Password app and approve its prompt, then try again.".to_owned(),
        OpError::ItemMissing => format!("The 1Password item behind {alias} no longer exists."),
        OpError::BadId | OpError::BadName | OpError::Parse | OpError::Cli { .. } | OpError::Run(_) => {
            format!("1Password could not be read ({error}).")
        }
    };
    RemoteFailure::new(error.code(), message)
}

fn problem_failure(alias: &str, problem: &ItemProblem) -> RemoteFailure {
    RemoteFailure::new(
        "ONEPASSWORD_ITEM_MISMATCH",
        format!(
            "The 1Password item behind {alias} no longer describes a host: {}.",
            problem.reason()
        ),
    )
}

fn settings() -> Result<OnePasswordSettings, RemoteFailure> {
    HostStore::load_recovering()
        .map(|store| store.onepassword)
        .map_err(|error| RemoteFailure::new("ONEPASSWORD_UNAVAILABLE", error.to_string()))
}

fn external_id(host: &HostProfile) -> Result<String, RemoteFailure> {
    host.vault()
        .and_then(|vault| vault.external_id.clone())
        .ok_or_else(|| RemoteFailure::new("ONEPASSWORD_UNAVAILABLE", "not a 1Password host"))
}

/// Reads the item and checks it still describes the host; a removed item
/// drops its mirror.
async fn read_item<R: OpRunner>(op: &Op<R>, host: &HostProfile) -> Result<OpItem, RemoteFailure> {
    let item_id = external_id(host)?;
    match op.get_item(&item_id).await {
        Ok(item) => Ok(item),
        Err(OpError::ItemMissing) => {
            vault_hosts::drop_mirror(host.id);
            Err(failure(OpError::ItemMissing, &host.alias))
        }
        Err(error) => Err(failure(error, &host.alias)),
    }
}

/// Admits a login operation for a 1Password host: reads the item (the app
/// asks the user when this process is not authorized yet) and refreshes the
/// mirror, so the connection uses the endpoint, account and password the
/// vault holds now.
pub(crate) async fn admit(host: &HostProfile) -> Result<AdmittedLogin, RemoteFailure> {
    let _turn = CLI_GATE.lock().await;
    let op = Op::from_settings(&settings()?)?;
    let item = read_item(&op, host).await?;
    let facts = item_facts(&item).map_err(|problem| problem_failure(&host.alias, &problem))?;
    let password = item
        .password()
        .map_err(|error| failure(error, &host.alias))?;
    let key = op.item_key(&item.summary().id);
    let refreshed = match vault_hosts::persist_refreshed(&key, &facts) {
        Ok(Some(stored)) => stored,
        _ => {
            let mut copy = host.clone();
            vault_hosts::apply(&facts, &key, &mut copy);
            copy
        }
    };
    Ok(AdmittedLogin {
        host: refreshed,
        admission: Admission::OnePassword {
            item_id: key.id,
            revision: facts.revision,
        },
        login: VaultLogin::OnePassword(password),
    })
}

/// Reads the login password of a 1Password host from its item; the item's
/// username must still be the host's.
pub(crate) async fn resolve_login_password(
    host: &HostProfile,
) -> Result<Zeroizing<String>, RemoteFailure> {
    let _turn = CLI_GATE.lock().await;
    let op = Op::from_settings(&settings()?)?;
    let item = read_item(&op, host).await?;
    let facts = item_facts(&item).map_err(|problem| problem_failure(&host.alias, &problem))?;
    if facts.username != host.username.trim() {
        return Err(RemoteFailure::new(
            "ONEPASSWORD_ITEM_MISMATCH",
            format!(
                "The 1Password item behind {} now names another user; use the host again so it is refreshed.",
                host.alias
            ),
        ));
    }
    item.password()
        .map_err(|error| failure(error, &host.alias))?
        .filter(|password| !password.is_empty())
        .ok_or_else(|| {
            RemoteFailure::new(
                "ONEPASSWORD_ITEM_MISSING",
                format!("The 1Password item behind {} has no password.", host.alias),
            )
        })
}

/// Lists the vault and mirrors it into the store (saved when something
/// changed); the listing is returned too, so a caller that shows it does
/// not start `op` again.
pub(crate) async fn refresh_store<R: OpRunner>(
    op: &Op<R>,
) -> Result<(Reconciled, Vec<OpItemSummary>), RemoteFailure> {
    let items = op
        .list_items()
        .await
        .map_err(|error| failure(error, &op.account.vault_name))?;
    let _write = crate::storage::write_lock();
    let mut store = HostStore::load()
        .map_err(|error| RemoteFailure::new("ONEPASSWORD_UNAVAILABLE", error.to_string()))?;
    let result = reconcile(op, &mut store, &items);
    if result.changed {
        store
            .save()
            .map_err(|error| RemoteFailure::new("ONEPASSWORD_UNAVAILABLE", error.to_string()))?;
    }
    Ok((result, items))
}

/// Mirrors the vault before an alias lookup fails, so a host created in
/// 1Password moments ago is found. `Ok(false)` when 1Password is off or not
/// set up.
pub(crate) async fn discover() -> Result<bool, RemoteFailure> {
    let settings = settings()?;
    if !settings.available() {
        return Ok(false);
    }
    let _turn = CLI_GATE.lock().await;
    let op = Op::from_settings(&settings)?;
    refresh_store(&op).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::cli::test_support::*;
    use super::*;

    fn account() -> OnePasswordAccount {
        OnePasswordAccount {
            account_id: ACCOUNT.to_owned(),
            url: "my.1password.com".to_owned(),
            email: String::new(),
            vault_id: VAULT.to_owned(),
            vault_name: "codex-hosts-test".to_owned(),
        }
    }

    const OTHER: &str = "testother00000000000000000";

    #[test]
    fn item_keys_are_stable_and_keep_the_op_id() {
        let key = ItemKey::onepassword(ACCOUNT, ITEM);
        assert_eq!(key, ItemKey::onepassword(ACCOUNT, ITEM));
        assert_ne!(key.id, ItemKey::onepassword(ACCOUNT, OTHER).id);
        assert_ne!(
            key.id,
            ItemKey::onepassword("OTHERACCOUNTAAAAAAAAAAAAAA", ITEM).id
        );
        assert_eq!(key.external_id.as_deref(), Some(ITEM));
        assert_eq!(key.provider, VaultProvider::OnePassword);
    }

    #[test]
    fn a_listing_mirrors_hosts_and_a_read_fills_in_description_and_tags() {
        let runner = Scripted::new(vec![
            (
                vec!["item", "list", "--vault", VAULT],
                Some(1),
                String::new(),
                "[ERROR] connecting to desktop app: cannot connect to 1Password app, make sure it is running",
            ),
            (
                vec!["item", "list", "--vault", VAULT],
                Some(0),
                item_list_json(&[
                    (ITEM, "p0-fake-host", 2, "ssh://p0@192.0.2.10:2222"),
                    (OTHER, "P0-FAKE-HOST", 1, "ssh://p0@192.0.2.10:2222"),
                ]),
                "",
            ),
            (
                vec!["item", "get", ITEM, "--vault", VAULT],
                Some(0),
                item_json(ITEM, "p0-fake-host", 3, "pw"),
                "",
            ),
        ]);
        let op = Op::new(runner.clone(), stub_op("listing"), account());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let items = runtime
            .block_on(op.list_items())
            .expect("the first 'cannot connect' is retried");
        let mut store = HostStore::default();
        let result = reconcile(&op, &mut store, &items);
        assert_eq!(result.hosts, 1);
        assert_eq!(
            result.problems,
            vec![("P0-FAKE-HOST".to_owned(), ItemProblem::NameTaken)],
            "two items with one name: the first keeps it"
        );
        let host = store.find_alias("p0-fake-host").unwrap().clone();
        assert_eq!(host.id, op.item_key(ITEM).id);
        assert_eq!((host.address.as_str(), host.port), ("192.0.2.10", 2222));
        assert_eq!(host.username, "p0");
        assert_eq!(host.vault().unwrap().revision_date, "2");
        assert!(host.tags.is_empty(), "the list carries no tags");

        let item = runtime.block_on(op.get_item(ITEM)).unwrap();
        let facts = item_facts(&item).unwrap();
        assert_eq!(facts.description, "P0 fake host");
        assert_eq!(facts.tags, ["p0", "test"]);
        assert_eq!(facts.revision, "3");
        let stored = store.hosts.iter_mut().find(|h| h.id == host.id).unwrap();
        vault_hosts::apply(&facts, &op.item_key(ITEM), stored);

        // A later listing keeps what the read found.
        let again = listed_facts(&items[0], Some(stored)).unwrap();
        assert_eq!(again.tags, ["p0", "test"]);
        assert!(runner.finished());
    }

    fn host_write(external_id: Option<&str>, expected_revision: &str) -> HostWrite {
        HostWrite {
            provider: VaultProvider::OnePassword,
            item_id: None,
            external_id: external_id.map(str::to_owned),
            name: "p0-fake-host".to_owned(),
            host_uri: "ssh://192.0.2.11:22".to_owned(),
            username: "p0".to_owned(),
            password: codex_hosts_bw::FieldEdit::Keep,
            description: "front".to_owned(),
            tags: vec!["web".to_owned()],
            expected_revision: expected_revision.to_owned(),
        }
    }

    #[test]
    fn saving_creates_or_edits_the_item_and_leaves_a_changed_one_alone() {
        let item = |version| item_json(ITEM, "p0-fake-host", version, "pw");
        let runner = Scripted::new(vec![
            (
                vec!["item", "create", "--vault", VAULT],
                Some(0),
                item(1),
                "",
            ),
            (
                vec!["item", "get", ITEM, "--vault", VAULT],
                Some(0),
                item(3),
                "",
            ),
            (
                vec!["item", "edit", ITEM, "--vault", VAULT],
                Some(0),
                item(4),
                "",
            ),
            (
                vec!["item", "get", ITEM, "--vault", VAULT],
                Some(0),
                item(5),
                "",
            ),
            (
                vec!["item", "delete", ITEM, "--vault", VAULT],
                Some(1),
                String::new(),
                "[ERROR] \"testitem000000000000000000\" isn't an item in the \"codex-hosts-test\" vault",
            ),
        ]);
        let op = Op::new(runner.clone(), stub_op("saving"), account());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        let Ok((key, facts)) = runtime.block_on(save_with(&op, &host_write(None, ""))) else {
            panic!("a new host creates its item");
        };
        assert_eq!(key, op.item_key(ITEM));
        assert_eq!(facts.revision, "1");
        let created = &runner.payloads()[0];
        assert_eq!(created["title"], "p0-fake-host");
        assert_eq!(created["urls"][0]["href"], "ssh://192.0.2.11:22");

        let Ok((_, facts)) = runtime.block_on(save_with(&op, &host_write(Some(ITEM), "3"))) else {
            panic!("the item the editor read is edited");
        };
        assert_eq!(facts.revision, "4");
        let edited = &runner.payloads()[1];
        assert_eq!(edited["id"], ITEM, "the whole item is sent back");
        assert_eq!(edited["urls"][0]["href"], "ssh://192.0.2.11:22");

        assert!(
            matches!(
                runtime.block_on(save_with(&op, &host_write(Some(ITEM), "3"))),
                Err(WriteFailure::Conflict)
            ),
            "an item changed since it was read is not overwritten"
        );
        assert_eq!(runner.payloads().len(), 2);

        runtime
            .block_on(op.archive(ITEM))
            .expect("an item already gone counts as archived");
        assert!(runner.finished());
    }

    #[test]
    fn non_host_items_are_reported() {
        let mut items: Vec<OpItemSummary> = serde_json::from_str(&item_list_json(&[
            (ITEM, "web", 1, "https://example.com"),
            (OTHER, " ", 1, "ssh://h"),
        ]))
        .unwrap();
        assert_eq!(listed_facts(&items[0], None), Err(ItemProblem::NoHostUri));
        assert_eq!(listed_facts(&items[1], None), Err(ItemProblem::NoName));
        items[0].category = "PASSWORD".to_owned();
        assert_eq!(listed_facts(&items[0], None), Err(ItemProblem::NotLogin));
    }

    /// Against the real CLI and app: `CODEX_HOSTS_OP_LIVE` = "<op.exe>|<account
    /// id>|<vault id>", with a `p0-fake-host` Login item in that vault. Run
    /// with `--ignored`; the app asks once for this test process.
    #[test]
    #[ignore = "needs the 1Password app and a test vault"]
    fn live_listing_and_item_read() {
        let spec = std::env::var("CODEX_HOSTS_OP_LIVE").expect("CODEX_HOSTS_OP_LIVE");
        let [exe, account_id, vault_id] = spec.split('|').collect::<Vec<_>>()[..] else {
            panic!("CODEX_HOSTS_OP_LIVE = <op.exe>|<account>|<vault>");
        };
        let op = Op::new(
            RealRunner,
            OpExecutable::new(exe).unwrap(),
            OnePasswordAccount {
                account_id: account_id.to_owned(),
                vault_id: vault_id.to_owned(),
                ..account()
            },
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let items = runtime.block_on(op.list_items()).unwrap();
        let mut store = HostStore::default();
        let result = reconcile(&op, &mut store, &items);
        println!("hosts {}, problems {:?}", result.hosts, result.problems);
        let host = store.find_alias("p0-fake-host").unwrap().clone();
        let item_id = host.vault().unwrap().external_id.clone().unwrap();
        let item = runtime.block_on(op.get_item(&item_id)).unwrap();
        let facts = item_facts(&item).unwrap();
        assert_eq!((facts.address.as_str(), facts.port), ("192.0.2.10", 2222));
        assert_eq!(facts.username, "p0");
        assert!(item.password().unwrap().is_some_and(|p| !p.is_empty()));
        let missing = runtime.block_on(op.get_item("zzzzzzzzzzzzzzzzzzzzzzzzzz"));
        assert_eq!(missing.unwrap_err().code(), "ONEPASSWORD_ITEM_MISSING");
    }

    #[test]
    fn failures_tell_the_user_what_to_do() {
        let off = failure(OpError::IntegrationOff, "box");
        assert_eq!(off.code, "ONEPASSWORD_NOT_CONFIGURED");
        assert!(off.message.contains("Integrate with 1Password CLI"));
        let gone = failure(OpError::ItemMissing, "box");
        assert!(gone.message.contains("box"));
        let disabled = Op::from_settings(&OnePasswordSettings {
            enabled: false,
            ..OnePasswordSettings::default()
        });
        assert_eq!(disabled.err().unwrap().code, "ONEPASSWORD_DISABLED");
        let unset = Op::from_settings(&OnePasswordSettings::default());
        assert_eq!(unset.err().unwrap().code, "ONEPASSWORD_NOT_CONFIGURED");
    }
}
