use std::fs;
use std::io;
use std::sync::mpsc;

use eframe::egui::{self, RichText};

use super::{FidoSetupAction, FidoSetupPrompt, HostsApp};
use crate::i18n::Catalog;
use crate::model::{AuthPersistence, HostProfile};
use zeroize::Zeroizing;

pub(super) enum DialogResult<T> {
    Chosen(T),
    Closed,
    Open,
}

fn dialog_width(context: &egui::Context, preferred: f32) -> f32 {
    (context.content_rect().width() - 48.0).min(preferred)
}

/// Common modal shell: heading, optional body text, then caller-provided content.
pub(super) fn dialog_shell<R>(
    context: &egui::Context,
    id: &str,
    preferred_width: f32,
    title: &str,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::Modal::new(egui::Id::new(id))
        .frame(
            egui::Frame::window(&context.global_style())
                .inner_margin(egui::Margin::same(20))
                .corner_radius(10.0),
        )
        .show(context, |ui| {
            ui.set_max_width(dialog_width(context, preferred_width));
            ui.set_min_width(dialog_width(context, preferred_width).min(360.0));
            ui.heading(title);
            ui.add_space(10.0);
            body(ui)
        })
        .inner
}

pub(super) fn dialog_button_row(
    ui: &mut egui::Ui,
    primary: &str,
    destructive: bool,
    secondary: &str,
) -> Option<bool> {
    let mut result = None;
    ui.add_space(16.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let primary_button = if destructive {
            super::theme::danger_confirm_button(ui, primary)
        } else {
            super::theme::primary_button(ui, primary)
        };
        if ui.add(primary_button).clicked() {
            result = Some(true);
        }
        if ui.button(secondary).clicked() {
            result = Some(false);
        }
    });
    result
}

fn confirm_dialog(
    context: &egui::Context,
    id: &str,
    title: &str,
    message: &str,
    confirm_label: &str,
    cancel_label: &str,
    destructive: bool,
) -> Option<bool> {
    dialog_shell(context, id, 480.0, title, |ui| {
        ui.add(egui::Label::new(message).wrap());
        dialog_button_row(ui, confirm_label, destructive, cancel_label)
    })
}

fn choice_dialog<T: Copy>(
    context: &egui::Context,
    id: &str,
    title: &str,
    hint: &str,
    options: &[(&str, T)],
    close_label: &str,
) -> DialogResult<T> {
    dialog_shell(context, id, 460.0, title, |ui| {
        ui.add(egui::Label::new(RichText::new(hint).weak()).wrap());
        ui.add_space(12.0);
        let mut result = DialogResult::Open;
        for (label, value) in options {
            if ui
                .add_sized([ui.available_width(), 40.0], egui::Button::new(*label))
                .clicked()
            {
                result = DialogResult::Chosen(*value);
            }
            ui.add_space(6.0);
        }
        ui.add_space(8.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(close_label).clicked() {
                result = DialogResult::Closed;
            }
        });
        result
    })
}

impl HostsApp {
    pub(super) fn show_dialogs(&mut self, context: &egui::Context) {
        self.fingerprint_dialog(context);
        self.fido_setup_dialog(context);
        self.import_dialog(context);
        self.import_cleanup_dialog(context);
        self.batch_export_dialog(context);
        self.delete_dialog(context);
        self.unsaved_switch_dialog(context);
        self.persistence_dialog(context);
        self.batch_delete_dialog(context);
        self.bitwarden_dialogs(context);
    }

    pub(super) fn any_dialog_open(&self) -> bool {
        self.fingerprint_prompt.is_some()
            || self.fido_setup_prompt.is_some()
            || self.import_window_open
            || self.import_cleanup_prompt.is_some()
            || self.batch_export_window_open
            || self.delete_prompt
            || self.pending_switch.is_some()
            || self.persistence_prompt.is_some()
            || self.batch_delete_prompt
            || self.bitwarden_dialog_open()
    }

    pub(super) fn open_fido_setup(&mut self) {
        self.fido_setup_prompt = Some(FidoSetupPrompt {
            pin: Zeroizing::new(String::new()),
            operation: None,
            status: None,
            identity: None,
        });
    }

    fn fido_setup_dialog(&mut self, context: &egui::Context) {
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
        let catalog = &self.catalog;
        let (action, close) = dialog_shell(
            context,
            "fido_setup",
            620.0,
            catalog.text("fido_setup_title"),
            |ui| {
                ui.label(RichText::new(catalog.text("fido_setup_hint")).weak());
                ui.add_space(12.0);
                ui.label(RichText::new(catalog.text("fido_pin")).strong());
                ui.horizontal(|ui| {
                    let key = egui::Id::new("fido_setup_pin");
                    let revealed = super::theme::password_revealed(ui, key);
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut *prompt.pin)
                            .password(!revealed)
                            .desired_width(320.0),
                    );
                    super::theme::forget_password_history(ui, &response);
                    super::theme::reveal_button(ui, catalog, key);
                });
                ui.label(RichText::new(catalog.text("fido_pin_hint")).small().weak());
                ui.add_space(14.0);
                let mut action = None;
                ui.add_enabled_ui(!busy, |ui| {
                    for (label_key, hint_key, value) in [
                        (
                            "fido_create_recommended",
                            "fido_create_recommended_hint",
                            FidoSetupAction::CreateRecommended,
                        ),
                        (
                            "fido_recover",
                            "fido_recover_hint",
                            FidoSetupAction::RecoverResident,
                        ),
                        (
                            "fido_create_compatible",
                            "fido_create_compatible_hint",
                            FidoSetupAction::CreateCompatible,
                        ),
                    ] {
                        if ui
                            .add_sized(
                                [ui.available_width(), 40.0],
                                egui::Button::new(catalog.text(label_key)),
                            )
                            .clicked()
                        {
                            action = Some(value);
                        }
                        ui.label(RichText::new(catalog.text(hint_key)).small().weak());
                        ui.add_space(8.0);
                    }
                });
                if busy {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        if let Some(status) = &prompt.status {
                            ui.add(egui::Label::new(status).wrap());
                        }
                    });
                } else if let Some(status) = &prompt.status {
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(status).wrap());
                }
                if let Some(identity) = &prompt.identity {
                    ui.add_space(10.0);
                    ui.add(
                        egui::Label::new(RichText::new(&identity.public_key).monospace()).wrap(),
                    );
                    if ui.button(catalog.text("copy_public_key")).clicked() {
                        ui.ctx().copy_text(identity.public_key.clone());
                    }
                }
                ui.add_space(16.0);
                let mut close = false;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new(catalog.text("close")))
                        .clicked()
                    {
                        close = true;
                    }
                });
                (action, close)
            },
        );
        if close {
            self.fido_setup_prompt = None;
        } else {
            self.fido_setup_prompt = Some(prompt);
            if let Some(action) = action {
                self.start_fido_setup_operation(action);
            }
        }
    }

    fn fingerprint_dialog(&mut self, context: &egui::Context) {
        let Some(prompt) = self.fingerprint_prompt.clone() else {
            return;
        };
        let changed = prompt.expected.is_some();
        let catalog = &self.catalog;
        let title = catalog.text(if changed {
            "fingerprint_changed_title"
        } else {
            "fingerprint_new_title"
        });
        let choice = dialog_shell(context, "fingerprint_confirmation", 560.0, title, |ui| {
            ui.add(
                egui::Label::new(catalog.format(
                    if changed {
                        "fingerprint_changed_message"
                    } else {
                        "fingerprint_new_message"
                    },
                    &[("alias", &prompt.alias)],
                ))
                .wrap(),
            );
            ui.add_space(14.0);
            if let Some(expected) = &prompt.expected {
                ui.label(RichText::new(catalog.text("fingerprint_previous")).strong());
                ui.add(egui::Label::new(RichText::new(expected).monospace()).wrap());
                if let Some(algorithm) = &prompt.expected_algorithm {
                    ui.label(
                        RichText::new(format!(
                            "{}: {algorithm}",
                            catalog.text("host_key_algorithm")
                        ))
                        .small()
                        .weak(),
                    );
                }
                ui.add_space(10.0);
            }
            ui.label(RichText::new(catalog.text("fingerprint_detected")).strong());
            if prompt.close_after_choice {
                ui.add(
                    egui::Label::new(
                        RichText::new(catalog.text("fingerprint_reported_by_codex")).small(),
                    )
                    .wrap(),
                );
            }
            ui.add(egui::Label::new(RichText::new(&prompt.observed).monospace()).wrap());
            if let Some(algorithm) = &prompt.observed_algorithm {
                ui.label(
                    RichText::new(format!(
                        "{}: {algorithm}",
                        catalog.text("host_key_algorithm")
                    ))
                    .small()
                    .weak(),
                );
            }
            dialog_button_row(
                ui,
                catalog.text("trust_and_retry"),
                changed,
                catalog.text("reject"),
            )
        });
        if let Some(trust) = choice {
            self.apply_fingerprint_choice(trust, context);
        }
    }

    fn import_dialog(&mut self, context: &egui::Context) {
        if !self.import_window_open {
            return;
        }
        #[derive(Clone, Copy)]
        enum ImportAction {
            Download,
            Import,
        }
        let catalog = &self.catalog;
        let result = choice_dialog(
            context,
            "import_hosts_dialog",
            catalog.text("import_title"),
            catalog.text("import_hint"),
            &[
                (catalog.text("download_template"), ImportAction::Download),
                (catalog.text("import_template"), ImportAction::Import),
            ],
            catalog.text("close"),
        );
        match result {
            DialogResult::Chosen(ImportAction::Download) => self.download_import_template(),
            DialogResult::Chosen(ImportAction::Import) => self.import_hosts_from_template(),
            DialogResult::Closed => self.import_window_open = false,
            DialogResult::Open => {}
        }
    }

    fn import_cleanup_dialog(&mut self, context: &egui::Context) {
        let Some(prompt) = self.import_cleanup_prompt.as_ref() else {
            return;
        };
        let path = prompt.path.display().to_string();
        let catalog = &self.catalog;
        let choice = confirm_dialog(
            context,
            "import_cleanup_confirmation",
            catalog.text("import_cleanup_title"),
            &catalog.format("import_cleanup_message", &[("path", path.as_str())]),
            catalog.text("delete_import_file"),
            catalog.text("keep_import_file"),
            true,
        );
        let Some(delete_file) = choice else {
            return;
        };
        let prompt = self.import_cleanup_prompt.take().unwrap();
        let path_text = prompt.path.display().to_string();
        let count = prompt.imported_count.to_string();
        if delete_file {
            match fs::remove_file(&prompt.path) {
                Ok(()) => self.set_status_format(
                    "import_file_deleted",
                    &[("count", &count), ("path", path_text.as_str())],
                ),
                Err(error) if error.kind() == io::ErrorKind::NotFound => self.set_status_format(
                    "import_file_deleted",
                    &[("count", &count), ("path", path_text.as_str())],
                ),
                Err(error) => self.set_status_format(
                    "import_file_delete_failed",
                    &[("error", &error.to_string()), ("path", path_text.as_str())],
                ),
            }
        } else {
            self.set_status_format("import_file_kept_warning", &[("path", path_text.as_str())]);
        }
    }

    fn batch_export_dialog(&mut self, context: &egui::Context) {
        if !self.batch_export_window_open {
            return;
        }
        #[derive(Clone, Copy)]
        enum ExportAction {
            Directory,
            File,
        }
        let catalog = &self.catalog;
        let result = choice_dialog(
            context,
            "batch_export_dialog",
            catalog.text("batch_export_title"),
            catalog.text("batch_export_hint"),
            &[
                (catalog.text("export_to_directory"), ExportAction::Directory),
                (catalog.text("export_to_file"), ExportAction::File),
            ],
            catalog.text("close"),
        );
        match result {
            DialogResult::Chosen(ExportAction::Directory) => self.export_batch_to_directory(),
            DialogResult::Chosen(ExportAction::File) => self.export_batch_to_file(),
            DialogResult::Closed => self.batch_export_window_open = false,
            DialogResult::Open => {}
        }
    }

    fn delete_dialog(&mut self, context: &egui::Context) {
        if !self.delete_prompt {
            return;
        }
        let catalog = &self.catalog;
        let vault_host = self
            .selected
            .and_then(|id| self.store.hosts.iter().find(|host| host.id == id))
            .is_some_and(HostProfile::is_vault_host);
        let choice = confirm_dialog(
            context,
            "delete_confirmation",
            catalog.text("delete_title"),
            catalog.text(if vault_host {
                "vault_delete_message"
            } else {
                "delete_message"
            }),
            catalog.text("confirm_delete"),
            catalog.text("cancel"),
            true,
        );
        if let Some(confirm) = choice {
            self.delete_prompt = false;
            if confirm {
                self.remove_selected();
            }
        }
    }

    /// The user picked another host (or a new one) while the form held
    /// unsaved changes: discard them and go, or stay and keep editing.
    fn unsaved_switch_dialog(&mut self, context: &egui::Context) {
        let Some(next) = self.pending_switch else {
            return;
        };
        let catalog = &self.catalog;
        let choice = confirm_dialog(
            context,
            "unsaved_switch",
            catalog.text("unsaved_switch_title"),
            catalog.text("unsaved_switch_message"),
            catalog.text("discard_changes"),
            catalog.text("keep_editing"),
            true,
        );
        if let Some(discard) = choice {
            self.pending_switch = None;
            if discard {
                self.apply_switch(next);
            }
        }
    }

    fn persistence_dialog(&mut self, context: &egui::Context) {
        let Some(prompt) = self.persistence_prompt.as_ref() else {
            return;
        };
        let catalog = &self.catalog;
        let to = persistence_label(catalog, prompt.to);
        let (title, message, confirm) = match prompt.from {
            Some(from) => (
                "persistence_change_title",
                catalog.format(
                    "persistence_change_message",
                    &[
                        ("alias", &prompt.alias),
                        ("from", &persistence_label(catalog, from)),
                        ("to", &to),
                    ],
                ),
                "persistence_change_confirm",
            ),
            None => (
                "persistence_first_title",
                catalog.format(
                    "persistence_first_message",
                    &[("alias", &prompt.alias), ("to", &to)],
                ),
                "persistence_first_confirm",
            ),
        };
        let relaxing = prompt.to != AuthPersistence::PerCall;
        let choice = dialog_shell(
            context,
            "persistence_confirmation",
            520.0,
            catalog.text(title),
            |ui| {
                ui.add(egui::Label::new(message).wrap());
                if relaxing {
                    ui.add_space(10.0);
                    ui.add(egui::Label::new(catalog.text("persistence_warning_ssh")).wrap());
                }
                dialog_button_row(ui, catalog.text(confirm), relaxing, catalog.text("cancel"))
            },
        );
        if let Some(confirm) = choice {
            self.apply_persistence_choice(confirm, context);
        }
    }

    fn batch_delete_dialog(&mut self, context: &egui::Context) {
        if !self.batch_delete_prompt {
            return;
        }
        let count = self.batch_selected.len().to_string();
        let catalog = &self.catalog;
        let choice = confirm_dialog(
            context,
            "batch_delete_confirmation",
            catalog.text("batch_delete_title"),
            &catalog.format("batch_delete_message", &[("count", count.as_str())]),
            catalog.text("confirm_delete_selected"),
            catalog.text("cancel"),
            true,
        );
        if let Some(confirm) = choice {
            self.batch_delete_prompt = false;
            if confirm {
                self.remove_batch();
            }
        }
    }
}

/// The persistence option's form label, with the idle minutes filled in.
pub(super) fn persistence_label(catalog: &Catalog, value: AuthPersistence) -> String {
    match value {
        AuthPersistence::PerCall => catalog.text("persistence_per_call").to_owned(),
        AuthPersistence::Session => catalog.text("persistence_session").to_owned(),
        AuthPersistence::Idle { minutes } => format!(
            "{} ({} {})",
            catalog.text("persistence_idle"),
            minutes,
            catalog.text("persistence_minutes")
        ),
    }
}
