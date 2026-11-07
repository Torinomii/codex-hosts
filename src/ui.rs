use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    mpsc::{self, Receiver},
};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use serde::Serialize;
use uuid::Uuid;
use zeroize::Zeroizing;

mod bitwarden;
mod dialogs;
mod host_form;
mod host_list;
mod onepassword;
mod services;
mod status;
pub(crate) mod theme;
mod toolbar;

pub(crate) use theme::{configure_fonts, forget_password_history};

use crate::vault_hosts::HostWrite;
use bitwarden::VaultTask;
use codex_hosts_bw::FieldEdit;
use status::{StatusKind, StatusMessage};
use theme::apply_style;

use crate::connection;
use crate::credentials::{self, CredentialKind};
use crate::fido::{self, FidoKeyInfo};
use crate::i18n::Catalog;
use crate::import;
use crate::model::{
    AuthPersistence, HostFilter, HostProfile, Prefill, Protocol, SshAuth, VaultHostRef,
    VaultProvider, normalize_tags,
};
use crate::ssh::{
    OperationLimits, RemoteFailure, RemoteResult, TOTAL_TIMEOUT_CODE, VerifiedHostKey,
};
use crate::storage::HostStore;

#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    pub codex_edit: bool,
    pub show_temporary: bool,
    pub prefill: Prefill,
    pub result_path: Option<PathBuf>,
    pub observed_fingerprint: Option<String>,
    pub observed_algorithm: Option<String>,
}

#[derive(Clone)]
struct PendingCallback {
    status: &'static str,
    alias: Option<String>,
}

struct HostEditor {
    sudo: SudoDraft,
    tag_input: String,
    profile: HostProfile,
    original: HostProfile,
    password: Zeroizing<String>,
    key_passphrase: Zeroizing<String>,
    password_mode: PasswordMode,
    saved_password_mode: Option<PasswordMode>,
    password_read_error: Option<String>,
    has_key_passphrase: bool,
    key_passphrase_read_error: Option<String>,
    /// Set by a failed save so empty required fields are highlighted.
    show_required: bool,
    /// Set by a failed save; the next frame moves focus to the first empty required field.
    focus_first_missing: bool,
    /// True until a host created in this session is saved once; the first save
    /// confirms the persistence choice whatever it is.
    first_save: bool,
    /// The persistence value the user confirmed in the change dialog; a
    /// different value in the form asks again.
    confirmed_persistence: Option<AuthPersistence>,
    /// A host that is not in the store yet: the first save adds it (and,
    /// for a vault host, creates the item, whose id the profile then takes).
    draft: bool,
    /// A vault save is running; the form waits for the item write.
    saving: bool,
}

/// The action that was waiting on the persistence-change confirmation.
#[derive(Clone, PartialEq, Eq, Debug)]
enum PendingAction {
    Save,
    Test,
    TestAll,
}

/// The item write that changes only the tags of a vault host, built from
/// the stored mirror so nothing else in the form leaks into it.
fn tags_write(stored: &HostProfile, tags: &[String]) -> HostWrite {
    let scheme = match stored.protocol {
        Protocol::Ssh => codex_hosts_bw::UriScheme::Ssh,
        Protocol::Telnet => codex_hosts_bw::UriScheme::Telnet,
    };
    let uri = codex_hosts_bw::HostUri {
        scheme,
        username: None,
        host: stored.address.trim().to_owned(),
        port: Some(stored.port),
    };
    HostWrite {
        provider: stored.vault_provider().unwrap_or_default(),
        item_id: Some(stored.id),
        external_id: stored.vault().and_then(|vault| vault.external_id.clone()),
        name: stored.alias.clone(),
        host_uri: uri.to_string(),
        username: stored.username.clone(),
        password: FieldEdit::Keep,
        description: stored.description.clone(),
        tags: normalize_tags(tags),
        expected_revision: stored
            .vault
            .as_ref()
            .map(|vault| vault.revision_date.clone())
            .unwrap_or_default(),
    }
}

/// Everything a validated save writes, computed before anything is written
/// so a vault-backed host can write its item first and the store after.
struct SavePlan {
    id: Uuid,
    profile: HostProfile,
    credential_update: Option<(CredentialKind, Zeroizing<String>)>,
    sudo_updates: Vec<credentials::CredentialUpdate>,
    store_password: bool,
    store_key_passphrase: bool,
    clear_login_password: bool,
    invalidate_test_state: bool,
    /// A vault draft: the profile is added to the store instead of replaced.
    is_new: bool,
}

#[derive(Default)]
struct SudoDraft {
    /// UI-only binding for the fixed current-login identity, never an execution target.
    current_user: Option<(String, Uuid)>,
    passwords: HashMap<Uuid, Zeroizing<String>>,
    stored: HashMap<Uuid, Result<bool, String>>,
    cleared: HashSet<Uuid>,
}

impl SudoDraft {
    fn load(profile: &HostProfile) -> Self {
        let mut draft = Self::default();
        if let Some(sudo) = &profile.advanced.sudo {
            for credential in &sudo.credentials {
                draft.stored.insert(
                    credential.id,
                    credentials::has(profile.id, CredentialKind::SudoPassword(credential.id))
                        .map_err(|error| error.to_string()),
                );
            }
        }
        draft
    }
}

struct PersistencePrompt {
    alias: String,
    /// `None` for a host's first save, which has no stored value to change from.
    from: Option<AuthPersistence>,
    to: AuthPersistence,
    then: PendingAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PasswordMode {
    Password,
    NoPassword,
}

impl HostEditor {
    fn load(profile: HostProfile) -> Self {
        let (saved_password_mode, password_read_error) = if profile.is_vault_host() {
            // The item holds the password; the form only offers to replace it.
            (Some(PasswordMode::Password), None)
        } else {
            match credentials::load(profile.id, credentials::login_password_kind(&profile)) {
                Ok(Some(password)) => (
                    Some(if password.is_empty() {
                        PasswordMode::NoPassword
                    } else {
                        PasswordMode::Password
                    }),
                    None,
                ),
                Ok(None) => (None, None),
                Err(error) => (None, Some(error.to_string())),
            }
        };
        let (has_key_passphrase, key_passphrase_read_error) =
            match credentials::has(profile.id, CredentialKind::KeyPassphrase) {
                Ok(has_key_passphrase) => (has_key_passphrase, None),
                Err(error) => (false, Some(error.to_string())),
            };
        Self {
            sudo: SudoDraft::load(&profile),
            original: profile.clone(),
            tag_input: String::new(),
            profile,
            password: Zeroizing::new(String::new()),
            key_passphrase: Zeroizing::new(String::new()),
            password_mode: saved_password_mode.unwrap_or(PasswordMode::Password),
            saved_password_mode,
            password_read_error,
            has_key_passphrase,
            key_passphrase_read_error,
            show_required: false,
            focus_first_missing: false,
            first_save: false,
            confirmed_persistence: None,
            draft: false,
            saving: false,
        }
    }

    /// A host created in this session. The form proposes keeping the session
    /// for SSH hosts; the stored draft (and every profile the store loads or
    /// imports) stays per-call until the user confirms the choice on save.
    fn fresh(profile: HostProfile) -> Self {
        let mut editor = Self::load(profile);
        editor.first_save = true;
        if editor.profile.protocol == Protocol::Ssh {
            editor.profile.auth_persistence = AuthPersistence::Session;
        }
        editor
    }

    /// Authentication persistence is the one relaxation of the per-call
    /// guarantee, so it is confirmed before anything is written: once on a
    /// host's first save, and again whenever a saved host's value changes, in
    /// either direction. Returns the stored value (none on a first save) and
    /// the value to confirm.
    fn persistence_change(&self) -> Option<(Option<AuthPersistence>, AuthPersistence)> {
        if self.profile.protocol != Protocol::Ssh {
            return None;
        }
        let to = self.profile.auth_persistence;
        if self.confirmed_persistence == Some(to) {
            return None;
        }
        if self.first_save {
            return Some((None, to));
        }
        let from = self.original.auth_persistence;
        (from != to).then_some((Some(from), to))
    }

    /// Required fields that are still empty, as form label keys in form order.
    fn missing_required_fields(&self) -> Vec<&'static str> {
        let profile = &self.profile;
        let mut missing = Vec::new();
        if profile.alias.trim().is_empty() {
            missing.push("alias");
        }
        if profile.address.trim().is_empty() || profile.port == 0 {
            missing.push("address");
        }
        if profile.username.trim().is_empty() {
            missing.push("username");
        }
        if profile.protocol == Protocol::Ssh
            && profile.ssh_auth == SshAuth::PrivateKey
            && profile.private_key_path.trim().is_empty()
        {
            missing.push("private_key");
        }
        missing
    }

    fn first_missing_field(&self) -> Option<&'static str> {
        self.missing_required_fields().into_iter().next()
    }

    fn connection_changed(&self) -> bool {
        !self.profile.connection_details_equal(&self.original)
    }

    fn has_unsaved_changes(&self) -> bool {
        self.profile != self.original
            || !self.tag_input.trim().is_empty()
            || !self.password.is_empty()
            || !self.key_passphrase.is_empty()
            || self
                .sudo
                .passwords
                .values()
                .any(|password| !password.is_empty())
            || !self.sudo.cleared.is_empty()
            || self.password_mode != self.saved_password_mode.unwrap_or(PasswordMode::Password)
            || self.draft
    }

    fn matches_stored_original(&self, store: &HostStore) -> bool {
        store.hosts.iter().any(|host| host == &self.original)
    }

    fn needs_password(&self) -> bool {
        self.profile.protocol == Protocol::Telnet || self.profile.ssh_auth == SshAuth::Password
    }

    /// The host is defined by a password-manager item: the password and
    /// the connection facts go to the item, never to Credential Manager.
    fn is_vault_host(&self) -> bool {
        self.profile.is_vault_host()
    }

    /// What the form says about the password manager behind the host.
    fn vault_texts(&self) -> Option<&'static services::VaultTexts> {
        services::HostGroup::of(&self.profile).texts()
    }

    /// What the vault item should hold after this save.
    fn vault_write(&self) -> HostWrite {
        let profile = &self.profile;
        let scheme = match profile.protocol {
            Protocol::Ssh => codex_hosts_bw::UriScheme::Ssh,
            Protocol::Telnet => codex_hosts_bw::UriScheme::Telnet,
        };
        let uri = codex_hosts_bw::HostUri {
            scheme,
            username: None,
            host: profile.address.trim().to_owned(),
            port: Some(profile.port),
        };
        // Key and Agent logins leave the item password alone.
        let password = if !self.needs_password() {
            FieldEdit::Keep
        } else {
            match self.password_mode {
                PasswordMode::Password if !self.password.is_empty() => {
                    FieldEdit::Set(self.password.clone())
                }
                PasswordMode::Password => FieldEdit::Keep,
                PasswordMode::NoPassword => FieldEdit::Clear,
            }
        };
        HostWrite {
            provider: profile.vault_provider().unwrap_or_default(),
            item_id: (!self.draft).then_some(profile.id),
            external_id: (!self.draft)
                .then(|| profile.vault().and_then(|vault| vault.external_id.clone()))
                .flatten(),
            name: profile.alias.trim().to_owned(),
            host_uri: uri.to_string(),
            username: profile.username.trim().to_owned(),
            password,
            description: profile.description.trim().to_owned(),
            tags: normalize_tags(profile.tags.iter().chain(std::iter::once(&self.tag_input))),
            expected_revision: (!self.draft)
                .then(|| {
                    profile
                        .vault
                        .as_ref()
                        .map(|vault| vault.revision_date.clone())
                })
                .flatten()
                .unwrap_or_default(),
        }
    }

    /// The write to retry after an unlock: the current form when it still
    /// edits the same item, else the kept write.
    pub(super) fn vault_write_or(&self, fallback: HostWrite) -> HostWrite {
        if self.is_vault_host() && (self.draft == fallback.item_id.is_none()) {
            self.vault_write()
        } else {
            fallback
        }
    }

    fn login_user_changed(&self) -> bool {
        self.profile.username.trim() != self.original.username.trim()
    }

    /// True while leaving the password field empty keeps a password already saved for this login.
    fn login_password_saved(&self) -> bool {
        self.password_mode == PasswordMode::Password
            && self.saved_password_mode == Some(PasswordMode::Password)
            && !self.login_user_changed()
    }

    fn should_clear_login_password(&self) -> bool {
        // A key/Agent login must not retain the previous account's password
        // for a later password login or sudo fallback under the new username.
        !self.is_vault_host() && self.login_user_changed() && !self.needs_password()
    }

    fn should_store_password(&self) -> bool {
        if !self.needs_password() || self.is_vault_host() {
            return false;
        }
        match self.password_mode {
            PasswordMode::Password => !self.password.is_empty(),
            PasswordMode::NoPassword => {
                self.saved_password_mode != Some(PasswordMode::NoPassword)
                    || self.login_user_changed()
            }
        }
    }

    fn should_store_key_passphrase(&self) -> bool {
        self.profile.protocol == Protocol::Ssh
            && self.profile.ssh_auth == SshAuth::PrivateKey
            && !self.key_passphrase.is_empty()
    }

    fn password_value_missing(&self) -> bool {
        if self.is_vault_host() {
            // An existing item keeps its password; a new one needs one.
            return self.draft
                && self.password_mode == PasswordMode::Password
                && self.password.is_empty();
        }
        self.needs_password()
            && self.password_mode == PasswordMode::Password
            && self.password.is_empty()
            && (self.saved_password_mode != Some(PasswordMode::Password)
                || self.login_user_changed())
    }

    fn test_result_is_stale(&self) -> bool {
        self.connection_changed()
            || self.should_store_password()
            || self.should_store_key_passphrase()
            || self.profile.vault != self.original.vault
            || (self.is_vault_host() && !self.password.is_empty())
    }

    fn sudo_updates(&self) -> Result<Vec<credentials::CredentialUpdate>, &'static str> {
        let mut updates = Vec::new();
        let current = self
            .profile
            .advanced
            .sudo
            .as_ref()
            .map(|sudo| sudo.credentials.as_slice())
            .unwrap_or(&[]);
        let original = self
            .original
            .advanced
            .sudo
            .as_ref()
            .map(|sudo| sudo.credentials.as_slice())
            .unwrap_or(&[]);
        for credential in current {
            let password = self
                .sudo
                .passwords
                .get(&credential.id)
                .filter(|password| !password.is_empty());
            if let Some(password) = password {
                if crate::sudo::validate_password(password).is_err() {
                    return Err("sudo_invalid_password");
                }
                updates.push((
                    CredentialKind::SudoPassword(credential.id),
                    Some(password.clone()),
                ));
            } else if self.sudo.cleared.contains(&credential.id) {
                updates.push((CredentialKind::SudoPassword(credential.id), None));
            }
        }
        for credential in original {
            if !current.iter().any(|new| new.id == credential.id) {
                updates.push((CredentialKind::SudoPassword(credential.id), None));
            }
        }
        Ok(updates)
    }

    fn sudo_changes(
        &self,
    ) -> Result<
        (
            Option<crate::model::SudoSettings>,
            Vec<credentials::CredentialUpdate>,
        ),
        &'static str,
    > {
        let mut updates = self.sudo_updates()?;
        let mut settings = self.profile.advanced.sudo.clone();
        if let Some(settings) = &mut settings {
            for credential in &mut settings.credentials {
                if self.original.advanced.sudo.as_ref().is_some_and(|old| {
                    old.credentials.iter().any(|old| {
                        old.id == credential.id && old.username.trim() != credential.username.trim()
                    })
                }) {
                    // A call using an older profile snapshot must never read a
                    // newly renamed user's password from the previous account's slot.
                    let old = CredentialKind::SudoPassword(credential.id);
                    credential.id = Uuid::new_v4();
                    if let Some((kind, _)) = updates.iter_mut().find(|(kind, _)| *kind == old) {
                        *kind = CredentialKind::SudoPassword(credential.id);
                    }
                    updates.push((old, None));
                }
            }
        }
        Ok((settings, updates))
    }
}

const GUI_TEST_TIMEOUT: Duration = Duration::from_secs(10);
const GUI_INTERACTIVE_TEST_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_CONCURRENT_TESTS: usize = 8;

struct TestOperation {
    receiver: Receiver<(Instant, Result<RemoteResult, RemoteFailure>)>,
    started_at: Instant,
    timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostTestState {
    Testing,
    Succeeded,
    Failed,
}

#[derive(Clone)]
struct FingerprintPrompt {
    host_id: Uuid,
    alias: String,
    expected: Option<String>,
    observed: String,
    expected_algorithm: Option<String>,
    observed_algorithm: Option<String>,
    retry_test: bool,
    close_after_choice: bool,
}

struct ImportCleanupPrompt {
    path: PathBuf,
    imported_count: usize,
}

enum EditorAction {
    Save,
    SaveTags,
    Test,
    CancelCodex,
    BrowsePrivateKey,
    DiscoverFido,
    OpenFidoSetup,
    /// Closes a new host that was never saved.
    DiscardDraft,
    /// Asks to delete the open host.
    Delete,
    /// Drops the unsaved changes to a saved host.
    Revert,
}

/// Where the user asked the form to go next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorSwitch {
    Select(Uuid),
    /// Select the host, then ask to delete it.
    Delete(Uuid),
    /// Select the host, then test it.
    Test(Uuid),
    New(host_list::HostGroup),
}

struct FidoSetupPrompt {
    pin: Zeroizing<String>,
    operation: Option<Receiver<Result<Vec<FidoKeyInfo>, String>>>,
    status: Option<String>,
    identity: Option<FidoKeyInfo>,
}

#[derive(Clone, Copy)]
enum FidoSetupAction {
    CreateRecommended,
    CreateCompatible,
    RecoverResident,
}

pub struct HostsApp {
    temporary: Option<crate::temporary_secrets::SecretsApp>,
    tray: Option<crate::tray::Tray>,
    exiting: bool,
    hidden: bool,
    tray_available: bool,
    host_refresh_pending: bool,
    store: HostStore,
    catalog: Catalog,
    selected: Option<Uuid>,
    editor: Option<HostEditor>,
    status: StatusMessage,
    test_operations: HashMap<Uuid, TestOperation>,
    pending_tests: VecDeque<Uuid>,
    test_hosts_snapshot: Option<Arc<Vec<HostProfile>>>,
    test_states: HashMap<Uuid, HostTestState>,
    testing_all: bool,
    test_store_dirty: bool,
    repaint_context: egui::Context,
    fingerprint_prompt: Option<FingerprintPrompt>,
    delete_prompt: bool,
    persistence_prompt: Option<PersistencePrompt>,
    import_window_open: bool,
    import_cleanup_prompt: Option<ImportCleanupPrompt>,
    fido_setup_prompt: Option<FidoSetupPrompt>,
    batch_mode: bool,
    batch_selected: HashSet<Uuid>,
    host_filter: HostFilter,
    batch_delete_prompt: bool,
    batch_export_window_open: bool,
    sidebar_width: Option<f32>,
    /// Scroll the host list so the selected row is visible on the next frame.
    scroll_to_selected: bool,
    launch: LaunchOptions,
    callback_written: bool,
    pending_callback: Option<PendingCallback>,
    /// The credential-services dialog, while open.
    credential_services: Option<bitwarden::CredentialServices>,
    /// The master-password prompt a tool call or a window task is waiting on.
    unlock_prompt: Option<bitwarden::UnlockPrompt>,
    /// The unlock an SSH call approval asked for along with the call.
    approval_unlock: Option<bitwarden::ApprovalUnlock>,
    /// The vault job the window runs for its hosts (refresh, save, delete).
    vault_job: Option<bitwarden::VaultJob>,
    /// The 1Password card and its refresh job.
    onepassword: onepassword::OnePasswordUi,
    /// The store side of a vault save, written once the item write lands.
    pending_vault_save: Option<SavePlan>,
    /// The session the window unlocked, reused by its own vault jobs.
    vault_session: Option<codex_hosts_bw::SessionKey>,
    /// How the vault was last seen (`None` until known), for the toolbar's
    /// status.
    vault_state: Option<bitwarden::VaultState>,
    /// The confirmation shown once the Bitwarden setup finished.
    setup_done: Option<bitwarden::SetupDone>,
    /// Where the form was asked to go while it held unsaved changes; the
    /// unsaved-changes dialog asks before it goes.
    pending_switch: Option<EditorSwitch>,
    /// When the window last checked the idle policy.
    vault_policy_checked: Option<Instant>,
    /// Why the Bitwarden section is empty, once known.
    vault_note: Option<String>,
    /// Folder items that are not served as hosts, from the last refresh.
    vault_problems: Vec<(String, crate::vault_hosts::ItemProblem)>,
    /// Host-list groups the user folded away.
    collapsed_groups: host_list::GroupFolds,
}

impl HostsApp {
    pub fn new(
        context: &eframe::CreationContext<'_>,
        launch: LaunchOptions,
    ) -> std::io::Result<Self> {
        context.egui_ctx.set_zoom_factor(1.06);
        apply_style(&context.egui_ctx);

        let (mut store, mut startup_error) = match HostStore::load_recovering() {
            Ok(store) => (store, None),
            Err(error) => {
                let error = error.to_string();
                (HostStore::blocked(error.clone()), Some(error))
            }
        };
        let catalog = Catalog::for_locale(store.preferred_locale.as_deref());
        configure_fonts(&context.egui_ctx, catalog.locale());
        let mut selected = store.hosts.first().map(|host| host.id);
        let mut created_now = None;

        if launch.codex_edit && startup_error.is_none() {
            let alias = launch
                .prefill
                .alias
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| store.next_neutral_alias());
            if let Some(existing) = store.find_alias(&alias) {
                selected = Some(existing.id);
            } else if launch.observed_fingerprint.is_some() {
                selected = None;
                startup_error = Some(format!("HOST_NOT_FOUND: {alias}"));
            } else {
                let mut draft = HostProfile::new(alias);
                draft.apply_prefill(&launch.prefill);
                if let Some(jump_alias) = launch.prefill.jump_alias.as_deref() {
                    draft.jump_host = store.find_alias(jump_alias).map(|host| host.id);
                }
                let id = draft.id;
                store.hosts.push(draft);
                match store.save() {
                    Ok(()) => {
                        selected = Some(id);
                        created_now = Some(id);
                    }
                    Err(error) => {
                        store.hosts.retain(|host| host.id != id);
                        selected = None;
                        startup_error = Some(error.to_string());
                    }
                }
            }
        }

        let mut editor = selected
            .and_then(|id| store.hosts.iter().find(|host| host.id == id))
            .cloned()
            .map(|profile| {
                if created_now == Some(profile.id) {
                    HostEditor::fresh(profile)
                } else {
                    HostEditor::load(profile)
                }
            });
        if launch.codex_edit
            && launch.observed_fingerprint.is_none()
            && let Some(editor) = &mut editor
        {
            editor.profile.apply_prefill(&launch.prefill);
            if let Some(jump_alias) = launch.prefill.jump_alias.as_deref() {
                editor.profile.jump_host = store.find_alias(jump_alias).map(|host| host.id);
            }
        }

        let fingerprint_prompt = launch.observed_fingerprint.as_ref().and_then(|observed| {
            editor.as_ref().map(|editor| FingerprintPrompt {
                host_id: editor.profile.id,
                alias: editor.profile.alias.clone(),
                expected: editor.profile.host_fingerprint.clone(),
                observed: observed.clone(),
                expected_algorithm: editor.profile.host_key_algorithm.clone(),
                observed_algorithm: launch.observed_algorithm.clone(),
                retry_test: false,
                close_after_choice: true,
            })
        });
        let status = if let Some(error) = startup_error {
            StatusMessage::new(
                StatusKind::Error,
                catalog.format("storage_error", &[("error", &error)]),
            )
        } else if let crate::storage::OwnerCheck::Mismatch { owner, .. } =
            crate::storage::owner_check()
        {
            StatusMessage::new(
                StatusKind::Warning,
                catalog.format(
                    "store_owner_warning",
                    &[
                        ("owner", &owner),
                        ("command", &crate::storage::owner_repair_hint()),
                    ],
                ),
            )
        } else if let crate::storage::OwnerCheck::Unavailable(error) = crate::storage::owner_check()
        {
            StatusMessage::new(
                StatusKind::Warning,
                catalog.format("store_owner_unavailable", &[("error", &error)]),
            )
        } else if launch.codex_edit {
            StatusMessage::info(catalog.text("draft_waiting"))
        } else {
            StatusMessage::info(catalog.text("status_ready"))
        };
        let mut temporary = if launch.codex_edit {
            None
        } else {
            Some(crate::temporary_secrets::SecretsApp::new(
                context.egui_ctx.clone(),
            )?)
        };
        if let Some(temporary) = &mut temporary {
            temporary.visible = launch.show_temporary;
            temporary.set_approval_default(store.temporary_call_approval);
        }
        let tray = if launch.codex_edit {
            None
        } else {
            crate::tray::Tray::new(context.egui_ctx.clone(), catalog.clone()).ok()
        };
        let tray_available = tray.is_some();
        let mut app = Self {
            temporary,
            tray,
            exiting: false,
            hidden: false,
            tray_available,
            host_refresh_pending: false,
            store,
            catalog,
            selected,
            editor,
            status,
            test_operations: HashMap::new(),
            pending_tests: VecDeque::new(),
            test_hosts_snapshot: None,
            test_states: HashMap::new(),
            testing_all: false,
            test_store_dirty: false,
            repaint_context: context.egui_ctx.clone(),
            fingerprint_prompt,
            delete_prompt: false,
            persistence_prompt: None,
            import_window_open: false,
            import_cleanup_prompt: None,
            fido_setup_prompt: None,
            batch_mode: false,
            batch_selected: HashSet::new(),
            host_filter: HostFilter::default(),
            batch_delete_prompt: false,
            batch_export_window_open: false,
            sidebar_width: None,
            scroll_to_selected: true,
            launch,
            callback_written: false,
            pending_callback: None,
            credential_services: None,
            unlock_prompt: None,
            approval_unlock: None,
            vault_job: None,
            onepassword: onepassword::OnePasswordUi::default(),
            pending_vault_save: None,
            vault_session: None,
            vault_state: None,
            setup_done: None,
            pending_switch: None,
            vault_policy_checked: None,
            vault_note: None,
            vault_problems: Vec::new(),
            collapsed_groups: host_list::GroupFolds::default(),
        };
        app.clear_stale_window_sessions();
        app.refresh_vault_hosts_quietly();
        Ok(app)
    }

    /// Hosts the list can show: vault hosts are hidden while their password
    /// manager is switched off.
    fn visible_host_count(&self) -> usize {
        self.store
            .hosts
            .iter()
            .filter(|host| self.store.serves(host))
            .count()
    }

    /// Moves the form where the user asked, asking first when that would
    /// drop unsaved changes to the open host.
    fn switch_editor(&mut self, next: EditorSwitch) {
        let leaves = match next {
            EditorSwitch::Select(id) | EditorSwitch::Test(id) => self.selected != Some(id),
            // Deleting the open host drops its edits anyway.
            EditorSwitch::Delete(id) => self.selected != Some(id),
            EditorSwitch::New(_) => true,
        };
        if leaves
            && self
                .editor
                .as_ref()
                .is_some_and(HostEditor::has_unsaved_changes)
        {
            self.pending_switch = Some(next);
            return;
        }
        self.apply_switch(next);
    }

    fn apply_switch(&mut self, next: EditorSwitch) {
        match next {
            // The open host keeps its form; reverting is its own button.
            EditorSwitch::Select(id) => {
                if self.selected != Some(id) {
                    self.select(id);
                }
            }
            EditorSwitch::Delete(id) => {
                if self.selected != Some(id) {
                    self.select(id);
                }
                self.delete_prompt = true;
            }
            EditorSwitch::Test(id) => {
                // Right-clicking the open host keeps its form; test it like
                // the editor's own button.
                if self.selected != Some(id) {
                    self.select(id);
                }
                self.start_test();
            }
            EditorSwitch::New(group) => self.new_service_host(group),
        }
    }

    fn select(&mut self, id: Uuid) {
        self.selected = Some(id);
        self.editor = self
            .store
            .hosts
            .iter()
            .find(|host| host.id == id)
            .cloned()
            .map(HostEditor::load);
        self.set_status_key("status_ready");
    }

    fn refresh_hosts(&mut self) -> Result<(), String> {
        let fresh = HostStore::load().map_err(|error| error.to_string())?;
        if self.store.same_revision(&fresh) {
            return Ok(());
        }
        let kept_edits = self
            .editor
            .as_ref()
            .is_some_and(HostEditor::has_unsaved_changes);
        reconcile_host_editor(&mut self.editor, &mut self.selected, &fresh);
        self.store = fresh;
        self.batch_selected
            .retain(|id| self.store.hosts.iter().any(|host| host.id == *id));
        self.test_states.clear();
        self.fingerprint_prompt = None;
        self.delete_prompt = false;
        self.persistence_prompt = None;
        self.batch_delete_prompt = false;
        // Only worth saying when the form kept edits over the new data.
        if kept_edits {
            self.set_status_key("hosts_refreshed");
        }
        Ok(())
    }

    /// Opens the form on a new local host; nothing is stored until Save.
    fn new_host(&mut self) {
        let mut editor = HostEditor::fresh(HostProfile::new(String::new()));
        editor.draft = true;
        self.selected = None;
        self.editor = Some(editor);
    }

    /// Opens the form on a new host of a password manager's folder or vault;
    /// the first save creates the item.
    pub(super) fn new_vault_host(&mut self, provider: VaultProvider) {
        if !self.service_ready(services::HostGroup::of_provider(Some(provider))) {
            self.set_status_key(match provider {
                VaultProvider::Bitwarden => "bw_not_configured",
                VaultProvider::OnePassword => "op_not_configured",
            });
            return;
        }
        let mut profile = HostProfile::new(String::new());
        profile.ssh_auth = SshAuth::Password;
        profile.vault = Some(VaultHostRef {
            item_id: profile.id,
            item_name: String::new(),
            revision_date: String::new(),
            provider,
            external_id: None,
        });
        let mut editor = HostEditor::fresh(profile);
        editor.draft = true;
        editor.saved_password_mode = None;
        self.selected = None;
        self.editor = Some(editor);
    }

    /// Reloads the open saved host from the store, dropping its edits.
    fn revert_editor(&mut self) {
        if let Some(id) = self.selected
            && self.editor.as_ref().is_some_and(|editor| !editor.draft)
        {
            self.select(id);
        }
    }

    /// Closes an unsaved new host; the store never saw it.
    fn discard_draft(&mut self) {
        if self.editor.as_ref().is_some_and(|editor| editor.draft) {
            self.editor = None;
            self.selected = None;
            self.set_status_key("status_ready");
        }
    }

    /// Validates the form and works out what a save writes, without writing.
    fn plan_editor_save(&mut self) -> Result<SavePlan, String> {
        self.refresh_hosts()?;
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| !editor.draft && !editor.matches_stored_original(&self.store))
        {
            return Err(self.catalog.text("host_changed").to_owned());
        }
        let saving = Self::vault_key(self.editor_group(), |texts| texts.saving);
        let editor = self.editor.as_mut().ok_or_else(|| "NO_EDITOR".to_owned())?;
        if editor.saving {
            return Err(self.catalog.text(saving).to_owned());
        }
        let missing = editor.missing_required_fields();
        if !missing.is_empty() {
            editor.show_required = true;
            editor.focus_first_missing = true;
            return Err(self.catalog.format(
                "validation_required_count",
                &[("count", &missing.len().to_string())],
            ));
        }
        editor.show_required = false;
        let editor = self.editor.as_ref().ok_or_else(|| "NO_EDITOR".to_owned())?;
        if let Some(issue) = editor.profile.validation_issue() {
            return Err(self.catalog.text(issue.translation_key()).to_owned());
        }
        if self.store.hosts.iter().any(|host| {
            host.id != editor.profile.id
                && host.alias.eq_ignore_ascii_case(editor.profile.alias.trim())
        }) {
            return Err(self.catalog.text("validation_alias").to_owned());
        }

        if editor.password_value_missing() {
            if let Some(error) = editor.password_read_error.as_deref() {
                return Err(self.catalog.format("credential_error", &[("error", error)]));
            }
            return Err(self.catalog.text("password_required").to_owned());
        }

        let invalidate_test_state = editor.test_result_is_stale();
        let store_password = editor.should_store_password();
        let store_key_passphrase = editor.should_store_key_passphrase();
        let clear_login_password = editor.should_clear_login_password();
        let mut profile = editor.profile.clone();
        let old_password_kind = credentials::login_password_kind(&editor.original);
        // Never reuse a host-wide slot for a new login identity, even when
        // switching to key/Agent auth without saving a password yet.
        if !editor.is_vault_host()
            && (editor.login_user_changed()
                || (store_password && profile.login_password_id.is_none()))
        {
            profile.login_password_id = Some(Uuid::new_v4());
        }
        let password_kind = credentials::login_password_kind(&profile);
        let credential_update = if store_password {
            Some((
                password_kind,
                match editor.password_mode {
                    PasswordMode::Password => editor.password.clone(),
                    PasswordMode::NoPassword => Zeroizing::new(String::new()),
                },
            ))
        } else if store_key_passphrase {
            Some((CredentialKind::KeyPassphrase, editor.key_passphrase.clone()))
        } else {
            None
        };
        let (sudo_settings, mut sudo_updates) = editor
            .sudo_changes()
            .map_err(|key| self.catalog.text(key).to_owned())?;
        if clear_login_password || (!editor.is_vault_host() && password_kind != old_password_kind) {
            sudo_updates.push((old_password_kind, None));
        }
        profile.advanced.sudo = sudo_settings;
        profile.tags = normalize_tags(
            profile
                .tags
                .iter()
                .chain(std::iter::once(&editor.tag_input)),
        );
        profile.alias = profile.alias.trim().to_owned();
        profile.address = profile.address.trim().to_owned();
        profile.username = profile.username.trim().to_owned();
        profile.private_key_path = profile.private_key_path.trim().to_owned();
        profile.agent_key_fingerprint = profile.agent_key_fingerprint.trim().to_owned();
        if let Some(sudo) = &mut profile.advanced.sudo {
            for credential in &mut sudo.credentials {
                credential.username = credential.username.trim().to_owned();
            }
            if profile.protocol == Protocol::Telnet {
                sudo.enabled = false;
            }
            if *sudo == crate::model::SudoSettings::default() {
                profile.advanced.sudo = None;
            }
        }
        if let Some(su) = &mut profile.advanced.su {
            if profile.protocol == Protocol::Telnet {
                su.enabled = false;
            }
            if *su == crate::model::SuSettings::default() {
                profile.advanced.su = None;
            }
        }
        if invalidate_test_state {
            profile.verified = false;
        }
        if profile.protocol == Protocol::Telnet {
            // Telnet never keeps sessions; store the strict value so a later
            // switch to SSH starts from per-call rather than a hidden default.
            profile.auth_persistence = AuthPersistence::PerCall;
            profile.jump_host = None;
            profile.host_fingerprint = None;
            profile.host_key_algorithm = None;
            profile.host_key_first_seen_unix = None;
            profile.host_key_last_verified_unix = None;
        }
        Ok(SavePlan {
            id: editor.profile.id,
            profile,
            credential_update,
            sudo_updates,
            store_password,
            store_key_passphrase,
            clear_login_password,
            invalidate_test_state,
            is_new: editor.draft,
        })
    }

    /// Saves the form. A vault-backed host is written to its Bitwarden item
    /// first, on a worker; `Ok(false)` means that write is still running and
    /// the store is updated when it finishes.
    fn persist_editor(&mut self) -> Result<bool, String> {
        let plan = self.plan_editor_save()?;
        let vault_host = self.editor.as_ref().is_some_and(HostEditor::is_vault_host);
        if vault_host {
            if self.vault_job_running() {
                return Err(self.catalog.text("busy").to_owned());
            }
            let editor = self.editor.as_mut().ok_or_else(|| "NO_EDITOR".to_owned())?;
            editor.saving = true;
            let write = editor.vault_write();
            self.pending_vault_save = Some(plan);
            self.start_vault_task(VaultTask::Save(write));
            return Ok(false);
        }
        self.commit_editor_save(plan)?;
        Ok(true)
    }

    /// Writes the planned save into the store and Credential Manager.
    fn commit_editor_save(&mut self, plan: SavePlan) -> Result<(), String> {
        let SavePlan {
            id,
            profile,
            credential_update,
            mut sudo_updates,
            store_password,
            store_key_passphrase,
            clear_login_password,
            invalidate_test_state,
            is_new,
        } = plan;
        let mut updated_store = self.store.clone();
        if is_new {
            updated_store.hosts.push(profile.clone());
        } else {
            let stored = updated_store
                .hosts
                .iter_mut()
                .find(|host| host.id == id)
                .ok_or_else(|| "HOST_NOT_FOUND".to_owned())?;
            stored.clone_from(&profile);
        }
        let id = profile.id;

        if !sudo_updates.is_empty() {
            if let Some((kind, secret)) = &credential_update {
                sudo_updates.push((*kind, Some(secret.clone())));
            }
            credentials::save_transaction(id, &sudo_updates, || {
                updated_store.save().map_err(|error| error.to_string())
            })
            .map_err(|error| {
                self.catalog
                    .format("credential_error", &[("error", &error)])
            })?;
        } else if let Some((kind, secret)) = credential_update.as_ref() {
            match credentials::snapshot_kind(id, *kind) {
                Ok(snapshot) => {
                    if let Err(error) = credentials::store(id, *kind, secret.as_str()) {
                        return Err(self
                            .catalog
                            .format("credential_error", &[("error", &error.to_string())]));
                    }
                    if let Err(error) = updated_store.save() {
                        let rollback = credentials::restore_kind(id, *kind, snapshot.as_ref())
                            .err()
                            .map(|error| error.to_string());
                        let primary = self
                            .catalog
                            .format("storage_error", &[("error", &error.to_string())]);
                        return Err(with_rollback_error(primary, rollback));
                    }
                }
                Err(_) => {
                    if let Err(error) = updated_store.save() {
                        return Err(self
                            .catalog
                            .format("storage_error", &[("error", &error.to_string())]));
                    }
                    if let Err(error) = credentials::store(id, *kind, secret.as_str()) {
                        let rollback = self
                            .store
                            .save_recovery_baseline_after(&updated_store)
                            .err()
                            .map(|error| format!("host metadata: {error}"));
                        let primary = self
                            .catalog
                            .format("credential_error", &[("error", &error.to_string())]);
                        return Err(with_rollback_error(primary, rollback));
                    }
                }
            }
        } else if let Err(error) = updated_store.save() {
            return Err(self
                .catalog
                .format("storage_error", &[("error", &error.to_string())]));
        }

        self.store = updated_store;
        if invalidate_test_state {
            self.test_states.remove(&id);
            crate::ssh::invalidate_profile(id);
        }
        let editor = self.editor.as_mut().ok_or_else(|| "NO_EDITOR".to_owned())?;
        editor.profile = profile.clone();
        editor.original = profile;
        editor.sudo = SudoDraft::load(&editor.profile);
        editor.first_save = false;
        editor.confirmed_persistence = None;
        editor.tag_input.clear();
        editor.draft = false;
        editor.saving = false;
        if store_password {
            editor.saved_password_mode = Some(editor.password_mode);
            editor.password_read_error = None;
            editor.password.clear();
        }
        if clear_login_password {
            editor.saved_password_mode = None;
            editor.password_read_error = None;
            editor.password.clear();
        }
        if editor.is_vault_host() {
            editor.saved_password_mode = Some(editor.password_mode);
            editor.password.clear();
        }
        if store_key_passphrase {
            editor.has_key_passphrase = true;
            editor.key_passphrase_read_error = None;
            editor.key_passphrase.clear();
        }
        if is_new {
            self.selected = Some(id);
            self.scroll_to_selected = true;
        }
        Ok(())
    }

    /// The item write finished: the store takes the host as the item now
    /// describes it, plus the local settings from the form.
    pub(super) fn finish_vault_save(
        &mut self,
        key: &crate::vault_hosts::ItemKey,
        facts: &crate::vault_hosts::HostFacts,
    ) {
        let item_id = key.id;
        let Some(mut plan) = self.pending_vault_save.take() else {
            // No form save pending: a tag write. The stored mirror takes
            // what the item says now.
            if let Some(stored) = self.store.hosts.iter_mut().find(|host| host.id == item_id) {
                crate::vault_hosts::apply(facts, key, stored);

                let stored = stored.clone();
                match self.store.save() {
                    Ok(()) => {
                        if let Some(editor) = self
                            .editor
                            .as_mut()
                            .filter(|editor| editor.profile.id == item_id)
                        {
                            editor.original.tags = stored.tags.clone();
                            editor.original.vault = stored.vault.clone();
                            editor.profile.vault = stored.vault.clone();
                        }
                        self.set_status_key("status_tags_saved");
                    }
                    Err(error) => {
                        self.set_status_format("storage_error", &[("error", &error.to_string())])
                    }
                }
            }
            return;
        };
        if plan.is_new {
            plan.profile.id = item_id;
            plan.profile.login_password_id = None;
        }
        crate::vault_hosts::apply(facts, key, &mut plan.profile);
        let saved_alias = plan.profile.alias.clone();
        let group = services::HostGroup::of_provider(Some(key.provider));
        match self.commit_editor_save(plan) {
            Ok(()) => {
                self.set_vault_status(group, |texts| texts.saved);
                if self.launch.codex_edit {
                    match self.write_callback("saved", Some(&saved_alias)) {
                        Ok(()) => self
                            .repaint_context
                            .send_viewport_cmd(egui::ViewportCommand::Close),
                        Err(error) => {
                            self.set_status_format("callback_error", &[("error", &error)])
                        }
                    }
                }
            }
            Err(error) => self.editor_save_failed(error),
        }
    }

    /// A vault save did not happen; the form stays a draft.
    pub(super) fn editor_save_failed(&mut self, error: String) {
        self.pending_vault_save = None;
        if let Some(editor) = self.editor.as_mut() {
            editor.saving = false;
        }
        self.set_status(StatusKind::Error, error);
    }

    /// The host's item is in the provider's trash or archive: its mirror and
    /// local secrets go too.
    pub(super) fn finish_vault_delete(&mut self, item_id: Uuid) {
        let mut group = services::HostGroup::Bitwarden;
        if let Some(profile) = self
            .store
            .hosts
            .iter()
            .find(|host| host.id == item_id)
            .cloned()
        {
            group = services::HostGroup::of(&profile);
            let _ = credentials::delete_host(&profile);
        }
        self.store.hosts.retain(|host| host.id != item_id);
        if let Err(error) = self.store.save() {
            self.set_status_format("storage_error", &[("error", &error.to_string())]);
            return;
        }
        if self.selected == Some(item_id) {
            self.selected = None;
            self.editor = None;
        }
        self.test_states.remove(&item_id);
        crate::ssh::invalidate_profile(item_id);
        self.set_vault_status(group, |texts| texts.deleted);
    }

    /// Writes only the tag list; every other field in the form stays a draft until Save.
    fn persist_tags(&mut self) -> Result<(), String> {
        self.refresh_hosts()?;
        let editor = self.editor.as_ref().ok_or_else(|| "NO_EDITOR".to_owned())?;
        if !editor.matches_stored_original(&self.store) {
            return Err(self.catalog.text("host_changed").to_owned());
        }
        let id = editor.profile.id;
        let tags = editor.profile.tags.clone();
        let mut updated_store = self.store.clone();
        updated_store
            .hosts
            .iter_mut()
            .find(|host| host.id == id)
            .ok_or_else(|| "HOST_NOT_FOUND".to_owned())?
            .tags
            .clone_from(&tags);
        updated_store.save().map_err(|error| {
            self.catalog
                .format("storage_error", &[("error", &error.to_string())])
        })?;
        self.store = updated_store;
        if let Some(editor) = &mut self.editor {
            editor.original.tags = tags;
        }
        Ok(())
    }

    fn save_tags(&mut self) {
        // A Codex draft and a new host are written only by their Save, so
        // Cancel or discarding leaves the store untouched.
        if self.launch.codex_edit || self.editor.as_ref().is_some_and(|editor| editor.draft) {
            return;
        }

        // Tags of a vault host live in its item: the write goes there first
        // and the store follows when it lands.
        if let Some(write) = self
            .editor
            .as_ref()
            .filter(|editor| editor.is_vault_host())
            .and_then(|editor| {
                self.store
                    .hosts
                    .iter()
                    .find(|host| host.id == editor.profile.id)
                    .map(|stored| tags_write(stored, &editor.profile.tags))
            })
        {
            if self.vault_job_running() {
                self.set_status_key("busy");
                return;
            }
            self.start_vault_task(VaultTask::Save(write));
            self.set_vault_status(self.editor_group(), |texts| texts.saving);
            return;
        }
        match self.persist_tags() {
            Ok(()) => self.set_status_key("status_tags_saved"),
            Err(error) => self.set_status(StatusKind::Error, error),
        }
    }

    /// Opens the confirmation for a changed persistence value and remembers
    /// what to run once the user has answered; false means nothing to confirm.
    fn ask_persistence_confirmation(&mut self, then: PendingAction) -> bool {
        let Some(editor) = self.editor.as_ref() else {
            return false;
        };
        let Some((from, to)) = editor.persistence_change() else {
            return false;
        };
        self.persistence_prompt = Some(PersistencePrompt {
            alias: editor.profile.alias.trim().to_owned(),
            from,
            to,
            then,
        });
        true
    }

    pub(super) fn apply_persistence_choice(&mut self, confirm: bool, context: &egui::Context) {
        let Some(prompt) = self.persistence_prompt.take() else {
            return;
        };
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        if !confirm {
            // A change is undone; a first save keeps what the user picked.
            if prompt.from.is_some() {
                editor.profile.auth_persistence = editor.original.auth_persistence;
            }
            self.set_status_key("status_cancelled");
            return;
        }
        editor.confirmed_persistence = Some(prompt.to);
        match prompt.then {
            PendingAction::Save => self.save(context),
            PendingAction::Test => self.start_test(),
            PendingAction::TestAll => self.start_all_tests(),
        }
    }

    fn save(&mut self, context: &egui::Context) {
        if self.ask_persistence_confirmation(PendingAction::Save) {
            return;
        }
        match self.persist_editor() {
            Ok(false) => self.set_vault_status(self.editor_group(), |texts| texts.saving),
            Ok(true) => {
                self.set_status_key("status_saved");
                if self.launch.codex_edit {
                    let alias = self
                        .editor
                        .as_ref()
                        .map(|editor| editor.profile.alias.clone())
                        .unwrap_or_default();
                    match self.write_callback("saved", Some(&alias)) {
                        Ok(()) => context.send_viewport_cmd(egui::ViewportCommand::Close),
                        Err(error) => {
                            self.set_status_format("callback_error", &[("error", &error)])
                        }
                    }
                }
            }
            Err(error) => self.set_status(StatusKind::Error, error),
        }
    }

    fn start_test(&mut self) {
        if self.ask_persistence_confirmation(PendingAction::Test) {
            return;
        }
        match self.persist_editor() {
            Ok(true) => {}
            Ok(false) => {
                self.set_vault_status(self.editor_group(), |texts| texts.saving);
                return;
            }
            Err(error) => {
                self.set_status(StatusKind::Error, error);
                return;
            }
        }
        let Some(id) = self.editor.as_ref().map(|editor| editor.profile.id) else {
            return;
        };
        self.start_test_host(id);
        self.set_status_key("testing");
    }

    fn host_test_running(&self, id: Uuid) -> bool {
        self.test_operations.contains_key(&id) || self.pending_tests.contains(&id)
    }

    fn start_test_host(&mut self, id: Uuid) {
        if self.testing_all || self.host_test_running(id) {
            return;
        }
        self.start_test_worker(id, Arc::new(self.store.hosts.clone()));
    }

    fn start_test_worker(&mut self, id: Uuid, hosts: Arc<Vec<HostProfile>>) {
        let Some(profile) = hosts.iter().find(|host| host.id == id).cloned() else {
            return;
        };
        let (sender, receiver) = mpsc::channel();
        let started_at = Instant::now();
        let timeout = gui_test_timeout(&profile, hosts.as_ref());
        let repaint_context = self.repaint_context.clone();
        self.repaint_context.request_repaint_after(timeout);
        thread::spawn(move || {
            let result = connection::probe(&profile, hosts.as_ref(), gui_test_limits(timeout));
            if sender.send((Instant::now(), result)).is_ok() {
                repaint_context.request_repaint();
            }
        });
        self.test_operations.insert(
            id,
            TestOperation {
                receiver,
                started_at,
                timeout,
            },
        );
        self.test_states.insert(id, HostTestState::Testing);
    }

    fn start_pending_tests(&mut self) {
        let Some(hosts) = self.test_hosts_snapshot.clone() else {
            return;
        };
        while self.test_operations.len() < MAX_CONCURRENT_TESTS {
            let Some(id) = self.pending_tests.pop_front() else {
                break;
            };
            self.start_test_worker(id, Arc::clone(&hosts));
        }
    }

    fn tests_idle(&self) -> bool {
        self.test_operations.is_empty() && self.pending_tests.is_empty()
    }

    fn start_all_tests(&mut self) {
        if !self.tests_idle() {
            return;
        }
        if self.ask_persistence_confirmation(PendingAction::TestAll) {
            return;
        }
        if self.editor.as_ref().is_some_and(|editor| {
            editor.profile != editor.original
                || editor.should_store_password()
                || editor.should_store_key_passphrase()
                || editor.password_value_missing()
        }) {
            match self.persist_editor() {
                Ok(true) => {}
                Ok(false) => {
                    self.set_vault_status(self.editor_group(), |texts| texts.saving);
                    return;
                }
                Err(error) => {
                    self.set_status(StatusKind::Error, error);
                    return;
                }
            }
        }
        let ids = self
            .store
            .hosts
            .iter()
            .map(|host| host.id)
            .collect::<Vec<_>>();
        if ids.is_empty() {
            self.set_status_key("test_all_empty");
            return;
        }
        self.test_states.clear();
        self.test_states
            .extend(ids.iter().copied().map(|id| (id, HostTestState::Testing)));
        self.pending_tests = ids.into_iter().collect();
        self.test_hosts_snapshot = Some(Arc::new(self.store.hosts.clone()));
        self.testing_all = true;
        self.start_pending_tests();
        self.set_status_key("testing_all");
    }

    fn poll_tests(&mut self) {
        let mut completed = Vec::new();
        for (&id, operation) in &self.test_operations {
            match operation.receiver.try_recv() {
                Ok((finished_at, result)) => {
                    if test_timed_out(
                        finished_at.duration_since(operation.started_at),
                        operation.timeout,
                    ) {
                        completed.push((
                            id,
                            Err(RemoteFailure::new(
                                "TEST_TIMEOUT",
                                "The connection test exceeded its configured time limit.",
                            )),
                        ));
                    } else {
                        completed.push((id, result));
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => completed.push((
                    id,
                    Err(RemoteFailure::new(
                        "TEST_WORKER_STOPPED",
                        "The connection test worker stopped unexpectedly.",
                    )),
                )),
                Err(mpsc::TryRecvError::Empty)
                    if test_timed_out(operation.started_at.elapsed(), operation.timeout) =>
                {
                    completed.push((
                        id,
                        Err(RemoteFailure::new(
                            "TEST_TIMEOUT",
                            "The connection test exceeded its configured time limit.",
                        )),
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        for (id, result) in completed {
            self.test_operations.remove(&id);
            self.finish_test(id, result);
        }
        if self.testing_all {
            self.start_pending_tests();
        }
        if self.testing_all && self.tests_idle() {
            self.testing_all = false;
            if self.test_store_dirty {
                if let Err(error) = self.store.save() {
                    if let Some(snapshot) = self.test_hosts_snapshot.take() {
                        self.store.hosts = snapshot.as_ref().clone();
                    }
                    self.test_store_dirty = false;
                    self.set_status_format("storage_error", &[("error", &error.to_string())]);
                    return;
                }
                self.test_store_dirty = false;
                if let Some(id) = self.editor.as_ref().map(|editor| editor.profile.id) {
                    self.sync_editor_verification_from_store(id);
                }
            }
            self.test_hosts_snapshot = None;
            let succeeded = self
                .test_states
                .values()
                .filter(|state| **state == HostTestState::Succeeded)
                .count()
                .to_string();
            let failed = self
                .test_states
                .values()
                .filter(|state| **state == HostTestState::Failed)
                .count()
                .to_string();
            let text = self.catalog.format(
                "status_test_all_done",
                &[("succeeded", &succeeded), ("failed", &failed)],
            );
            let kind = if failed == "0" {
                StatusKind::Success
            } else {
                StatusKind::Warning
            };
            self.set_status(kind, text);
        }
    }

    fn finish_test(&mut self, id: Uuid, result: Result<RemoteResult, RemoteFailure>) {
        match result {
            Ok(result) => {
                self.test_states.insert(id, HostTestState::Succeeded);
                let identity = result.stdout.trim().to_owned();
                let mut updated_store = self.store.clone();
                let mut metadata_changed = false;
                for verified in &result.verified_host_keys {
                    if let Some(stored) = updated_store
                        .hosts
                        .iter_mut()
                        .find(|host| host.id == verified.host_id)
                    {
                        metadata_changed |= apply_verified_host_key(stored, verified);
                    }
                }
                if self.testing_all {
                    self.store = updated_store;
                    self.test_store_dirty |= metadata_changed;
                } else if metadata_changed {
                    if let Err(error) = updated_store.save() {
                        self.set_status_format("storage_error", &[("error", &error.to_string())]);
                        return;
                    }
                    self.store = updated_store;
                    self.sync_editor_verification_from_store(id);
                }
                if !self.testing_all && self.selected == Some(id) {
                    self.set_status_format("status_test_ok", &[("identity", identity.as_str())]);
                }
            }
            Err(error)
                if matches!(error.code, "HOSTKEY_UNKNOWN" | "HOSTKEY_MISMATCH")
                    && error
                        .host_key
                        .as_ref()
                        .is_some_and(|details| details.observed_fingerprint.is_some()) =>
            {
                self.test_states.insert(id, HostTestState::Failed);
                if !self.testing_all && self.selected == Some(id) {
                    let details = error.host_key.unwrap_or_default();
                    self.fingerprint_prompt = Some(FingerprintPrompt {
                        host_id: id,
                        alias: error
                            .host_alias
                            .map(|value| value.into_string())
                            .unwrap_or_default(),
                        expected: details
                            .expected_fingerprint
                            .map(|value| value.into_string()),
                        observed: details
                            .observed_fingerprint
                            .map(|value| value.into_string())
                            .unwrap_or_default(),
                        expected_algorithm: details
                            .expected_algorithm
                            .map(|value| value.into_string()),
                        observed_algorithm: details
                            .observed_algorithm
                            .map(|value| value.into_string()),
                        retry_test: true,
                        close_after_choice: false,
                    });
                }
            }
            Err(error) => {
                self.test_states.insert(id, HostTestState::Failed);
                if !self.testing_all && self.selected == Some(id) {
                    if matches!(error.code, "TEST_TIMEOUT" | TOTAL_TIMEOUT_CODE) {
                        self.set_status_key("status_test_timeout");
                    } else {
                        self.set_status_format("status_test_failed", &[("error", error.code)]);
                    }
                }
            }
        }
    }

    fn sync_editor_verification_from_store(&mut self, id: Uuid) {
        let Some(stored) = self.store.hosts.iter().find(|host| host.id == id) else {
            return;
        };
        let Some(editor) = self
            .editor
            .as_mut()
            .filter(|editor| editor.profile.id == id)
        else {
            return;
        };
        for profile in [&mut editor.profile, &mut editor.original] {
            profile.verified = stored.verified;
            profile
                .host_fingerprint
                .clone_from(&stored.host_fingerprint);
            profile
                .host_key_algorithm
                .clone_from(&stored.host_key_algorithm);
            profile.host_key_first_seen_unix = stored.host_key_first_seen_unix;
            profile.host_key_last_verified_unix = stored.host_key_last_verified_unix;
        }
    }

    fn apply_fingerprint_choice(&mut self, trust: bool, context: &egui::Context) {
        let Some(prompt) = self.fingerprint_prompt.take() else {
            return;
        };
        if !trust {
            self.set_status_key("status_cancelled");
            if prompt.close_after_choice {
                match self.write_callback("cancelled", Some(&prompt.alias)) {
                    Ok(()) => context.send_viewport_cmd(egui::ViewportCommand::Close),
                    Err(error) => {
                        self.set_status_format("callback_error", &[("error", &error)]);
                        self.fingerprint_prompt = Some(prompt);
                    }
                }
            }
            return;
        }

        let mut updated_store = self.store.clone();
        let Some(host) = updated_store
            .hosts
            .iter_mut()
            .find(|host| host.id == prompt.host_id)
        else {
            self.set_status_format("storage_error", &[("error", "HOST_NOT_FOUND")]);
            return;
        };
        host.host_fingerprint = Some(prompt.observed.clone());
        host.host_key_algorithm = prompt.observed_algorithm.clone();
        host.host_key_first_seen_unix = None;
        host.host_key_last_verified_unix = None;
        host.verified = false;
        if let Err(error) = updated_store.save() {
            self.set_status_format("storage_error", &[("error", &error.to_string())]);
            self.fingerprint_prompt = Some(prompt);
            return;
        }
        self.store = updated_store;

        if let Some(editor) = &mut self.editor
            && editor.profile.id == prompt.host_id
        {
            editor.profile.host_fingerprint = Some(prompt.observed.clone());
            editor.profile.host_key_algorithm = prompt.observed_algorithm.clone();
            editor.profile.host_key_first_seen_unix = None;
            editor.profile.host_key_last_verified_unix = None;
            editor.profile.verified = false;
            editor.original.host_fingerprint = Some(prompt.observed.clone());
            editor.original.host_key_algorithm = prompt.observed_algorithm.clone();
            editor.original.host_key_first_seen_unix = None;
            editor.original.host_key_last_verified_unix = None;
            editor.original.verified = false;
        }
        if prompt.close_after_choice {
            match self.write_callback("trusted", Some(&prompt.alias)) {
                Ok(()) => context.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(error) => {
                    self.set_status_format("callback_error", &[("error", &error)]);
                }
            }
        } else if prompt.retry_test {
            self.start_test_host(prompt.host_id);
            if self.selected == Some(prompt.host_id) {
                self.set_status_key("testing");
            }
        }
    }

    fn remove_selected(&mut self) {
        let Some(id) = self.selected else {
            return;
        };
        if self
            .store
            .hosts
            .iter()
            .any(|host| host.jump_host == Some(id))
        {
            self.set_status_key("chain_in_use");
            return;
        }

        // A vault-backed host is its item: deleting it moves the item to the
        // provider's trash or archive first; the mirror goes when that lands.
        if self
            .store
            .hosts
            .iter()
            .any(|host| host.id == id && host.is_vault_host())
        {
            self.start_vault_task(VaultTask::Delete(id));
            return;
        }
        let Some(profile) = self.store.hosts.iter().find(|host| host.id == id).cloned() else {
            return;
        };
        let credential_snapshot = match credentials::snapshot_host(&profile) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.set_status_format("credential_error", &[("error", &error.to_string())]);
                return;
            }
        };
        let credential_snapshots = vec![(id, credential_snapshot)];
        if let Err(error) = credentials::delete_host(&profile) {
            let error = with_rollback_error(
                error.to_string(),
                restore_credential_snapshots(&credential_snapshots).err(),
            );
            self.set_status_format("credential_error", &[("error", &error)]);
            return;
        }
        let original_hosts = self.store.hosts.clone();
        self.store.hosts.retain(|host| host.id != id);
        if let Err(error) = self.store.save() {
            self.store.hosts = original_hosts;
            let mut rollback_errors = Vec::new();
            if let Err(rollback_error) = restore_credential_snapshots(&credential_snapshots) {
                rollback_errors.push(rollback_error);
            }
            if let Err(rollback_error) = self.store.save() {
                rollback_errors.push(format!("host metadata: {rollback_error}"));
            }
            let error = with_rollback_error(
                error.to_string(),
                (!rollback_errors.is_empty()).then(|| rollback_errors.join("; ")),
            );
            self.set_status_format("storage_error", &[("error", &error)]);
            return;
        }
        crate::ssh::invalidate_profile(id);
        self.test_operations.remove(&id);
        self.pending_tests.retain(|pending| *pending != id);
        self.test_states.remove(&id);
        self.selected = self.store.hosts.first().map(|host| host.id);
        self.editor = self
            .selected
            .and_then(|selected| self.store.hosts.iter().find(|host| host.id == selected))
            .cloned()
            .map(HostEditor::load);
    }

    fn begin_batch_mode(&mut self) {
        self.batch_mode = true;
        self.batch_selected.clear();
        self.set_status_key("batch_select_hint");
    }

    fn toggle_batch_selection(&mut self) {
        toggle_visible_selection(
            &self.store.hosts,
            &self.host_filter,
            &mut self.batch_selected,
        );
    }

    fn cancel_batch_mode(&mut self) {
        self.batch_mode = false;
        self.batch_selected.clear();
        self.batch_delete_prompt = false;
        self.batch_export_window_open = false;
        self.set_status_key("status_ready");
    }

    fn request_batch_delete(&mut self) {
        if self.batch_selected.is_empty() {
            self.set_status_key("batch_nothing_selected");
            return;
        }
        if batch_has_external_dependents(&self.store.hosts, &self.batch_selected) {
            self.set_status_key("chain_in_use");
            return;
        }
        self.batch_delete_prompt = true;
    }

    fn remove_batch(&mut self) {
        let ids = self.batch_selected.iter().copied().collect::<Vec<_>>();
        let profiles = self
            .store
            .hosts
            .iter()
            .filter(|host| ids.contains(&host.id))
            .cloned()
            .collect::<Vec<_>>();
        let credential_snapshots = match profiles
            .iter()
            .map(|host| credentials::snapshot_host(host).map(|snapshot| (host.id, snapshot)))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(snapshots) => snapshots,
            Err(error) => {
                self.set_status_format("credential_error", &[("error", &error.to_string())]);
                return;
            }
        };
        for profile in &profiles {
            if let Err(error) = credentials::delete_host(profile) {
                let error = with_rollback_error(
                    error.to_string(),
                    restore_credential_snapshots(&credential_snapshots).err(),
                );
                self.set_status_format("credential_error", &[("error", &error)]);
                return;
            }
        }
        let original_hosts = self.store.hosts.clone();
        self.store
            .hosts
            .retain(|host| !self.batch_selected.contains(&host.id));
        if let Err(error) = self.store.save() {
            self.store.hosts = original_hosts;
            let mut rollback_errors = Vec::new();
            if let Err(rollback_error) = restore_credential_snapshots(&credential_snapshots) {
                rollback_errors.push(rollback_error);
            }
            if let Err(rollback_error) = self.store.save() {
                rollback_errors.push(format!("host metadata: {rollback_error}"));
            }
            let error = with_rollback_error(
                error.to_string(),
                (!rollback_errors.is_empty()).then(|| rollback_errors.join("; ")),
            );
            self.set_status_format("storage_error", &[("error", &error)]);
            return;
        }
        for id in &ids {
            crate::ssh::invalidate_profile(*id);
            self.test_operations.remove(id);
            self.pending_tests.retain(|pending| pending != id);
            self.test_states.remove(id);
        }
        self.selected = self.store.hosts.first().map(|host| host.id);
        self.editor = self
            .selected
            .and_then(|selected| self.store.hosts.iter().find(|host| host.id == selected))
            .cloned()
            .map(HostEditor::load);
        self.batch_mode = false;
        self.batch_selected.clear();
        if self.testing_all && self.tests_idle() {
            self.testing_all = false;
            self.test_hosts_snapshot = None;
        }
        self.set_status_key("batch_deleted");
    }

    fn download_import_template(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("CSV", &["csv"])
            .set_file_name("codex-hosts-import-template.csv")
            .save_file()
        else {
            return;
        };
        match fs::write(&path, import::template_bytes()) {
            Ok(()) => {
                let path = path.display().to_string();
                self.set_status_format("template_saved", &[("path", path.as_str())]);
            }
            Err(error) => {
                self.set_status_format("template_save_failed", &[("error", &error.to_string())]);
            }
        }
    }

    fn import_hosts_from_template(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("CSV", &["csv"])
            .pick_file()
        else {
            return;
        };
        let batch = fs::read(&path)
            .map(Zeroizing::new)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                import::parse_template(bytes.as_slice(), &self.store.hosts)
                    .map_err(|error| error.to_string())
            });
        let batch = match batch {
            Ok(batch) => batch,
            Err(error) => {
                self.set_status_format("import_failed", &[("error", error.as_str())]);
                return;
            }
        };
        let imported_ids = batch
            .hosts
            .iter()
            .map(|item| item.profile.clone())
            .collect::<Vec<_>>();
        for item in &batch.hosts {
            if let Some((kind, secret)) = imported_credential(item)
                && let Err(error) = credentials::store(item.profile.id, kind, secret)
            {
                rollback_import_credentials(&imported_ids);
                self.set_status_format("credential_error", &[("error", &error.to_string())]);
                return;
            }
        }
        let first_id = batch.hosts.first().map(|item| item.profile.id);
        let count = batch.hosts.len();
        let contains_sensitive_values = batch.contains_sensitive_values;
        self.store
            .hosts
            .extend(batch.hosts.into_iter().map(|item| item.profile));
        if let Err(error) = self.store.save() {
            self.store.hosts.truncate(self.store.hosts.len() - count);
            rollback_import_credentials(&imported_ids);
            self.set_status_format("storage_error", &[("error", &error.to_string())]);
            return;
        }
        if let Some(id) = first_id {
            self.select(id);
            self.scroll_to_selected = true;
        }
        self.import_window_open = false;
        if contains_sensitive_values {
            self.set_status_format(
                "import_succeeded_with_credentials",
                &[("count", &count.to_string())],
            );
            self.import_cleanup_prompt = Some(ImportCleanupPrompt {
                path,
                imported_count: count,
            });
        } else {
            self.set_status_format("import_succeeded", &[("count", &count.to_string())]);
        }
    }

    fn request_batch_export(&mut self) {
        if self.batch_selected.is_empty() {
            self.set_status_key("batch_nothing_selected");
            return;
        }
        self.batch_export_window_open = true;
    }

    fn selected_export_bytes(&self) -> Result<Vec<u8>, String> {
        let selected = self
            .store
            .hosts
            .iter()
            .filter(|host| self.batch_selected.contains(&host.id))
            .cloned()
            .collect::<Vec<_>>();
        import::export_bytes(&selected, &self.store.hosts).map_err(|error| error.to_string())
    }

    fn export_batch_to_directory(&mut self) {
        let Some(directory) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        let result = self
            .selected_export_bytes()
            .and_then(|bytes| write_unique_export(&directory, &bytes).map_err(|e| e.to_string()));
        self.finish_batch_export(result);
    }

    fn export_batch_to_file(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("CSV", &["csv"])
            .set_file_name("codex-hosts-export.csv")
            .save_file()
        else {
            return;
        };
        let result = self
            .selected_export_bytes()
            .and_then(|bytes| fs::write(&path, bytes).map_err(|error| error.to_string()))
            .map(|()| path);
        self.finish_batch_export(result);
    }

    fn finish_batch_export(&mut self, result: Result<PathBuf, String>) {
        match result {
            Ok(path) => {
                let path = path.display().to_string();
                self.set_status_format("batch_export_succeeded", &[("path", path.as_str())]);
                self.batch_export_window_open = false;
                self.batch_mode = false;
                self.batch_selected.clear();
            }
            Err(error) => {
                self.set_status_format("batch_export_failed", &[("error", error.as_str())]);
            }
        }
    }

    fn write_callback(&mut self, status: &'static str, alias: Option<&str>) -> Result<(), String> {
        if self.callback_written {
            return Ok(());
        }
        let should_replace = match self.pending_callback.as_ref() {
            None => true,
            Some(pending) => should_replace_callback(pending.status, status),
        };
        if should_replace {
            self.pending_callback = Some(PendingCallback {
                status,
                alias: alias.map(str::to_owned),
            });
        }
        self.flush_callback()
    }

    fn flush_callback(&mut self) -> Result<(), String> {
        if self.callback_written {
            return Ok(());
        }
        let Some(callback) = self.pending_callback.clone() else {
            return Ok(());
        };
        let Some(path) = self.launch.result_path.as_deref() else {
            self.callback_written = true;
            self.pending_callback = None;
            return Ok(());
        };
        #[derive(Serialize)]
        struct Callback<'a> {
            status: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            alias: Option<&'a str>,
        }
        let bytes = serde_json::to_vec_pretty(&Callback {
            status: callback.status,
            alias: callback.alias.as_deref(),
        })
        .map_err(|error| error.to_string())?;
        fs::write(path, bytes).map_err(|error| error.to_string())?;
        self.callback_written = true;
        self.pending_callback = None;
        Ok(())
    }

    fn start_fido_setup_operation(&mut self, action: FidoSetupAction) {
        let Some(prompt) = self.fido_setup_prompt.as_mut() else {
            return;
        };
        if prompt.operation.is_some() {
            return;
        }
        let pin = std::mem::take(&mut prompt.pin);
        let recover_existing_message = self.catalog.text("fido_recover_existing").to_owned();
        let recovery_cancelled_message = self.catalog.text("fido_recovery_cancelled").to_owned();
        let enrollment_cancelled_message =
            self.catalog.text("fido_enrollment_cancelled").to_owned();
        let (sender, receiver) = mpsc::channel();
        let repaint_context = self.repaint_context.clone();
        prompt.identity = None;
        prompt.status = Some(self.catalog.text("fido_wait_touch").to_owned());
        prompt.operation = Some(receiver);
        thread::spawn(move || {
            let result = (|| -> Result<Vec<FidoKeyInfo>, String> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?;
                let keys = match action {
                    FidoSetupAction::CreateRecommended => {
                        let enrollment = fido::recoverable_enrollment();
                        vec![
                            runtime
                                .block_on(fido::enroll(
                                    enrollment.algorithm,
                                    enrollment.application,
                                    enrollment.user_id,
                                    enrollment.flags,
                                    pin,
                                ))
                                .map_err(|error| {
                                    if matches!(
                                        &error,
                                        fido::FidoError::RecoverableCredentialExists
                                    ) {
                                        recover_existing_message.clone()
                                    } else if matches!(
                                        &error,
                                        fido::FidoError::WindowsEnrollmentCancelled
                                    ) {
                                        enrollment_cancelled_message.clone()
                                    } else {
                                        error.to_string()
                                    }
                                })?,
                        ]
                    }
                    FidoSetupAction::CreateCompatible => {
                        let enrollment = fido::compatible_enrollment();
                        vec![
                            runtime
                                .block_on(fido::enroll(
                                    enrollment.algorithm,
                                    enrollment.application,
                                    enrollment.user_id,
                                    enrollment.flags,
                                    pin,
                                ))
                                .map_err(|error| error.to_string())?,
                        ]
                    }
                    FidoSetupAction::RecoverResident => runtime
                        .block_on(fido::load_resident(pin))
                        .map_err(|error| match error {
                            fido::FidoError::WindowsRecoveryCancelled { prompt } => {
                                recovery_cancelled_message.replace("{step}", &prompt.to_string())
                            }
                            _ => error.to_string(),
                        })?,
                };
                if keys.is_empty() {
                    return Err(
                        "No resident SSH credentials were found on the security key.".to_owned(),
                    );
                }
                keys.iter()
                    .map(fido::save_handle)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| error.to_string())
            })();
            if sender.send(result).is_ok() {
                repaint_context.request_repaint();
            }
        });
    }
}

impl eframe::App for HostsApp {
    // eframe's default clear color is near-black regardless of theme; `App::ui`
    // draws without a panel frame, so the window background must follow the theme.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    fn on_exit(&mut self) {
        self.forget_window_scoped_session();
    }

    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_tests();
        let mut restore = false;
        let mut open_secrets = false;
        if let Some(tray) = &self.tray {
            tray.set_catalog(&self.catalog);
            while let Some(event) = tray.poll() {
                match event {
                    crate::tray::Event::Open => restore = true,
                    crate::tray::Event::Secrets => {
                        restore = true;
                        open_secrets = true;
                    }
                    crate::tray::Event::Exit => {
                        self.exiting = true;
                        context.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    crate::tray::Event::Unavailable => {
                        self.tray_available = false;
                        restore = true;
                    }
                }
            }
        }
        let mut requested = false;
        if let Some(temporary) = &mut self.temporary {
            if open_secrets {
                temporary.visible = true;
            }
            requested = temporary.logic(context);
            restore |= requested;
        }
        if restore && !self.exiting {
            self.host_refresh_pending = true;
            self.hidden = false;
            context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            context.send_viewport_cmd(egui::ViewportCommand::Focus);
            // Windows may refuse the focus to a background process; the
            // taskbar button then flashes until the user switches over.
            if requested && !context.input(|input| input.viewport().focused.unwrap_or(false)) {
                context.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                    egui::UserAttentionType::Critical,
                ));
            }
        }
        if self.host_refresh_pending && self.tests_idle() && !self.testing_all {
            self.host_refresh_pending = false;
            if let Err(error) = self.refresh_hosts() {
                self.set_status_format("storage_error", &[("error", &error)]);
            }
        }
        if should_hide_to_tray(
            context.input(|i| i.viewport().close_requested()),
            self.launch.codex_edit,
            self.exiting,
            self.hidden,
            restore,
        ) {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.tray_available {
                self.hidden = true;
                context.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            } else {
                self.set_status_key("tray_unavailable");
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }
}

impl HostsApp {
    /// Full window layout; separate from `App::ui` so headless tests can drive it.
    fn render(&mut self, ui: &mut egui::Ui) {
        let context = ui.ctx().clone();
        let panel_fill = ui.visuals().panel_fill;
        egui::Panel::top("toolbar")
            .frame(
                egui::Frame::new()
                    .fill(panel_fill)
                    .inner_margin(egui::Margin::symmetric(8, 8)),
            )
            .show(ui, |ui| self.toolbar(ui, &context));
        if self.batch_mode {
            egui::Panel::top("selection_bar")
                .frame(egui::Frame::new().fill(panel_fill))
                .show_separator_line(false)
                .show(ui, |ui| self.selection_bar(ui));
        }
        egui::Panel::bottom("status_bar")
            .frame(
                egui::Frame::new()
                    .fill(panel_fill)
                    .inner_margin(egui::Margin::symmetric(0, 5)),
            )
            .show(ui, |ui| self.status_bar(ui));
        let window_width = ui.available_width();
        let sidebar_max = (window_width * 0.36).clamp(240.0, 440.0);
        let sidebar_width = self
            .sidebar_width
            .unwrap_or((window_width * 0.30).clamp(240.0, 340.0))
            .clamp(240.0, sidebar_max);
        let list_response = egui::Panel::left("host_list")
            // Rows run edge to edge; the list inset is applied per row so the selection bar
            // can sit flush against the window edge.
            .frame(egui::Frame::new().fill(panel_fill))
            .resizable(true)
            .default_size(sidebar_width)
            // Until the user drags the divider the list follows the window width; a
            // dragged width is kept but never allowed past a third of the window.
            .size_range(if self.sidebar_width.is_some() {
                240.0..=sidebar_max
            } else {
                sidebar_width..=sidebar_width
            })
            .show(ui, |ui| self.host_list(ui));
        let shown_width = list_response.response.rect.width();
        if self
            .sidebar_width
            .is_some_and(|width| (width - shown_width).abs() > 0.5)
        {
            self.sidebar_width = Some(shown_width);
        }
        if self.sidebar_width.is_none()
            && ui
                .ctx()
                .read_response(egui::Id::new("host_list").with("__resize"))
                .is_some_and(|response| response.dragged())
        {
            self.sidebar_width = Some(shown_width);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(panel_fill))
            .show(ui, |ui| self.editor_panel(ui, &context));
        self.show_dialogs(&context);
        self.toasts(&context);
        if let Some(temporary) = &mut self.temporary {
            temporary.show(&context, &self.catalog);
            if let Some(on) = temporary.take_approval_default_change() {
                self.store.temporary_call_approval = on;
                if let Err(error) = self.store.save() {
                    self.set_status_format("storage_error", &[("error", &error.to_string())]);
                }
            }
        }
    }
}

impl Drop for HostsApp {
    fn drop(&mut self) {
        if self.launch.codex_edit && !self.callback_written {
            let alias = self
                .editor
                .as_ref()
                .map(|editor| editor.profile.alias.clone());
            let _ = self.write_callback("cancelled", alias.as_deref());
        }
    }
}

fn reconcile_host_editor(
    editor: &mut Option<HostEditor>,
    selected: &mut Option<Uuid>,
    store: &HostStore,
) {
    if editor.as_ref().is_some_and(HostEditor::has_unsaved_changes) {
        return; // Preserve drafts, but persist_editor must reject an externally changed original.
    }
    let host = selected
        .and_then(|id| store.hosts.iter().find(|host| host.id == id))
        .or_else(|| store.hosts.first());
    *selected = host.map(|host| host.id);
    *editor = host.cloned().map(HostEditor::load);
}

fn should_hide_to_tray(
    close_requested: bool,
    codex_edit: bool,
    exiting: bool,
    hidden: bool,
    restoring: bool,
) -> bool {
    close_requested && !codex_edit && !exiting && !hidden && !restoring
}

fn test_timed_out(elapsed: Duration, timeout: Duration) -> bool {
    elapsed >= timeout
}

fn should_replace_callback(existing: &str, new: &str) -> bool {
    fn priority(status: &str) -> u8 {
        match status {
            "trusted" => 2,
            "saved" => 1,
            _ => 0,
        }
    }

    priority(new) > priority(existing) || new == existing
}

fn gui_test_timeout(profile: &HostProfile, hosts: &[HostProfile]) -> Duration {
    if profile.protocol == Protocol::Ssh
        && crate::ssh::profile_may_require_interaction(profile, hosts)
    {
        GUI_INTERACTIVE_TEST_TIMEOUT
    } else {
        GUI_TEST_TIMEOUT
    }
}

fn gui_test_limits(timeout: Duration) -> OperationLimits {
    OperationLimits {
        total_timeout: Some(timeout),
        connect_timeout: Some(timeout),
        command_timeout: Some(timeout),
        output_bytes: None,
        batch_scope: None,
        retain_sessions: false,
        approved: false,
    }
}

fn rollback_import_credentials(hosts: &[HostProfile]) {
    for host in hosts {
        let _ = credentials::delete_host(host);
    }
}

fn restore_credential_snapshots(
    snapshots: &[(Uuid, credentials::CredentialSnapshot)],
) -> Result<(), String> {
    let mut errors = Vec::new();
    for (id, snapshot) in snapshots {
        if let Err(error) = credentials::restore(*id, snapshot) {
            errors.push(format!("{id}: {error}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn with_rollback_error(primary: String, rollback: Option<String>) -> String {
    match rollback {
        Some(rollback) => format!("{primary}; rollback failed: {rollback}"),
        None => primary,
    }
}

fn imported_credential(item: &import::ImportedHost) -> Option<(CredentialKind, &str)> {
    match (item.profile.protocol, item.profile.ssh_auth) {
        (Protocol::Telnet, _) | (Protocol::Ssh, SshAuth::Password) => (!item.password.is_empty())
            .then_some((
                credentials::login_password_kind(&item.profile),
                item.password.as_str(),
            )),
        (Protocol::Ssh, SshAuth::PrivateKey) => (!item.key_passphrase.is_empty())
            .then_some((CredentialKind::KeyPassphrase, item.key_passphrase.as_str())),
        (Protocol::Ssh, SshAuth::SshAgent) => None,
    }
}

fn apply_verified_host_key(host: &mut HostProfile, verified: &VerifiedHostKey) -> bool {
    if host.id != verified.host_id
        || host.host_fingerprint.as_deref() != Some(verified.fingerprint.as_str())
    {
        return false;
    }

    let first_seen = host
        .host_key_first_seen_unix
        .or(Some(verified.verified_at_unix));
    let last_verified = Some(
        host.host_key_last_verified_unix
            .unwrap_or_default()
            .max(verified.verified_at_unix),
    );
    let changed = !host.verified
        || host.host_key_algorithm.as_deref() != Some(verified.algorithm.as_str())
        || host.host_key_first_seen_unix != first_seen
        || host.host_key_last_verified_unix != last_verified;

    host.verified = true;
    host.host_key_algorithm = Some(verified.algorithm.clone());
    host.host_key_first_seen_unix = first_seen;
    host.host_key_last_verified_unix = last_verified;
    changed
}

fn export_file_name(index: u32) -> String {
    if index == 0 {
        "codex-hosts-export.csv".to_owned()
    } else {
        format!("codex-hosts-export-{index}.csv")
    }
}

fn write_unique_export(directory: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    for index in 0..=u32::MAX {
        let path = directory.join(export_file_name(index));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                if let Err(error) = file.write_all(bytes) {
                    drop(file);
                    let _ = fs::remove_file(&path);
                    return Err(error);
                }
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no available export file name",
    ))
}

fn batch_has_external_dependents(hosts: &[HostProfile], selected: &HashSet<Uuid>) -> bool {
    hosts.iter().any(|host| {
        !selected.contains(&host.id)
            && host
                .jump_host
                .is_some_and(|jump_id| selected.contains(&jump_id))
    })
}

#[cfg(test)]
fn batch_all_selected(hosts: &[HostProfile], selected: &HashSet<Uuid>) -> bool {
    !hosts.is_empty() && hosts.iter().all(|host| selected.contains(&host.id))
}

fn retain_visible_selection(
    hosts: &[HostProfile],
    filter: &HostFilter,
    selected: &mut HashSet<Uuid>,
) {
    let visible = hosts
        .iter()
        .filter(|host| filter.matches(host))
        .map(|host| host.id)
        .collect::<HashSet<_>>();
    selected.retain(|id| visible.contains(id));
}

fn toggle_visible_selection(
    hosts: &[HostProfile],
    filter: &HostFilter,
    selected: &mut HashSet<Uuid>,
) {
    let visible = hosts
        .iter()
        .filter(|host| filter.matches(host))
        .map(|host| host.id)
        .collect::<HashSet<_>>();
    if !visible.is_empty() && visible.iter().all(|id| selected.contains(id)) {
        selected.clear();
    } else {
        *selected = visible;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Finds the painted rectangle of an exact text run in a frame's output shapes.
    pub(super) fn find_text_rect(
        shapes: &[egui::epaint::ClippedShape],
        text: &str,
    ) -> Option<egui::Rect> {
        fn find(shape: &egui::Shape, text: &str) -> Option<egui::Rect> {
            match shape {
                egui::Shape::Text(shape) if shape.galley.job.text == text => {
                    Some(shape.galley.rect.translate(shape.pos.to_vec2()))
                }
                egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| find(shape, text)),
                _ => None,
            }
        }
        shapes.iter().find_map(|shape| find(&shape.shape, text))
    }

    /// Re-runs the named test in a child process with its own data directory, so the real
    /// save path never touches this user's host list. Only the child gets `true`.
    pub(super) fn in_isolated_store(test: &str) -> bool {
        if std::env::var_os("CODEX_HOSTS_ISOLATED_TEST").is_some() {
            return true;
        }
        let directory =
            std::env::temp_dir().join(format!("codex-hosts-isolated-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--test-threads=1"])
            .env("CODEX_HOSTS_ISOLATED_TEST", "1")
            .env("CODEX_HOSTS_DATA_DIR", &directory)
            .output()
            .unwrap();
        fs::remove_dir_all(&directory).unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "isolated test failed: {stdout} {}",
            String::from_utf8_lossy(&output.stderr)
        );
        false
    }

    pub(super) fn metadata_test_app(
        context: &egui::Context,
        locale: &str,
        profile: HostProfile,
    ) -> HostsApp {
        let editor = HostEditor {
            sudo: SudoDraft::default(),
            tag_input: String::new(),
            profile: profile.clone(),
            original: profile.clone(),
            password: Zeroizing::new(String::new()),
            key_passphrase: Zeroizing::new(String::new()),
            password_mode: PasswordMode::Password,
            saved_password_mode: None,
            password_read_error: None,
            has_key_passphrase: false,
            key_passphrase_read_error: None,
            show_required: false,
            focus_first_missing: false,
            first_save: false,
            confirmed_persistence: None,
            draft: false,
            saving: false,
        };
        let mut store = HostStore::default();
        store.hosts.push(profile.clone());
        HostsApp {
            temporary: None,
            tray: None,
            exiting: false,
            hidden: false,
            tray_available: false,
            host_refresh_pending: false,
            store,
            catalog: Catalog::for_locale(Some(locale)),
            selected: Some(profile.id),
            editor: Some(editor),
            status: StatusMessage::info(""),
            test_operations: HashMap::new(),
            pending_tests: VecDeque::new(),
            test_hosts_snapshot: None,
            test_states: HashMap::new(),
            testing_all: false,
            test_store_dirty: false,
            repaint_context: context.clone(),
            fingerprint_prompt: None,
            delete_prompt: false,
            persistence_prompt: None,
            import_window_open: false,
            import_cleanup_prompt: None,
            fido_setup_prompt: None,
            batch_mode: false,
            batch_selected: HashSet::new(),
            host_filter: HostFilter::default(),
            batch_delete_prompt: false,
            batch_export_window_open: false,
            sidebar_width: None,
            scroll_to_selected: false,
            launch: LaunchOptions::default(),
            callback_written: false,
            pending_callback: None,
            credential_services: None,
            unlock_prompt: None,
            approval_unlock: None,
            vault_job: None,
            onepassword: onepassword::OnePasswordUi::default(),
            pending_vault_save: None,
            vault_session: None,
            vault_state: None,
            setup_done: None,
            pending_switch: None,
            vault_policy_checked: None,
            vault_note: None,
            vault_problems: Vec::new(),
            collapsed_groups: host_list::GroupFolds::default(),
        }
    }

    /// Every painted text run with its clip rectangle.
    pub(super) fn painted_texts(
        shapes: &[egui::epaint::ClippedShape],
    ) -> Vec<(String, egui::Rect, egui::Rect)> {
        fn collect(
            shape: &egui::Shape,
            clip: egui::Rect,
            out: &mut Vec<(String, egui::Rect, egui::Rect)>,
        ) {
            match shape {
                egui::Shape::Text(text) => out.push((
                    text.galley.job.text.clone(),
                    text.galley.rect.translate(text.pos.to_vec2()),
                    clip,
                )),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, clip, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for shape in shapes {
            collect(&shape.shape, shape.clip_rect, &mut out);
        }
        out
    }

    fn layout_test_app(context: &egui::Context, locale: &str) -> HostsApp {
        let selected = HostProfile {
            alias: "production-web-frontend-01".into(),
            address: "web-frontend-01.internal.example.com".into(),
            description: "long description ".repeat(6),
            tags: vec!["production".into(), "web".into(), "frontend".into()],
            verified: true,
            ..Default::default()
        };
        let mut app = metadata_test_app(context, locale, selected);
        for index in 0..12 {
            // Alternate tagged and untagged hosts so uneven row heights would show up.
            app.store.hosts.push(HostProfile {
                alias: format!("host-{index}"),
                address: format!("10.0.{index}.1"),
                tags: if index % 2 == 0 {
                    vec!["lab".into()]
                } else {
                    Vec::new()
                },
                ..Default::default()
            });
        }
        app.set_status_key("status_ready");
        app
    }

    const WINDOW_SIZES: [[f32; 2]; 5] = [
        [820.0, 620.0],
        [1040.0, 760.0],
        [1280.0, 800.0],
        [1600.0, 1000.0],
        [1920.0, 1080.0],
    ];

    fn run_full_window(
        app: &mut HostsApp,
        context: &egui::Context,
        size: [f32; 2],
    ) -> egui::FullOutput {
        let mut output = None;
        for frame in 0..4 {
            output = Some(context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(size[0], size[1]),
                    )),
                    time: Some(frame as f64 * 0.1),
                    ..Default::default()
                },
                |ui| app.render(ui),
            ));
        }
        output.unwrap()
    }

    fn assert_layout_invariants(
        app: &HostsApp,
        output: &egui::FullOutput,
        size: [f32; 2],
        label: &str,
    ) {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size[0], size[1]));
        let texts = painted_texts(&output.shapes);
        assert!(!texts.is_empty(), "{label}: nothing painted");
        for (text, rect, clip) in &texts {
            let visible = rect.intersect(*clip);
            if visible.is_negative() {
                continue; // scrolled out of view
            }
            assert!(
                rect.left() >= clip.left() - 1.0 && rect.right() <= clip.right() + 1.0,
                "{label}: text {text:?} is cut horizontally: {rect:?} clip {clip:?}"
            );
            assert!(
                screen.contains_rect(visible),
                "{label}: text {text:?} leaves the window: {rect:?}"
            );
        }
        let find = |key: &str| {
            let wanted = app.catalog.text(key);
            texts
                .iter()
                .find(|(text, _, _)| text == wanted)
                .map(|(_, rect, _)| *rect)
                .unwrap_or_else(|| panic!("{label}: missing control {key}"))
        };
        let toolbar_y = find("credential_services").center().y;
        for key in ["tray_exit", "language"] {
            let rect = find(key);
            assert!(
                (rect.center().y - toolbar_y).abs() < 4.0,
                "{label}: toolbar control {key} wrapped to another row"
            );
        }
        // The list's own actions sit in a menu beside the list title.
        let menu = texts
            .iter()
            .find(|(text, _, _)| text == "⋯")
            .map(|(_, rect, _)| *rect)
            .unwrap_or_else(|| panic!("{label}: missing host actions menu"));
        assert!(
            menu.center().y > toolbar_y + 10.0,
            "{label}: menu below the toolbar"
        );
        assert!(
            menu.right() < size[0] * 0.5,
            "{label}: menu beside the host list"
        );
        let save = find("save");
        let test = find("test_connection");
        assert!(
            save.bottom() < size[1] - 20.0,
            "{label}: save button hidden"
        );
        assert!((save.center().y - test.center().y).abs() < 4.0);
        let status = find("status_ready");
        assert!(status.bottom() <= size[1] && status.top() > save.bottom());
        let list_title = texts
            .iter()
            .find(|(text, _, _)| text.starts_with(&format!("{} (", app.catalog.text("nav_title"))))
            .expect("host list title");
        assert!(
            list_title.1.left() < size[0] * 0.3,
            "{label}: list title {:?} at {:?}",
            list_title.0,
            list_title.1
        );
        assert!(find("section_basic").left() > list_title.1.right());
        // The row for the selected host must be readable in the list.
        assert!(
            texts
                .iter()
                .any(|(text, _, _)| text.starts_with("production-web"))
        );
        // Every list row must have the same pitch regardless of how many lines it fills.
        let mut alias_tops = texts
            .iter()
            .filter(|(text, rect, _)| text.starts_with("host-") && rect.left() < size[0] * 0.3)
            .map(|(_, rect, _)| rect.top())
            .collect::<Vec<_>>();
        alias_tops.sort_by(f32::total_cmp);
        let pitches = alias_tops
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .collect::<Vec<_>>();
        assert!(pitches.len() >= 3, "{label}: expected several visible rows");
        for pitch in &pitches {
            assert!(
                (pitch - pitches[0]).abs() < 1.0,
                "{label}: uneven row pitch {pitches:?}"
            );
        }
        // Design decision: two-column rows from 1040 up, stacked labels only near the minimum size.
        let alias_label = find("alias");
        let alias_field_top = texts
            .iter()
            .filter(|(text, rect, _)| {
                text == "production-web-frontend-01" && rect.left() > alias_label.left()
            })
            .map(|(_, rect, _)| rect.top())
            .fold(f32::MAX, f32::min);
        if size[0] >= 1040.0 {
            assert!(
                (alias_field_top - alias_label.top()).abs() < 12.0,
                "{label}: alias field should sit beside its label"
            );
        } else {
            assert!(
                alias_field_top > alias_label.bottom(),
                "{label}: alias field should stack under its label"
            );
        }
    }

    #[test]
    fn full_window_layout_holds_at_every_size_and_locale() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            for size in WINDOW_SIZES {
                let context = egui::Context::default();
                configure_fonts(&context, locale);
                apply_style(&context);
                let mut app = layout_test_app(&context, locale);
                let output = run_full_window(&mut app, &context, size);
                assert_layout_invariants(&app, &output, size, &format!("{locale} {size:?}"));
            }
        }
    }

    #[test]
    fn batch_mode_layout_holds_at_every_size_and_locale() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            for size in WINDOW_SIZES {
                let context = egui::Context::default();
                configure_fonts(&context, locale);
                apply_style(&context);
                let mut app = layout_test_app(&context, locale);
                app.begin_batch_mode();
                app.batch_selected
                    .extend(app.store.hosts.iter().take(3).map(|host| host.id));
                let output = run_full_window(&mut app, &context, size);
                let label = format!("batch {locale} {size:?}");
                let texts = painted_texts(&output.shapes);
                for key in ["deselect_all", "select_all", "export", "delete", "cancel"] {
                    let wanted = app.catalog.text(key);
                    if key == "deselect_all" || key == "select_all" {
                        continue;
                    }
                    assert!(
                        texts.iter().any(|(text, _, _)| text.starts_with(wanted)),
                        "{label}: missing selection-bar control {key}"
                    );
                }
                let count = app
                    .catalog
                    .format("batch_selected_count", &[("count", "3")]);
                assert!(
                    texts.iter().any(|(text, _, _)| *text == count),
                    "{label}: count"
                );
                for (text, rect, clip) in &texts {
                    if rect.intersect(*clip).is_negative() {
                        continue;
                    }
                    assert!(
                        rect.right() <= clip.right() + 1.0,
                        "{label}: text {text:?} is cut horizontally"
                    );
                }
            }
        }
    }

    #[test]
    fn persistence_change_dialog_renders_and_cancel_reverts() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            for size in WINDOW_SIZES {
                let context = egui::Context::default();
                configure_fonts(&context, locale);
                apply_style(&context);
                let mut app = layout_test_app(&context, locale);
                app.editor.as_mut().unwrap().profile.auth_persistence = AuthPersistence::Session;
                app.save(&context);
                assert!(
                    app.persistence_prompt.is_some(),
                    "{locale} {size:?}: changing persistence must ask first"
                );
                let output = run_full_window(&mut app, &context, size);
                let texts = painted_texts(&output.shapes);
                let label = format!("persistence dialog {locale} {size:?}");
                for key in [
                    "persistence_change_title",
                    "persistence_change_confirm",
                    "cancel",
                ] {
                    let wanted = app.catalog.text(key);
                    assert!(
                        texts.iter().any(|(text, _, _)| text.starts_with(wanted)),
                        "{label}: missing {key}"
                    );
                }
                let warning = app.catalog.text("persistence_warning_ssh");
                assert!(
                    texts
                        .iter()
                        .any(|(text, _, _)| text.contains(&warning[..12])),
                    "{label}: relaxing must repeat the warning"
                );
                for (text, rect, clip) in &texts {
                    if rect.intersect(*clip).is_negative() {
                        continue;
                    }
                    assert!(
                        rect.right() <= clip.right() + 1.0,
                        "{label}: text {text:?} is cut horizontally"
                    );
                }
                app.apply_persistence_choice(false, &context);
                let editor = app.editor.as_ref().unwrap();
                assert_eq!(editor.profile.auth_persistence, AuthPersistence::PerCall);
                assert!(app.persistence_prompt.is_none());
                assert_eq!(
                    app.store.hosts[0].auth_persistence,
                    AuthPersistence::PerCall,
                    "{label}: cancel writes nothing"
                );
            }
        }
    }

    #[test]
    fn a_new_host_confirms_its_proposed_persistence_on_first_save() {
        let context = egui::Context::default();
        configure_fonts(&context, "en");
        apply_style(&context);
        let mut app = layout_test_app(&context, "en");
        // Built in memory: new_host() would write the store to the real data directory.
        let profile = HostProfile::new("fresh-box".into());
        app.store.hosts.push(profile.clone());
        app.selected = Some(profile.id);
        app.editor = Some(HostEditor::fresh(profile));
        {
            let editor = app.editor.as_mut().unwrap();
            assert_eq!(editor.profile.auth_persistence, AuthPersistence::Session);
            editor.profile.address = "10.0.0.9".into();
            editor.profile.username = "deploy".into();
            editor.profile.ssh_auth = SshAuth::Password;
            editor.password_mode = PasswordMode::NoPassword;
        }
        app.save(&context);
        let prompt = app.persistence_prompt.as_ref().expect("first save asks");
        assert_eq!(prompt.from, None);
        assert_eq!(prompt.to, AuthPersistence::Session);
        let output = run_full_window(&mut app, &context, [1280.0, 800.0]);
        let texts = painted_texts(&output.shapes);
        for key in ["persistence_first_title", "persistence_first_confirm"] {
            let wanted = app.catalog.text(key);
            assert!(
                texts.iter().any(|(text, _, _)| text.starts_with(wanted)),
                "missing {key}"
            );
        }
        app.apply_persistence_choice(false, &context);
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(
            editor.profile.auth_persistence,
            AuthPersistence::Session,
            "cancelling a first save keeps the proposed value in the form"
        );
        assert!(
            app.store
                .hosts
                .iter()
                .all(|host| host.auth_persistence == AuthPersistence::PerCall),
            "nothing relaxed was written"
        );
    }

    #[test]
    fn filtered_bulk_selection_never_keeps_hidden_hosts() {
        let visible = HostProfile {
            tags: vec!["prod".into()],
            ..Default::default()
        };
        let hidden = HostProfile::default();
        let filter = crate::model::HostFilter {
            search: String::new(),
            tags: vec!["PROD".into()],
        };
        let hosts = vec![visible.clone(), hidden.clone()];
        let mut selected = HashSet::from([hidden.id]);
        toggle_visible_selection(&hosts, &filter, &mut selected);
        assert_eq!(selected, HashSet::from([visible.id]));
        toggle_visible_selection(&hosts, &filter, &mut selected);
        assert!(selected.is_empty());
        selected.extend([visible.id, hidden.id]);
        retain_visible_selection(&hosts, &filter, &mut selected);
        assert_eq!(selected, HashSet::from([visible.id]));
    }

    #[test]
    fn refresh_preserves_dirty_drafts_and_reloads_clean_or_deleted_hosts() {
        let original = HostProfile::new("original".into());
        let mut store = HostStore::default();
        store.hosts.push(original.clone());
        let mut selected = Some(original.id);
        let mut editor = Some(HostEditor::load(original.clone()));
        store.hosts[0].address = "external.example".into();
        reconcile_host_editor(&mut editor, &mut selected, &store);
        assert_eq!(editor.as_ref().unwrap().profile.address, "external.example");
        editor.as_mut().unwrap().profile.alias = "unrelated-local-draft".into();
        store
            .hosts
            .push(HostProfile::new("another-window-host".into()));
        reconcile_host_editor(&mut editor, &mut selected, &store);
        assert!(editor.as_ref().unwrap().matches_stored_original(&store));
        editor.as_mut().unwrap().profile.alias = "unsaved-draft".into();
        editor
            .as_mut()
            .unwrap()
            .password
            .push_str("synthetic-draft");
        store.hosts[0].address = "newer.example".into();
        reconcile_host_editor(&mut editor, &mut selected, &store);
        let draft = editor.as_ref().unwrap();
        assert_eq!(draft.profile.alias, "unsaved-draft");
        assert_eq!(draft.password.as_str(), "synthetic-draft");
        assert!(!draft.matches_stored_original(&store));
        // Explicitly discarding the draft allows the current stored profile to be loaded.
        editor = None;
        reconcile_host_editor(&mut editor, &mut selected, &store);
        assert!(editor.as_ref().unwrap().matches_stored_original(&store));
        store.hosts.clear();
        reconcile_host_editor(&mut editor, &mut selected, &store);
        assert!(editor.is_none());
        assert!(selected.is_none());
    }

    #[test]
    fn connection_test_timeout_allows_windows_hardware_prompts() {
        assert!(!test_timed_out(
            Duration::from_millis(9_999),
            GUI_TEST_TIMEOUT
        ));
        assert!(test_timed_out(Duration::from_secs(10), GUI_TEST_TIMEOUT));
        assert_eq!(
            gui_test_limits(GUI_INTERACTIVE_TEST_TIMEOUT).total_timeout,
            Some(GUI_INTERACTIVE_TEST_TIMEOUT)
        );

        let mut host = HostProfile {
            protocol: Protocol::Ssh,
            ssh_auth: SshAuth::PrivateKey,
            private_key_path: "id_ecdsa_sk".to_owned(),
            ..HostProfile::default()
        };
        assert_eq!(
            gui_test_timeout(&host, &[host.clone()]),
            GUI_INTERACTIVE_TEST_TIMEOUT
        );
        host.private_key_path = "id_ed25519".to_owned();
        assert_eq!(gui_test_timeout(&host, &[host.clone()]), GUI_TEST_TIMEOUT);
    }

    #[test]
    fn connection_changes_and_new_credentials_invalidate_test_results() {
        let profile = HostProfile {
            address: "127.0.0.1".to_owned(),
            ..HostProfile::default()
        };
        let mut editor = HostEditor {
            tag_input: String::new(),
            sudo: SudoDraft::default(),
            profile: profile.clone(),
            original: profile,
            password: Zeroizing::new(String::new()),
            key_passphrase: Zeroizing::new(String::new()),
            password_mode: PasswordMode::Password,
            saved_password_mode: Some(PasswordMode::Password),
            password_read_error: None,
            has_key_passphrase: false,
            key_passphrase_read_error: None,
            show_required: false,
            focus_first_missing: false,
            first_save: false,
            confirmed_persistence: None,
            draft: false,
            saving: false,
        };
        assert!(!editor.test_result_is_stale());
        editor.password.push_str("replacement");
        assert!(editor.test_result_is_stale());
        editor.password.clear();
        editor.saved_password_mode = None;
        assert!(editor.password_value_missing());
        assert!(!editor.should_store_password());
        editor.password_mode = PasswordMode::NoPassword;
        assert!(editor.should_store_password());
        assert!(editor.test_result_is_stale());
        editor.saved_password_mode = Some(PasswordMode::NoPassword);
        assert!(!editor.should_store_password());
        editor.password_mode = PasswordMode::Password;
        assert!(editor.password_value_missing());
        editor.password_read_error = Some("credential lookup failed".to_owned());
        assert!(!editor.should_store_password());
        editor.password.push_str("explicit replacement");
        assert!(editor.should_store_password());
        editor.password.clear();
        editor.profile.address = "127.0.0.2".to_owned();
        assert!(editor.test_result_is_stale());
    }

    #[test]
    fn changing_login_user_requires_its_password_or_clears_the_old_password_for_key_login() {
        let context = egui::Context::default();
        let profile = HostProfile {
            username: "alice".into(),
            ..Default::default()
        };
        let mut app = metadata_test_app(&context, "en", profile);
        let editor = app.editor.as_mut().unwrap();
        editor.saved_password_mode = Some(PasswordMode::Password);
        assert!(!editor.password_value_missing());
        editor.profile.username = " alice ".into();
        assert!(!editor.password_value_missing());
        editor.profile.username = "bob".into();
        assert!(editor.password_value_missing());
        editor.password.push_str("bob login token");
        assert!(!editor.password_value_missing());
        assert!(editor.should_store_password());
        assert!(!editor.should_clear_login_password());
        editor.password.clear();
        for auth in [SshAuth::PrivateKey, SshAuth::SshAgent] {
            editor.profile.ssh_auth = auth;
            assert!(!editor.password_value_missing());
            assert!(editor.should_clear_login_password());
            editor.profile.username = "alice".into();
            assert!(!editor.should_clear_login_password());
            editor.profile.username = "bob".into();
        }
        editor.profile.ssh_auth = SshAuth::Password;
        editor.password_mode = PasswordMode::NoPassword;
        editor.saved_password_mode = Some(PasswordMode::NoPassword);
        assert!(!editor.password_value_missing());
        assert!(editor.should_store_password());
        assert!(!editor.should_clear_login_password());
    }

    #[test]
    fn user_password_drafts_allow_passwordless_rename_without_reusing_old_secrets() {
        let context = egui::Context::default();
        let id = Uuid::new_v4();
        let mut profile = HostProfile::default();
        profile.advanced.sudo = Some(crate::model::SudoSettings {
            enabled: true,
            credentials: vec![crate::model::SudoCredential {
                id,
                username: "alice".into(),
            }],
        });
        let mut app = metadata_test_app(&context, "en", profile);
        let editor = app.editor.as_mut().unwrap();
        assert!(editor.sudo_updates().unwrap().is_empty());
        editor.profile.advanced.sudo.as_mut().unwrap().credentials[0].username = "bob".into();
        assert!(editor.sudo_updates().unwrap().is_empty());
        let (passwordless, updates) = editor.sudo_changes().unwrap();
        assert_ne!(passwordless.unwrap().credentials[0].id, id);
        assert_eq!(updates.len(), 1);
        assert!(matches!(updates[0], (CredentialKind::SudoPassword(record), None) if record == id));
        editor
            .sudo
            .passwords
            .insert(id, Zeroizing::new("  ".into()));
        assert_eq!(
            editor.sudo_updates().unwrap()[0]
                .1
                .as_ref()
                .unwrap()
                .as_str(),
            "  "
        );
        assert!(editor.has_unsaved_changes());
        let (renamed, updates) = editor.sudo_changes().unwrap();
        let replacement = renamed.unwrap().credentials[0].id;
        assert_ne!(replacement, id);
        assert!(
            updates
                .iter()
                .any(|(kind, value)| *kind == CredentialKind::SudoPassword(id) && value.is_none())
        );
        assert!(updates.iter().any(|(kind, value)| *kind
            == CredentialKind::SudoPassword(replacement)
            && value.as_ref().is_some_and(|value| value.as_str() == "  ")));
        editor.profile.advanced.sudo.as_mut().unwrap().credentials[0].username = "alice".into();
        editor.sudo.passwords.clear();
        editor.sudo.cleared.insert(id);
        assert!(editor.sudo_updates().unwrap()[0].1.is_none());
        editor.sudo.cleared.clear();
        editor
            .profile
            .advanced
            .sudo
            .as_mut()
            .unwrap()
            .credentials
            .clear();
        let updates = editor.sudo_updates().unwrap();
        assert_eq!(updates.len(), 1);
        assert!(matches!(updates[0], (CredentialKind::SudoPassword(record), None) if record == id));
    }

    #[test]
    fn committed_callback_outcomes_cannot_be_downgraded() {
        assert!(should_replace_callback("cancelled", "saved"));
        assert!(should_replace_callback("saved", "saved"));
        assert!(should_replace_callback("saved", "trusted"));
        assert!(!should_replace_callback("saved", "cancelled"));
        assert!(!should_replace_callback("trusted", "saved"));
        assert!(!should_replace_callback("trusted", "cancelled"));
    }

    #[test]
    fn repeated_host_verification_updates_last_verified_and_requires_save() {
        let id = Uuid::new_v4();
        let mut host = HostProfile {
            id,
            host_fingerprint: Some("SHA256:example".to_owned()),
            host_key_algorithm: Some("ssh-ed25519".to_owned()),
            host_key_first_seen_unix: Some(10),
            host_key_last_verified_unix: Some(20),
            verified: true,
            ..HostProfile::default()
        };
        let verified = VerifiedHostKey {
            host_id: id,
            alias: "example".to_owned(),
            fingerprint: "SHA256:example".to_owned(),
            algorithm: "ssh-ed25519".to_owned(),
            verified_at_unix: 30,
        };

        assert!(apply_verified_host_key(&mut host, &verified));
        assert_eq!(host.host_key_first_seen_unix, Some(10));
        assert_eq!(host.host_key_last_verified_unix, Some(30));
        assert!(!apply_verified_host_key(&mut host, &verified));

        let stale = VerifiedHostKey {
            verified_at_unix: 25,
            ..verified.clone()
        };
        assert!(!apply_verified_host_key(&mut host, &stale));
        assert_eq!(host.host_key_last_verified_unix, Some(30));

        let unchanged = host.clone();
        let mismatched = VerifiedHostKey {
            fingerprint: "SHA256:different".to_owned(),
            verified_at_unix: 40,
            ..verified
        };
        assert!(!apply_verified_host_key(&mut host, &mismatched));
        assert_eq!(host, unchanged);
    }

    #[test]
    fn rollback_errors_preserve_the_primary_failure() {
        assert_eq!(
            with_rollback_error("delete failed".to_owned(), None),
            "delete failed"
        );
        assert_eq!(
            with_rollback_error(
                "delete failed".to_owned(),
                Some("restore failed".to_owned())
            ),
            "delete failed; rollback failed: restore failed"
        );
    }

    #[test]
    fn batch_delete_rejects_selected_jump_host_used_by_unselected_host() {
        let jump = HostProfile::new("jump".to_owned());
        let mut target = HostProfile::new("target".to_owned());
        target.jump_host = Some(jump.id);
        let selected = HashSet::from([jump.id]);
        assert!(batch_has_external_dependents(&[jump, target], &selected));
    }

    #[test]
    fn batch_select_all_requires_every_saved_host() {
        let first = HostProfile::default();
        let second = HostProfile::default();
        assert!(!batch_all_selected(&[], &HashSet::new()));
        assert!(!batch_all_selected(
            &[first.clone(), second.clone()],
            &HashSet::from([first.id])
        ));
        assert!(batch_all_selected(
            &[first.clone(), second.clone()],
            &HashSet::from([first.id, second.id])
        ));
    }

    #[test]
    fn export_file_names_advance_without_reusing_the_default() {
        assert_eq!(export_file_name(0), "codex-hosts-export.csv");
        assert_eq!(export_file_name(1), "codex-hosts-export-1.csv");
        assert_eq!(export_file_name(2), "codex-hosts-export-2.csv");
    }

    #[test]
    fn directory_export_advances_without_overwriting() {
        let directory = std::env::temp_dir().join(format!("codex-hosts-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let original = directory.join(export_file_name(0));
        fs::write(&original, b"original").unwrap();

        let exported = write_unique_export(&directory, b"new").unwrap();
        assert_eq!(exported, directory.join(export_file_name(1)));
        assert_eq!(fs::read(&original).unwrap(), b"original");
        assert_eq!(fs::read(&exported).unwrap(), b"new");

        fs::remove_file(original).unwrap();
        fs::remove_file(exported).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn imported_credentials_follow_the_selected_authentication_method() {
        let password_host = import::ImportedHost {
            profile: HostProfile::default(),
            password: Zeroizing::new("example-password".to_owned()),
            key_passphrase: Zeroizing::new("ignored-passphrase".to_owned()),
        };
        assert_eq!(
            imported_credential(&password_host),
            Some((CredentialKind::Password, "example-password"))
        );

        let key_host = import::ImportedHost {
            profile: HostProfile {
                ssh_auth: SshAuth::PrivateKey,
                ..HostProfile::default()
            },
            password: Zeroizing::new("ignored-password".to_owned()),
            key_passphrase: Zeroizing::new("example-passphrase".to_owned()),
        };
        assert_eq!(
            imported_credential(&key_host),
            Some((CredentialKind::KeyPassphrase, "example-passphrase"))
        );

        let agent_host = import::ImportedHost {
            profile: HostProfile {
                ssh_auth: SshAuth::SshAgent,
                ..HostProfile::default()
            },
            password: Zeroizing::new("ignored-password".to_owned()),
            key_passphrase: Zeroizing::new("ignored-passphrase".to_owned()),
        };
        assert_eq!(imported_credential(&agent_host), None);
    }

    #[test]
    fn stale_close_cannot_override_restore_or_explicit_exit() {
        assert!(should_hide_to_tray(true, false, false, false, false));
        assert!(!should_hide_to_tray(true, false, false, true, false));
        assert!(!should_hide_to_tray(true, false, false, false, true));
        assert!(!should_hide_to_tray(true, false, true, false, false));
        assert!(!should_hide_to_tray(true, true, false, false, false));
    }

    fn render_texts(app: &mut HostsApp, context: &egui::Context) -> Vec<String> {
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
            texts = painted_texts(&output.shapes)
                .into_iter()
                .map(|(text, _, _)| text)
                .collect();
        }
        texts
    }

    #[test]
    fn unsaved_edits_ask_before_the_form_moves_elsewhere() {
        let context = egui::Context::default();
        theme::configure_fonts(&context, "en");
        let first = HostProfile::new("first".into());
        let second = HostProfile::new("second".into());
        let mut app = metadata_test_app(&context, "en", first.clone());
        app.store.hosts.push(second.clone());

        // Nothing typed: the form just moves.
        app.switch_editor(EditorSwitch::Select(second.id));
        assert_eq!(app.selected, Some(second.id));
        assert!(app.pending_switch.is_none());

        app.switch_editor(EditorSwitch::Select(first.id));
        app.editor.as_mut().unwrap().profile.description = "typed".into();
        for next in [
            EditorSwitch::Select(second.id),
            EditorSwitch::Test(second.id),
            EditorSwitch::Delete(second.id),
            EditorSwitch::New(host_list::HostGroup::Local),
        ] {
            app.switch_editor(next);
            assert_eq!(
                app.pending_switch,
                Some(next),
                "{next:?} waits for a choice"
            );
            assert_eq!(app.selected, Some(first.id), "{next:?} keeps the form");
            assert_eq!(app.editor.as_ref().unwrap().profile.description, "typed");
            let texts = render_texts(&mut app, &context);
            assert!(
                texts
                    .iter()
                    .any(|text| text == app.catalog.text("unsaved_switch_message")),
                "{next:?}: the dialog explains"
            );
            app.pending_switch = None;
        }
        // Clicking the open host again keeps the form and its edits.
        app.switch_editor(EditorSwitch::Select(first.id));
        assert!(app.pending_switch.is_none());
        assert_eq!(app.editor.as_ref().unwrap().profile.description, "typed");

        // Discarding goes where the user asked.
        app.switch_editor(EditorSwitch::Select(second.id));
        let next = app.pending_switch.take().unwrap();
        app.apply_switch(next);
        assert_eq!(app.selected, Some(second.id));
        assert!(!app.editor.as_ref().unwrap().has_unsaved_changes());
    }

    #[test]
    fn the_status_bar_counts_hosts_per_ready_store() {
        let context = egui::Context::default();
        theme::configure_fonts(&context, "en");
        let mut app = metadata_test_app(&context, "en", HostProfile::new("local".into()));
        for (alias, provider) in [
            ("vaulted", VaultProvider::Bitwarden),
            ("op-host", VaultProvider::OnePassword),
        ] {
            let mut host = HostProfile::new(alias.into());
            host.vault = Some(VaultHostRef {
                item_id: host.id,
                item_name: alias.into(),
                provider,
                ..Default::default()
            });
            app.store.hosts.push(host);
        }
        let part = |app: &HostsApp, key: &str| {
            app.catalog.format(
                "host_count_part",
                &[("name", app.catalog.text(key)), ("count", "1")],
            )
        };
        let texts = render_texts(&mut app, &context);
        assert!(
            texts.contains(&app.catalog.format("host_count", &[("count", "3")])),
            "nothing set up, one plain count: {texts:?}"
        );

        app.store.bitwarden.cli_path = Some(PathBuf::from("C:/bw/bw.exe"));
        app.store.bitwarden.profile = Some(crate::model::BitwardenProfile::default());
        app.store.onepassword.cli_path = Some(PathBuf::from("C:/op/op.exe"));
        app.store.onepassword.account = Some(crate::model::OnePasswordAccount::default());
        let texts = render_texts(&mut app, &context);
        let all = [
            part(&app, "host_count_local"),
            part(&app, "group_bitwarden"),
            part(&app, "group_onepassword"),
        ]
        .join(" · ");
        assert!(texts.contains(&all), "{texts:?}");

        // A service switched off is hidden and not counted.
        app.store.bitwarden.enabled = false;
        let texts = render_texts(&mut app, &context);
        let two = [
            part(&app, "host_count_local"),
            part(&app, "group_onepassword"),
        ]
        .join(" · ");
        assert!(texts.contains(&two), "{texts:?}");
        app.store.onepassword.enabled = false;
        let texts = render_texts(&mut app, &context);
        assert!(
            texts.contains(&app.catalog.format("host_count", &[("count", "1")])),
            "{texts:?}"
        );
    }
}

/// `YYYY-MM-DD HH:MM UTC` for a Unix timestamp; the store keeps UTC seconds and
/// the value only needs to be recognisable, so no time-zone crate is pulled in.
pub(super) fn format_unix_utc(stamp: u64) -> String {
    let days = stamp / 86_400;
    let seconds = stamp % 86_400;
    // Civil-from-days (Howard Hinnant), valid for any date after 1970.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        seconds / 3600,
        (seconds % 3600) / 60
    )
}

#[cfg(test)]
mod persistence_confirmation_tests {
    use super::*;

    #[test]
    fn persistence_is_confirmed_on_first_save_and_on_every_change() {
        let mut saved = HostEditor::load(HostProfile::new("box".into()));
        assert_eq!(saved.persistence_change(), None);
        saved.profile.auth_persistence = AuthPersistence::Session;
        assert_eq!(
            saved.persistence_change(),
            Some((Some(AuthPersistence::PerCall), AuthPersistence::Session))
        );
        saved.confirmed_persistence = Some(AuthPersistence::Session);
        assert_eq!(saved.persistence_change(), None);
        saved.profile.auth_persistence = AuthPersistence::Idle { minutes: 5 };
        assert!(
            saved.persistence_change().is_some(),
            "a different value asks again"
        );
        saved.profile.auth_persistence = AuthPersistence::PerCall;
        assert_eq!(
            saved.persistence_change(),
            None,
            "back to the original is no change"
        );

        let mut relaxed = HostProfile::new("kept".into());
        relaxed.auth_persistence = AuthPersistence::Session;
        let mut tightening = HostEditor::load(relaxed);
        tightening.profile.auth_persistence = AuthPersistence::PerCall;
        assert!(
            tightening.persistence_change().is_some(),
            "either direction is confirmed"
        );

        let mut fresh = HostEditor::fresh(HostProfile::new("new".into()));
        assert_eq!(
            fresh.profile.auth_persistence,
            AuthPersistence::Session,
            "a new SSH host proposes keeping the session"
        );
        assert_eq!(
            fresh.original.auth_persistence,
            AuthPersistence::PerCall,
            "the stored draft stays per-call until confirmed"
        );
        assert_eq!(
            fresh.persistence_change(),
            Some((None, AuthPersistence::Session)),
            "the first save confirms the proposed value"
        );
        fresh.profile.auth_persistence = AuthPersistence::PerCall;
        assert_eq!(
            fresh.persistence_change(),
            Some((None, AuthPersistence::PerCall)),
            "the first save confirms even the strict value"
        );
        fresh.confirmed_persistence = Some(AuthPersistence::PerCall);
        assert_eq!(fresh.persistence_change(), None);

        let mut telnet = HostProfile::new("tel".into());
        telnet.protocol = Protocol::Telnet;
        let fresh_telnet = HostEditor::fresh(telnet.clone());
        assert_eq!(
            fresh_telnet.profile.auth_persistence,
            AuthPersistence::PerCall
        );
        let mut telnet = HostEditor::load(telnet);
        telnet.profile.auth_persistence = AuthPersistence::Session;
        assert_eq!(telnet.persistence_change(), None);
    }
}

#[cfg(test)]
mod time_tests {
    #[test]
    fn unix_timestamps_render_as_utc_dates() {
        assert_eq!(super::format_unix_utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(
            super::format_unix_utc(1_758_500_000),
            "2025-09-22 00:13 UTC"
        );
    }
}
