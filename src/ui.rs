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

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, RichText};
use serde::Serialize;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::connection;
use crate::credentials::{self, CredentialKind};
use crate::fido::{self, FidoKeyInfo};
use crate::i18n::Catalog;
use crate::import;
use crate::model::{HostProfile, Prefill, Protocol, SshAuth, can_use_as_jump};
use crate::ssh::{
    OperationLimits, RemoteFailure, RemoteResult, TOTAL_TIMEOUT_CODE, VerifiedHostKey,
};
use crate::storage::HostStore;

#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    pub codex_edit: bool,
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
    profile: HostProfile,
    original: HostProfile,
    password: Zeroizing<String>,
    key_passphrase: Zeroizing<String>,
    password_mode: PasswordMode,
    saved_password_mode: Option<PasswordMode>,
    password_read_error: Option<String>,
    has_key_passphrase: bool,
    key_passphrase_read_error: Option<String>,
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
            profile,
            password: Zeroizing::new(String::new()),
            key_passphrase: Zeroizing::new(String::new()),
            password_mode: saved_password_mode.unwrap_or(PasswordMode::Password),
            saved_password_mode,
            password_read_error,
            has_key_passphrase,
            key_passphrase_read_error,
        }
    }

    fn connection_changed(&self) -> bool {
        !self.profile.connection_details_equal(&self.original)
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
const TEST_SUCCESS_FILL: Color32 = Color32::from_rgb(36, 105, 67);
const TEST_FAILURE_FILL: Color32 = Color32::from_rgb(132, 48, 53);
const SELECTED_ROW_STROKE: Color32 = Color32::from_rgb(230, 230, 230);

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
    store: HostStore,
    catalog: Catalog,
    selected: Option<Uuid>,
    editor: Option<HostEditor>,
    status: String,
    test_operations: HashMap<Uuid, TestOperation>,
    pending_tests: VecDeque<Uuid>,
    test_hosts_snapshot: Option<Arc<Vec<HostProfile>>>,
    test_states: HashMap<Uuid, HostTestState>,
    testing_all: bool,
    test_store_dirty: bool,
    repaint_context: egui::Context,
    fingerprint_prompt: Option<FingerprintPrompt>,
    delete_prompt: bool,
    import_window_open: bool,
    import_cleanup_prompt: Option<ImportCleanupPrompt>,
    fido_setup_prompt: Option<FidoSetupPrompt>,
    batch_mode: bool,
    batch_selected: HashSet<Uuid>,
    batch_delete_prompt: bool,
    batch_export_window_open: bool,
    launch: LaunchOptions,
    callback_written: bool,
    pending_callback: Option<PendingCallback>,
}

impl HostsApp {
    pub fn new(context: &eframe::CreationContext<'_>, launch: LaunchOptions) -> Self {
        context.egui_ctx.set_zoom_factor(1.06);

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
                    Ok(()) => selected = Some(id),
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
            .map(HostEditor::load);
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
            catalog.format("storage_error", &[("error", &error)])
        } else if launch.codex_edit {
            catalog.text("draft_waiting").to_owned()
        } else {
            catalog.text("status_ready").to_owned()
        };
        Self {
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
            import_window_open: false,
            import_cleanup_prompt: None,
            fido_setup_prompt: None,
            batch_mode: false,
            batch_selected: HashSet::new(),
            batch_delete_prompt: false,
            batch_export_window_open: false,
            launch,
            callback_written: false,
            pending_callback: None,
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
        self.status = self.catalog.text("status_ready").to_owned();
    }

    fn new_host(&mut self) {
        let profile = HostProfile::new(self.store.next_neutral_alias());
        let id = profile.id;
        self.store.hosts.push(profile.clone());
        if let Err(error) = self.store.save() {
            self.store.hosts.retain(|host| host.id != id);
            self.status = self
                .catalog
                .format("storage_error", &[("error", &error.to_string())]);
            return;
        }
        self.selected = Some(id);
        self.editor = Some(HostEditor::load(profile));
    }

    fn persist_editor(&mut self) -> Result<(), String> {
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
                            .save_recovery_baseline()
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

    fn save(&mut self, context: &egui::Context) {
        match self.persist_editor() {
            Ok(()) => {
                self.status = self.catalog.text("status_saved").to_owned();
                if self.launch.codex_edit {
                    let alias = self
                        .editor
                        .as_ref()
                        .map(|editor| editor.profile.alias.clone())
                        .unwrap_or_default();
                    match self.write_callback("saved", Some(&alias)) {
                        Ok(()) => context.send_viewport_cmd(egui::ViewportCommand::Close),
                        Err(error) => {
                            self.status =
                                self.catalog.format("callback_error", &[("error", &error)])
                        }
                    }
                }
            }
            Err(error) => self.status = error,
        }
    }

    fn start_test(&mut self) {
        if let Err(error) = self.persist_editor() {
            self.status = error;
            return;
        }
        let Some(id) = self.editor.as_ref().map(|editor| editor.profile.id) else {
            return;
        };
        self.start_test_host(id);
        self.status = self.catalog.text("testing").to_owned();
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
        if self.editor.as_ref().is_some_and(|editor| {
            editor.profile != editor.original
                || editor.should_store_password()
                || editor.should_store_key_passphrase()
                || editor.password_value_missing()
        }) && let Err(error) = self.persist_editor()
        {
            self.status = error;
            return;
        }
        let ids = self
            .store
            .hosts
            .iter()
            .map(|host| host.id)
            .collect::<Vec<_>>();
        if ids.is_empty() {
            self.status = self.catalog.text("test_all_empty").to_owned();
            return;
        }
        self.test_states.clear();
        self.test_states
            .extend(ids.iter().copied().map(|id| (id, HostTestState::Testing)));
        self.pending_tests = ids.into_iter().collect();
        self.test_hosts_snapshot = Some(Arc::new(self.store.hosts.clone()));
        self.testing_all = true;
        self.start_pending_tests();
        self.status = self.catalog.text("testing_all").to_owned();
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
                    self.status = self
                        .catalog
                        .format("storage_error", &[("error", &error.to_string())]);
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
            self.status = self.catalog.format(
                "status_test_all_done",
                &[("succeeded", &succeeded), ("failed", &failed)],
            );
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
                        self.status = self
                            .catalog
                            .format("storage_error", &[("error", &error.to_string())]);
                        return;
                    }
                    self.store = updated_store;
                    self.sync_editor_verification_from_store(id);
                }
                if !self.testing_all && self.selected == Some(id) {
                    self.status = self
                        .catalog
                        .format("status_test_ok", &[("identity", identity.as_str())]);
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
                    self.status = if matches!(error.code, "TEST_TIMEOUT" | TOTAL_TIMEOUT_CODE) {
                        self.catalog.text("status_test_timeout").to_owned()
                    } else {
                        self.catalog
                            .format("status_test_failed", &[("error", error.code)])
                    };
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
            self.status = self.catalog.text("status_cancelled").to_owned();
            if prompt.close_after_choice {
                match self.write_callback("cancelled", Some(&prompt.alias)) {
                    Ok(()) => context.send_viewport_cmd(egui::ViewportCommand::Close),
                    Err(error) => {
                        self.status = self.catalog.format("callback_error", &[("error", &error)]);
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
            self.status = self
                .catalog
                .format("storage_error", &[("error", "HOST_NOT_FOUND")]);
            return;
        };
        host.host_fingerprint = Some(prompt.observed.clone());
        host.host_key_algorithm = prompt.observed_algorithm.clone();
        host.host_key_first_seen_unix = None;
        host.host_key_last_verified_unix = None;
        host.verified = false;
        if let Err(error) = updated_store.save() {
            self.status = self
                .catalog
                .format("storage_error", &[("error", &error.to_string())]);
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
                    self.status = self.catalog.format("callback_error", &[("error", &error)]);
                }
            }
        } else if prompt.retry_test {
            self.start_test_host(prompt.host_id);
            if self.selected == Some(prompt.host_id) {
                self.status = self.catalog.text("testing").to_owned();
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
            self.status = self.catalog.text("chain_in_use").to_owned();
            return;
        }
        let credential_snapshot = match credentials::snapshot(id) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.status = self
                    .catalog
                    .format("credential_error", &[("error", &error.to_string())]);
                return;
            }
        };
        let credential_snapshots = vec![(id, credential_snapshot)];
        if let Err(error) = credentials::delete_all(id) {
            let error = with_rollback_error(
                error.to_string(),
                restore_credential_snapshots(&credential_snapshots).err(),
            );
            self.status = self
                .catalog
                .format("credential_error", &[("error", &error)]);
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
            self.status = self.catalog.format("storage_error", &[("error", &error)]);
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
        self.status = self.catalog.text("batch_select_hint").to_owned();
    }

    fn toggle_batch_selection(&mut self) {
        if batch_all_selected(&self.store.hosts, &self.batch_selected) {
            self.batch_selected.clear();
        } else {
            self.batch_selected = self.store.hosts.iter().map(|host| host.id).collect();
        }
    }

    fn cancel_batch_mode(&mut self) {
        self.batch_mode = false;
        self.batch_selected.clear();
        self.batch_delete_prompt = false;
        self.batch_export_window_open = false;
        self.status = self.catalog.text("status_ready").to_owned();
    }

    fn request_batch_delete(&mut self) {
        if self.batch_selected.is_empty() {
            self.status = self.catalog.text("batch_nothing_selected").to_owned();
            return;
        }
        if batch_has_external_dependents(&self.store.hosts, &self.batch_selected) {
            self.status = self.catalog.text("chain_in_use").to_owned();
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
                self.status = self
                    .catalog
                    .format("credential_error", &[("error", &error.to_string())]);
                return;
            }
        };
        for id in &ids {
            if let Err(error) = credentials::delete_all(*id) {
                let error = with_rollback_error(
                    error.to_string(),
                    restore_credential_snapshots(&credential_snapshots).err(),
                );
                self.status = self
                    .catalog
                    .format("credential_error", &[("error", &error)]);
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
            self.status = self.catalog.format("storage_error", &[("error", &error)]);
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
        self.status = self.catalog.text("batch_deleted").to_owned();
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
                self.status = self
                    .catalog
                    .format("template_saved", &[("path", path.as_str())]);
            }
            Err(error) => {
                self.status = self
                    .catalog
                    .format("template_save_failed", &[("error", &error.to_string())]);
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
                self.status = self
                    .catalog
                    .format("import_failed", &[("error", error.as_str())]);
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
                self.status = self
                    .catalog
                    .format("credential_error", &[("error", &error.to_string())]);
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
            self.status = self
                .catalog
                .format("storage_error", &[("error", &error.to_string())]);
            return;
        }
        if let Some(id) = first_id {
            self.select(id);
        }
        self.import_window_open = false;
        if contains_sensitive_values {
            self.status = self.catalog.format(
                "import_succeeded_with_credentials",
                &[("count", &count.to_string())],
            );
            self.import_cleanup_prompt = Some(ImportCleanupPrompt {
                path,
                imported_count: count,
            });
        } else {
            self.status = self
                .catalog
                .format("import_succeeded", &[("count", &count.to_string())]);
        }
    }

    fn request_batch_export(&mut self) {
        if self.batch_selected.is_empty() {
            self.status = self.catalog.text("batch_nothing_selected").to_owned();
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
                self.status = self
                    .catalog
                    .format("batch_export_succeeded", &[("path", path.as_str())]);
                self.batch_export_window_open = false;
                self.batch_mode = false;
                self.batch_selected.clear();
            }
            Err(error) => {
                self.status = self
                    .catalog
                    .format("batch_export_failed", &[("error", error.as_str())]);
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

    fn top_bar(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        let mut open_import = false;
        let mut test_all = false;
        let mut begin_batch = false;
        let mut toggle_batch_selection = false;
        let mut delete_batch = false;
        let mut export_batch = false;
        let mut cancel_batch = false;
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 48.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add_space(18.0);
                if self.batch_mode {
                    let all_selected = batch_all_selected(&self.store.hosts, &self.batch_selected);
                    if ui
                        .add_enabled(
                            !self.store.hosts.is_empty(),
                            egui::Button::new(self.catalog.text(if all_selected {
                                "deselect_all"
                            } else {
                                "select_all"
                            }))
                            .min_size([92.0, 34.0].into()),
                        )
                        .clicked()
                    {
                        toggle_batch_selection = true;
                    }
                    if ui
                        .add(
                            egui::Button::new(self.catalog.text("delete"))
                                .min_size([92.0, 34.0].into()),
                        )
                        .clicked()
                    {
                        delete_batch = true;
                    }
                    if ui
                        .add(
                            egui::Button::new(self.catalog.text("export"))
                                .min_size([92.0, 34.0].into()),
                        )
                        .clicked()
                    {
                        export_batch = true;
                    }
                    if ui
                        .add(
                            egui::Button::new(self.catalog.text("cancel"))
                                .min_size([92.0, 34.0].into()),
                        )
                        .clicked()
                    {
                        cancel_batch = true;
                    }
                } else if !self.launch.codex_edit {
                    if ui
                        .add(
                            egui::Button::new(self.catalog.text("import_hosts"))
                                .min_size([112.0, 34.0].into()),
                        )
                        .clicked()
                    {
                        open_import = true;
                    }
                    if ui
                        .add_enabled(
                            self.tests_idle(),
                            egui::Button::new(self.catalog.text("test_all"))
                                .min_size([112.0, 34.0].into()),
                        )
                        .clicked()
                    {
                        test_all = true;
                    }
                    if ui
                        .add(
                            egui::Button::new(self.catalog.text("batch_manage"))
                                .min_size([112.0, 34.0].into()),
                        )
                        .clicked()
                    {
                        begin_batch = true;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(18.0);
                    let current = self.catalog.locale().to_owned();
                    let selected_name = Catalog::available()
                        .iter()
                        .find(|language| language.locale == current)
                        .map(|language| language.display_name)
                        .unwrap_or(current.as_str());
                    egui::ComboBox::from_id_salt("language_selector")
                        .selected_text(selected_name)
                        .width(130.0)
                        .show_ui(ui, |ui| {
                            for language in Catalog::available() {
                                if ui
                                    .selectable_label(
                                        language.locale == current,
                                        language.display_name,
                                    )
                                    .clicked()
                                {
                                    self.catalog = Catalog::for_locale(Some(language.locale));
                                    configure_fonts(context, language.locale);
                                    self.store.preferred_locale = Some(language.locale.to_owned());
                                    context.send_viewport_cmd(egui::ViewportCommand::Title(
                                        self.catalog.text("app_title").to_owned(),
                                    ));
                                    self.status = match self.store.save() {
                                        Ok(()) => self.catalog.text("status_ready").to_owned(),
                                        Err(error) => self.catalog.format(
                                            "storage_error",
                                            &[("error", &error.to_string())],
                                        ),
                                    };
                                }
                            }
                        });
                    ui.label(self.catalog.text("language"));
                });
            },
        );
        if open_import {
            self.import_window_open = true;
        }
        if test_all {
            self.start_all_tests();
        }
        if begin_batch {
            self.begin_batch_mode();
        }
        if toggle_batch_selection {
            self.toggle_batch_selection();
        }
        if delete_batch {
            self.request_batch_delete();
        }
        if export_batch {
            self.request_batch_export();
        }
        if cancel_batch {
            self.cancel_batch_mode();
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let sidebar_width = ui.available_width();
        ui.set_width(sidebar_width);
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.vertical(|ui| {
                ui.set_width((sidebar_width - 32.0).max(0.0));
                ui.heading(self.catalog.text("nav_title"));
                ui.add_space(4.0);
                ui.label(
                    RichText::new(self.catalog.text("nav_subtitle"))
                        .color(ui.visuals().weak_text_color()),
                );
                ui.add_space(16.0);
                if ui
                    .add_sized(
                        [ui.available_width(), 38.0],
                        egui::Button::new(format!("＋ {}", self.catalog.text("new_host"))),
                    )
                    .clicked()
                {
                    self.new_host();
                }
            });
        });
        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);

        let hosts = &self.store.hosts;
        let selected_id = self.selected;
        let batch_mode = self.batch_mode;
        let batch_selected = &self.batch_selected;
        let test_states = &self.test_states;
        let delete_label = self.catalog.text("delete").to_owned();
        let mut selection_request = None;
        let mut deletion_request = None;
        let mut batch_toggle_request = None;
        ui.spacing_mut().item_spacing.y = 4.0;
        egui::ScrollArea::vertical()
            .id_salt("host_list_scroll")
            .auto_shrink([false, false])
            .show_rows(ui, 62.0, hosts.len(), |ui, row_range| {
                ui.set_width(ui.available_width());
                for row in row_range {
                    let host = &hosts[row];
                    let id = host.id;
                    let test_state = test_states.get(&id).copied();
                    let selected = selected_id == Some(id);
                    let response = ui
                        .horizontal(|ui| {
                            if batch_mode {
                                let mut checked = batch_selected.contains(&id);
                                if ui.checkbox(&mut checked, "").changed() {
                                    batch_toggle_request = Some((id, checked));
                                }
                            }
                            let verified_marker = if test_state == Some(HostTestState::Succeeded)
                                || (test_state.is_none() && host.verified)
                            {
                                "  ✓"
                            } else if test_state == Some(HostTestState::Testing) {
                                "  …"
                            } else {
                                ""
                            };
                            let text = RichText::new(format!(
                                "{}\n{} · {}:{}{}",
                                host.alias,
                                host.protocol.stable_name().to_ascii_uppercase(),
                                host.address,
                                host.port,
                                verified_marker
                            ))
                            .line_height(Some(20.0));
                            let mut button = egui::Button::new(()).left_text(text);
                            if let Some(fill) = host_row_fill(test_state) {
                                button = button.fill(fill);
                            }
                            if selected {
                                button = button.stroke(egui::Stroke::new(2.0, SELECTED_ROW_STROKE));
                            }
                            let size = [ui.available_width(), 62.0];
                            ui.scope(|ui| {
                                ui.spacing_mut().button_padding.x = 12.0;
                                ui.add_sized(size, button)
                            })
                            .inner
                        })
                        .inner;
                    if response.clicked() || response.secondary_clicked() {
                        if batch_mode {
                            batch_toggle_request = Some((id, !batch_selected.contains(&id)));
                        } else {
                            selection_request = Some(id);
                        }
                    }
                    if !batch_mode {
                        response.context_menu(|ui| {
                            if ui.button(&delete_label).clicked() {
                                deletion_request = Some(id);
                                ui.close();
                            }
                        });
                    }
                }
            });

        if let Some((id, checked)) = batch_toggle_request {
            if checked {
                self.batch_selected.insert(id);
            } else {
                self.batch_selected.remove(&id);
            }
        } else if let Some(id) = deletion_request {
            self.select(id);
            self.delete_prompt = true;
        } else if let Some(id) = selection_request {
            self.select(id);
        }
    }

    fn editor_panel(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        let catalog = self.catalog.clone();
        let hosts = self.store.hosts.clone();
        let testing = self.testing_all
            || self.selected.is_some_and(|id| {
                self.test_operations.contains_key(&id) || self.pending_tests.contains(&id)
            });
        let codex_edit = self.launch.codex_edit;
        let mut action = None;
        let editor_scroll = egui::ScrollArea::vertical().id_salt("host_editor_scroll");
        editor_scroll.show(ui, |ui| {
            ui.set_max_width(760.0);
            ui.add_space(24.0);
            let Some(editor) = self.editor.as_mut() else {
                ui.heading(catalog.text("editor_new_title"));
                return;
            };
            ui.heading(if editor.original.alias.is_empty() {
                catalog.text("editor_new_title")
            } else {
                catalog.text("editor_edit_title")
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(catalog.text("editor_subtitle"))
                    .color(ui.visuals().weak_text_color()),
            );
            if codex_edit {
                ui.add_space(12.0);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.label(RichText::new(catalog.text("codex_draft")).strong());
                    ui.label(catalog.text("secret_not_exported"));
                });
            }
            ui.add_space(18.0);
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::same(18))
                .show(ui, |ui| {
                    egui::Grid::new("host_form")
                        .num_columns(2)
                        .min_col_width(160.0)
                        .spacing([24.0, 14.0])
                        .show(ui, |ui| {
                            form_label(ui, catalog.text("alias"));
                            ui.add(
                                egui::TextEdit::singleline(&mut editor.profile.alias)
                                    .desired_width(420.0),
                            );
                            ui.end_row();

                            form_label(ui, catalog.text("address"));
                            ui.add(
                                egui::TextEdit::singleline(&mut editor.profile.address)
                                    .desired_width(420.0),
                            );
                            ui.end_row();

                            form_label(ui, catalog.text("port"));
                            ui.add(egui::DragValue::new(&mut editor.profile.port).range(1..=65535));
                            ui.end_row();

                            form_label(ui, catalog.text("username"));
                            ui.add(
                                egui::TextEdit::singleline(&mut editor.profile.username)
                                    .desired_width(420.0),
                            );
                            ui.end_row();

                            form_label(ui, catalog.text("protocol"));
                            let previous_protocol = editor.profile.protocol;
                            ui.horizontal(|ui| {
                                ui.selectable_value(
                                    &mut editor.profile.protocol,
                                    Protocol::Ssh,
                                    catalog.text("ssh"),
                                );
                                ui.selectable_value(
                                    &mut editor.profile.protocol,
                                    Protocol::Telnet,
                                    catalog.text("telnet"),
                                );
                            });
                            if previous_protocol != editor.profile.protocol {
                                if editor.profile.port == previous_protocol.default_port() {
                                    editor.profile.port = editor.profile.protocol.default_port();
                                }
                                if editor.profile.protocol == Protocol::Telnet {
                                    editor.profile.jump_host = None;
                                }
                            }
                            ui.end_row();

                            if editor.profile.protocol == Protocol::Ssh {
                                form_label(ui, catalog.text("auth_method"));
                                egui::ComboBox::from_id_salt("ssh_auth")
                                    .selected_text(match editor.profile.ssh_auth {
                                        SshAuth::Password => catalog.text("password_auth"),
                                        SshAuth::PrivateKey => catalog.text("private_key_auth"),
                                        SshAuth::SshAgent => catalog.text("ssh_agent_auth"),
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut editor.profile.ssh_auth,
                                            SshAuth::Password,
                                            catalog.text("password_auth"),
                                        );
                                        ui.selectable_value(
                                            &mut editor.profile.ssh_auth,
                                            SshAuth::PrivateKey,
                                            catalog.text("private_key_auth"),
                                        );
                                        ui.selectable_value(
                                            &mut editor.profile.ssh_auth,
                                            SshAuth::SshAgent,
                                            catalog.text("ssh_agent_auth"),
                                        );
                                    });
                                ui.end_row();
                            }

                            if editor.profile.protocol == Protocol::Telnet
                                || editor.profile.ssh_auth == SshAuth::Password
                            {
                                form_label_with_hint(ui, catalog.text("password_mode"));
                                ui.horizontal(|ui| {
                                    ui.radio_value(
                                        &mut editor.password_mode,
                                        PasswordMode::Password,
                                        catalog.text("password"),
                                    );
                                    ui.radio_value(
                                        &mut editor.password_mode,
                                        PasswordMode::NoPassword,
                                        catalog.text("no_password"),
                                    );
                                });
                                ui.end_row();

                                form_label_with_hint(ui, catalog.text("password"));
                                ui.vertical(|ui| {
                                    ui.add_enabled(
                                        editor.password_mode == PasswordMode::Password,
                                        egui::TextEdit::singleline(&mut *editor.password)
                                            .password(true)
                                            .desired_width(420.0),
                                    );
                                    let password_hint =
                                        if editor.password_mode == PasswordMode::NoPassword {
                                            catalog.text("no_password_hint").to_owned()
                                        } else if let Some(error) =
                                            editor.password_read_error.as_deref()
                                        {
                                            catalog.format("credential_error", &[("error", error)])
                                        } else if editor.saved_password_mode
                                            == Some(PasswordMode::Password)
                                        {
                                            catalog.text("password_saved").to_owned()
                                        } else {
                                            catalog.text("password_required").to_owned()
                                        };
                                    ui.small(password_hint);
                                });
                                ui.end_row();
                            } else if editor.profile.ssh_auth == SshAuth::PrivateKey {
                                form_label_with_hint(ui, catalog.text("private_key"));
                                ui.vertical(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(
                                            &mut editor.profile.private_key_path,
                                        )
                                        .desired_width(420.0),
                                    );
                                    ui.small(catalog.text("private_key_hint"));
                                    ui.horizontal_wrapped(|ui| {
                                        if ui.button(catalog.text("browse_private_key")).clicked() {
                                            action = Some(EditorAction::BrowsePrivateKey);
                                        }
                                        if ui.button(catalog.text("find_fido_key")).clicked() {
                                            action = Some(EditorAction::DiscoverFido);
                                        }
                                        if ui.button(catalog.text("setup_fido_key")).clicked() {
                                            action = Some(EditorAction::OpenFidoSetup);
                                        }
                                    });
                                    ui.small(catalog.text("fido_direct_hint"));
                                });
                                ui.end_row();

                                form_label_with_hint(ui, catalog.text("key_passphrase"));
                                ui.vertical(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut *editor.key_passphrase)
                                            .password(true)
                                            .desired_width(420.0),
                                    );
                                    let passphrase_hint = if let Some(error) =
                                        editor.key_passphrase_read_error.as_deref()
                                    {
                                        catalog.format("credential_error", &[("error", error)])
                                    } else if editor.has_key_passphrase {
                                        catalog.text("passphrase_saved").to_owned()
                                    } else {
                                        catalog.text("passphrase_optional").to_owned()
                                    };
                                    ui.small(passphrase_hint);
                                });
                                ui.end_row();
                            } else {
                                form_label_with_hint(ui, catalog.text("agent_key_fingerprint"));
                                ui.vertical(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(
                                            &mut editor.profile.agent_key_fingerprint,
                                        )
                                        .desired_width(420.0),
                                    );
                                    ui.small(catalog.text("agent_key_hint"));
                                });
                                ui.end_row();
                            }

                            if editor.profile.protocol == Protocol::Ssh {
                                form_label_with_hint(ui, catalog.text("host_chain"));
                                ui.vertical(|ui| {
                                    let selected_name = editor
                                        .profile
                                        .jump_host
                                        .and_then(|id| hosts.iter().find(|host| host.id == id))
                                        .map(|host| host.alias.as_str())
                                        .unwrap_or(catalog.text("direct_connection"));
                                    egui::ComboBox::from_id_salt("jump_host")
                                        .selected_text(selected_name)
                                        .width(300.0)
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut editor.profile.jump_host,
                                                None,
                                                catalog.text("direct_connection"),
                                            );
                                            let mut count = 0;
                                            for candidate in &hosts {
                                                if can_use_as_jump(
                                                    candidate,
                                                    &editor.profile,
                                                    &hosts,
                                                ) {
                                                    count += 1;
                                                    ui.selectable_value(
                                                        &mut editor.profile.jump_host,
                                                        Some(candidate.id),
                                                        &candidate.alias,
                                                    );
                                                }
                                            }
                                            if count == 0 {
                                                ui.add_enabled(
                                                    false,
                                                    egui::Label::new(
                                                        catalog.text("no_verified_hosts"),
                                                    ),
                                                );
                                            }
                                        });
                                    ui.small(catalog.text("chain_hint"));
                                });
                                ui.end_row();

                                form_label(ui, catalog.text("host_key"));
                                ui.vertical(|ui| {
                                    ui.label(
                                        editor
                                            .profile
                                            .host_fingerprint
                                            .as_deref()
                                            .unwrap_or(catalog.text("host_key_unverified")),
                                    );
                                    if let Some(algorithm) = &editor.profile.host_key_algorithm {
                                        ui.small(format!(
                                            "{}: {algorithm}",
                                            catalog.text("host_key_algorithm")
                                        ));
                                    }
                                });
                                ui.end_row();
                            }
                        });
                });

            if editor.profile.protocol == Protocol::Telnet {
                ui.add_space(12.0);
                ui.colored_label(
                    Color32::from_rgb(210, 145, 40),
                    catalog.text("telnet_warning"),
                );
            }
            if editor.connection_changed() {
                ui.add_space(10.0);
                ui.label(
                    RichText::new(catalog.text("unsaved_changes"))
                        .color(ui.visuals().warn_fg_color),
                );
            }
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !testing,
                        egui::Button::new(if testing {
                            catalog.text("testing")
                        } else {
                            catalog.text("test_connection")
                        })
                        .min_size([140.0, 38.0].into()),
                    )
                    .clicked()
                {
                    action = Some(EditorAction::Test);
                }
                if ui
                    .add_enabled(
                        !testing,
                        egui::Button::new(catalog.text("save")).min_size([100.0, 38.0].into()),
                    )
                    .clicked()
                {
                    action = Some(EditorAction::Save);
                }
                if codex_edit
                    && ui
                        .add(
                            egui::Button::new(catalog.text("cancel"))
                                .min_size([100.0, 38.0].into()),
                        )
                        .clicked()
                {
                    action = Some(EditorAction::CancelCodex);
                }
            });
            ui.add_space(18.0);
            ui.separator();
            ui.add_space(12.0);
            ui.label(&self.status);
            ui.add_space(28.0);
        });

        match action {
            Some(EditorAction::Save) => self.save(context),
            Some(EditorAction::Test) => self.start_test(),
            Some(EditorAction::CancelCodex) => {
                let alias = self
                    .editor
                    .as_ref()
                    .map(|editor| editor.profile.alias.clone());
                match self.write_callback("cancelled", alias.as_deref()) {
                    Ok(()) => context.send_viewport_cmd(egui::ViewportCommand::Close),
                    Err(error) => {
                        self.status = self.catalog.format("callback_error", &[("error", &error)])
                    }
                }
            }
            Some(EditorAction::BrowsePrivateKey) => {
                if let Some(path) = rfd::FileDialog::new().pick_file()
                    && let Some(editor) = &mut self.editor
                {
                    editor.profile.private_key_path = path.display().to_string();
                }
            }
            Some(EditorAction::DiscoverFido) => {
                if let Some(identity) = fido::discover_handles().into_iter().next() {
                    if let Some(editor) = &mut self.editor {
                        editor.profile.private_key_path = identity.path.display().to_string();
                    }
                    self.status = self.catalog.format(
                        "fido_found",
                        &[("fingerprint", identity.fingerprint.as_str())],
                    );
                } else {
                    self.status = self.catalog.text("fido_not_found").to_owned();
                }
            }
            Some(EditorAction::OpenFidoSetup) => {
                self.fido_setup_prompt = Some(FidoSetupPrompt {
                    pin: Zeroizing::new(String::new()),
                    operation: None,
                    status: None,
                    identity: None,
                });
            }
            None => {}
        }
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

    fn fido_setup_modal(&mut self, context: &egui::Context) {
        let Some(mut prompt) = self.fido_setup_prompt.take() else {
            return;
        };
        if let Some(operation) = &prompt.operation {
            match operation.try_recv() {
                Ok(Ok(identities)) => {
                    if let Some(identity) = identities.into_iter().next() {
                        if let Some(editor) = &mut self.editor {
                            editor.profile.private_key_path = identity.path.display().to_string();
                        }
                        prompt.status = Some(self.catalog.format(
                            "fido_setup_succeeded",
                            &[("fingerprint", identity.fingerprint.as_str())],
                        ));
                        prompt.identity = Some(identity);
                    }
                    prompt.operation = None;
                }
                Ok(Err(error)) => {
                    prompt.status = Some(
                        self.catalog
                            .format("fido_setup_failed", &[("error", error.as_str())]),
                    );
                    prompt.operation = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    prompt.status = Some(self.catalog.text("fido_setup_disconnected").to_owned());
                    prompt.operation = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let busy = prompt.operation.is_some();
        let choice = egui::Modal::new(egui::Id::new("fido_setup"))
            .show(context, |ui| {
                ui.set_max_width(620.0);
                ui.heading(self.catalog.text("fido_setup_title"));
                ui.add_space(8.0);
                ui.label(self.catalog.text("fido_setup_hint"));
                ui.add_space(12.0);
                ui.label(RichText::new(self.catalog.text("fido_pin")).strong());
                ui.add(
                    egui::TextEdit::singleline(&mut *prompt.pin)
                        .password(true)
                        .desired_width(320.0),
                );
                ui.small(self.catalog.text("fido_pin_hint"));
                ui.add_space(14.0);
                let mut action = None;
                ui.add_enabled_ui(!busy, |ui| {
                    if ui
                        .add_sized(
                            [ui.available_width(), 40.0],
                            egui::Button::new(self.catalog.text("fido_create_recommended")),
                        )
                        .clicked()
                    {
                        action = Some(FidoSetupAction::CreateRecommended);
                    }
                    ui.small(self.catalog.text("fido_create_recommended_hint"));
                    ui.add_space(8.0);
                    if ui
                        .add_sized(
                            [ui.available_width(), 40.0],
                            egui::Button::new(self.catalog.text("fido_recover")),
                        )
                        .clicked()
                    {
                        action = Some(FidoSetupAction::RecoverResident);
                    }
                    ui.small(self.catalog.text("fido_recover_hint"));
                    ui.add_space(8.0);
                    if ui
                        .add_sized(
                            [ui.available_width(), 40.0],
                            egui::Button::new(self.catalog.text("fido_create_compatible")),
                        )
                        .clicked()
                    {
                        action = Some(FidoSetupAction::CreateCompatible);
                    }
                    ui.small(self.catalog.text("fido_create_compatible_hint"));
                });
                if busy {
                    ui.add_space(12.0);
                    ui.spinner();
                }
                if let Some(status) = &prompt.status {
                    ui.add_space(12.0);
                    ui.label(status);
                }
                if let Some(identity) = &prompt.identity {
                    ui.add_space(10.0);
                    ui.monospace(&identity.public_key);
                    if ui.button(self.catalog.text("copy_public_key")).clicked() {
                        ui.ctx().copy_text(identity.public_key.clone());
                    }
                }
                ui.add_space(16.0);
                if ui
                    .add_enabled(!busy, egui::Button::new(self.catalog.text("close")))
                    .clicked()
                {
                    return (None, true);
                }
                (action, false)
            })
            .inner;
        self.fido_setup_prompt = Some(prompt);
        if choice.1 {
            self.fido_setup_prompt = None;
        } else if let Some(action) = choice.0 {
            self.start_fido_setup_operation(action);
        }
    }

    fn fingerprint_modal(&mut self, context: &egui::Context) {
        let Some(prompt) = self.fingerprint_prompt.clone() else {
            return;
        };
        let changed = prompt.expected.is_some();
        let choice = egui::Modal::new(egui::Id::new("fingerprint_confirmation"))
            .show(context, |ui| {
                ui.set_max_width(560.0);
                ui.heading(self.catalog.text(if changed {
                    "fingerprint_changed_title"
                } else {
                    "fingerprint_new_title"
                }));
                ui.add_space(10.0);
                ui.label(self.catalog.format(
                    if changed {
                        "fingerprint_changed_message"
                    } else {
                        "fingerprint_new_message"
                    },
                    &[("alias", &prompt.alias)],
                ));
                ui.add_space(14.0);
                if let Some(expected) = &prompt.expected {
                    ui.label(RichText::new(self.catalog.text("fingerprint_previous")).strong());
                    ui.monospace(expected);
                    if let Some(algorithm) = &prompt.expected_algorithm {
                        ui.small(format!(
                            "{}: {algorithm}",
                            self.catalog.text("host_key_algorithm")
                        ));
                    }
                    ui.add_space(10.0);
                }
                ui.label(RichText::new(self.catalog.text("fingerprint_detected")).strong());
                ui.monospace(&prompt.observed);
                if let Some(algorithm) = &prompt.observed_algorithm {
                    ui.small(format!(
                        "{}: {algorithm}",
                        self.catalog.text("host_key_algorithm")
                    ));
                }
                ui.add_space(18.0);
                let mut result = None;
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(self.catalog.text("trust_and_retry"))
                                .min_size([140.0, 38.0].into()),
                        )
                        .clicked()
                    {
                        result = Some(true);
                    }
                    if ui
                        .add(
                            egui::Button::new(self.catalog.text("reject"))
                                .min_size([100.0, 38.0].into()),
                        )
                        .clicked()
                    {
                        result = Some(false);
                    }
                });
                result
            })
            .inner;
        if let Some(trust) = choice {
            self.apply_fingerprint_choice(trust, context);
        }
    }

    fn import_window(&mut self, context: &egui::Context) {
        if !self.import_window_open {
            return;
        }
        enum ImportAction {
            Download,
            Import,
        }
        let mut open = self.import_window_open;
        let mut action = None;
        egui::Window::new(self.catalog.text("import_title"))
            .id(egui::Id::new("import_hosts_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(context, |ui| {
                ui.set_min_width(380.0);
                ui.label(self.catalog.text("import_hint"));
                ui.add_space(12.0);
                if ui
                    .add_sized(
                        [ui.available_width(), 40.0],
                        egui::Button::new(self.catalog.text("download_template")),
                    )
                    .clicked()
                {
                    action = Some(ImportAction::Download);
                }
                ui.add_space(8.0);
                if ui
                    .add_sized(
                        [ui.available_width(), 40.0],
                        egui::Button::new(self.catalog.text("import_template")),
                    )
                    .clicked()
                {
                    action = Some(ImportAction::Import);
                }
            });
        self.import_window_open = open;
        match action {
            Some(ImportAction::Download) => self.download_import_template(),
            Some(ImportAction::Import) => self.import_hosts_from_template(),
            None => {}
        }
    }

    fn import_cleanup_modal(&mut self, context: &egui::Context) {
        let Some(prompt) = self.import_cleanup_prompt.as_ref() else {
            return;
        };
        let path = prompt.path.display().to_string();
        let choice = egui::Modal::new(egui::Id::new("import_cleanup_confirmation"))
            .show(context, |ui| {
                ui.set_max_width(480.0);
                ui.heading(self.catalog.text("import_cleanup_title"));
                ui.add_space(8.0);
                ui.label(
                    self.catalog
                        .format("import_cleanup_message", &[("path", path.as_str())]),
                );
                ui.add_space(16.0);
                let mut result = None;
                ui.horizontal(|ui| {
                    if ui.button(self.catalog.text("delete_import_file")).clicked() {
                        result = Some(true);
                    }
                    if ui.button(self.catalog.text("keep_import_file")).clicked() {
                        result = Some(false);
                    }
                });
                result
            })
            .inner;
        if let Some(delete_file) = choice {
            let prompt = self.import_cleanup_prompt.take().unwrap();
            let path_text = prompt.path.display().to_string();
            if delete_file {
                match fs::remove_file(&prompt.path) {
                    Ok(()) => {
                        self.status = self.catalog.format(
                            "import_file_deleted",
                            &[
                                ("count", &prompt.imported_count.to_string()),
                                ("path", path_text.as_str()),
                            ],
                        );
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        self.status = self.catalog.format(
                            "import_file_deleted",
                            &[
                                ("count", &prompt.imported_count.to_string()),
                                ("path", path_text.as_str()),
                            ],
                        );
                    }
                    Err(error) => {
                        self.status = self.catalog.format(
                            "import_file_delete_failed",
                            &[("error", &error.to_string()), ("path", path_text.as_str())],
                        );
                    }
                }
            } else {
                self.status = self
                    .catalog
                    .format("import_file_kept_warning", &[("path", path_text.as_str())]);
            }
        }
    }

    fn batch_export_window(&mut self, context: &egui::Context) {
        if !self.batch_export_window_open {
            return;
        }
        enum ExportAction {
            Directory,
            File,
        }
        let mut open = self.batch_export_window_open;
        let mut action = None;
        egui::Window::new(self.catalog.text("batch_export_title"))
            .id(egui::Id::new("batch_export_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(context, |ui| {
                ui.set_min_width(420.0);
                ui.label(self.catalog.text("batch_export_hint"));
                ui.add_space(12.0);
                if ui
                    .add_sized(
                        [ui.available_width(), 40.0],
                        egui::Button::new(self.catalog.text("export_to_directory")),
                    )
                    .clicked()
                {
                    action = Some(ExportAction::Directory);
                }
                ui.add_space(8.0);
                if ui
                    .add_sized(
                        [ui.available_width(), 40.0],
                        egui::Button::new(self.catalog.text("export_to_file")),
                    )
                    .clicked()
                {
                    action = Some(ExportAction::File);
                }
            });
        self.batch_export_window_open = open;
        match action {
            Some(ExportAction::Directory) => self.export_batch_to_directory(),
            Some(ExportAction::File) => self.export_batch_to_file(),
            None => {}
        }
    }

    fn delete_modal(&mut self, context: &egui::Context) {
        if !self.delete_prompt {
            return;
        }
        let choice = egui::Modal::new(egui::Id::new("delete_confirmation"))
            .show(context, |ui| {
                ui.set_max_width(440.0);
                ui.heading(self.catalog.text("delete_title"));
                ui.add_space(8.0);
                ui.label(self.catalog.text("delete_message"));
                ui.add_space(16.0);
                let mut result = None;
                ui.horizontal(|ui| {
                    if ui.button(self.catalog.text("confirm_delete")).clicked() {
                        result = Some(true);
                    }
                    if ui.button(self.catalog.text("cancel")).clicked() {
                        result = Some(false);
                    }
                });
                result
            })
            .inner;
        if let Some(confirm) = choice {
            self.delete_prompt = false;
            if confirm {
                self.remove_selected();
            }
        }
    }

    fn batch_delete_modal(&mut self, context: &egui::Context) {
        if !self.batch_delete_prompt {
            return;
        }
        let count = self.batch_selected.len().to_string();
        let choice = egui::Modal::new(egui::Id::new("batch_delete_confirmation"))
            .show(context, |ui| {
                ui.set_max_width(440.0);
                ui.heading(self.catalog.text("batch_delete_title"));
                ui.add_space(8.0);
                ui.label(
                    self.catalog
                        .format("batch_delete_message", &[("count", count.as_str())]),
                );
                ui.add_space(16.0);
                let mut result = None;
                ui.horizontal(|ui| {
                    if ui
                        .button(self.catalog.text("confirm_delete_selected"))
                        .clicked()
                    {
                        result = Some(true);
                    }
                    if ui.button(self.catalog.text("cancel")).clicked() {
                        result = Some(false);
                    }
                });
                result
            })
            .inner;
        if let Some(confirm) = choice {
            self.batch_delete_prompt = false;
            if confirm {
                self.remove_batch();
            }
        }
    }
}

impl eframe::App for HostsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = ui.ctx().clone();
        self.poll_tests();
        self.top_bar(ui, &context);
        ui.separator();
        let body = ui.available_rect_before_wrap();
        let divider_x = body.min.x + 340.0;
        let sidebar = egui::Rect::from_min_max(body.min, egui::pos2(divider_x, body.max.y));
        let editor = egui::Rect::from_min_max(egui::pos2(divider_x + 10.0, body.min.y), body.max);
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(sidebar)
                .layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| self.sidebar(ui),
        );
        ui.painter().vline(
            divider_x,
            body.y_range(),
            ui.visuals().widgets.noninteractive.bg_stroke,
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(editor)
                .layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| self.editor_panel(ui, &context),
        );
        ui.allocate_rect(body, egui::Sense::hover());
        self.fingerprint_modal(&context);
        self.fido_setup_modal(&context);
        self.import_window(&context);
        self.import_cleanup_modal(&context);
        self.batch_export_window(&context);
        self.delete_modal(&context);
        self.batch_delete_modal(&context);
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
    }
}

fn host_row_fill(state: Option<HostTestState>) -> Option<Color32> {
    match state {
        Some(HostTestState::Succeeded) => Some(TEST_SUCCESS_FILL),
        Some(HostTestState::Failed) => Some(TEST_FAILURE_FILL),
        _ => None,
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

fn batch_all_selected(hosts: &[HostProfile], selected: &HashSet<Uuid>) -> bool {
    !hosts.is_empty() && hosts.iter().all(|host| selected.contains(&host.id))
}

fn form_label(ui: &mut egui::Ui, text: &str) {
    ui.add_sized(
        [160.0, 24.0],
        egui::Label::new(RichText::new(text).strong()).halign(egui::Align::Min),
    );
}

fn form_label_with_hint(ui: &mut egui::Ui, text: &str) {
    ui.allocate_ui_with_layout(
        egui::vec2(160.0, 48.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.add_sized(
                [160.0, 24.0],
                egui::Label::new(RichText::new(text).strong()).halign(egui::Align::Min),
            );
        },
    );
}

fn font_candidates(locale: &str) -> Vec<(&'static str, &'static str)> {
    const SEGOE: (&str, &str) = ("segoe", r"C:\Windows\Fonts\segoeui.ttf");
    const YAHEI: (&str, &str) = ("yahei", r"C:\Windows\Fonts\msyh.ttc");
    const JHENGHEI: (&str, &str) = ("jhenghei", r"C:\Windows\Fonts\msjh.ttc");
    const MEIRYO: (&str, &str) = ("meiryo", r"C:\Windows\Fonts\meiryo.ttc");
    let regional = match locale {
        "zh-CN" => [YAHEI, JHENGHEI, MEIRYO],
        "zh-TW" => [JHENGHEI, YAHEI, MEIRYO],
        "ja" => [MEIRYO, YAHEI, JHENGHEI],
        _ => [YAHEI, JHENGHEI, MEIRYO],
    };
    std::iter::once(SEGOE).chain(regional).collect()
}

fn configure_fonts(context: &egui::Context, locale: &str) {
    let mut fonts = FontDefinitions::default();
    let mut installed = Vec::new();
    for (name, path) in font_candidates(locale) {
        if let Ok(bytes) = fs::read(Path::new(path)) {
            fonts
                .font_data
                .insert(name.to_owned(), FontData::from_owned(bytes).into());
            installed.push(name.to_owned());
        }
    }
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        if let Some(fonts_for_family) = fonts.families.get_mut(&family) {
            for name in installed.iter().rev() {
                fonts_for_family.insert(0, name.clone());
            }
        }
    }
    context.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_loading_prioritizes_the_active_language_and_keeps_cjk_fallbacks() {
        assert_eq!(font_candidates("en").len(), 4);
        assert_eq!(font_candidates("zh-CN").len(), 4);
        assert_eq!(font_candidates("zh-CN")[1].0, "yahei");
        assert_eq!(font_candidates("zh-TW")[1].0, "jhenghei");
        assert_eq!(font_candidates("ja")[1].0, "meiryo");
        assert_eq!(font_candidates("unknown"), font_candidates("en"));
    }

    #[test]
    fn test_rows_use_requested_result_colors() {
        assert_eq!(
            host_row_fill(Some(HostTestState::Succeeded)),
            Some(TEST_SUCCESS_FILL)
        );
        assert_eq!(
            host_row_fill(Some(HostTestState::Failed)),
            Some(TEST_FAILURE_FILL)
        );
        assert_eq!(host_row_fill(Some(HostTestState::Testing)), None);
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
            profile: profile.clone(),
            original: profile,
            password: Zeroizing::new(String::new()),
            key_passphrase: Zeroizing::new(String::new()),
            password_mode: PasswordMode::Password,
            saved_password_mode: Some(PasswordMode::Password),
            password_read_error: None,
            has_key_passphrase: false,
            key_passphrase_read_error: None,
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
}
