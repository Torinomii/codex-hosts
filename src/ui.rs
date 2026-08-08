use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, RichText};
use serde::Serialize;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::connection;
use crate::credentials::{self, CredentialKind};
use crate::i18n::Catalog;
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

struct TestOperation {
    receiver: Receiver<Result<RemoteResult, RemoteFailure>>,
}

#[derive(Clone)]
struct FingerprintPrompt {
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
    test_operation: Option<TestOperation>,
    fingerprint_prompt: Option<FingerprintPrompt>,
    delete_prompt: bool,
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
            test_operation: None,
            fingerprint_prompt,
            delete_prompt: false,
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
        if self.test_operation.is_some() {
            return;
        }
        if let Err(error) = self.persist_editor() {
            self.status = error;
            return;
        }
        let Some(profile) = self.editor.as_ref().map(|editor| editor.profile.clone()) else {
            return;
        };
        let hosts = self.store.hosts.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = connection::probe(&profile, &hosts, OperationLimits::default());
            let _ = sender.send(result);
        });
        self.test_operation = Some(TestOperation { receiver });
        self.status = self.catalog.text("testing").to_owned();
    }

    fn poll_test(&mut self) {
        let Some(operation) = &self.test_operation else {
            return;
        };
        let Ok(result) = operation.receiver.try_recv() else {
            return;
        };
        self.test_operation = None;
        match result {
            Ok(result) => {
                let identity = result.stdout.trim().to_owned();
                if let Some(editor) = &mut self.editor {
                    editor.profile.verified = true;
                    editor.original.verified = true;
                    if let Some(stored) = self
                        .store
                        .hosts
                        .iter_mut()
                        .find(|host| host.id == editor.profile.id)
                    {
                        stored.verified = true;
                    }
                    let _ = self.store.save();
                }
                self.status = self
                    .catalog
                    .format("status_test_ok", &[("identity", identity.as_str())]);
            }
            Err(error)
                if matches!(error.code, "HOSTKEY_UNKNOWN" | "HOSTKEY_MISMATCH")
                    && error.observed_fingerprint.is_some() =>
            {
                self.fingerprint_prompt = Some(FingerprintPrompt {
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
            Err(error) => {
                self.status = self
                    .catalog
                    .format("status_test_failed", &[("error", error.code)]);
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
        if let Some(host) = self.store.find_alias_mut(&prompt.alias) {
            host.host_fingerprint = Some(prompt.observed.clone());
            host.verified = false;
        }
        if let Some(editor) = &mut self.editor
            && editor.profile.alias.eq_ignore_ascii_case(&prompt.alias)
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
            self.start_test();
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
        self.store.hosts.retain(|host| host.id != id);
        let _ = self.store.save();
        self.selected = self.store.hosts.first().map(|host| host.id);
        self.editor = self
            .selected
            .and_then(|selected| self.store.hosts.iter().find(|host| host.id == selected))
            .cloned()
            .map(HostEditor::load);
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
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 48.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
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
                                .selectable_label(language.locale == current, language.display_name)
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
            },
        );
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
                )
            })
            .collect::<Vec<_>>();
        let mut selection_request = None;
        let mut deletion_request = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for (id, alias, address, port, protocol, verified) in entries {
                    let selected = self.selected == Some(id);
                    let response = ui.add_sized(
                        [ui.available_width(), 62.0],
                        egui::Button::new(
                            RichText::new(format!(
                                "{}\n{} · {}:{}{}",
                                alias,
                                protocol.stable_name().to_ascii_uppercase(),
                                address,
                                port,
                                if verified { "  ✓" } else { "" }
                            ))
                            .line_height(Some(20.0)),
                        )
                        .selected(selected),
                    );
                    if response.clicked() || response.secondary_clicked() {
                        selection_request = Some(id);
                    }
                    response.context_menu(|ui| {
                        if ui.button(self.catalog.text("delete")).clicked() {
                            deletion_request = Some(id);
                            ui.close();
                        }
                    });
                    ui.add_space(4.0);
                }
            });

        if let Some(id) = deletion_request {
            self.select(id);
            self.delete_prompt = true;
        } else if let Some(id) = selection_request {
            self.select(id);
        }
    }

    fn editor_panel(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        let catalog = self.catalog.clone();
        let hosts = self.store.hosts.clone();
        let testing = self.test_operation.is_some();
        let codex_edit = self.launch.codex_edit;
        let mut action = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
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
}

impl eframe::App for HostsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = ui.ctx().clone();
        self.poll_test();
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
        self.delete_modal(&context);
        if self.test_operation.is_some() {
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
