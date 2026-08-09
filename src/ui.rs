use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, RichText};
use serde::Serialize;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::connection;
use crate::credentials::{self, CredentialKind};
use crate::i18n::Catalog;
use crate::import;
use crate::model::{HostProfile, Prefill, Protocol, SshAuth, can_use_as_jump};
use crate::ssh::{OperationLimits, RemoteFailure, RemoteResult};
use crate::storage::HostStore;

#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    pub codex_edit: bool,
    pub prefill: Prefill,
    pub result_path: Option<PathBuf>,
    pub observed_fingerprint: Option<String>,
}

struct HostEditor {
    profile: HostProfile,
    original: HostProfile,
    password: Zeroizing<String>,
    key_passphrase: Zeroizing<String>,
    has_password: bool,
    has_key_passphrase: bool,
}

impl HostEditor {
    fn load(profile: HostProfile) -> Self {
        let has_password = credentials::has(profile.id, CredentialKind::Password).unwrap_or(false);
        let has_key_passphrase =
            credentials::has(profile.id, CredentialKind::KeyPassphrase).unwrap_or(false);
        Self {
            original: profile.clone(),
            profile,
            password: Zeroizing::new(String::new()),
            key_passphrase: Zeroizing::new(String::new()),
            has_password,
            has_key_passphrase,
        }
    }

    fn connection_changed(&self) -> bool {
        !self.profile.connection_details_equal(&self.original)
    }
}

const GUI_TEST_TIMEOUT: Duration = Duration::from_secs(10);
const TEST_SUCCESS_FILL: Color32 = Color32::from_rgb(36, 105, 67);
const TEST_FAILURE_FILL: Color32 = Color32::from_rgb(132, 48, 53);
const SELECTED_ROW_STROKE: Color32 = Color32::from_rgb(230, 230, 230);

struct TestOperation {
    receiver: Receiver<(Instant, Result<RemoteResult, RemoteFailure>)>,
    started_at: Instant,
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
    retry_test: bool,
    close_after_choice: bool,
}

enum EditorAction {
    Save,
    Test,
    CancelCodex,
}

pub struct HostsApp {
    store: HostStore,
    catalog: Catalog,
    selected: Option<Uuid>,
    editor: Option<HostEditor>,
    status: String,
    test_operations: HashMap<Uuid, TestOperation>,
    test_states: HashMap<Uuid, HostTestState>,
    testing_all: bool,
    fingerprint_prompt: Option<FingerprintPrompt>,
    delete_prompt: bool,
    import_window_open: bool,
    batch_mode: bool,
    batch_selected: HashSet<Uuid>,
    batch_delete_prompt: bool,
    launch: LaunchOptions,
    callback_written: bool,
}

impl HostsApp {
    pub fn new(context: &eframe::CreationContext<'_>, launch: LaunchOptions) -> Self {
        configure_fonts(&context.egui_ctx);
        context.egui_ctx.set_zoom_factor(1.06);

        let mut store = HostStore::load().unwrap_or_default();
        let catalog = Catalog::for_locale(store.preferred_locale.as_deref());
        let mut selected = store.hosts.first().map(|host| host.id);

        if launch.codex_edit {
            let alias = launch
                .prefill
                .alias
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| store.next_neutral_alias());
            if let Some(existing) = store.find_alias(&alias) {
                selected = Some(existing.id);
            } else {
                let mut draft = HostProfile::new(alias);
                draft.apply_prefill(&launch.prefill);
                if let Some(jump_alias) = launch.prefill.jump_alias.as_deref() {
                    draft.jump_host = store.find_alias(jump_alias).map(|host| host.id);
                }
                selected = Some(draft.id);
                store.hosts.push(draft);
                let _ = store.save();
            }
        }

        let mut editor = selected
            .and_then(|id| store.hosts.iter().find(|host| host.id == id))
            .cloned()
            .map(HostEditor::load);
        if launch.codex_edit
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
                retry_test: false,
                close_after_choice: true,
            })
        });
        let status = if launch.codex_edit {
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
            test_states: HashMap::new(),
            testing_all: false,
            fingerprint_prompt,
            delete_prompt: false,
            import_window_open: false,
            batch_mode: false,
            batch_selected: HashSet::new(),
            batch_delete_prompt: false,
            launch,
            callback_written: false,
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
            self.status = self
                .catalog
                .format("storage_error", &[("error", &error.to_string())]);
        }
        self.selected = Some(id);
        self.editor = Some(HostEditor::load(profile));
    }

    fn persist_editor(&mut self) -> Result<(), String> {
        let editor = self.editor.as_mut().ok_or_else(|| "NO_EDITOR".to_owned())?;
        if let Some(issue) = editor.profile.validation_issue() {
            return Err(self.catalog.text(issue.translation_key()).to_owned());
        }
        if self.store.hosts.iter().any(|host| {
            host.id != editor.profile.id
                && host.alias.eq_ignore_ascii_case(editor.profile.alias.trim())
        }) {
            return Err(self.catalog.text("validation_alias").to_owned());
        }

        let needs_password = editor.profile.protocol == Protocol::Telnet
            || editor.profile.ssh_auth == SshAuth::Password;
        if needs_password && editor.password.is_empty() && !editor.has_password {
            return Err(self.catalog.text("password_required").to_owned());
        }
        if !editor.password.is_empty() {
            credentials::store(
                editor.profile.id,
                CredentialKind::Password,
                editor.password.as_str(),
            )
            .map_err(|error| {
                self.catalog
                    .format("credential_error", &[("error", &error.to_string())])
            })?;
            editor.has_password = true;
            editor.password.clear();
        }
        if !editor.key_passphrase.is_empty() {
            credentials::store(
                editor.profile.id,
                CredentialKind::KeyPassphrase,
                editor.key_passphrase.as_str(),
            )
            .map_err(|error| {
                self.catalog
                    .format("credential_error", &[("error", &error.to_string())])
            })?;
            editor.has_key_passphrase = true;
            editor.key_passphrase.clear();
        }
        if editor.connection_changed() {
            editor.profile.verified = false;
        }
        editor.profile.alias = editor.profile.alias.trim().to_owned();
        editor.profile.address = editor.profile.address.trim().to_owned();
        editor.profile.username = editor.profile.username.trim().to_owned();
        editor.profile.private_key_path = editor.profile.private_key_path.trim().to_owned();
        if editor.profile.protocol == Protocol::Telnet {
            editor.profile.jump_host = None;
            editor.profile.host_fingerprint = None;
        }
        let stored = self
            .store
            .hosts
            .iter_mut()
            .find(|host| host.id == editor.profile.id)
            .ok_or_else(|| "HOST_NOT_FOUND".to_owned())?;
        stored.clone_from(&editor.profile);
        self.store.save().map_err(|error| {
            self.catalog
                .format("storage_error", &[("error", &error.to_string())])
        })?;
        editor.original.clone_from(&editor.profile);
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
                    self.write_callback("saved", Some(&alias));
                    context.send_viewport_cmd(egui::ViewportCommand::Close);
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
        if self.test_operations.contains_key(&id) {
            return;
        }
        let Some(profile) = self.store.hosts.iter().find(|host| host.id == id).cloned() else {
            return;
        };
        let hosts = self.store.hosts.clone();
        let (sender, receiver) = mpsc::channel();
        let started_at = Instant::now();
        thread::spawn(move || {
            let limits = OperationLimits {
                connect_timeout: Some(GUI_TEST_TIMEOUT),
                command_timeout: Some(GUI_TEST_TIMEOUT),
            };
            let result = connection::probe(&profile, &hosts, limits);
            let _ = sender.send((Instant::now(), result));
        });
        self.test_operations.insert(
            id,
            TestOperation {
                receiver,
                started_at,
            },
        );
        self.test_states.insert(id, HostTestState::Testing);
    }

    fn start_all_tests(&mut self) {
        if !self.test_operations.is_empty() {
            return;
        }
        if self.editor.as_ref().is_some_and(|editor| {
            editor.profile != editor.original
                || !editor.password.is_empty()
                || !editor.key_passphrase.is_empty()
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
        self.testing_all = true;
        for id in ids {
            self.start_test_host(id);
        }
        self.status = self.catalog.text("testing_all").to_owned();
    }

    fn poll_tests(&mut self) {
        let mut completed = Vec::new();
        for (&id, operation) in &self.test_operations {
            match operation.receiver.try_recv() {
                Ok((finished_at, result)) => {
                    if test_timed_out(finished_at.duration_since(operation.started_at)) {
                        completed.push((
                            id,
                            Err(RemoteFailure::new(
                                "TEST_TIMEOUT",
                                "The connection test exceeded 10 seconds.",
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
                    if test_timed_out(operation.started_at.elapsed()) =>
                {
                    completed.push((
                        id,
                        Err(RemoteFailure::new(
                            "TEST_TIMEOUT",
                            "The connection test exceeded 10 seconds.",
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
        if self.testing_all && self.test_operations.is_empty() {
            self.testing_all = false;
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
                if let Some(editor) = &mut self.editor
                    && editor.profile.id == id
                {
                    editor.profile.verified = true;
                    editor.original.verified = true;
                }
                if let Some(stored) = self.store.hosts.iter_mut().find(|host| host.id == id) {
                    stored.verified = true;
                }
                let _ = self.store.save();
                if !self.testing_all && self.selected == Some(id) {
                    self.status = self
                        .catalog
                        .format("status_test_ok", &[("identity", identity.as_str())]);
                }
            }
            Err(error)
                if matches!(error.code, "HOSTKEY_UNKNOWN" | "HOSTKEY_MISMATCH")
                    && error.observed_fingerprint.is_some() =>
            {
                self.test_states.insert(id, HostTestState::Failed);
                if !self.testing_all && self.selected == Some(id) {
                    self.fingerprint_prompt = Some(FingerprintPrompt {
                        host_id: id,
                        alias: error
                            .host_alias
                            .map(|value| value.into_string())
                            .unwrap_or_default(),
                        expected: error.expected_fingerprint.map(|value| value.into_string()),
                        observed: error
                            .observed_fingerprint
                            .map(|value| value.into_string())
                            .unwrap_or_default(),
                        retry_test: true,
                        close_after_choice: false,
                    });
                }
            }
            Err(error) => {
                self.test_states.insert(id, HostTestState::Failed);
                if !self.testing_all && self.selected == Some(id) {
                    self.status = if error.code == "TEST_TIMEOUT" {
                        self.catalog.text("status_test_timeout").to_owned()
                    } else {
                        self.catalog
                            .format("status_test_failed", &[("error", error.code)])
                    };
                }
            }
        }
    }

    fn apply_fingerprint_choice(&mut self, trust: bool, context: &egui::Context) {
        let Some(prompt) = self.fingerprint_prompt.take() else {
            return;
        };
        if !trust {
            self.status = self.catalog.text("status_cancelled").to_owned();
            if prompt.close_after_choice {
                self.write_callback("cancelled", Some(&prompt.alias));
                context.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        if let Some(host) = self
            .store
            .hosts
            .iter_mut()
            .find(|host| host.id == prompt.host_id)
        {
            host.host_fingerprint = Some(prompt.observed.clone());
            host.verified = false;
        }
        if let Some(editor) = &mut self.editor
            && editor.profile.id == prompt.host_id
        {
            editor.profile.host_fingerprint = Some(prompt.observed.clone());
            editor.profile.verified = false;
            editor.original.host_fingerprint = Some(prompt.observed.clone());
            editor.original.verified = false;
        }
        let _ = self.store.save();
        if prompt.close_after_choice {
            self.write_callback("trusted", Some(&prompt.alias));
            context.send_viewport_cmd(egui::ViewportCommand::Close);
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
        if let Err(error) = credentials::delete_all(id) {
            self.status = self
                .catalog
                .format("credential_error", &[("error", &error.to_string())]);
            return;
        }
        self.test_operations.remove(&id);
        self.test_states.remove(&id);
        self.store.hosts.retain(|host| host.id != id);
        let _ = self.store.save();
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

    fn cancel_batch_mode(&mut self) {
        self.batch_mode = false;
        self.batch_selected.clear();
        self.batch_delete_prompt = false;
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
        for id in &ids {
            if let Err(error) = credentials::delete_all(*id) {
                self.status = self
                    .catalog
                    .format("credential_error", &[("error", &error.to_string())]);
                return;
            }
        }
        for id in &ids {
            self.test_operations.remove(id);
            self.test_states.remove(id);
        }
        self.store
            .hosts
            .retain(|host| !self.batch_selected.contains(&host.id));
        if let Err(error) = self.store.save() {
            self.status = self
                .catalog
                .format("storage_error", &[("error", &error.to_string())]);
            return;
        }
        self.selected = self.store.hosts.first().map(|host| host.id);
        self.editor = self
            .selected
            .and_then(|selected| self.store.hosts.iter().find(|host| host.id == selected))
            .cloned()
            .map(HostEditor::load);
        self.batch_mode = false;
        self.batch_selected.clear();
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
        let imported = fs::read(&path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                import::parse_template(&bytes, &self.store.hosts).map_err(|error| error.to_string())
            });
        let imported = match imported {
            Ok(imported) => imported,
            Err(error) => {
                self.status = self
                    .catalog
                    .format("import_failed", &[("error", error.as_str())]);
                return;
            }
        };
        let first_id = imported.first().map(|host| host.id);
        let count = imported.len();
        self.store.hosts.extend(imported);
        if let Err(error) = self.store.save() {
            self.store.hosts.truncate(self.store.hosts.len() - count);
            self.status = self
                .catalog
                .format("storage_error", &[("error", &error.to_string())]);
            return;
        }
        if let Some(id) = first_id {
            self.select(id);
        }
        self.import_window_open = false;
        self.status = self
            .catalog
            .format("import_succeeded", &[("count", &count.to_string())]);
    }

    fn write_callback(&mut self, status: &'static str, alias: Option<&str>) {
        if self.callback_written {
            return;
        }
        let Some(path) = self.launch.result_path.as_deref() else {
            self.callback_written = true;
            return;
        };
        #[derive(Serialize)]
        struct Callback<'a> {
            status: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            alias: Option<&'a str>,
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&Callback { status, alias }) {
            let _ = fs::write(path, bytes);
        }
        self.callback_written = true;
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        let mut open_import = false;
        let mut test_all = false;
        let mut begin_batch = false;
        let mut delete_batch = false;
        let mut cancel_batch = false;
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 48.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add_space(18.0);
                if self.batch_mode {
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
                            self.test_operations.is_empty(),
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
                                    self.store.preferred_locale = Some(language.locale.to_owned());
                                    let _ = self.store.save();
                                    self.status = self.catalog.text("status_ready").to_owned();
                                    context.send_viewport_cmd(egui::ViewportCommand::Title(
                                        self.catalog.text("app_title").to_owned(),
                                    ));
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
        if delete_batch {
            self.request_batch_delete();
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

        let entries = self
            .store
            .hosts
            .iter()
            .map(|host| {
                (
                    host.id,
                    host.alias.clone(),
                    host.address.clone(),
                    host.port,
                    host.protocol,
                    host.verified,
                    self.test_states.get(&host.id).copied(),
                )
            })
            .collect::<Vec<_>>();
        let mut selection_request = None;
        let mut deletion_request = None;
        let mut batch_toggle_request = None;
        egui::ScrollArea::vertical()
            .id_salt("host_list_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for (id, alias, address, port, protocol, verified, test_state) in entries {
                    let selected = self.selected == Some(id);
                    let response = ui
                        .horizontal(|ui| {
                            if self.batch_mode {
                                let mut checked = self.batch_selected.contains(&id);
                                if ui.checkbox(&mut checked, "").changed() {
                                    batch_toggle_request = Some((id, checked));
                                }
                            }
                            let verified_marker = if test_state == Some(HostTestState::Succeeded)
                                || (test_state.is_none() && verified)
                            {
                                "  ✓"
                            } else if test_state == Some(HostTestState::Testing) {
                                "  …"
                            } else {
                                ""
                            };
                            let selection_marker = if selected { "▶ " } else { "" };
                            let text = RichText::new(format!(
                                "{}{}\n{} · {}:{}{}",
                                selection_marker,
                                alias,
                                protocol.stable_name().to_ascii_uppercase(),
                                address,
                                port,
                                verified_marker
                            ))
                            .line_height(Some(20.0));
                            let mut button = egui::Button::new(text);
                            if let Some(fill) = host_row_fill(test_state) {
                                button = button.fill(fill);
                            }
                            if selected {
                                button = button.stroke(egui::Stroke::new(2.0, SELECTED_ROW_STROKE));
                            }
                            let size = [ui.available_width(), 62.0];
                            ui.add_sized(size, button)
                        })
                        .inner;
                    if response.clicked() || response.secondary_clicked() {
                        if self.batch_mode {
                            batch_toggle_request = Some((id, !self.batch_selected.contains(&id)));
                        } else {
                            selection_request = Some(id);
                        }
                    }
                    if !self.batch_mode {
                        response.context_menu(|ui| {
                            if ui.button(self.catalog.text("delete")).clicked() {
                                deletion_request = Some(id);
                                ui.close();
                            }
                        });
                    }
                    ui.add_space(4.0);
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
        let testing = self
            .selected
            .is_some_and(|id| self.test_operations.contains_key(&id));
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
                        .min_col_width(150.0)
                        .spacing([20.0, 14.0])
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
                                    });
                                ui.end_row();
                            }

                            if editor.profile.protocol == Protocol::Telnet
                                || editor.profile.ssh_auth == SshAuth::Password
                            {
                                form_label(ui, catalog.text("password"));
                                ui.vertical(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut *editor.password)
                                            .password(true)
                                            .desired_width(420.0),
                                    );
                                    ui.small(if editor.has_password {
                                        catalog.text("password_saved")
                                    } else {
                                        catalog.text("password_required")
                                    });
                                });
                                ui.end_row();
                            } else {
                                form_label(ui, catalog.text("private_key"));
                                ui.vertical(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(
                                            &mut editor.profile.private_key_path,
                                        )
                                        .desired_width(420.0),
                                    );
                                    ui.small(catalog.text("private_key_hint"));
                                });
                                ui.end_row();

                                form_label(ui, catalog.text("key_passphrase"));
                                ui.vertical(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut *editor.key_passphrase)
                                            .password(true)
                                            .desired_width(420.0),
                                    );
                                    ui.small(if editor.has_key_passphrase {
                                        catalog.text("passphrase_saved")
                                    } else {
                                        catalog.text("passphrase_optional")
                                    });
                                });
                                ui.end_row();
                            }

                            if editor.profile.protocol == Protocol::Ssh {
                                form_label(ui, catalog.text("host_chain"));
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
                                ui.label(
                                    editor
                                        .profile
                                        .host_fingerprint
                                        .as_deref()
                                        .unwrap_or(catalog.text("host_key_unverified")),
                                );
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
                    .add(egui::Button::new(catalog.text("save")).min_size([100.0, 38.0].into()))
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
                self.write_callback("cancelled", alias.as_deref());
                context.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            None => {}
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
                    ui.add_space(10.0);
                }
                ui.label(RichText::new(self.catalog.text("fingerprint_detected")).strong());
                ui.monospace(&prompt.observed);
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
        let divider_x = body.min.x + 300.0;
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
        self.import_window(&context);
        self.delete_modal(&context);
        self.batch_delete_modal(&context);
        if !self.test_operations.is_empty() {
            context.request_repaint_after(std::time::Duration::from_millis(100));
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
            self.write_callback("cancelled", alias.as_deref());
        }
    }
}

fn test_timed_out(elapsed: Duration) -> bool {
    elapsed >= GUI_TEST_TIMEOUT
}

fn host_row_fill(state: Option<HostTestState>) -> Option<Color32> {
    match state {
        Some(HostTestState::Succeeded) => Some(TEST_SUCCESS_FILL),
        Some(HostTestState::Failed) => Some(TEST_FAILURE_FILL),
        _ => None,
    }
}

fn batch_has_external_dependents(hosts: &[HostProfile], selected: &HashSet<Uuid>) -> bool {
    hosts.iter().any(|host| {
        !selected.contains(&host.id)
            && host
                .jump_host
                .is_some_and(|jump_id| selected.contains(&jump_id))
    })
}

fn form_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).strong());
}

fn configure_fonts(context: &egui::Context) {
    let candidates = [
        ("segoe", r"C:\Windows\Fonts\segoeui.ttf"),
        ("yahei", r"C:\Windows\Fonts\msyh.ttc"),
        ("meiryo", r"C:\Windows\Fonts\meiryo.ttc"),
        ("yugothic", r"C:\Windows\Fonts\YuGothM.ttc"),
    ];
    let mut fonts = FontDefinitions::default();
    let mut installed = Vec::new();
    for (name, path) in candidates {
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
    fn connection_test_timeout_is_capped_at_ten_seconds() {
        assert!(!test_timed_out(Duration::from_millis(9_999)));
        assert!(test_timed_out(Duration::from_secs(10)));
    }

    #[test]
    fn batch_delete_rejects_selected_jump_host_used_by_unselected_host() {
        let jump = HostProfile::new("jump".to_owned());
        let mut target = HostProfile::new("target".to_owned());
        target.jump_host = Some(jump.id);
        let selected = HashSet::from([jump.id]);
        assert!(batch_has_external_dependents(&[jump, target], &selected));
    }
}
