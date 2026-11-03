//! Bitwarden in the window: the credential-manager dialog (account setup
//! wizard, vault status, the folder's items, settings), the jobs that keep
//! vault-backed hosts in step with the folder (refresh, save, delete), and
//! the master-password prompt. Every `bw` call runs on a worker thread with
//! its own runtime; the window only polls for results, so it never blocks on
//! the CLI. Master passwords live in these structs as zeroizing strings for
//! exactly as long as a dialog needs them.

use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use codex_hosts_bw::{
    BwVersion, CodePrompt, FieldEdit, ItemKind, ItemSummary, LoginPatch, MasterPassword,
    SessionKey, StatusReport, TwoFactorMethod, VaultFolder, VaultStatus, VersionSupport,
};
use eframe::egui::{self, RichText};
use uuid::Uuid;
use zeroize::Zeroizing;

use super::dialogs::dialog_shell;
use super::host_list::HostGroup;
use super::theme;
use super::{EditorSwitch, HostsApp};
use crate::bitwarden::hosts;
use crate::bitwarden::vault::LoginStep;
use crate::bitwarden::{Account, Vault, VaultError, session};
use crate::i18n::Catalog;
use crate::model::VaultProvider;
use crate::model::{
    BitwardenProfile, BitwardenSettings, DEFAULT_BITWARDEN_FOLDER, DEFAULT_REAUTH_IDLE_MINUTES,
    MAX_REAUTH_IDLE_MINUTES, ReauthPolicy, SYNC_MAX_AGE_CHOICES,
};
use crate::ssh::RemoteFailure;
use crate::vault_hosts::{self, HostFacts, HostWrite, ItemKey, ItemProblem, Reconciled};

const US_SERVER: &str = "https://vault.bitwarden.com";
const EU_SERVER: &str = "https://vault.bitwarden.eu";
/// Content width of the settings tab inside the 760 px dialog.
const SETTINGS_WIDTH: f32 = 700.0;

/// The window's wording for a vault failure; `bw` output never appears.
pub(super) fn explain(catalog: &Catalog, error: &VaultError) -> String {
    let key = match error {
        VaultError::Locked => "bw_err_locked",
        VaultError::SignedOut => "bw_err_signed_out",
        VaultError::VerificationRequired => "bw_err_verification",
        VaultError::InvalidCode => "bw_err_invalid_code",
        VaultError::Network => "bw_err_network",
        VaultError::InvalidCredentials => "bw_err_invalid_credentials",
        VaultError::ItemMissing | VaultError::ItemDeleted | VaultError::PasswordMissing => {
            "bw_err_item_missing"
        }
        VaultError::ItemKindMismatch | VaultError::UsernameMismatch => "bw_err_item_mismatch",
        VaultError::SyncRequired => "bw_err_sync",
        VaultError::VersionUnverified => "bw_err_cli_unverified",
        other => return catalog.format("bw_error", &[("error", &other.to_string())]),
    };
    catalog.text(key).to_owned()
}

fn explain_remote(catalog: &Catalog, error: &RemoteFailure) -> String {
    if let Some(text) = super::onepassword::explain_code(catalog, error) {
        return text;
    }
    match error.code {
        "BITWARDEN_LOCKED" => catalog.text("bw_err_locked").to_owned(),
        "BITWARDEN_SIGNED_OUT" => catalog.text("bw_err_signed_out").to_owned(),
        "BITWARDEN_DISABLED" => catalog.text("bw_disabled_hint").to_owned(),
        "BITWARDEN_NOT_CONFIGURED" => catalog.text("bw_not_configured").to_owned(),
        _ => catalog.format("bw_error", &[("error", &error.message)]),
    }
}

/// The window's wording for why a folder item is not served as a host.
pub(super) fn problem_key(problem: &ItemProblem) -> &'static str {
    match problem {
        ItemProblem::NotLogin => "vault_problem_not_login",
        ItemProblem::NoName => "vault_problem_no_name",
        ItemProblem::NoHostUri => "vault_problem_no_uri",
        ItemProblem::NoUsername => "vault_problem_no_username",
        ItemProblem::NameTaken => "vault_problem_name_taken",
    }
}

// ---------------------------------------------------------------------------
// Worker jobs

/// A worker result the dialog polls for.
pub(super) struct Job<T>(Receiver<Result<T, String>>);

impl<T> Job<T> {
    /// `None` while the worker runs.
    pub(super) fn poll(&self) -> Option<Result<T, String>> {
        match self.0.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("worker stopped".to_owned())),
        }
    }
}

pub(super) fn spawn_job<T: Send + 'static>(
    repaint: egui::Context,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Job<T> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = work();
        if sender.send(result).is_ok() {
            repaint.request_repaint();
        }
    });
    Job(receiver)
}

pub(super) fn block_on<T>(future: impl std::future::Future<Output = T>) -> Result<T, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    Ok(runtime.block_on(future))
}

/// A vault error carried to the window as its classification, so the
/// dialog can word it and react to a lock.
enum JobError {
    Vault(VaultError),
    Remote(RemoteFailure),
    /// The item changed in the vault since the editor read it.
    Conflict,
    /// The saved item does not describe a host after all.
    Problem(ItemProblem),
    Other(String),
}

type JobResult<T> = Result<T, JobError>;

impl From<VaultError> for JobError {
    fn from(error: VaultError) -> Self {
        Self::Vault(error)
    }
}

impl From<RemoteFailure> for JobError {
    fn from(error: RemoteFailure) -> Self {
        Self::Remote(error)
    }
}

impl From<String> for JobError {
    fn from(error: String) -> Self {
        Self::Other(error)
    }
}

impl From<codex_hosts_bw::VaultParseError> for JobError {
    fn from(error: codex_hosts_bw::VaultParseError) -> Self {
        Self::Vault(VaultError::Parse(error))
    }
}

impl JobError {
    fn is_locked(&self) -> bool {
        match self {
            Self::Vault(VaultError::Locked) => true,
            Self::Remote(error) => error.code == "BITWARDEN_LOCKED",
            _ => false,
        }
    }
}

fn word(catalog: &Catalog, error: &JobError) -> String {
    match error {
        JobError::Vault(error) => explain(catalog, error),
        JobError::Remote(error) => explain_remote(catalog, error),
        JobError::Conflict => catalog.text("vault_conflict").to_owned(),
        JobError::Problem(problem) => catalog.text(problem_key(problem)).to_owned(),
        JobError::Other(text) => text.clone(),
    }
}

/// What a worker needs from the window to report back.
#[derive(Clone)]
struct Reporter {
    repaint: egui::Context,
    catalog: Catalog,
}

impl HostsApp {
    fn reporter(&self) -> Reporter {
        Reporter {
            repaint: self.repaint_context.clone(),
            catalog: self.catalog.clone(),
        }
    }
}

/// Runs `work` against the configured account on a worker thread; the
/// classified error reaches the dialog as text.
fn vault_job<T: Send + 'static>(
    reporter: &Reporter,
    settings: BitwardenSettings,
    work: impl FnOnce(Account) -> JobResult<T> + Send + 'static,
) -> Job<Result<T, JobError>> {
    let catalog = reporter.catalog.clone();
    spawn_job(reporter.repaint.clone(), move || {
        let account =
            Account::from_settings(&settings).map_err(|error| explain_remote(&catalog, &error))?;
        Ok(work(account))
    })
}

/// Locks `bw` with a key codex-hosts just retired, off the window thread,
/// so the CLI's own state follows the policy and not only the stored copy.
fn lock_in_background(settings: &BitwardenSettings, key: SessionKey) {
    if let Some(account) = account_for_lock(settings) {
        thread::spawn(move || {
            let _ = block_on(account.vault(Some(key)).lock());
        });
    }
}

/// The account to lock with, even while the switch is off: locking is
/// always allowed.
fn account_for_lock(settings: &BitwardenSettings) -> Option<Account> {
    let mut settings = settings.clone();
    settings.enabled = true;
    Account::from_settings(&settings).ok()
}

/// How often the window checks whether the idle policy has run out.
const POLICY_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// A second handle on a key a dialog keeps in memory for its own calls.
fn copy_key(key: &SessionKey) -> SessionKey {
    SessionKey::new(key.expose().to_owned())
}

/// The session a dialog holds in memory, else the stored one; otherwise a
/// vault error the caller words as "locked".
fn unlocked(account: &Account, memory: Option<SessionKey>) -> JobResult<Vault> {
    if let Some(key) = memory {
        return Ok(account.vault(Some(key)));
    }
    match session::load(&account.profile) {
        Ok(Some(key)) => Ok(account.vault(Some(key))),
        Ok(None) => Err(VaultError::Locked.into()),
        Err(error) => Err(error.into()),
    }
}

/// Unlocks with the master password and keeps the key per the policy; the
/// dialog that asked also keeps a copy in memory.
fn unlock_and_store(account: &Account, password: &MasterPassword) -> JobResult<Vault> {
    let mut vault = account.vault(None);
    block_on(vault.unlock(password))??;
    if let Some(key) = vault.session() {
        session::store(&account.profile, key)?;
    }
    Ok(vault)
}

fn sync_if_stale(account: &Account, vault: &mut Vault, max_age_seconds: u64) -> JobResult<()> {
    if session::sync_is_fresh(account.id(), max_age_seconds) {
        return Ok(());
    }
    block_on(vault.sync())??;
    session::mark_synced(account.id())?;
    Ok(())
}

/// The JSON of a new host item: a Login item in the folder with the host
/// URI and the codex-hosts fields.
fn host_item_json(write: &HostWrite, folder: Option<Uuid>) -> Zeroizing<Vec<u8>> {
    let password = match &write.password {
        FieldEdit::Set(password) => serde_json::Value::String(password.as_str().to_owned()),
        FieldEdit::Keep | FieldEdit::Clear => serde_json::Value::Null,
    };
    let item = serde_json::json!({
        "type": 1,
        "name": write.name,
        "notes": null,
        "favorite": false,
        "reprompt": 0,
        "folderId": folder,
        "fields": write.codex_fields().iter().map(|(key, value)| serde_json::json!({
            "name": format!("{}{key}", codex_hosts_bw::CODEX_FIELD_PREFIX),
            "value": value,
            "type": 0,
            "linkedId": null,
        })).collect::<Vec<_>>(),
        "login": {
            "username": write.username,
            "password": password,
            "uris": [{ "match": null, "uri": write.host_uri }]
        }
    });
    Zeroizing::new(item.to_string().into_bytes())
}

// ---------------------------------------------------------------------------
// Vault-backed hosts: refresh, save, delete

/// A job on a password manager the window runs for its hosts. Saves and
/// deletes go to the provider of their host; one runs at a time.
pub(super) enum VaultTask {
    /// Mirror the Bitwarden folder into the host list. A quiet refresh never
    /// asks for the master password: without a usable key it only reports
    /// the lock.
    Refresh { quiet: bool },
    /// Write the editor's draft into its item (or create it).
    Save(HostWrite),
    /// Move the host's item to the provider's trash or archive.
    Delete(Uuid),
}

pub(super) enum VaultOutcome {
    Refreshed(Reconciled),
    Saved { key: ItemKey, facts: HostFacts },
    Deleted(Uuid),
}

impl From<vault_hosts::WriteFailure> for JobError {
    fn from(error: vault_hosts::WriteFailure) -> Self {
        match error {
            vault_hosts::WriteFailure::Conflict => Self::Conflict,
            vault_hosts::WriteFailure::Problem(problem) => Self::Problem(problem),
            vault_hosts::WriteFailure::Remote(error) => Self::Remote(error),
        }
    }
}

/// Runs a 1Password write on a worker; `op` itself waits for the app's
/// prompt, so there is no unlock step to come back to.
fn onepassword_job(
    reporter: &Reporter,
    work: impl FnOnce() -> JobResult<VaultOutcome> + Send + 'static,
) -> Job<Result<VaultOutcome, JobError>> {
    spawn_job(reporter.repaint.clone(), move || Ok(work()))
}

impl HostsApp {
    /// A key left under "until the window exits" by a window that did not
    /// exit cleanly is dropped, and `bw` locked, when the next main window
    /// starts.
    pub(super) fn clear_stale_window_sessions(&self) {
        if self.launch.codex_edit {
            return;
        }
        if let Some(profile) = &self.store.bitwarden.profile
            && let Ok(Some(key)) = session::take_if_window_scoped(profile)
        {
            lock_in_background(&self.store.bitwarden, key);
        }
    }

    /// The main window is going away: under "until the window exits" the
    /// key goes and `bw` is locked before the process ends.
    pub(super) fn forget_window_scoped_session(&self) {
        if self.launch.codex_edit {
            return;
        }
        if let Some(profile) = &self.store.bitwarden.profile
            && profile.policy == ReauthPolicy::UntilExit
        {
            let key = session::take(profile.id)
                .ok()
                .flatten()
                .or_else(|| self.vault_session.as_ref().map(copy_key));
            if let (Some(key), Some(account)) = (key, account_for_lock(&self.store.bitwarden)) {
                let _ = block_on(account.vault(Some(key)).lock());
            }
        }
    }

    /// Enforces the idle policy while the window runs: once the key has not
    /// been used for the chosen time it is removed, `bw` is locked, and the
    /// window's own copies are dropped. Other processes check at their next
    /// use.
    fn enforce_vault_policy(&mut self) {
        if self.launch.codex_edit
            || self
                .vault_policy_checked
                .is_some_and(|at| at.elapsed() < POLICY_CHECK_INTERVAL)
        {
            return;
        }
        self.vault_policy_checked = Some(std::time::Instant::now());
        self.repaint_context
            .request_repaint_after(POLICY_CHECK_INTERVAL);
        let settings = &self.store.bitwarden;
        let Some(profile) = settings.profile.as_ref() else {
            return;
        };
        if !matches!(profile.policy, ReauthPolicy::Idle { .. }) || self.vault_job.is_some() {
            return;
        }
        if let Ok(Some(key)) = session::take_expired(profile) {
            lock_in_background(settings, key);
            self.vault_session = None;
            self.vault_state = Some(VaultState::Locked);
            self.vault_note = Some(self.catalog.text("vault_section_locked").to_owned());
            if let Some(manager) = self.credential_manager.as_mut() {
                manager.session = None;
            }
        }
    }

    /// Re-reads the folder (syncing per the window) and updates the host
    /// list; asks for the master password when the vault is locked.
    pub(super) fn refresh_vault_hosts(&mut self) {
        if !self.store.bitwarden.available() {
            return;
        }
        self.start_vault_task(VaultTask::Refresh { quiet: false });
    }

    /// The same without a prompt, for moments the user did not ask for a
    /// read (window start, after an unlock elsewhere); skipped while another
    /// vault job runs.
    pub(super) fn refresh_vault_hosts_quietly(&mut self) {
        if !self.store.bitwarden.available() || self.vault_job.is_some() || self.launch.codex_edit {
            return;
        }
        self.start_vault_task(VaultTask::Refresh { quiet: true });
    }

    pub(super) fn vault_job_running(&self) -> bool {
        self.vault_job.is_some()
    }

    pub(super) fn start_vault_task(&mut self, task: VaultTask) {
        if self.vault_job.is_some() {
            self.set_status_key("busy");
            return;
        }
        let settings = self.store.bitwarden.clone();
        let reporter = self.reporter();
        let memory = self.vault_session.as_ref().map(copy_key);
        let max_age = settings.sync_max_age_seconds;
        let folder = settings
            .profile
            .as_ref()
            .and_then(|profile| profile.folder_id);
        // A 1Password host is archived through `op`, not `bw`.
        let onepassword_host = match &task {
            VaultTask::Delete(id) => self
                .store
                .hosts
                .iter()
                .find(|host| {
                    host.id == *id && host.vault_provider() == Some(VaultProvider::OnePassword)
                })
                .cloned(),
            _ => None,
        };
        let (kind, job, provider) = match (task, onepassword_host) {
            (VaultTask::Save(write), _) if write.provider == VaultProvider::OnePassword => (
                VaultTaskKind::Save(write.clone_without_password()),
                onepassword_job(&reporter, move || {
                    let (key, facts) = block_on(crate::onepassword::save_item(&write))??;
                    Ok(VaultOutcome::Saved { key, facts })
                }),
                VaultProvider::OnePassword,
            ),
            (VaultTask::Delete(id), Some(host)) => (
                VaultTaskKind::Delete(id),
                onepassword_job(&reporter, move || {
                    block_on(crate::onepassword::archive_item(&host))??;
                    Ok(VaultOutcome::Deleted(id))
                }),
                VaultProvider::OnePassword,
            ),
            (VaultTask::Refresh { quiet }, _) => (
                VaultTaskKind::Refresh { quiet },
                vault_job(&reporter, settings, move |account| {
                    let mut vault = if quiet {
                        let key = match memory {
                            Some(key) => Some(key),
                            None => session::load_non_consuming(&account.profile)?,
                        };
                        account.vault(Some(key.ok_or(VaultError::Locked)?))
                    } else {
                        unlocked(&account, memory)?
                    };
                    let (result, _) = block_on(crate::bitwarden::refresh_store(
                        &account, &mut vault, max_age,
                    ))??;
                    Ok(VaultOutcome::Refreshed(result))
                }),
                VaultProvider::Bitwarden,
            ),
            (VaultTask::Save(write), _) => (
                VaultTaskKind::Save(write.clone_without_password()),
                vault_job(&reporter, settings, move |account| {
                    let mut vault = unlocked(&account, memory)?;
                    sync_if_stale(&account, &mut vault, max_age)?;
                    let item = match write.item_id {
                        None => block_on(vault.create_item(host_item_json(&write, folder)))??,
                        Some(id) => {
                            let current = block_on(vault.get_item(id))??;
                            if !write.expected_revision.is_empty()
                                && current.changed_since(&write.expected_revision)
                            {
                                return Err(JobError::Conflict);
                            }
                            let fields = write.codex_fields();
                            let name = write.name.trim().to_owned();
                            let HostWrite {
                                username,
                                password,
                                host_uri,
                                ..
                            } = write;
                            let summary = current.summary();
                            let patch = LoginPatch {
                                name: (name != summary.name).then_some(name),
                                username: if Some(username.as_str()) == summary.username.as_deref()
                                {
                                    FieldEdit::Keep
                                } else {
                                    FieldEdit::Set(username)
                                },
                                password,
                                host_uri: Some(host_uri),
                                codex_fields: Some(fields),
                            };
                            let json = current.apply_login_patch(&patch)?;
                            block_on(vault.edit_item(id, json))??
                        }
                    };
                    let facts = hosts::facts(item.summary()).map_err(JobError::Problem)?;
                    Ok(VaultOutcome::Saved {
                        key: ItemKey::bitwarden(item.summary().id),
                        facts,
                    })
                }),
                VaultProvider::Bitwarden,
            ),
            (VaultTask::Delete(item_id), None) => (
                VaultTaskKind::Delete(item_id),
                vault_job(&reporter, settings, move |account| {
                    let mut vault = unlocked(&account, memory)?;
                    block_on(vault.delete_item(item_id))??;
                    Ok(VaultOutcome::Deleted(item_id))
                }),
                VaultProvider::Bitwarden,
            ),
        };
        self.vault_job = Some(VaultJob {
            kind,
            provider,
            job,
        });
    }

    /// Picks up a finished vault job: updates the host list or the editor,
    /// or raises the master-password prompt and keeps the task for after it.
    pub(super) fn poll_vault_job(&mut self) {
        let Some(result) = self
            .vault_job
            .as_ref()
            .and_then(|running| running.job.poll())
        else {
            return;
        };
        let Some(VaultJob { kind, provider, .. }) = self.vault_job.take() else {
            return;
        };
        if provider == VaultProvider::OnePassword {
            self.onepassword.note_result(match &result {
                Ok(Err(JobError::Remote(error))) => Some(error.code),
                Ok(_) => None,
                Err(_) => Some(""),
            });
        }
        match result {
            Ok(Ok(VaultOutcome::Refreshed(reconciled))) => {
                self.vault_state = Some(VaultState::Unlocked);
                if let Err(error) = self.refresh_hosts() {
                    self.set_status_format("storage_error", &[("error", &error)]);
                }
                self.vault_note = Some(if reconciled.hosts == 0 {
                    self.catalog.text("vault_section_empty").to_owned()
                } else {
                    String::new()
                });
                self.vault_problems = reconciled.problems.clone();
                let count = reconciled.hosts.to_string();
                let problems = reconciled.problems.len().to_string();
                let quiet = matches!(kind, VaultTaskKind::Refresh { quiet: true });
                // A read nobody asked for speaks up only about problems.
                if !quiet || !reconciled.problems.is_empty() {
                    self.set_status_format(
                        if reconciled.problems.is_empty() {
                            "vault_refreshed"
                        } else {
                            "vault_refreshed_with_problems"
                        },
                        &[("count", &count), ("problems", &problems)],
                    );
                }
            }
            Ok(Ok(VaultOutcome::Saved { key, facts })) => {
                self.finish_vault_save(&key, &facts);
            }
            Ok(Ok(VaultOutcome::Deleted(item_id))) => {
                self.finish_vault_delete(item_id);
            }
            Ok(Err(error)) if error.is_locked() => {
                self.vault_session = None;
                self.vault_state = Some(VaultState::Locked);
                self.vault_note = Some(self.catalog.text("vault_section_locked").to_owned());
                if !matches!(kind, VaultTaskKind::Refresh { quiet: true }) {
                    self.open_unlock_prompt(None, Some(kind.into_task(&self.editor)));
                }
            }
            Ok(Err(error)) => {
                let text = match error {
                    JobError::Conflict => {
                        let group = HostGroup::of_provider(Some(provider));
                        self.catalog
                            .text(Self::vault_key(group, |texts| texts.conflict))
                            .to_owned()
                    }
                    _ => word(&self.catalog, &error),
                };
                if let VaultTaskKind::Save(_) = kind {
                    self.editor_save_failed(text);
                } else {
                    self.set_status(super::status::StatusKind::Error, text);
                }
            }
            Err(error) => {
                if let VaultTaskKind::Save(_) = kind {
                    self.editor_save_failed(error);
                } else {
                    self.set_status(super::status::StatusKind::Error, error);
                }
            }
        }
    }

    /// Opens the master-password prompt: for a tool call waiting on the
    /// window pipe (`request`), or for a window task to run afterwards.
    fn open_unlock_prompt(&mut self, request: Option<Uuid>, then: Option<VaultTask>) {
        match self.unlock_prompt.as_mut() {
            Some(prompt) => {
                if request.is_some() {
                    prompt.request = request;
                }
                if then.is_some() {
                    prompt.then = then;
                }
            }
            None => {
                self.unlock_prompt = Some(UnlockPrompt {
                    request,
                    then,
                    master_password: Zeroizing::new(String::new()),
                    job: None,
                    error: None,
                });
            }
        }
    }
}

/// Which task a running job belongs to, kept so a lock can re-run it after
/// the prompt. The save keeps its write without the password: the editor
/// still holds the typed password and rebuilds the write.
enum VaultTaskKind {
    Refresh { quiet: bool },
    Save(HostWrite),
    Delete(Uuid),
}

impl VaultTaskKind {
    fn into_task(self, editor: &Option<super::HostEditor>) -> VaultTask {
        match self {
            Self::Refresh { quiet } => VaultTask::Refresh { quiet },
            Self::Delete(id) => VaultTask::Delete(id),
            Self::Save(write) => VaultTask::Save(match editor {
                Some(editor) => editor.vault_write_or(write),
                None => write,
            }),
        }
    }
}

pub(super) struct VaultJob {
    kind: VaultTaskKind,
    provider: VaultProvider,
    job: Job<Result<VaultOutcome, JobError>>,
}

/// The master-password prompt: raised by a tool call through the window
/// pipe, or by a window task that found the vault locked.
pub(super) struct UnlockPrompt {
    /// The waiting tool request, answered through `temporary`.
    request: Option<Uuid>,
    /// The window task to run once unlocked.
    then: Option<VaultTask>,
    master_password: Zeroizing<String>,
    job: Option<Job<Result<Option<SessionKey>, JobError>>>,
    error: Option<String>,
}

/// The unlock behind an allowed SSH call approval whose calls read the
/// vault: the approval dialog took the master password, the window runs it.
pub(super) struct ApprovalUnlock(Job<Result<Option<SessionKey>, JobError>>);

// ---------------------------------------------------------------------------
// Credential manager dialog

/// The dialog's two levels: every credential service, or one service.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ManagerView {
    Services,
    Bitwarden,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ManagerTab {
    Account,
    Items,
    Settings,
}

struct ManagerStatus {
    report: Option<StatusReport>,
    unlocked: bool,
    version: Option<(BwVersion, VersionSupport)>,
}

/// Inline repair of a folder item that lacks what a host needs.
struct FixForm {
    id: Uuid,
    name: String,
    uri: String,
    username: String,
}

enum ManagerEvent {
    Status(ManagerStatus),
    /// The folder's items and the host list reconciled against them, with
    /// the session that listed them.
    Items(Option<SessionKey>, Vec<ItemSummary>, Reconciled),
    /// A write or unlock finished, with the session that did it.
    Saved(Option<SessionKey>),
    Locked,
    Wizard(WizardEvent),
}

/// How the vault was last seen, for the toolbar's status.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum VaultState {
    Unlocked,
    Locked,
    SignedOut,
}

/// What the confirmation after a password manager's setup tells the user.
pub(super) struct SetupDone {
    pub(super) service: HostGroup,
    /// The account's email (or sign-in address).
    pub(super) account: String,
    /// The folder or vault whose items are the hosts.
    pub(super) place: String,
    /// Keeping the session in Credential Manager failed; the vault is
    /// unlocked for this window only.
    pub(super) store_error: Option<String>,
}

pub(super) struct CredentialManager {
    view: ManagerView,
    tab: ManagerTab,
    status: Option<ManagerStatus>,
    master_password: Zeroizing<String>,
    /// The session this dialog unlocked or loaded, for its own calls.
    session: Option<SessionKey>,
    wizard: Option<Wizard>,
    items: Vec<ItemSummary>,
    problems: Vec<(String, ItemProblem)>,
    item_filter: String,
    fix_form: Option<FixForm>,
    idle_minutes: u32,
    sync_max_age: u64,
    job: Option<Job<Result<ManagerEvent, JobError>>>,
    message: Option<(bool, String)>,
    /// A hard-to-undo account action waiting for its confirmation.
    confirm: Option<AccountConfirm>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AccountConfirm {
    Logout,
    RemoveSetup,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ServerChoice {
    Us,
    Eu,
    Custom,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WizardStep {
    Cli,
    Login,
    Code,
    Folder,
    Policy,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FolderChoice {
    Existing(Uuid),
    Create,
}

/// The account setup, one step at a time. The master password stays here
/// until the wizard closes and goes only to `bw` through its environment.
struct Wizard {
    step: WizardStep,
    cli_path: String,
    profile_id: Uuid,
    email: String,
    server: ServerChoice,
    custom_url: String,
    master_password: Zeroizing<String>,
    /// The two-step method picked when bw asks which one to use.
    method: TwoFactorMethod,
    /// Whether the next login passes [`Wizard::method`].
    use_method: bool,
    /// bw listed several two-step methods; the user picks one.
    choose_method: bool,
    code: String,
    /// The `bw login` that is running, while it runs.
    login: Option<LoginLink>,
    /// The window could not finish the login; offer the terminal hand-off.
    offer_terminal: bool,
    session: Option<SessionKey>,
    folders: Vec<VaultFolder>,
    /// Login items per folder, so the user sees what a folder already holds.
    folder_counts: BTreeMap<Uuid, usize>,
    folder: FolderChoice,
    new_folder: String,
    policy: ReauthPolicy,
    idle_minutes: u32,
    busy: bool,
    error: Option<String>,
}

/// The window's end of a running interactive `bw login`: bw's code prompts
/// arrive on `prompts`, the typed code leaves on `answers`, and dropping the
/// link stops the login.
struct LoginLink {
    prompts: Receiver<CodePrompt>,
    answers: tokio::sync::mpsc::UnboundedSender<Zeroizing<String>>,
    /// The code bw is waiting for now; `None` while bw works.
    waiting: Option<CodePrompt>,
}

enum WizardEvent {
    LoggedIn(SessionKey),
    /// The login ended without a session or an error (method list,
    /// unexpected prompt, stopped by the user).
    LoginStopped(LoginStep),
    /// The account is still signed out after the terminal hand-off.
    StillSignedOut,
    Folders(Vec<VaultFolder>, BTreeMap<Uuid, usize>),
    FolderReady(VaultFolder, SessionKey),
}

impl Wizard {
    fn new(settings: &BitwardenSettings) -> Self {
        let current = settings.profile.as_ref();
        Self {
            step: WizardStep::Cli,
            cli_path: settings
                .cli_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            profile_id: current
                .map(|profile| profile.id)
                .or_else(session::leftover_profile)
                .unwrap_or_else(Uuid::new_v4),
            email: current
                .map(|profile| profile.email.clone())
                .unwrap_or_default(),
            server: match current.and_then(|profile| profile.server_url.as_deref()) {
                None | Some(US_SERVER) => ServerChoice::Us,
                Some(EU_SERVER) => ServerChoice::Eu,
                Some(_) => ServerChoice::Custom,
            },
            custom_url: current
                .and_then(|profile| profile.server_url.clone())
                .filter(|url| url != US_SERVER && url != EU_SERVER)
                .unwrap_or_default(),
            master_password: Zeroizing::new(String::new()),
            method: TwoFactorMethod::Authenticator,
            use_method: false,
            choose_method: false,
            code: String::new(),
            login: None,
            offer_terminal: false,
            session: None,
            folders: Vec::new(),
            folder_counts: BTreeMap::new(),
            folder: FolderChoice::Create,
            new_folder: current
                .map(|profile| profile.folder_name.clone())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| DEFAULT_BITWARDEN_FOLDER.to_owned()),
            policy: current.map(|profile| profile.policy).unwrap_or_default(),
            idle_minutes: match current.map(|profile| profile.policy) {
                Some(ReauthPolicy::Idle { minutes }) => minutes,
                _ => DEFAULT_REAUTH_IDLE_MINUTES,
            },
            busy: false,
            error: None,
        }
    }

    fn server_url(&self) -> String {
        match self.server {
            ServerChoice::Us => US_SERVER.to_owned(),
            ServerChoice::Eu => EU_SERVER.to_owned(),
            ServerChoice::Custom => self.custom_url.trim().to_owned(),
        }
    }

    /// Settings for the account being set up, before it is saved.
    fn draft_settings(&self, current: &BitwardenSettings) -> BitwardenSettings {
        BitwardenSettings {
            enabled: true,
            cli_path: Some(std::path::PathBuf::from(self.cli_path.trim())),
            profile: Some(BitwardenProfile {
                id: self.profile_id,
                email: self.email.trim().to_owned(),
                server_url: Some(self.server_url()),
                folder_id: None,
                folder_name: String::new(),
                policy: self.chosen_policy(),
            }),
            sync_max_age_seconds: current.sync_max_age_seconds,
        }
    }

    fn chosen_policy(&self) -> ReauthPolicy {
        match self.policy {
            ReauthPolicy::Idle { .. } => ReauthPolicy::Idle {
                minutes: self.idle_minutes.clamp(1, MAX_REAUTH_IDLE_MINUTES),
            },
            other => other,
        }
    }
}

/// Whether the account `bw status` reports is the one being set up, so an
/// existing login is unlocked instead of repeated. A status without a server
/// URL counts as the same server.
fn same_account(status: &StatusReport, email: &str, server: &str) -> bool {
    let same_email = status
        .user_email
        .as_deref()
        .is_some_and(|known| known.trim().eq_ignore_ascii_case(email.trim()));
    let same_server = status
        .server_url
        .as_deref()
        .is_none_or(|known| normalize_server(known) == normalize_server(server));
    same_email && same_server
}

fn normalize_server(url: &str) -> String {
    url.trim().trim_end_matches('/').to_ascii_lowercase()
}

enum ManagerAction {
    Close,
    OpenBitwarden,
    BackToServices,
    RefreshStatus,
    Unlock,
    Lock,
    Sync,
    Logout,
    RemoveSetup,
    StartWizard,
    SaveSettings,
    /// The re-authentication policy chosen in the Settings tab.
    SetPolicy(ReauthPolicy),
    LoadItems,
    FixItem,
    TrashItem(Uuid, bool),
    NewHost,
    /// Checks the profile directory, points bw at the server, and logs in.
    WizardLogin,
    /// Logs in again in the prepared directory (after a method choice or a
    /// rejected code).
    WizardRetryLogin,
    /// Types the entered code at bw's waiting prompt.
    WizardSubmitCode,
    WizardStopLogin,
    WizardFolders,
    /// The user logged in from their own terminal; re-check and continue.
    WizardTerminalLogin,
    WizardFolderNext,
    WizardFinish,
    WizardCancel,
}

impl HostsApp {
    /// Opens the credential-manager dialog from the toolbar.
    pub(super) fn open_credential_manager(&mut self) {
        let idle_minutes = match self
            .store
            .bitwarden
            .profile
            .as_ref()
            .map(|profile| profile.policy)
        {
            Some(ReauthPolicy::Idle { minutes }) => minutes,
            _ => DEFAULT_REAUTH_IDLE_MINUTES,
        };
        self.onepassword.close();
        self.credential_manager = Some(CredentialManager {
            view: ManagerView::Services,
            tab: ManagerTab::Account,
            status: None,
            master_password: Zeroizing::new(String::new()),
            session: self.vault_session.as_ref().map(copy_key),
            wizard: None,
            items: Vec::new(),
            problems: self.vault_problems.clone(),
            item_filter: String::new(),
            fix_form: None,
            idle_minutes,
            sync_max_age: self.store.bitwarden.sync_max_age_seconds,
            job: None,
            message: None,
            confirm: None,
        });
        if self.store.bitwarden.available() {
            self.manager_act(ManagerAction::RefreshStatus);
        }
    }

    /// Opens the credential services straight on the Bitwarden page.
    pub(super) fn open_bitwarden_page(&mut self) {
        self.open_credential_manager();
        if self.store.bitwarden.profile.is_some() {
            self.manager_act(ManagerAction::OpenBitwarden);
        }
    }

    fn save_bitwarden_settings(&mut self) {
        match self.store.save() {
            Ok(()) => {}
            Err(error) => self.set_status_format("storage_error", &[("error", &error.to_string())]),
        }
    }

    fn manager_act(&mut self, action: ManagerAction) {
        let settings = self.store.bitwarden.clone();
        let reporter = self.reporter();
        let max_age = settings.sync_max_age_seconds;
        let Some(manager) = self.credential_manager.as_mut() else {
            return;
        };
        match action {
            ManagerAction::Close => {
                self.credential_manager = None;
                self.onepassword.close();
            }
            ManagerAction::OpenBitwarden => {
                manager.view = ManagerView::Bitwarden;
                manager.tab = ManagerTab::Account;
                manager.message = None;
            }
            ManagerAction::BackToServices => {
                manager.view = ManagerView::Services;
                manager.message = None;
            }
            ManagerAction::NewHost => {
                self.credential_manager = None;
                self.switch_editor(EditorSwitch::New(HostGroup::Bitwarden));
            }
            ManagerAction::SaveSettings => {
                self.store.bitwarden.sync_max_age_seconds = manager.sync_max_age;
                self.save_bitwarden_settings();
            }
            ManagerAction::SetPolicy(policy) => {
                let policy = match policy {
                    ReauthPolicy::Idle { minutes } => ReauthPolicy::Idle {
                        minutes: minutes.clamp(1, MAX_REAUTH_IDLE_MINUTES),
                    },
                    other => other,
                };
                let Some(profile) = self.store.bitwarden.profile.as_mut() else {
                    return;
                };
                profile.policy = policy;
                let applied = session::apply_policy(profile);
                self.save_bitwarden_settings();
                match applied {
                    // "Every call" from now on: the kept key is retired and
                    // `bw` locked, so the next use asks.
                    Ok(Some(key)) => {
                        lock_in_background(&self.store.bitwarden, key);
                        self.vault_session = None;
                        self.vault_state = Some(VaultState::Locked);
                        if let Some(manager) = self.credential_manager.as_mut() {
                            manager.session = None;
                            manager.message =
                                Some((false, self.catalog.text("bw_policy_locked_now").to_owned()));
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        if let Some(manager) = self.credential_manager.as_mut() {
                            manager.message = Some((true, error));
                        }
                    }
                }
            }
            ManagerAction::RefreshStatus => {
                let memory = manager.session.as_ref().map(copy_key);
                manager.job = Some(vault_job(&reporter, settings, move |account| {
                    let unlocked = memory.is_some() || session::peek(&account.profile)?;
                    let mut vault = account.vault(memory);
                    let version = block_on(vault.version())?.ok();
                    let report = block_on(vault.status())?.ok();
                    Ok(ManagerEvent::Status(ManagerStatus {
                        report,
                        unlocked,
                        version,
                    }))
                }));
            }
            ManagerAction::Unlock => {
                let password = MasterPassword::new(std::mem::take(&mut *manager.master_password));
                manager.job = Some(vault_job(&reporter, settings, move |account| {
                    let mut vault = unlock_and_store(&account, &password)?;
                    Ok(ManagerEvent::Saved(vault.take_session()))
                }));
            }
            ManagerAction::Lock => {
                let memory = manager.session.take();
                self.vault_session = None;
                self.vault_state = Some(VaultState::Locked);
                manager.job = Some(vault_job(&reporter, settings, move |account| {
                    if let Ok(mut vault) = unlocked(&account, memory) {
                        block_on(vault.lock())?;
                    }
                    session::forget(account.id())?;
                    Ok(ManagerEvent::Locked)
                }));
            }
            ManagerAction::Sync => {
                let memory = manager.session.as_ref().map(copy_key);
                manager.job = Some(vault_job(&reporter, settings, move |account| {
                    let mut vault = unlocked(&account, memory)?;
                    block_on(vault.sync())??;
                    session::mark_synced(account.id())?;
                    Ok(ManagerEvent::Saved(vault.take_session()))
                }));
            }
            ManagerAction::Logout => {
                let memory = manager.session.take();
                self.vault_session = None;
                self.vault_state = Some(VaultState::SignedOut);
                manager.job = Some(vault_job(&reporter, settings, move |account| {
                    let stored = session::load(&account.profile)?;
                    let mut vault = account.vault(memory.or(stored));
                    let _ = block_on(vault.logout())?;
                    session::forget(account.id())?;
                    Ok(ManagerEvent::Locked)
                }));
            }
            ManagerAction::RemoveSetup => {
                if let Some(profile) = self.store.bitwarden.profile.take() {
                    session::remove_account(profile.id);
                }
                // The mirrors are defined by the account; without it they
                // are meaningless rows.
                self.store.hosts.retain(|host| !host.is_bitwarden_host());
                self.vault_session = None;
                self.vault_state = None;
                self.vault_note = None;
                self.vault_problems.clear();
                manager.status = None;
                manager.session = None;
                manager.items.clear();
                manager.problems.clear();
                self.save_bitwarden_settings();
                if let Err(error) = self.refresh_hosts() {
                    self.set_status_format("storage_error", &[("error", &error)]);
                }
            }
            ManagerAction::StartWizard => {
                manager.wizard = Some(Wizard::new(&settings));
            }
            ManagerAction::LoadItems => {
                let memory = manager.session.as_ref().map(copy_key);
                manager.job = Some(vault_job(&reporter, settings, move |account| {
                    let mut vault = unlocked(&account, memory)?;
                    let (reconciled, items) = block_on(crate::bitwarden::refresh_store(
                        &account, &mut vault, max_age,
                    ))??;
                    Ok(ManagerEvent::Items(vault.take_session(), items, reconciled))
                }));
            }
            ManagerAction::FixItem => {
                let Some(form) = manager.fix_form.take() else {
                    return;
                };
                let memory = manager.session.as_ref().map(copy_key);
                manager.job = Some(vault_job(&reporter, settings, move |account| {
                    let mut vault = unlocked(&account, memory)?;
                    sync_if_stale(&account, &mut vault, max_age)?;
                    let item = block_on(vault.get_item(form.id))??;
                    let username = form.username.trim().to_owned();
                    let name = form.name.trim().to_owned();
                    let patch = LoginPatch {
                        name: (!name.is_empty() && name != item.summary().name).then_some(name),
                        username: if username.is_empty() {
                            FieldEdit::Keep
                        } else {
                            FieldEdit::Set(username)
                        },
                        host_uri: Some(form.uri.trim().to_owned()).filter(|uri| !uri.is_empty()),
                        ..LoginPatch::default()
                    };
                    let json = item.apply_login_patch(&patch)?;
                    block_on(vault.edit_item(form.id, json))??;
                    Ok(ManagerEvent::Saved(vault.take_session()))
                }));
            }
            ManagerAction::TrashItem(id, restore) => {
                let memory = manager.session.as_ref().map(copy_key);
                manager.job = Some(vault_job(&reporter, settings, move |account| {
                    let mut vault = unlocked(&account, memory)?;
                    if restore {
                        block_on(vault.restore_item(id))??;
                    } else {
                        block_on(vault.delete_item(id))??;
                    }
                    Ok(ManagerEvent::Saved(vault.take_session()))
                }));
            }
            ManagerAction::WizardCancel => {
                manager.wizard = None;
            }
            ManagerAction::WizardLogin | ManagerAction::WizardRetryLogin => {
                let Some(wizard) = manager.wizard.as_mut() else {
                    return;
                };
                let prepare = matches!(action, ManagerAction::WizardLogin);
                wizard.busy = true;
                wizard.error = None;
                wizard.offer_terminal = false;
                wizard.choose_method = false;
                wizard.code.clear();
                let method = wizard.use_method.then_some(wizard.method);
                let (answer_sender, mut answers) = tokio::sync::mpsc::unbounded_channel();
                let (prompt_sender, prompts) = mpsc::channel();
                wizard.login = Some(LoginLink {
                    prompts,
                    answers: answer_sender,
                    waiting: None,
                });
                let draft = wizard.draft_settings(&settings);
                let email = wizard.email.trim().to_owned();
                let password = MasterPassword::new(wizard.master_password.to_string());
                let server = wizard.server_url();
                let repaint = reporter.repaint.clone();
                manager.job = Some(vault_job(&reporter, draft, move |account| {
                    let mut vault = account.vault(None);
                    if prepare {
                        // The profile directory may already hold a login from
                        // an interrupted setup: the same account only needs an
                        // unlock, any other account is logged out first (`bw`
                        // refuses `config server` while logged in).
                        let status = block_on(vault.status())??;
                        if status.status != VaultStatus::Unauthenticated {
                            if same_account(&status, &email, &server) {
                                block_on(vault.unlock(&password))??;
                                return Ok(ManagerEvent::Wizard(WizardEvent::LoggedIn(
                                    vault.take_session().ok_or(VaultError::Locked)?,
                                )));
                            }
                            block_on(vault.logout())??;
                        }
                        block_on(vault.config_server(&server))??;
                    }
                    // One `bw login` with its own prompts: a two-step code
                    // and the e-mailed new-device code are each typed at the
                    // prompt bw shows, so no attempt sends a needless e-mail.
                    let on_prompt = |prompt| {
                        if prompt_sender.send(prompt).is_ok() {
                            repaint.request_repaint();
                        }
                    };
                    let step =
                        block_on(vault.login(&email, &password, method, on_prompt, &mut answers))??;
                    Ok(ManagerEvent::Wizard(match step {
                        LoginStep::LoggedIn => {
                            WizardEvent::LoggedIn(vault.take_session().ok_or(VaultError::Locked)?)
                        }
                        other => WizardEvent::LoginStopped(other),
                    }))
                }));
            }
            ManagerAction::WizardSubmitCode => {
                let Some(wizard) = manager.wizard.as_mut() else {
                    return;
                };
                let code = Zeroizing::new(wizard.code.trim().to_owned());
                wizard.code.clear();
                if let Some(link) = wizard.login.as_mut()
                    && link.waiting.is_some()
                    && !code.is_empty()
                    && link.answers.send(code).is_ok()
                {
                    link.waiting = None;
                    wizard.busy = true;
                    wizard.error = None;
                }
            }
            ManagerAction::WizardStopLogin => {
                if let Some(wizard) = manager.wizard.as_mut() {
                    // The adapter sees the closed channel and stops bw; the
                    // job then reports the login as stopped. Until then no
                    // second login may start in the same data directory.
                    wizard.login = None;
                    wizard.busy = true;
                }
            }
            ManagerAction::WizardFolders => {
                let Some(wizard) = manager.wizard.as_mut() else {
                    return;
                };
                let Some(session) = wizard.session.as_ref().map(copy_key) else {
                    return;
                };
                wizard.busy = true;
                wizard.error = None;
                let draft = wizard.draft_settings(&settings);
                manager.job = Some(vault_job(&reporter, draft, move |account| {
                    let mut vault = account.vault(Some(session));
                    block_on(vault.sync())??;
                    session::mark_synced(account.id())?;
                    let folders = block_on(vault.list_folders())??;
                    // One listing of the whole vault tells how many Login
                    // items each folder already holds.
                    let items = block_on(vault.list_items(None))??;
                    let mut counts: BTreeMap<Uuid, usize> = BTreeMap::new();
                    for item in items
                        .iter()
                        .filter(|item| item.kind == ItemKind::Login && item.deleted_date.is_none())
                    {
                        if let Some(folder) = item.folder_id {
                            *counts.entry(folder).or_default() += 1;
                        }
                    }
                    Ok(ManagerEvent::Wizard(WizardEvent::Folders(folders, counts)))
                }));
            }
            ManagerAction::WizardTerminalLogin => {
                let Some(wizard) = manager.wizard.as_mut() else {
                    return;
                };
                wizard.busy = true;
                wizard.error = None;
                let draft = wizard.draft_settings(&settings);
                let password = MasterPassword::new(wizard.master_password.to_string());
                manager.job = Some(vault_job(&reporter, draft, move |account| {
                    let mut vault = account.vault(None);
                    let status = block_on(vault.status())??;
                    if status.status == VaultStatus::Unauthenticated {
                        return Ok(ManagerEvent::Wizard(WizardEvent::StillSignedOut));
                    }
                    // A terminal login leaves the account signed in but
                    // locked; the password typed in this wizard unlocks it.
                    block_on(vault.unlock(&password))??;
                    Ok(ManagerEvent::Wizard(WizardEvent::LoggedIn(
                        vault.take_session().ok_or(VaultError::Locked)?,
                    )))
                }));
            }
            ManagerAction::WizardFolderNext => {
                let Some(wizard) = manager.wizard.as_mut() else {
                    return;
                };
                let Some(session) = wizard.session.as_ref().map(copy_key) else {
                    return;
                };
                wizard.busy = true;
                wizard.error = None;
                let draft = wizard.draft_settings(&settings);
                let choice = wizard.folder;
                let name = wizard.new_folder.trim().to_owned();
                let existing = match choice {
                    FolderChoice::Existing(id) => wizard
                        .folders
                        .iter()
                        .find(|folder| folder.id == id)
                        .cloned(),
                    FolderChoice::Create => None,
                };
                manager.job = Some(vault_job(&reporter, draft, move |account| {
                    let mut vault = account.vault(Some(session));
                    let folder = match existing {
                        Some(folder) => folder,
                        None => block_on(vault.create_folder(&name))??,
                    };
                    let session = vault.take_session().ok_or(VaultError::Locked)?;
                    Ok(ManagerEvent::Wizard(WizardEvent::FolderReady(
                        folder, session,
                    )))
                }));
            }
            ManagerAction::WizardFinish => {
                let Some(wizard) = manager.wizard.take() else {
                    return;
                };
                let mut settings = wizard.draft_settings(&settings);
                let folder = match wizard.folder {
                    FolderChoice::Existing(id) => wizard
                        .folders
                        .iter()
                        .find(|folder| folder.id == id)
                        .cloned(),
                    FolderChoice::Create => None,
                };
                let mut store_error = None;
                if let Some(profile) = settings.profile.as_mut() {
                    profile.folder_id = folder.as_ref().map(|folder| folder.id);
                    profile.folder_name = folder.map(|folder| folder.name).unwrap_or_default();
                    profile.policy = wizard.chosen_policy();
                    if let Some(key) = wizard.session.as_ref()
                        && let Err(error) = session::store(profile, key)
                    {
                        store_error = Some(error);
                    }
                }
                self.vault_session = wizard.session.as_ref().map(copy_key);
                self.vault_state = Some(if self.vault_session.is_some() {
                    VaultState::Unlocked
                } else {
                    VaultState::Locked
                });
                let previous = self
                    .store
                    .bitwarden
                    .profile
                    .as_ref()
                    .map(|profile| profile.id);
                let done = settings.profile.as_ref().map(|profile| SetupDone {
                    service: HostGroup::Bitwarden,
                    account: profile.email.clone(),
                    place: profile.folder_name.clone(),
                    store_error,
                });
                self.store.bitwarden = settings;
                if let Some(previous) = previous.filter(|id| *id != wizard.profile_id) {
                    session::remove_account(previous);
                    self.store.hosts.retain(|host| !host.is_bitwarden_host());
                }
                self.save_bitwarden_settings();
                self.set_status_key("bw_setup_done");
                // The wizard's job is done: the window says so and offers
                // the next step instead of leaving the user on a settings page.
                self.credential_manager = None;
                self.setup_done = done;
                // The folder's items become hosts right away.
                self.refresh_vault_hosts();
            }
        }
    }

    fn poll_credential_manager(&mut self) {
        let catalog = self.catalog.clone();
        let Some(manager) = self.credential_manager.as_mut() else {
            return;
        };
        if let Some(wizard) = manager.wizard.as_mut()
            && let Some(link) = wizard.login.as_mut()
            && let Ok(prompt) = link.prompts.try_recv()
        {
            // bw waits at a code prompt; the code step takes the input.
            link.waiting = Some(prompt);
            wizard.step = WizardStep::Code;
            wizard.busy = false;
            wizard.error = None;
            wizard.code.clear();
        }
        let Some(result) = manager.job.as_ref().and_then(Job::poll) else {
            return;
        };
        manager.job = None;
        if let Some(wizard) = manager.wizard.as_mut() {
            wizard.busy = false;
            wizard.login = None;
        }
        let mut follow_up = None;
        let mut hosts_changed = false;
        let mut session_copy = None;
        match result {
            Ok(Ok(ManagerEvent::Status(status))) => {
                if let Some(report) = &status.report {
                    self.vault_state = Some(if report.status == VaultStatus::Unauthenticated {
                        VaultState::SignedOut
                    } else if status.unlocked {
                        VaultState::Unlocked
                    } else {
                        VaultState::Locked
                    });
                }
                manager.status = Some(status);
            }
            Ok(Ok(ManagerEvent::Items(session, items, reconciled))) => {
                if session.is_some() {
                    session_copy = session.as_ref().map(copy_key);
                    manager.session = session;
                }
                manager.items = items;
                manager.problems = reconciled.problems.clone();
                hosts_changed = true;
            }
            Ok(Ok(ManagerEvent::Saved(session))) => {
                if session.is_some() {
                    session_copy = session.as_ref().map(copy_key);
                    manager.session = session;
                }
                manager.message = Some((false, catalog.text("bw_done").to_owned()));
                follow_up = Some(if manager.tab == ManagerTab::Items {
                    ManagerAction::LoadItems
                } else {
                    ManagerAction::RefreshStatus
                });
            }
            Ok(Ok(ManagerEvent::Locked)) => {
                manager.session = None;
                manager.message = Some((false, catalog.text("bw_status_locked").to_owned()));
                follow_up = Some(ManagerAction::RefreshStatus);
            }
            Ok(Ok(ManagerEvent::Wizard(event))) => {
                if let Some(wizard) = manager.wizard.as_mut() {
                    match event {
                        WizardEvent::LoggedIn(session) => {
                            wizard.session = Some(session);
                            wizard.step = WizardStep::Folder;
                            follow_up = Some(ManagerAction::WizardFolders);
                        }
                        WizardEvent::LoginStopped(LoginStep::MethodRequired) => {
                            wizard.step = WizardStep::Code;
                            wizard.choose_method = true;
                        }
                        WizardEvent::LoginStopped(LoginStep::UnexpectedPrompt) => {
                            wizard.step = WizardStep::Code;
                            wizard.offer_terminal = true;
                            wizard.error =
                                Some(catalog.text("wizard_unexpected_prompt").to_owned());
                        }
                        WizardEvent::LoginStopped(_) => wizard.step = WizardStep::Login,
                        WizardEvent::StillSignedOut => {
                            wizard.error = Some(catalog.text("wizard_still_signed_out").to_owned())
                        }
                        WizardEvent::Folders(folders, counts) => {
                            let existing = folders
                                .iter()
                                .find(|folder| folder.name == wizard.new_folder.trim())
                                .map(|folder| folder.id);
                            wizard.folder =
                                existing.map_or(FolderChoice::Create, FolderChoice::Existing);
                            wizard.folders = folders;
                            wizard.folder_counts = counts;
                        }
                        WizardEvent::FolderReady(folder, session) => {
                            wizard.folders.retain(|known| known.id != folder.id);
                            wizard.folders.push(folder.clone());
                            wizard.folder = FolderChoice::Existing(folder.id);
                            wizard.session = Some(session);
                            wizard.step = WizardStep::Policy;
                        }
                    }
                }
            }
            Ok(Err(error)) => {
                let text = word(&catalog, &error);
                if let Some(wizard) = manager.wizard.as_mut() {
                    // Failures the window can neither explain nor retry
                    // usefully leave the terminal hand-off as the way on.
                    wizard.offer_terminal = matches!(
                        error,
                        JobError::Vault(
                            VaultError::VerificationRequired
                                | VaultError::Cli { .. }
                                | VaultError::Run(_)
                                | VaultError::Output(_)
                        )
                    );
                    wizard.error = Some(text);
                } else {
                    let state = match error {
                        JobError::Vault(VaultError::Locked) => Some(VaultState::Locked),
                        JobError::Vault(VaultError::SignedOut) => Some(VaultState::SignedOut),
                        _ => None,
                    };
                    if state.is_some() {
                        manager.session = None;
                        self.vault_session = None;
                        self.vault_state = state;
                        follow_up = Some(ManagerAction::RefreshStatus);
                    }
                    manager.message = Some((true, text));
                }
            }
            Err(error) => {
                if let Some(wizard) = manager.wizard.as_mut() {
                    wizard.error = Some(error);
                } else {
                    manager.message = Some((true, error));
                }
            }
        }
        if session_copy.is_some() {
            self.vault_session = session_copy;
            self.vault_state = Some(VaultState::Unlocked);
            if !hosts_changed {
                self.refresh_vault_hosts_quietly();
            }
        }
        if hosts_changed {
            if let Err(error) = self.refresh_hosts() {
                self.set_status_format("storage_error", &[("error", &error)]);
            }
            if let Some(manager) = self.credential_manager.as_ref() {
                self.vault_problems = manager.problems.clone();
            }
        }
        if let Some(action) = follow_up {
            self.manager_act(action);
        }
    }

    pub(super) fn bitwarden_dialogs(&mut self, context: &egui::Context) {
        self.enforce_vault_policy();
        self.poll_vault_job();
        self.poll_onepassword();
        self.approval_unlock();
        self.unlock_prompt_dialog(context);
        self.credential_manager_dialog(context);
        self.setup_done_dialog(context);
    }

    pub(super) fn bitwarden_dialog_open(&self) -> bool {
        self.unlock_prompt.is_some()
            || self.credential_manager.is_some()
            || self.setup_done.is_some()
    }

    /// Says a password manager's setup is finished and what the user can do
    /// now.
    fn setup_done_dialog(&mut self, context: &egui::Context) {
        let Some(done) = &self.setup_done else {
            return;
        };
        let Some(texts) = done.service.texts() else {
            self.setup_done = None;
            return;
        };
        let service = done.service;
        let catalog = &self.catalog;
        let choice = super::dialogs::dialog_shell(
            context,
            "setup_done",
            520.0,
            catalog.text(texts.done_title),
            |ui| {
                let place = if done.place.is_empty() {
                    catalog.text("none")
                } else {
                    done.place.as_str()
                };
                ui.add(
                    egui::Label::new(catalog.format(
                        texts.done_text,
                        &[("account", done.account.as_str()), ("place", place)],
                    ))
                    .wrap(),
                );
                ui.add_space(8.0);
                for key in texts.done_next {
                    ui.add(egui::Label::new(format!("•  {}", catalog.text(key))).wrap());
                }
                if let Some(error) = &done.store_error {
                    ui.add_space(8.0);
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        catalog.format("bw_done_store_error", &[("error", error.as_str())]),
                    );
                }
                super::dialogs::dialog_button_row(
                    ui,
                    catalog.text(texts.done_add_host),
                    false,
                    catalog.text("bw_done_close"),
                )
            },
        );
        match choice {
            Some(true) => {
                self.setup_done = None;
                self.switch_editor(EditorSwitch::New(service));
            }
            Some(false) => self.setup_done = None,
            None => {}
        }
    }

    /// Runs the unlock an SSH call approval was allowed with and reports the
    /// result back, so the calls go ahead without a second prompt.
    fn approval_unlock(&mut self) {
        if let Some(ApprovalUnlock(job)) = &self.approval_unlock {
            let Some(result) = job.poll() else {
                return;
            };
            self.approval_unlock = None;
            let outcome = match result {
                Ok(Ok(session)) => {
                    if session.is_some() {
                        self.vault_session = session;
                        self.vault_state = Some(VaultState::Unlocked);
                    }
                    if let Some(manager) = self.credential_manager.as_mut() {
                        manager.session = self.vault_session.as_ref().map(copy_key);
                    }
                    Ok(())
                }
                Ok(Err(error)) => Err(word(&self.catalog, &error)),
                Err(error) => Err(error),
            };
            let unlocked = outcome.is_ok();
            if let Some(temporary) = self.temporary.as_mut() {
                temporary.finish_approval_unlock(outcome);
            }
            if unlocked {
                self.refresh_vault_hosts_quietly();
            }
            return;
        }
        let Some(mut password) = self
            .temporary
            .as_mut()
            .and_then(|temporary| temporary.take_approval_unlock())
        else {
            return;
        };
        let password = MasterPassword::new(std::mem::take(&mut *password));
        let settings = self.store.bitwarden.clone();
        self.approval_unlock = Some(ApprovalUnlock(vault_job(
            &self.reporter(),
            settings,
            move |account| {
                let mut vault = unlock_and_store(&account, &password)?;
                Ok(vault.take_session())
            },
        )));
    }

    /// The master-password prompt: raised by a tool call through the window
    /// pipe (shown while the call waits, answered through `temporary`) or by
    /// a window task that runs once the vault is unlocked.
    fn unlock_prompt_dialog(&mut self, context: &egui::Context) {
        let pending = self
            .temporary
            .as_ref()
            .and_then(|temporary| temporary.bitwarden_unlock_pending());
        match (
            pending,
            self.unlock_prompt.as_ref().map(|prompt| prompt.request),
        ) {
            (Some(id), Some(Some(current))) if id == current => {}
            (Some(id), _) => self.open_unlock_prompt(Some(id), None),
            (None, Some(Some(_))) => {
                // The call stopped waiting; a window task keeps the prompt.
                if let Some(prompt) = self.unlock_prompt.as_mut() {
                    prompt.request = None;
                    if prompt.then.is_none() {
                        self.unlock_prompt = None;
                    }
                }
            }
            (None, _) => {}
        }
        let catalog = self.catalog.clone();
        let settings = self.store.bitwarden.clone();
        let reporter = self.reporter();
        let mut finished: Option<(Option<Uuid>, &'static str, Option<SessionKey>)> = None;
        let mut unlock = false;
        {
            let Some(prompt) = self.unlock_prompt.as_mut() else {
                return;
            };
            if let Some(result) = prompt.job.as_ref().and_then(Job::poll) {
                prompt.job = None;
                match result {
                    Ok(Ok(session)) => finished = Some((prompt.request, "unlocked", session)),
                    Ok(Err(error)) => prompt.error = Some(word(&catalog, &error)),
                    Err(error) => prompt.error = Some(error),
                }
            }
            if finished.is_none() {
                let busy = prompt.job.is_some();
                dialog_shell(
                    context,
                    "bitwarden_unlock_prompt",
                    460.0,
                    catalog.text("unlock_prompt_title"),
                    |ui| {
                        ui.add(
                            egui::Label::new(catalog.text(if prompt.request.is_some() {
                                "unlock_prompt_text"
                            } else {
                                "unlock_prompt_window_text"
                            }))
                            .wrap(),
                        );
                        ui.add_space(8.0);
                        if let Some(error) = &prompt.error {
                            ui.colored_label(ui.visuals().error_fg_color, error);
                        }
                        ui.horizontal(|ui| {
                            ui.label(catalog.text("master_password"));
                            let key = egui::Id::new("unlock_prompt_password");
                            let revealed = theme::password_revealed(ui, key);
                            let response = ui.add_enabled(
                                !busy,
                                egui::TextEdit::singleline(&mut *prompt.master_password)
                                    .password(!revealed)
                                    .desired_width(240.0),
                            );
                            theme::forget_password_history(ui, &response);
                            theme::reveal_button(ui, &catalog, key);
                            if response.lost_focus()
                                && ui.input(|input| input.key_pressed(egui::Key::Enter))
                                && !prompt.master_password.is_empty()
                            {
                                unlock = true;
                            }
                            if busy {
                                ui.spinner();
                            }
                        });
                        ui.add_space(12.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let ready = !busy && !prompt.master_password.is_empty();
                            if ui
                                .add_enabled(
                                    ready,
                                    theme::primary_button(ui, catalog.text("unlock")),
                                )
                                .clicked()
                            {
                                unlock = true;
                            }
                            if ui.button(catalog.text("cancel")).clicked() {
                                finished = Some((prompt.request, "cancelled", None));
                            }
                        });
                    },
                );
                if unlock && !busy {
                    let password =
                        MasterPassword::new(std::mem::take(&mut *prompt.master_password));
                    prompt.error = None;
                    prompt.job = Some(vault_job(&reporter, settings, move |account| {
                        let mut vault = unlock_and_store(&account, &password)?;
                        Ok(vault.take_session())
                    }));
                }
            }
        }
        if let Some((request, status, session)) = finished {
            if let (Some(id), Some(temporary)) = (request, self.temporary.as_mut()) {
                temporary.finish_bitwarden_unlock(id, status);
            }
            let then = self.unlock_prompt.take().and_then(|prompt| prompt.then);
            if status == "unlocked" {
                if session.is_some() {
                    self.vault_session = session;
                    self.vault_state = Some(VaultState::Unlocked);
                }
                if let Some(manager) = self.credential_manager.as_mut() {
                    manager.session = self.vault_session.as_ref().map(copy_key);
                }
                match then {
                    Some(task) => self.start_vault_task(task),
                    None => self.refresh_vault_hosts_quietly(),
                }
            } else if let Some(VaultTask::Save(_)) = then {
                self.editor_save_failed(catalog.text("bw_err_locked").to_owned());
            }
        }
    }

    fn credential_manager_dialog(&mut self, context: &egui::Context) {
        self.poll_credential_manager();
        let catalog = self.catalog.clone();
        let settings = self.store.bitwarden.clone();
        let hosts = self.store.hosts.clone();
        let mut enabled = settings.enabled;
        let Some(manager) = self.credential_manager.as_mut() else {
            return;
        };
        let mut action = None;
        let mut onepassword_action = None;
        let onepassword_open = self.onepassword.showing();
        // Each page names itself; a service page leads with the way back.
        let title = if manager.wizard.is_some() {
            catalog.text("bw_setup_title")
        } else if onepassword_open {
            self.onepassword.title(&catalog)
        } else if manager.view == ManagerView::Bitwarden {
            "Bitwarden"
        } else {
            catalog.text("credential_manager")
        };
        dialog_shell(context, "credential_manager", 760.0, title, |ui| {
            if let Some(wizard) = manager.wizard.as_mut() {
                action = wizard_body(ui, &catalog, wizard);
                return;
            }
            if onepassword_open {
                onepassword_action =
                    super::onepassword::page(ui, &catalog, &self.store, &mut self.onepassword);
                return;
            }
            if manager.view == ManagerView::Bitwarden {
                if back_link(ui, &catalog) {
                    action = Some(ManagerAction::BackToServices);
                }
                ui.add_space(4.0);
            }
            if manager.view == ManagerView::Services {
                services_overview(
                    ui,
                    &catalog,
                    &settings,
                    &hosts,
                    &mut enabled,
                    manager,
                    &mut action,
                );
                ui.add_space(6.0);
                onepassword_action =
                    super::onepassword::card(ui, &catalog, &self.store, &mut self.onepassword);
            } else {
                bitwarden_page(ui, &catalog, &settings, &hosts, manager, &mut action);
            }
            if close_row(ui, &catalog) {
                action = Some(ManagerAction::Close);
            }
        });
        if enabled != settings.enabled {
            self.store.bitwarden.enabled = enabled;
            self.save_bitwarden_settings();
        }
        if let Some(act) = onepassword_action {
            self.onepassword_act(act);
        }
        if let Some(action) = action {
            self.manager_act(action);
        }
    }

    /// Closes the credential-services dialog, whichever page it shows.
    pub(super) fn close_credential_manager(&mut self) {
        self.onepassword.close();
        self.manager_act(ManagerAction::Close);
    }
}

/// "‹ Back to credential services" at the top of a service page.
pub(super) fn back_link(ui: &mut egui::Ui, catalog: &Catalog) -> bool {
    let back = format!("‹  {}", catalog.text("back_to_services"));
    ui.add(egui::Button::new(back).frame(false))
        .on_hover_text(catalog.text("back_to_services_hint"))
        .clicked()
}

/// The dialog's closing row.
pub(super) fn close_row(ui: &mut egui::Ui, catalog: &Catalog) -> bool {
    ui.add_space(12.0);
    ui.separator();
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.button(catalog.text("close")).clicked()
    })
    .inner
}
/// The first page: every credential service codex-hosts knows, what it
/// holds, whether it is on, and the way into its own management page.
fn services_overview(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    settings: &BitwardenSettings,
    hosts: &[crate::model::HostProfile],
    enabled: &mut bool,
    manager: &mut CredentialManager,
    action: &mut Option<ManagerAction>,
) {
    ui.set_max_width(SETTINGS_WIDTH);
    ui.add(egui::Label::new(catalog.text("services_intro")).wrap());
    ui.add_space(8.0);
    let local_hosts = hosts.iter().filter(|host| !host.is_vault_host()).count();
    let vault_hosts = hosts.iter().filter(|host| host.is_bitwarden_host()).count();
    let busy = manager.job.is_some();

    // Windows Credential Manager: built in, always on.
    super::services::service_card(
        ui,
        catalog,
        super::services::ServiceCard {
            name: catalog.text("service_local_name"),
            text: catalog.text("service_local_text"),
            hosts: local_hosts,
            enabled: None,
            status: catalog.text("service_local_status").to_owned(),
            busy: false,
            button: None,
        },
    );
    ui.add_space(6.0);

    // Bitwarden: switchable once set up, with its own page.
    let configured = settings.profile.is_some() && settings.cli_path.is_some();
    let status = match settings.profile.as_ref().filter(|_| configured) {
        None => catalog.text("bw_not_configured").to_owned(),
        Some(profile) => {
            let state = if !*enabled {
                catalog.text("service_status_disabled")
            } else {
                match &manager.status {
                    None => catalog.text("bw_status_unknown"),
                    Some(status) => match status.report.as_ref().map(|report| report.status) {
                        Some(VaultStatus::Unauthenticated) => catalog.text("bw_status_signed_out"),
                        Some(_) if status.unlocked => catalog.text("bw_status_unlocked"),
                        Some(_) => catalog.text("bw_status_locked"),
                        None => catalog.text("bw_status_unknown"),
                    },
                }
            };
            format!(
                "{state}  ·  {}  ·  {}",
                profile.email,
                if profile.folder_name.is_empty() {
                    catalog.text("none")
                } else {
                    &profile.folder_name
                }
            )
        }
    };
    let clicked = super::services::service_card(
        ui,
        catalog,
        super::services::ServiceCard {
            name: "Bitwarden",
            text: catalog.text("service_bitwarden_text"),
            hosts: vault_hosts,
            enabled: configured.then_some(enabled),
            status,
            busy,
            button: Some(catalog.text(if configured {
                "service_manage"
            } else {
                "bw_start_setup"
            })),
        },
    );
    if clicked {
        *action = Some(if configured {
            ManagerAction::OpenBitwarden
        } else {
            ManagerAction::StartWizard
        });
    }
    if let Some((is_error, text)) = &manager.message {
        ui.add_space(6.0);
        let color = if *is_error {
            ui.visuals().error_fg_color
        } else {
            ui.visuals().strong_text_color()
        };
        ui.colored_label(color, text);
    }
}

/// The Bitwarden service page: account, folder items, settings.
fn bitwarden_page(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    settings: &BitwardenSettings,
    hosts: &[crate::model::HostProfile],
    manager: &mut CredentialManager,
    action: &mut Option<ManagerAction>,
) {
    ui.horizontal(|ui| {
        for (tab, key) in [
            (ManagerTab::Account, "bw_tab_account"),
            (ManagerTab::Items, "bw_tab_items"),
            (ManagerTab::Settings, "bw_tab_settings"),
        ] {
            if ui
                .selectable_label(manager.tab == tab, catalog.text(key))
                .clicked()
                && manager.tab != tab
            {
                manager.tab = tab;
                // A message belongs to the tab that produced it.
                manager.message = None;
                if tab == ManagerTab::Items && settings.available() {
                    *action = Some(ManagerAction::LoadItems);
                }
            }
        }
        if manager.job.is_some() {
            ui.spinner();
        }
    });
    ui.add_space(8.0);
    if let Some((is_error, text)) = &manager.message {
        let color = if *is_error {
            ui.visuals().error_fg_color
        } else {
            ui.visuals().strong_text_color()
        };
        ui.colored_label(color, text);
    }
    match manager.tab {
        ManagerTab::Account => account_tab(ui, catalog, settings, manager, action),
        ManagerTab::Items => items_tab(ui, catalog, settings, hosts, manager, action),
        ManagerTab::Settings => settings_tab(ui, catalog, settings, manager, action),
    }
}

fn account_tab(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    settings: &BitwardenSettings,
    manager: &mut CredentialManager,
    action: &mut Option<ManagerAction>,
) {
    let Some(profile) = settings
        .profile
        .as_ref()
        .filter(|_| settings.cli_path.is_some())
    else {
        ui.add(egui::Label::new(catalog.text("bw_intro")).wrap());
        ui.add_space(8.0);
        if ui
            .add(theme::primary_button(ui, catalog.text("bw_start_setup")))
            .clicked()
        {
            *action = Some(ManagerAction::StartWizard);
        }
        return;
    };
    let busy = manager.job.is_some();
    egui::Grid::new("bw_account")
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            ui.label(catalog.text("bw_account"));
            ui.label(&profile.email);
            ui.end_row();
            ui.label(catalog.text("bw_server"));
            ui.label(profile.server_url.as_deref().unwrap_or(US_SERVER));
            ui.end_row();
            ui.label(catalog.text("bw_folder"));
            ui.label(if profile.folder_name.is_empty() {
                catalog.text("none")
            } else {
                &profile.folder_name
            });
            ui.end_row();
            ui.label(catalog.text("bw_status"));
            ui.label(match &manager.status {
                None => catalog.text("bw_status_unknown").to_owned(),
                Some(status) => {
                    let mut text = match status.report.as_ref().map(|report| report.status) {
                        Some(VaultStatus::Unauthenticated) => catalog.text("bw_status_signed_out"),
                        Some(_) if status.unlocked => catalog.text("bw_status_unlocked"),
                        Some(_) => catalog.text("bw_status_locked"),
                        None => catalog.text("bw_status_unknown"),
                    }
                    .to_owned();
                    if let Some((version, support)) = &status.version {
                        text.push_str(&format!(
                            "  ·  bw {version} ({})",
                            catalog.text(match support {
                                VersionSupport::Verified => "verified",
                                VersionSupport::Unverified => "unverified",
                            })
                        ));
                    }
                    text.push_str("  ·  ");
                    // With "before every use" the copy is never kept fresh
                    // on purpose, so "stale" would only alarm.
                    if settings.sync_max_age_seconds == 0 {
                        text.push_str(&catalog.format(
                            "bw_sync_label",
                            &[("window", catalog.text("bw_sync_each_use"))],
                        ));
                    } else {
                        text.push_str(catalog.text(
                            if session::sync_is_fresh(profile.id, settings.sync_max_age_seconds) {
                                "bw_sync_fresh"
                            } else {
                                "bw_sync_stale"
                            },
                        ));
                    }
                    text
                }
            });
            ui.end_row();
        });
    ui.add_space(8.0);
    let unlocked = manager
        .status
        .as_ref()
        .is_some_and(|status| status.unlocked);
    let signed_out = manager.status.as_ref().is_some_and(|status| {
        status
            .report
            .as_ref()
            .is_some_and(|report| report.status == VaultStatus::Unauthenticated)
    });
    if signed_out {
        // A signed-out account needs a full login; the wizard runs it with
        // its code prompts, and the terminal is only the last resort.
        ui.add(egui::Label::new(catalog.text("bw_signed_out_hint")).wrap());
        ui.add_space(6.0);
        if ui
            .add_enabled(!busy, theme::primary_button(ui, catalog.text("bw_relogin")))
            .clicked()
        {
            *action = Some(ManagerAction::StartWizard);
        }
        ui.add_space(6.0);
        egui::CollapsingHeader::new(catalog.text("terminal_login_title"))
            .id_salt("bw_terminal_login")
            .show(ui, |ui| {
                ui.add(egui::Label::new(catalog.text("terminal_login_text")).wrap());
                let commands = terminal_login_commands(settings, profile);
                let mut shown = commands.as_str();
                ui.add(
                    egui::TextEdit::multiline(&mut shown)
                        .code_editor()
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                if ui.button(catalog.text("copy_commands")).clicked() {
                    ui.ctx().copy_text(commands);
                    manager.message = Some((false, catalog.text("copied").to_owned()));
                }
            });
    } else {
        ui.label(
            RichText::new(catalog.text("bw_vault_actions"))
                .weak()
                .small(),
        );
        ui.horizontal(|ui| {
            if !unlocked {
                ui.label(catalog.text("master_password"));
                let key = egui::Id::new("manager_unlock_password");
                let revealed = theme::password_revealed(ui, key);
                let response = ui.add(
                    egui::TextEdit::singleline(&mut *manager.master_password)
                        .password(!revealed)
                        .desired_width(220.0),
                );
                theme::forget_password_history(ui, &response);
                theme::reveal_button(ui, catalog, key);
                let submit =
                    response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                let ready = !busy && !manager.master_password.is_empty();
                if ui
                    .add_enabled(ready, theme::primary_button(ui, catalog.text("unlock")))
                    .clicked()
                    || (submit && ready)
                {
                    *action = Some(ManagerAction::Unlock);
                }
            } else if ui
                .add_enabled(!busy, egui::Button::new(catalog.text("lock")))
                .clicked()
            {
                *action = Some(ManagerAction::Lock);
            }
            if ui
                .add_enabled(!busy && unlocked, egui::Button::new(catalog.text("sync")))
                .on_disabled_hover_text(catalog.text("bw_needs_unlock"))
                .clicked()
            {
                *action = Some(ManagerAction::Sync);
            }
            if ui
                .add_enabled(!busy, egui::Button::new(catalog.text("bw_refresh_status")))
                .clicked()
            {
                *action = Some(ManagerAction::RefreshStatus);
            }
        });
    }
    ui.add_space(12.0);
    ui.label(
        RichText::new(catalog.text("bw_account_actions"))
            .weak()
            .small(),
    );
    match manager.confirm {
        None => {
            ui.horizontal(|ui| {
                // Signed out, "Log in again" above already opens the wizard.
                if !signed_out
                    && ui
                        .add_enabled(!busy, egui::Button::new(catalog.text("bw_reconfigure")))
                        .clicked()
                {
                    *action = Some(ManagerAction::StartWizard);
                }
                if !signed_out
                    && ui
                        .add_enabled(!busy, theme::danger_button(ui, catalog.text("logout")))
                        .clicked()
                {
                    manager.confirm = Some(AccountConfirm::Logout);
                }
                if ui
                    .add_enabled(
                        !busy,
                        theme::danger_button(ui, catalog.text("bw_remove_setup")),
                    )
                    .clicked()
                {
                    manager.confirm = Some(AccountConfirm::RemoveSetup);
                }
            });
        }
        // Both are hard to undo, so each asks once, in place.
        Some(confirm) => {
            let (message, label, confirmed) = match confirm {
                AccountConfirm::Logout => ("bw_confirm_logout", "logout", ManagerAction::Logout),
                AccountConfirm::RemoveSetup => (
                    "bw_confirm_remove",
                    "bw_remove_setup",
                    ManagerAction::RemoveSetup,
                ),
            };
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(catalog.text(message)).wrap());
                ui.horizontal(|ui| {
                    if ui
                        .add(theme::danger_confirm_button(ui, catalog.text(label)))
                        .clicked()
                    {
                        manager.confirm = None;
                        *action = Some(confirmed);
                    }
                    if ui.button(catalog.text("cancel")).clicked() {
                        manager.confirm = None;
                    }
                });
            });
        }
    }
}

/// PowerShell lines that log the account in from the user's own terminal,
/// where `bw` can prompt for a new-device or two-step code.
fn terminal_login_commands(settings: &BitwardenSettings, profile: &BitwardenProfile) -> String {
    let bw = settings
        .cli_path
        .as_ref()
        .map_or_else(|| "bw.exe".to_owned(), |path| path.display().to_string());
    let data_dir = session::data_dir(profile.id)
        .map(|dir| dir.display().to_string())
        .unwrap_or_default();
    ps_login_commands(
        &bw,
        &data_dir,
        profile.server_url.as_deref(),
        &profile.email,
    )
}

/// The PowerShell lines that log an account in from the user's own terminal,
/// where `bw` can prompt for a new-device or two-step code.
fn ps_login_commands(bw: &str, data_dir: &str, server: Option<&str>, email: &str) -> String {
    let mut lines = vec![format!("$env:BITWARDENCLI_APPDATA_DIR = \"{data_dir}\"")];
    if let Some(server) = server.filter(|url| !url.is_empty()) {
        lines.push(format!("& \"{bw}\" config server {server}"));
    }
    lines.push(format!("& \"{bw}\" login {email}"));
    lines.join("\n")
}

fn policy_selector(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    id: &str,
    policy: &mut ReauthPolicy,
    idle_minutes: &mut u32,
) {
    let label = |policy: ReauthPolicy| -> &str {
        match policy {
            ReauthPolicy::PerCall => catalog.text("policy_per_call"),
            ReauthPolicy::Idle { .. } => catalog.text("policy_idle"),
            ReauthPolicy::UntilExit => catalog.text("policy_until_exit"),
            ReauthPolicy::Permanent => catalog.text("policy_permanent"),
        }
    };
    let mut choice = *policy;
    egui::ComboBox::from_id_salt(id)
        .selected_text(label(choice))
        .show_ui(ui, |ui| {
            for candidate in [
                ReauthPolicy::PerCall,
                ReauthPolicy::Idle {
                    minutes: *idle_minutes,
                },
                ReauthPolicy::UntilExit,
                ReauthPolicy::Permanent,
            ] {
                let selected =
                    std::mem::discriminant(&candidate) == std::mem::discriminant(&choice);
                if ui.selectable_label(selected, label(candidate)).clicked() {
                    choice = candidate;
                }
            }
        });
    if let ReauthPolicy::Idle { minutes } = &mut choice {
        ui.add(
            egui::DragValue::new(idle_minutes)
                .range(1..=MAX_REAUTH_IDLE_MINUTES)
                .suffix(format!(" {}", catalog.text("policy_idle_minutes"))),
        );
        *minutes = *idle_minutes;
    }
    *policy = choice;
}

fn sync_window_label(catalog: &Catalog, seconds: u64) -> String {
    match seconds {
        0 => catalog.text("bw_sync_each_use").to_owned(),
        60 => catalog.text("bw_sync_every_minute").to_owned(),
        other if other % 60 == 0 => catalog.format(
            "bw_sync_every_minutes",
            &[("minutes", &(other / 60).to_string())],
        ),
        other => catalog.format("bw_sync_every_seconds", &[("seconds", &other.to_string())]),
    }
}

fn settings_tab(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    settings: &BitwardenSettings,
    manager: &mut CredentialManager,
    action: &mut Option<ManagerAction>,
) {
    // Long hints wrap inside the dialog instead of widening it.
    ui.set_max_width(SETTINGS_WIDTH);
    let hint = |ui: &mut egui::Ui, key: &str| {
        ui.add(egui::Label::new(RichText::new(catalog.text(key)).weak().small()).wrap());
    };
    let Some(profile) = settings
        .profile
        .as_ref()
        .filter(|_| settings.cli_path.is_some())
    else {
        ui.add_space(8.0);
        ui.label(catalog.text("bw_not_configured"));
        return;
    };
    ui.horizontal(|ui| {
        ui.label(catalog.text("bw_policy"));
        let mut policy = profile.policy;
        policy_selector(
            ui,
            catalog,
            "bw_policy_select",
            &mut policy,
            &mut manager.idle_minutes,
        );
        if policy != profile.policy {
            *action = Some(ManagerAction::SetPolicy(policy));
        }
    });
    hint(ui, "bw_policy_hint");
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(catalog.text("bw_sync_window"));
        let mut choice = manager.sync_max_age;
        egui::ComboBox::from_id_salt("bw_sync_window")
            .selected_text(sync_window_label(catalog, choice))
            .show_ui(ui, |ui| {
                for candidate in SYNC_MAX_AGE_CHOICES {
                    if ui
                        .selectable_label(
                            candidate == choice,
                            sync_window_label(catalog, candidate),
                        )
                        .clicked()
                    {
                        choice = candidate;
                    }
                }
            });
        if choice != manager.sync_max_age {
            manager.sync_max_age = choice;
            *action = Some(ManagerAction::SaveSettings);
        }
    });
    hint(ui, "bw_sync_window_hint");
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(catalog.text("bw_cli_path"));
        ui.label(
            RichText::new(
                settings
                    .cli_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            )
            .weak(),
        );
    });
    ui.separator();
    ui.add(egui::Label::new(catalog.text("vault_host_fields_hint")).wrap());
}

fn items_tab(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    settings: &BitwardenSettings,
    hosts: &[crate::model::HostProfile],
    manager: &mut CredentialManager,
    action: &mut Option<ManagerAction>,
) {
    if !settings.available() {
        ui.label(catalog.text("bw_not_configured"));
        return;
    }
    let busy = manager.job.is_some();
    if let Some(form) = manager.fix_form.as_mut() {
        ui.label(RichText::new(catalog.text("vault_fix_title")).strong());
        ui.add(egui::Label::new(catalog.text("vault_fix_text")).wrap());
        egui::Grid::new("bw_fix_form")
            .num_columns(2)
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                ui.label(catalog.text("item_name"));
                ui.text_edit_singleline(&mut form.name);
                ui.end_row();
                ui.label(catalog.text("vault_host_uri"));
                ui.add(
                    egui::TextEdit::singleline(&mut form.uri)
                        .hint_text("ssh://user@host:22")
                        .desired_width(320.0),
                );
                ui.end_row();
                ui.label(catalog.text("username"));
                ui.text_edit_singleline(&mut form.username);
                ui.end_row();
            });
        let mut cancel = false;
        ui.horizontal(|ui| {
            let ready = codex_hosts_bw::parse_host_uri(&form.uri).is_some();
            if ui
                .add_enabled(ready && !busy, egui::Button::new(catalog.text("save")))
                .clicked()
            {
                *action = Some(ManagerAction::FixItem);
            }
            if ui.button(catalog.text("cancel")).clicked() {
                cancel = true;
            }
        });
        if cancel {
            manager.fix_form = None;
        }
        return;
    }
    ui.add(egui::Label::new(catalog.text("vault_items_text")).wrap());
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(catalog.text("bw_search"));
        ui.text_edit_singleline(&mut manager.item_filter);
        if ui
            .add_enabled(!busy, egui::Button::new(catalog.text("bw_new_host")))
            .clicked()
        {
            *action = Some(ManagerAction::NewHost);
        }
        if ui
            .add_enabled(!busy, egui::Button::new(catalog.text("bw_refresh_status")))
            .clicked()
        {
            *action = Some(ManagerAction::LoadItems);
        }
    });
    let filter = manager.item_filter.trim().to_lowercase();
    let mut fix = None;
    egui::ScrollArea::vertical()
        .max_height(320.0)
        .show(ui, |ui| {
            let mut shown = 0;
            for item in &manager.items {
                if !filter.is_empty()
                    && !item.name.to_lowercase().contains(&filter)
                    && !item
                        .username
                        .as_deref()
                        .is_some_and(|user| user.to_lowercase().contains(&filter))
                {
                    continue;
                }
                shown += 1;
                let trashed = item.deleted_date.is_some();
                let served = hosts.iter().any(|host| host.id == item.id);
                let problem = manager
                    .problems
                    .iter()
                    .find(|(name, _)| *name == item.name)
                    .map(|(_, problem)| problem.clone());
                ui.horizontal(|ui| {
                    let mut text = RichText::new(&item.name);
                    if trashed {
                        text = text.weak().strikethrough();
                    }
                    ui.label(text);
                    ui.label(RichText::new(item.username.as_deref().unwrap_or("")).weak());
                    if trashed {
                        ui.label(RichText::new(catalog.text("bw_in_trash")).weak().small());
                    } else if served {
                        ui.label(
                            RichText::new(catalog.text("vault_item_is_host"))
                                .weak()
                                .small(),
                        );
                    } else if let Some(problem) = &problem {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            RichText::new(catalog.text(problem_key(problem))).small(),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if trashed {
                            if ui
                                .add_enabled(!busy, egui::Button::new(catalog.text("bw_restore")))
                                .clicked()
                            {
                                *action = Some(ManagerAction::TrashItem(item.id, true));
                            }
                        } else {
                            if ui
                                .add_enabled(!busy, egui::Button::new(catalog.text("bw_trash")))
                                .clicked()
                            {
                                *action = Some(ManagerAction::TrashItem(item.id, false));
                            }
                            if matches!(
                                problem,
                                Some(ItemProblem::NoHostUri | ItemProblem::NoUsername)
                            ) && ui
                                .add_enabled(!busy, egui::Button::new(catalog.text("vault_fix")))
                                .clicked()
                            {
                                fix = Some(FixForm {
                                    id: item.id,
                                    name: item.name.clone(),
                                    uri: item
                                        .host_uri()
                                        .map(|uri| uri.to_string())
                                        .unwrap_or_default(),
                                    username: item.username.clone().unwrap_or_default(),
                                });
                            }
                        }
                    });
                });
            }
            if shown == 0 {
                ui.label(RichText::new(catalog.text("bw_items_empty")).weak());
            }
        });
    if fix.is_some() {
        manager.fix_form = fix;
    }
}

/// The fall-back when the window cannot finish a login: the same data
/// directory logged in from the user's own terminal, then unlocked here.
fn terminal_handoff(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    wizard: &Wizard,
    busy: bool,
) -> Option<ManagerAction> {
    let mut action = None;
    egui::CollapsingHeader::new(catalog.text("wizard_terminal_title"))
        .id_salt("wizard_terminal_login")
        .open(wizard.offer_terminal.then_some(true))
        .show(ui, |ui| {
            ui.add(egui::Label::new(catalog.text("wizard_terminal_text")).wrap());
            let bw = if wizard.cli_path.trim().is_empty() {
                "bw.exe".to_owned()
            } else {
                wizard.cli_path.trim().to_owned()
            };
            let data_dir = session::data_dir(wizard.profile_id)
                .map(|dir| dir.display().to_string())
                .unwrap_or_default();
            let commands = ps_login_commands(
                &bw,
                &data_dir,
                Some(wizard.server_url().as_str()),
                wizard.email.trim(),
            );
            let mut shown = commands.as_str();
            ui.add(
                egui::TextEdit::multiline(&mut shown)
                    .code_editor()
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            ui.horizontal(|ui| {
                if ui.button(catalog.text("copy_commands")).clicked() {
                    ui.ctx().copy_text(commands);
                }
                if ui
                    .add_enabled(
                        !busy,
                        egui::Button::new(catalog.text("wizard_terminal_done")),
                    )
                    .clicked()
                {
                    action = Some(ManagerAction::WizardTerminalLogin);
                }
            });
        });
    action
}

fn wizard_body(ui: &mut egui::Ui, catalog: &Catalog, wizard: &mut Wizard) -> Option<ManagerAction> {
    let mut action = None;
    let steps = [
        WizardStep::Cli,
        WizardStep::Login,
        WizardStep::Code,
        WizardStep::Folder,
        WizardStep::Policy,
    ];
    super::dialogs::wizard_header(
        ui,
        catalog,
        &[
            "wizard_step_cli",
            "wizard_step_login",
            "wizard_step_code",
            "wizard_step_folder",
            "wizard_step_policy",
        ],
        steps
            .iter()
            .position(|step| *step == wizard.step)
            .unwrap_or_default(),
        wizard.error.as_deref(),
        wizard.busy.then(|| catalog.text("busy")),
    );
    let busy = wizard.busy;
    match wizard.step {
        WizardStep::Cli => {
            ui.add(egui::Label::new(catalog.text("wizard_cli_hint")).wrap());
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut wizard.cli_path).desired_width(420.0));
                if ui.button(catalog.text("wizard_browse_cli")).clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("bw", &["exe"])
                        .pick_file()
                {
                    wizard.cli_path = path.display().to_string();
                }
            });
            let ready = std::path::Path::new(wizard.cli_path.trim()).is_file();
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(ready, egui::Button::new(catalog.text("wizard_next")))
                    .clicked()
                {
                    wizard.step = WizardStep::Login;
                }
                if ui.button(catalog.text("cancel")).clicked() {
                    action = Some(ManagerAction::WizardCancel);
                }
            });
        }
        WizardStep::Login => {
            ui.add(egui::Label::new(catalog.text("wizard_login_text")).wrap());
            ui.add_space(4.0);
            egui::Grid::new("wizard_login")
                .num_columns(2)
                .spacing([16.0, 6.0])
                .show(ui, |ui| {
                    ui.label(catalog.text("email"));
                    ui.add(egui::TextEdit::singleline(&mut wizard.email).desired_width(320.0));
                    ui.end_row();
                    ui.label(catalog.text("bw_server"));
                    ui.vertical(|ui| {
                        ui.radio_value(
                            &mut wizard.server,
                            ServerChoice::Us,
                            catalog.text("wizard_server_us"),
                        );
                        ui.radio_value(
                            &mut wizard.server,
                            ServerChoice::Eu,
                            catalog.text("wizard_server_eu"),
                        );
                        ui.radio_value(
                            &mut wizard.server,
                            ServerChoice::Custom,
                            catalog.text("wizard_server_custom"),
                        );
                        if wizard.server == ServerChoice::Custom {
                            ui.add(
                                egui::TextEdit::singleline(&mut wizard.custom_url)
                                    .hint_text("https://vault.example.com")
                                    .desired_width(320.0),
                            );
                        }
                    });
                    ui.end_row();
                    ui.label(catalog.text("master_password"));
                    ui.horizontal(|ui| {
                        let key = egui::Id::new("wizard_master_password");
                        let revealed = theme::password_revealed(ui, key);
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut *wizard.master_password)
                                .password(!revealed)
                                .desired_width(320.0 - theme::control_height(ui) - 8.0),
                        );
                        theme::forget_password_history(ui, &response);
                        theme::reveal_button(ui, catalog, key);
                    });
                    ui.end_row();
                });
            ui.label(
                RichText::new(catalog.text("wizard_login_hint"))
                    .weak()
                    .small(),
            );
            if wizard.offer_terminal
                && !busy
                && let Some(chosen) = terminal_handoff(ui, catalog, wizard, busy)
            {
                action = Some(chosen);
            }
            let ready = !busy
                && wizard.email.contains('@')
                && !wizard.master_password.is_empty()
                && (wizard.server != ServerChoice::Custom
                    || wizard.custom_url.trim().starts_with("https://"));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        ready,
                        theme::primary_button(ui, catalog.text("wizard_login")),
                    )
                    .clicked()
                {
                    wizard.use_method = false;
                    action = Some(ManagerAction::WizardLogin);
                }
                if ui
                    .add_enabled(!busy, egui::Button::new(catalog.text("wizard_back")))
                    .clicked()
                {
                    wizard.step = WizardStep::Cli;
                }
                if ui.button(catalog.text("cancel")).clicked() {
                    action = Some(ManagerAction::WizardCancel);
                }
            });
        }
        WizardStep::Code => {
            let waiting = wizard.login.as_ref().map(|link| link.waiting);
            match waiting {
                // bw waits at a prompt: one field for the code it asked for.
                Some(Some(prompt)) => {
                    let text = match prompt {
                        CodePrompt::NewDevice => catalog
                            .format("wizard_new_device_text", &[("email", wizard.email.trim())]),
                        CodePrompt::TwoStep => catalog.text("wizard_two_step_text").to_owned(),
                    };
                    ui.add(egui::Label::new(text).wrap());
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(catalog.text("wizard_code"));
                        let response = ui
                            .add(egui::TextEdit::singleline(&mut wizard.code).desired_width(160.0));
                        if !response.has_focus() && wizard.code.is_empty() {
                            response.request_focus();
                        }
                        let submit = response.lost_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter));
                        let ready = !wizard.code.trim().is_empty();
                        if ui
                            .add_enabled(
                                ready,
                                theme::primary_button(ui, catalog.text("wizard_verify")),
                            )
                            .clicked()
                            || (submit && ready)
                        {
                            action = Some(ManagerAction::WizardSubmitCode);
                        }
                    });
                }
                // bw is checking the code or talking to the server.
                Some(None) => {}
                // No login runs: a method to pick, or a failed attempt.
                None if wizard.choose_method => {
                    ui.add(egui::Label::new(catalog.text("wizard_method_text")).wrap());
                    ui.horizontal(|ui| {
                        for (method, key) in [
                            (TwoFactorMethod::Authenticator, "method_authenticator"),
                            (TwoFactorMethod::Email, "method_email"),
                            (TwoFactorMethod::YubiKey, "method_yubikey"),
                        ] {
                            ui.radio_value(&mut wizard.method, method, catalog.text(key));
                        }
                    });
                }
                None => {
                    ui.add(
                        egui::Label::new(RichText::new(catalog.text("wizard_retry_hint")).weak())
                            .wrap(),
                    );
                }
            }
            if waiting.is_none() {
                ui.add_space(6.0);
                if let Some(chosen) = terminal_handoff(ui, catalog, wizard, busy) {
                    action = Some(chosen);
                }
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| match waiting {
                Some(_) => {
                    if ui.button(catalog.text("wizard_stop_login")).clicked() {
                        action = Some(ManagerAction::WizardStopLogin);
                    }
                }
                None => {
                    let label = if wizard.choose_method {
                        "wizard_next"
                    } else {
                        "wizard_retry_login"
                    };
                    if ui
                        .add_enabled(!busy, theme::primary_button(ui, catalog.text(label)))
                        .clicked()
                    {
                        wizard.use_method |= wizard.choose_method;
                        action = Some(ManagerAction::WizardRetryLogin);
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new(catalog.text("wizard_back")))
                        .clicked()
                    {
                        wizard.step = WizardStep::Login;
                        wizard.code.clear();
                        wizard.choose_method = false;
                        wizard.use_method = false;
                        wizard.offer_terminal = false;
                        wizard.error = None;
                    }
                    if ui.button(catalog.text("cancel")).clicked() {
                        action = Some(ManagerAction::WizardCancel);
                    }
                }
            });
        }
        WizardStep::Folder => {
            ui.add(egui::Label::new(catalog.text("wizard_folder_text")).wrap());
            ui.add_space(6.0);
            let mut choice = wizard.folder;
            if wizard.folders.is_empty() {
                ui.label(RichText::new(catalog.text("wizard_folder_none")).weak());
            } else {
                ui.label(RichText::new(catalog.text("wizard_folder_existing")).strong());
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        for folder in &wizard.folders {
                            let count = wizard.folder_counts.get(&folder.id).copied().unwrap_or(0);
                            let label = format!(
                                "{}  ·  {}",
                                folder.name,
                                catalog.format(
                                    "wizard_folder_items",
                                    &[("count", &count.to_string())]
                                )
                            );
                            ui.radio_value(&mut choice, FolderChoice::Existing(folder.id), label);
                        }
                    });
                ui.add_space(6.0);
            }
            ui.horizontal(|ui| {
                ui.radio_value(
                    &mut choice,
                    FolderChoice::Create,
                    catalog.text("wizard_folder_create"),
                );
                ui.add_enabled(
                    choice == FolderChoice::Create,
                    egui::TextEdit::singleline(&mut wizard.new_folder).desired_width(220.0),
                );
            });
            ui.label(
                RichText::new(catalog.text("wizard_folder_hint"))
                    .weak()
                    .small(),
            );
            wizard.folder = choice;
            let ready = !busy
                && match wizard.folder {
                    FolderChoice::Existing(_) => true,
                    FolderChoice::Create => !wizard.new_folder.trim().is_empty(),
                };
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        ready,
                        theme::primary_button(ui, catalog.text("wizard_next")),
                    )
                    .clicked()
                {
                    action = Some(ManagerAction::WizardFolderNext);
                }
                if ui
                    .add_enabled(!busy, egui::Button::new(catalog.text("bw_refresh_status")))
                    .clicked()
                {
                    action = Some(ManagerAction::WizardFolders);
                }
                if ui.button(catalog.text("cancel")).clicked() {
                    action = Some(ManagerAction::WizardCancel);
                }
            });
        }
        WizardStep::Policy => {
            ui.add(egui::Label::new(catalog.text("wizard_policy_text")).wrap());
            ui.horizontal(|ui| {
                policy_selector(
                    ui,
                    catalog,
                    "wizard_policy",
                    &mut wizard.policy,
                    &mut wizard.idle_minutes,
                );
            });
            ui.label(RichText::new(catalog.text("bw_policy_hint")).weak().small());
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !busy,
                        theme::primary_button(ui, catalog.text("wizard_finish")),
                    )
                    .clicked()
                {
                    action = Some(ManagerAction::WizardFinish);
                }
                if ui.button(catalog.text("cancel")).clicked() {
                    action = Some(ManagerAction::WizardCancel);
                }
            });
        }
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(email: Option<&str>, server: Option<&str>) -> StatusReport {
        StatusReport {
            status: VaultStatus::Locked,
            server_url: server.map(str::to_owned),
            user_email: email.map(str::to_owned),
            last_sync: None,
        }
    }

    #[test]
    fn an_existing_login_is_reused_only_for_the_same_account_and_server() {
        assert!(same_account(
            &status(Some("Me@Example.com"), Some("https://vault.bitwarden.com/")),
            " me@example.com ",
            US_SERVER,
        ));
        assert!(same_account(
            &status(Some("me@example.com"), None),
            "me@example.com",
            EU_SERVER,
        ));
        assert!(!same_account(
            &status(Some("me@example.com"), Some(US_SERVER)),
            "other@example.com",
            US_SERVER,
        ));
        assert!(!same_account(
            &status(Some("me@example.com"), Some(US_SERVER)),
            "me@example.com",
            EU_SERVER,
        ));
        assert!(!same_account(&status(None, Some(US_SERVER)), "", US_SERVER));
    }

    #[test]
    fn a_new_host_item_carries_the_uri_the_fields_and_the_folder() {
        let write = HostWrite {
            provider: VaultProvider::Bitwarden,
            item_id: None,
            external_id: None,
            name: "build-box".to_owned(),
            host_uri: "ssh://10.0.0.5:2222".to_owned(),
            username: "deploy".to_owned(),
            password: FieldEdit::Set(Zeroizing::new("pw".to_owned())),
            description: " front door ".to_owned(),
            tags: vec!["prod".to_owned(), "web".to_owned()],
            expected_revision: String::new(),
        };
        let folder = Uuid::from_u128(7);
        let json: serde_json::Value =
            serde_json::from_slice(&host_item_json(&write, Some(folder))).unwrap();
        assert_eq!(json["type"], 1);
        assert_eq!(json["name"], "build-box");
        assert_eq!(json["folderId"], folder.to_string());
        assert_eq!(json["login"]["username"], "deploy");
        assert_eq!(json["login"]["password"], "pw");
        assert_eq!(json["login"]["uris"][0]["uri"], "ssh://10.0.0.5:2222");
        let fields = json["fields"].as_array().unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0]["name"], "codex-hosts:description");
        assert_eq!(fields[0]["value"], "front door");
        assert_eq!(fields[1]["name"], "codex-hosts:tags");
        assert_eq!(fields[1]["value"], "prod, web");
        assert_eq!(
            sync_window_label(&Catalog::for_locale(Some("en")), 300),
            "At most every 5 minutes"
        );
    }

    fn render_wizard(
        context: &egui::Context,
        catalog: &Catalog,
        wizard: &mut Wizard,
    ) -> Vec<String> {
        let mut texts = Vec::new();
        for _ in 0..2 {
            let output = context.run_ui(egui::RawInput::default(), |ui| {
                wizard_body(ui, catalog, wizard);
            });
            texts = output
                .shapes
                .iter()
                .flat_map(|shape| shape_texts(&shape.shape))
                .collect();
        }
        texts
    }

    fn shape_texts(shape: &egui::Shape) -> Vec<String> {
        match shape {
            egui::Shape::Text(text) => vec![text.galley.job.text.clone()],
            egui::Shape::Vec(shapes) => shapes.iter().flat_map(shape_texts).collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn the_code_step_follows_the_prompt_bw_shows_and_offers_the_terminal_last() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            let catalog = Catalog::for_locale(Some(locale));
            let context = egui::Context::default();
            super::super::theme::configure_fonts(&context, locale);
            let mut wizard = Wizard::new(&BitwardenSettings::default());
            wizard.email = "me@example.com".to_owned();
            wizard.step = WizardStep::Code;
            let (_prompts_sender, prompts) = mpsc::channel();
            let (answers, _answers_receiver) = tokio::sync::mpsc::unbounded_channel();
            wizard.login = Some(LoginLink {
                prompts,
                answers,
                waiting: Some(CodePrompt::NewDevice),
            });
            let has =
                |texts: &[String], key: &str| texts.iter().any(|text| text == catalog.text(key));
            let texts = render_wizard(&context, &catalog, &mut wizard);
            let new_device =
                catalog.format("wizard_new_device_text", &[("email", "me@example.com")]);
            assert!(texts.contains(&new_device), "{locale}: new-device wording");
            assert!(
                !has(&texts, "method_authenticator"),
                "{locale}: no method choice"
            );
            assert!(
                has(&texts, "wizard_stop_login"),
                "{locale}: the login can be stopped"
            );
            assert!(
                !has(&texts, "wizard_terminal_title"),
                "{locale}: no terminal while bw waits"
            );

            wizard.login.as_mut().unwrap().waiting = Some(CodePrompt::TwoStep);
            let texts = render_wizard(&context, &catalog, &mut wizard);
            assert!(
                has(&texts, "wizard_two_step_text"),
                "{locale}: two-step wording"
            );

            // A finished attempt: retry, or the terminal as the last resort.
            wizard.login = None;
            let texts = render_wizard(&context, &catalog, &mut wizard);
            assert!(has(&texts, "wizard_retry_login"), "{locale}: retry");
            assert!(
                has(&texts, "wizard_terminal_title"),
                "{locale}: terminal fall-back"
            );

            wizard.choose_method = true;
            let texts = render_wizard(&context, &catalog, &mut wizard);
            assert!(
                has(&texts, "wizard_method_text"),
                "{locale}: method wording"
            );
            assert!(has(&texts, "method_email"), "{locale}: method choice");
            assert!(
                has(&texts, "wizard_next"),
                "{locale}: continue with the method"
            );
        }
    }

    fn window_texts(app: &mut HostsApp, context: &egui::Context) -> Vec<String> {
        let mut texts = Vec::new();
        for _ in 0..3 {
            let output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1100.0, 760.0),
                    )),
                    ..Default::default()
                },
                |ui| app.render(ui),
            );
            texts = super::super::tests::painted_texts(&output.shapes)
                .into_iter()
                .map(|(text, _, _)| text)
                .collect();
        }
        texts
    }

    #[test]
    fn a_finished_setup_says_so_and_the_toolbar_shows_the_vault() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            let context = egui::Context::default();
            super::super::theme::configure_fonts(&context, locale);
            let mut app = super::super::tests::metadata_test_app(
                &context,
                locale,
                crate::model::HostProfile {
                    alias: "example".into(),
                    address: "server.example.com".into(),
                    port: 22,
                    ..Default::default()
                },
            );
            let catalog = app.catalog.clone();
            app.store.bitwarden.cli_path = Some(std::path::PathBuf::from("C:/bw/bw.exe"));
            app.store.bitwarden.profile = Some(BitwardenProfile {
                email: "me@example.com".into(),
                folder_name: "codex-hosts".into(),
                ..BitwardenProfile::default()
            });
            app.vault_state = Some(VaultState::Unlocked);
            app.setup_done = Some(SetupDone {
                service: HostGroup::Bitwarden,
                account: "me@example.com".into(),
                place: "codex-hosts".into(),
                store_error: None,
            });
            let texts = window_texts(&mut app, &context);
            let has = |key: &str| texts.iter().any(|text| text == catalog.text(key));
            assert!(has("bw_done_title"), "{locale}: completion title");
            assert!(
                texts.contains(&catalog.format(
                    "bw_done_text",
                    &[("account", "me@example.com"), ("place", "codex-hosts")]
                )),
                "{locale}: completion text"
            );
            assert!(
                has("bw_done_add_host") && has("bw_done_close"),
                "{locale}: next steps"
            );
            let chip = format!("●  Bitwarden · {}", catalog.text("bw_status_unlocked"));
            assert!(
                texts.contains(&chip),
                "{locale}: vault state in the toolbar"
            );

            app.setup_done = None;
            for (state, key) in [
                (VaultState::Locked, "bw_status_locked"),
                (VaultState::SignedOut, "bw_status_signed_out"),
            ] {
                app.vault_state = Some(state);
                let texts = window_texts(&mut app, &context);
                let chip = format!("●  Bitwarden · {}", catalog.text(key));
                assert!(
                    texts.contains(&chip),
                    "{locale}: {state:?} vault in the toolbar"
                );
            }
        }
    }

    #[test]
    fn hard_to_undo_account_actions_ask_first() {
        let catalog = Catalog::for_locale(Some("zh-TW"));
        let context = egui::Context::default();
        super::super::theme::configure_fonts(&context, "zh-TW");
        let settings = BitwardenSettings {
            enabled: true,
            cli_path: Some(std::path::PathBuf::from("C:/bw/bw.exe")),
            profile: Some(BitwardenProfile {
                email: "me@example.com".into(),
                ..BitwardenProfile::default()
            }),
            ..BitwardenSettings::default()
        };
        let mut manager = CredentialManager {
            view: ManagerView::Bitwarden,
            tab: ManagerTab::Account,
            status: None,
            master_password: Zeroizing::new(String::new()),
            session: None,
            wizard: None,
            items: Vec::new(),
            problems: Vec::new(),
            item_filter: String::new(),
            fix_form: None,
            idle_minutes: 5,
            sync_max_age: 0,
            job: None,
            message: None,
            confirm: Some(AccountConfirm::RemoveSetup),
        };
        let mut action = None;
        let output = context.run_ui(egui::RawInput::default(), |ui| {
            account_tab(ui, &catalog, &settings, &mut manager, &mut action);
        });
        let texts: Vec<String> = output
            .shapes
            .iter()
            .flat_map(|shape| shape_texts(&shape.shape))
            .collect();
        assert!(
            texts
                .iter()
                .any(|text| text == catalog.text("bw_confirm_remove"))
        );
        assert!(action.is_none(), "nothing happens before the confirmation");
        assert!(
            !texts
                .iter()
                .any(|text| text == catalog.text("bw_reconfigure")),
            "the confirmation replaces the account buttons"
        );
    }

    #[test]
    fn a_policy_chosen_in_settings_is_saved_and_applied_at_once() {
        if !super::super::tests::in_isolated_store(
            "ui::bitwarden::tests::a_policy_chosen_in_settings_is_saved_and_applied_at_once",
        ) {
            return;
        }
        let _guard = crate::credentials::test_lock();
        let context = egui::Context::default();
        let mut app = super::super::tests::metadata_test_app(
            &context,
            "en",
            crate::model::HostProfile::new("local".into()),
        );
        app.store.bitwarden = BitwardenSettings {
            enabled: true,
            cli_path: Some(std::path::PathBuf::from("C:/bw/bw.exe")),
            profile: Some(BitwardenProfile {
                email: "me@example.com".into(),
                policy: ReauthPolicy::Permanent,
                ..BitwardenProfile::default()
            }),
            ..BitwardenSettings::default()
        };
        app.store.save().unwrap();
        let profile = app.store.bitwarden.profile.clone().unwrap();
        session::store(&profile, &SessionKey::new("kept".into())).unwrap();
        app.vault_session = Some(SessionKey::new("kept".into()));
        app.vault_state = Some(VaultState::Unlocked);
        app.credential_manager = Some(CredentialManager {
            view: ManagerView::Bitwarden,
            tab: ManagerTab::Settings,
            status: None,
            master_password: Zeroizing::new(String::new()),
            session: Some(SessionKey::new("kept".into())),
            wizard: None,
            items: Vec::new(),
            problems: Vec::new(),
            item_filter: String::new(),
            fix_form: None,
            idle_minutes: 5,
            sync_max_age: 0,
            job: None,
            message: None,
            confirm: None,
        });

        app.manager_act(ManagerAction::SetPolicy(ReauthPolicy::Idle { minutes: 9 }));
        let saved = crate::storage::HostStore::load()
            .unwrap()
            .bitwarden
            .profile
            .unwrap();
        assert_eq!(saved.policy, ReauthPolicy::Idle { minutes: 9 }, "saved");
        assert!(session::peek(&saved).unwrap(), "idle keeps the key for now");

        app.manager_act(ManagerAction::SetPolicy(ReauthPolicy::PerCall));
        let saved = crate::storage::HostStore::load()
            .unwrap()
            .bitwarden
            .profile
            .unwrap();
        let retired = !session::peek(&saved).unwrap();
        let _ = session::forget(saved.id);
        assert_eq!(saved.policy, ReauthPolicy::PerCall);
        assert!(retired, "every call retires the kept key");
        assert!(app.vault_session.is_none());
        assert_eq!(app.vault_state, Some(VaultState::Locked));
        assert!(
            app.credential_manager
                .as_ref()
                .is_some_and(|manager| manager.session.is_none()),
            "the dialog drops its copy too"
        );
    }
}
