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

mod dialogs;
mod host_form;
mod host_list;
mod status;
mod theme;
mod toolbar;

pub(crate) use theme::configure_fonts;

use status::{StatusKind, StatusMessage};
use theme::apply_style;

use crate::connection;
use crate::credentials::{self, CredentialKind};
use crate::fido::{self, FidoKeyInfo};
use crate::i18n::Catalog;
use crate::import;
use crate::model::{
    AuthPersistence, HostFilter, HostProfile, Prefill, Protocol, SshAuth, normalize_tags,
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
    /// False for a host created in this session until the user picks an
    /// authentication-persistence option; saved profiles always carry one.
    persistence_chosen: bool,
    /// Set by a failed save so empty required fields are highlighted.
    show_required: bool,
    /// Set by a failed save; the next frame moves focus to the first empty required field.
    focus_first_missing: bool,
    /// True until a host created in this session is saved once: its first
    /// explicit persistence choice needs no further confirmation.
    first_save: bool,
    /// The persistence value the user confirmed in the change dialog; a
    /// different value in the form asks again.
    confirmed_persistence: Option<AuthPersistence>,
}

/// The action that was waiting on the persistence-change confirmation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PendingAction {
    Save,
    Test,
    TestAll,
}

struct PersistencePrompt {
    alias: String,
    from: AuthPersistence,
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
        let (saved_password_mode, password_read_error) =
            match credentials::load(profile.id, CredentialKind::Password) {
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
            };
        let (has_key_passphrase, key_passphrase_read_error) =
            match credentials::has(profile.id, CredentialKind::KeyPassphrase) {
                Ok(has_key_passphrase) => (has_key_passphrase, None),
                Err(error) => (false, Some(error.to_string())),
            };
        Self {
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
            persistence_chosen: true,
            show_required: false,
            focus_first_missing: false,
            first_save: false,
            confirmed_persistence: None,
        }
    }

    fn fresh(profile: HostProfile) -> Self {
        Self {
            persistence_chosen: false,
            first_save: true,
            ..Self::load(profile)
        }
    }

    /// Authentication persistence is the one relaxation of the per-call
    /// guarantee, so changing it on a saved SSH host is confirmed once, in
    /// either direction, before anything is written.
    fn persistence_change(&self) -> Option<(AuthPersistence, AuthPersistence)> {
        if self.first_save || self.profile.protocol != Protocol::Ssh {
            return None;
        }
        let (from, to) = (
            self.original.auth_persistence,
            self.profile.auth_persistence,
        );
        (from != to && self.confirmed_persistence != Some(to)).then_some((from, to))
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
        if profile.protocol == Protocol::Ssh && !self.persistence_chosen {
            missing.push("auth_persistence");
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
            || self.password_mode != self.saved_password_mode.unwrap_or(PasswordMode::Password)
    }

    fn matches_stored_original(&self, store: &HostStore) -> bool {
        store.hosts.iter().any(|host| host == &self.original)
    }

    fn needs_password(&self) -> bool {
        self.profile.protocol == Protocol::Telnet || self.profile.ssh_auth == SshAuth::Password
    }

    fn should_store_password(&self) -> bool {
        if !self.needs_password() {
            return false;
        }
        match self.password_mode {
            PasswordMode::Password => !self.password.is_empty(),
            PasswordMode::NoPassword => self.saved_password_mode != Some(PasswordMode::NoPassword),
        }
    }

    fn should_store_key_passphrase(&self) -> bool {
        self.profile.protocol == Protocol::Ssh
            && self.profile.ssh_auth == SshAuth::PrivateKey
            && !self.key_passphrase.is_empty()
    }

    fn password_value_missing(&self) -> bool {
        self.needs_password()
            && self.password_mode == PasswordMode::Password
            && self.password.is_empty()
            && self.saved_password_mode != Some(PasswordMode::Password)
    }

    fn test_result_is_stale(&self) -> bool {
        self.connection_changed()
            || self.should_store_password()
            || self.should_store_key_passphrase()
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
    Test,
    CancelCodex,
    BrowsePrivateKey,
    DiscoverFido,
    OpenFidoSetup,
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
        }
        let tray = if launch.codex_edit {
            None
        } else {
            crate::tray::Tray::new(context.egui_ctx.clone(), catalog.clone()).ok()
        };
        let tray_available = tray.is_some();
        Ok(Self {
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
        })
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
        reconcile_host_editor(&mut self.editor, &mut self.selected, &fresh);
        self.store = fresh;
        self.batch_selected
            .retain(|id| self.store.hosts.iter().any(|host| host.id == *id));
        self.test_states.clear();
        self.fingerprint_prompt = None;
        self.delete_prompt = false;
        self.persistence_prompt = None;
        self.batch_delete_prompt = false;
        self.set_status_key("hosts_refreshed");
        Ok(())
    }

    fn new_host(&mut self) {
        let profile = HostProfile::new(self.store.next_neutral_alias());
        let id = profile.id;
        self.store.hosts.push(profile.clone());
        if let Err(error) = self.store.save() {
            self.store.hosts.retain(|host| host.id != id);
            self.set_status_format("storage_error", &[("error", &error.to_string())]);
            return;
        }
        self.selected = Some(id);
        self.editor = Some(HostEditor::fresh(profile));
        self.scroll_to_selected = true;
    }

    fn persist_editor(&mut self) -> Result<(), String> {
        self.refresh_hosts()?;
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| !editor.matches_stored_original(&self.store))
        {
            return Err(self.catalog.text("host_changed").to_owned());
        }
        let editor = self.editor.as_mut().ok_or_else(|| "NO_EDITOR".to_owned())?;
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
        let credential_update = if store_password {
            Some((
                CredentialKind::Password,
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
        let id = editor.profile.id;
        let mut profile = editor.profile.clone();
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
        if invalidate_test_state {
            profile.verified = false;
        }
        if profile.protocol == Protocol::Telnet {
            profile.jump_host = None;
            profile.host_fingerprint = None;
            profile.host_key_algorithm = None;
            profile.host_key_first_seen_unix = None;
            profile.host_key_last_verified_unix = None;
        }

        let mut updated_store = self.store.clone();
        let stored = updated_store
            .hosts
            .iter_mut()
            .find(|host| host.id == id)
            .ok_or_else(|| "HOST_NOT_FOUND".to_owned())?;
        stored.clone_from(&profile);

        if let Some((kind, secret)) = credential_update.as_ref() {
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
        editor.first_save = false;
        editor.confirmed_persistence = None;
        editor.tag_input.clear();
        if store_password {
            editor.saved_password_mode = Some(editor.password_mode);
            editor.password_read_error = None;
            editor.password.clear();
        }
        if store_key_passphrase {
            editor.has_key_passphrase = true;
            editor.key_passphrase_read_error = None;
            editor.key_passphrase.clear();
        }
        Ok(())
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
            editor.profile.auth_persistence = editor.original.auth_persistence;
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
            Ok(()) => {
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
        if let Err(error) = self.persist_editor() {
            self.set_status(StatusKind::Error, error);
            return;
        }
        let Some(id) = self.editor.as_ref().map(|editor| editor.profile.id) else {
            return;
        };
        self.start_test_host(id);
        self.set_status_key("testing");
    }

    fn start_test_host(&mut self, id: Uuid) {
        if self.testing_all
            || self.test_operations.contains_key(&id)
            || self.pending_tests.contains(&id)
        {
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
        }) && let Err(error) = self.persist_editor()
        {
            self.set_status(StatusKind::Error, error);
            return;
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
        let credential_snapshot = match credentials::snapshot(id) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.set_status_format("credential_error", &[("error", &error.to_string())]);
                return;
            }
        };
        let credential_snapshots = vec![(id, credential_snapshot)];
        if let Err(error) = credentials::delete_all(id) {
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
        let credential_snapshots = match ids
            .iter()
            .map(|id| credentials::snapshot(*id).map(|snapshot| (*id, snapshot)))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(snapshots) => snapshots,
            Err(error) => {
                self.set_status_format("credential_error", &[("error", &error.to_string())]);
                return;
            }
        };
        for id in &ids {
            if let Err(error) = credentials::delete_all(*id) {
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
            .map(|item| item.profile.id)
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
        if let Some(temporary) = &mut self.temporary {
            if open_secrets {
                temporary.visible = true;
            }
            restore |= temporary.logic(context);
        }
        if restore && !self.exiting {
            self.host_refresh_pending = true;
            self.hidden = false;
            context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            context.send_viewport_cmd(egui::ViewportCommand::Focus);
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
    }
}

fn rollback_import_credentials(ids: &[Uuid]) {
    for id in ids {
        let _ = credentials::delete_all(*id);
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
            .then_some((CredentialKind::Password, item.password.as_str())),
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

    pub(super) fn metadata_test_app(
        context: &egui::Context,
        locale: &str,
        profile: HostProfile,
    ) -> HostsApp {
        let editor = HostEditor {
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
            persistence_chosen: true,
            show_required: false,
            focus_first_missing: false,
            first_save: false,
            confirmed_persistence: None,
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
        let toolbar_y = find("import_hosts").center().y;
        for key in ["test_all", "batch_manage", "tray_exit", "language"] {
            let rect = find(key);
            assert!(
                (rect.center().y - toolbar_y).abs() < 4.0,
                "{label}: toolbar control {key} wrapped to another row"
            );
        }
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
            .find(|(text, _, _)| text.starts_with(app.catalog.text("nav_title")))
            .expect("host list title");
        assert!(list_title.1.left() < size[0] * 0.3);
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
            profile: profile.clone(),
            original: profile,
            password: Zeroizing::new(String::new()),
            key_passphrase: Zeroizing::new(String::new()),
            password_mode: PasswordMode::Password,
            saved_password_mode: Some(PasswordMode::Password),
            password_read_error: None,
            has_key_passphrase: false,
            key_passphrase_read_error: None,
            persistence_chosen: true,
            show_required: false,
            focus_first_missing: false,
            first_save: false,
            confirmed_persistence: None,
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
    fn only_a_changed_value_on_a_saved_ssh_host_needs_confirmation() {
        let mut saved = HostEditor::load(HostProfile::new("box".into()));
        assert_eq!(saved.persistence_change(), None);
        saved.profile.auth_persistence = AuthPersistence::Session;
        assert_eq!(
            saved.persistence_change(),
            Some((AuthPersistence::PerCall, AuthPersistence::Session))
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
        fresh.profile.auth_persistence = AuthPersistence::Session;
        assert_eq!(
            fresh.persistence_change(),
            None,
            "the first explicit choice is the confirmation"
        );

        let mut telnet = HostProfile::new("tel".into());
        telnet.protocol = Protocol::Telnet;
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
