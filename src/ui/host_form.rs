use eframe::egui::{self, RichText};

use super::theme::{self, control_height, input_margin};
use super::{EditorAction, HostEditor, HostsApp, PasswordMode};
use crate::i18n::Catalog;
use crate::model::{HostProfile, Protocol, SshAuth, can_use_as_jump, normalize_tags};

const STACKED_BREAKPOINT: f32 = 520.0;
const LABEL_WIDTH: f32 = 150.0;
const ROW_MARGIN: egui::Margin = egui::Margin::symmetric(10, 7);
const MAX_FORM_WIDTH: f32 = 1100.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum FormLayout {
    TwoColumn { label_width: f32, field_width: f32 },
    Stacked { field_width: f32 },
}

/// Picks the row layout from the width available inside a section, before any row is laid
/// out, so every field gets an explicit width instead of relying on per-cell measurements.
pub(super) fn form_layout_for(available: f32, spacing: f32) -> FormLayout {
    let inner = available - (ROW_MARGIN.left + ROW_MARGIN.right) as f32;
    if available < STACKED_BREAKPOINT {
        FormLayout::Stacked {
            field_width: inner.max(120.0),
        }
    } else {
        FormLayout::TwoColumn {
            label_width: LABEL_WIDTH,
            field_width: (inner - LABEL_WIDTH - spacing).max(120.0),
        }
    }
}

struct Form<'a> {
    layout: FormLayout,
    catalog: &'a Catalog,
    row_index: usize,
}

impl<'a> Form<'a> {
    fn new(ui: &egui::Ui, catalog: &'a Catalog) -> Self {
        Self {
            layout: form_layout_for(ui.available_width(), ui.spacing().item_spacing.x),
            catalog,
            row_index: 0,
        }
    }

    fn row(
        &mut self,
        ui: &mut egui::Ui,
        label_key: &str,
        hint: Option<String>,
        add_field: impl FnOnce(&mut egui::Ui, f32),
    ) {
        let striped = self.row_index % 2 == 1;
        self.row_index += 1;
        let fill = if striped {
            ui.visuals().faint_bg_color
        } else {
            egui::Color32::TRANSPARENT
        };
        let label = RichText::new(self.catalog.text(label_key)).strong();
        egui::Frame::new()
            .fill(fill)
            .corner_radius(4.0)
            .inner_margin(ROW_MARGIN)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                match self.layout {
                    FormLayout::TwoColumn {
                        label_width,
                        field_width,
                    } => {
                        ui.horizontal_top(|ui| {
                            ui.allocate_ui_with_layout(
                                egui::vec2(label_width, ui.spacing().interact_size.y),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.set_width(label_width);
                                    ui.add(egui::Label::new(label).truncate());
                                },
                            );
                            ui.vertical(|ui| {
                                add_field(ui, field_width);
                                if let Some(hint) = hint {
                                    ui.label(RichText::new(hint).small().weak());
                                }
                            });
                        });
                    }
                    FormLayout::Stacked { field_width } => {
                        ui.label(label);
                        add_field(ui, field_width);
                        if let Some(hint) = hint {
                            ui.label(RichText::new(hint).small().weak());
                        }
                    }
                }
            });
    }
}

fn section(ui: &mut egui::Ui, title: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(theme::section_fill(ui.visuals()))
        .stroke(theme::section_stroke(ui.visuals()))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).strong().size(15.0));
            ui.add_space(2.0);
            ui.separator();
            ui.add_space(4.0);
            add_contents(ui);
        });
    ui.add_space(12.0);
}

fn single_line<'t>(ui: &egui::Ui, value: &'t mut String, width: f32) -> egui::TextEdit<'t> {
    egui::TextEdit::singleline(value)
        .desired_width(width)
        .margin(input_margin(ui))
        .vertical_align(egui::Align::Center)
}

fn text_field(ui: &mut egui::Ui, value: &mut String, width: f32) -> egui::Response {
    let edit = single_line(ui, value, width);
    ui.add(edit)
}

fn basic_section(ui: &mut egui::Ui, editor: &mut HostEditor, catalog: &Catalog) {
    section(ui, catalog.text("section_basic"), |ui| {
        let mut form = Form::new(ui, catalog);
        form.row(ui, "alias", None, |ui, width| {
            text_field(ui, &mut editor.profile.alias, width);
        });
        form.row(ui, "description", None, |ui, width| {
            let margin = input_margin(ui);
            ui.add(
                egui::TextEdit::multiline(&mut editor.profile.description)
                    .desired_width(width)
                    .desired_rows(3)
                    .margin(margin),
            );
        });
        form.row(ui, "tags", None, |ui, width| {
            ui.set_max_width(width);
            let control = control_height(ui);
            ui.allocate_ui_with_layout(
                egui::vec2(width, control),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    let add = ui.add_sized(
                        [ui.spacing().interact_size.x.max(72.0), control],
                        egui::Button::new(catalog.text("add_tag")),
                    );
                    let input_width = ui.available_width();
                    let input = ui.add(
                        single_line(ui, &mut editor.tag_input, input_width)
                            .hint_text(catalog.text("tag_hint")),
                    );
                    let enter =
                        input.lost_focus() && ui.input(|state| state.key_pressed(egui::Key::Enter));
                    if add.clicked() || enter {
                        editor
                            .profile
                            .tags
                            .push(std::mem::take(&mut editor.tag_input));
                        editor.profile.tags = normalize_tags(&editor.profile.tags);
                    }
                },
            );
            if !editor.profile.tags.is_empty() {
                let mut remove = None;
                ui.horizontal_wrapped(|ui| {
                    for (index, tag) in editor.profile.tags.iter().enumerate() {
                        if ui
                            .add(egui::Button::new(format!("{tag} ×")).small().truncate())
                            .clicked()
                        {
                            remove = Some(index);
                        }
                    }
                });
                if let Some(index) = remove {
                    editor.profile.tags.remove(index);
                }
            }
        });
    });
}

fn connection_section(
    ui: &mut egui::Ui,
    editor: &mut HostEditor,
    catalog: &Catalog,
    hosts: &[HostProfile],
) {
    section(ui, catalog.text("section_connection"), |ui| {
        let mut form = Form::new(ui, catalog);
        form.row(ui, "protocol", None, |ui, _| {
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
        });
        form.row(ui, "address", None, |ui, width| {
            ui.horizontal(|ui| {
                let port_width = 72.0;
                let port_label_width = ui
                    .painter()
                    .layout_no_wrap(
                        catalog.text("port").to_owned(),
                        egui::TextStyle::Body.resolve(ui.style()),
                        egui::Color32::PLACEHOLDER,
                    )
                    .size()
                    .x;
                let spacing = ui.spacing().item_spacing.x;
                let address_width =
                    (width - port_width - port_label_width - spacing * 3.0).max(120.0);
                text_field(ui, &mut editor.profile.address, address_width);
                ui.add_space(spacing);
                ui.label(RichText::new(catalog.text("port")).strong());
                ui.add_sized(
                    [port_width, control_height(ui)],
                    egui::DragValue::new(&mut editor.profile.port).range(1..=65535),
                );
            });
        });
        form.row(ui, "username", None, |ui, width| {
            text_field(ui, &mut editor.profile.username, width);
        });
        if editor.profile.protocol == Protocol::Ssh {
            form.row(
                ui,
                "host_chain",
                Some(catalog.text("chain_hint").to_owned()),
                |ui, width| {
                    let selected_name = editor
                        .profile
                        .jump_host
                        .and_then(|id| hosts.iter().find(|host| host.id == id))
                        .map(|host| host.alias.as_str())
                        .unwrap_or(catalog.text("direct_connection"));
                    egui::ComboBox::from_id_salt("jump_host")
                        .selected_text(selected_name)
                        .width(width.min(360.0))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut editor.profile.jump_host,
                                None,
                                catalog.text("direct_connection"),
                            );
                            let mut count = 0;
                            for candidate in hosts {
                                if can_use_as_jump(candidate, &editor.profile, hosts) {
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
                                    egui::Label::new(catalog.text("no_verified_hosts")),
                                );
                            }
                        });
                },
            );
        }
    });
}

fn auth_section(
    ui: &mut egui::Ui,
    editor: &mut HostEditor,
    catalog: &Catalog,
    action: &mut Option<EditorAction>,
) {
    section(ui, catalog.text("section_auth"), |ui| {
        let mut form = Form::new(ui, catalog);
        if editor.profile.protocol == Protocol::Ssh {
            form.row(ui, "auth_method", None, |ui, width| {
                egui::ComboBox::from_id_salt("ssh_auth")
                    .selected_text(match editor.profile.ssh_auth {
                        SshAuth::Password => catalog.text("password_auth"),
                        SshAuth::PrivateKey => catalog.text("private_key_auth"),
                        SshAuth::SshAgent => catalog.text("ssh_agent_auth"),
                    })
                    .width(width.min(360.0))
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
            });
        }

        if editor.profile.protocol == Protocol::Telnet
            || editor.profile.ssh_auth == SshAuth::Password
        {
            form.row(ui, "password_mode", None, |ui, _| {
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
            });
            let password_hint = if editor.password_mode == PasswordMode::NoPassword {
                catalog.text("no_password_hint").to_owned()
            } else if let Some(error) = editor.password_read_error.as_deref() {
                catalog.format("credential_error", &[("error", error)])
            } else if editor.saved_password_mode == Some(PasswordMode::Password) {
                catalog.text("password_saved").to_owned()
            } else {
                catalog.text("password_required").to_owned()
            };
            form.row(ui, "password", Some(password_hint), |ui, width| {
                let edit = single_line(ui, &mut editor.password, width).password(true);
                ui.add_enabled(editor.password_mode == PasswordMode::Password, edit);
            });
        } else if editor.profile.ssh_auth == SshAuth::PrivateKey {
            form.row(
                ui,
                "private_key",
                Some(format!(
                    "{}\n{}",
                    catalog.text("private_key_hint"),
                    catalog.text("fido_direct_hint")
                )),
                |ui, width| {
                    text_field(ui, &mut editor.profile.private_key_path, width);
                    ui.horizontal_wrapped(|ui| {
                        if ui.button(catalog.text("browse_private_key")).clicked() {
                            *action = Some(EditorAction::BrowsePrivateKey);
                        }
                        if ui.button(catalog.text("find_fido_key")).clicked() {
                            *action = Some(EditorAction::DiscoverFido);
                        }
                        if ui.button(catalog.text("setup_fido_key")).clicked() {
                            *action = Some(EditorAction::OpenFidoSetup);
                        }
                    });
                },
            );
            let passphrase_hint = if let Some(error) = editor.key_passphrase_read_error.as_deref() {
                catalog.format("credential_error", &[("error", error)])
            } else if editor.has_key_passphrase {
                catalog.text("passphrase_saved").to_owned()
            } else {
                catalog.text("passphrase_optional").to_owned()
            };
            form.row(ui, "key_passphrase", Some(passphrase_hint), |ui, width| {
                let edit = single_line(ui, &mut editor.key_passphrase, width).password(true);
                ui.add(edit);
            });
        } else {
            form.row(
                ui,
                "agent_key_fingerprint",
                Some(catalog.text("agent_key_hint").to_owned()),
                |ui, width| {
                    text_field(ui, &mut editor.profile.agent_key_fingerprint, width);
                },
            );
        }
    });
}

fn host_key_section(ui: &mut egui::Ui, editor: &HostEditor, catalog: &Catalog) {
    section(ui, catalog.text("section_host_key"), |ui| {
        match editor.profile.host_fingerprint.as_deref() {
            Some(fingerprint) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("✓").color(theme::success_color(ui.visuals())));
                    ui.add(egui::Label::new(RichText::new(fingerprint).monospace()).wrap());
                });
                if let Some(algorithm) = &editor.profile.host_key_algorithm {
                    ui.label(
                        RichText::new(format!(
                            "{}: {algorithm}",
                            catalog.text("host_key_algorithm")
                        ))
                        .small()
                        .weak(),
                    );
                }
            }
            None => {
                ui.label(RichText::new(catalog.text("host_key_unverified")).weak());
            }
        }
    });
}

fn warning_callout(ui: &mut egui::Ui, text: &str) {
    let color = ui.visuals().warn_fg_color;
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, color))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.label(RichText::new("!").strong().color(color));
                ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
            });
        });
    ui.add_space(12.0);
}

impl HostsApp {
    pub(super) fn editor_panel(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        let catalog = self.catalog.clone();
        let hosts = self.store.hosts.clone();
        let testing = self.testing_all
            || self.selected.is_some_and(|id| {
                self.test_operations.contains_key(&id) || self.pending_tests.contains(&id)
            });
        let codex_edit = self.launch.codex_edit;
        let mut action = None;

        if self.editor.is_some() {
            egui::Panel::bottom("editor_actions")
                .frame(
                    egui::Frame::new()
                        .fill(ui.visuals().panel_fill)
                        .inner_margin(egui::Margin::symmetric(20, 10)),
                )
                .show_separator_line(true)
                .show(ui, |ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(!testing, egui::Button::new(catalog.text("save")))
                            .clicked()
                        {
                            action = Some(EditorAction::Save);
                        }
                        if ui
                            .add_enabled(
                                !testing,
                                egui::Button::new(if testing {
                                    catalog.text("testing")
                                } else {
                                    catalog.text("test_connection")
                                }),
                            )
                            .clicked()
                        {
                            action = Some(EditorAction::Test);
                        }
                        if codex_edit && ui.button(catalog.text("cancel")).clicked() {
                            action = Some(EditorAction::CancelCodex);
                        }
                        if testing {
                            ui.spinner();
                        }
                    });
                });
        }

        egui::ScrollArea::vertical()
            .id_salt("host_editor_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(16.0);
                let form_width = (ui.available_width() - 40.0).min(MAX_FORM_WIDTH);
                let margin = ((ui.available_width() - form_width) / 2.0).max(20.0);
                ui.horizontal(|ui| {
                    ui.add_space(margin);
                    ui.vertical(|ui| {
                        ui.set_max_width(form_width);
                        self.editor_contents(ui, &catalog, &hosts, codex_edit, &mut action);
                    });
                });
                ui.add_space(16.0);
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
                    Err(error) => self.set_status_format("callback_error", &[("error", &error)]),
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
                if let Some(identity) = crate::fido::discover_handles().into_iter().next() {
                    if let Some(editor) = &mut self.editor {
                        editor.profile.private_key_path = identity.path.display().to_string();
                    }
                    self.set_status_format(
                        "fido_found",
                        &[("fingerprint", identity.fingerprint.as_str())],
                    );
                } else {
                    self.set_status_key("fido_not_found");
                }
            }
            Some(EditorAction::OpenFidoSetup) => self.open_fido_setup(),
            None => {}
        }
    }

    fn editor_contents(
        &mut self,
        ui: &mut egui::Ui,
        catalog: &Catalog,
        hosts: &[HostProfile],
        codex_edit: bool,
        action: &mut Option<EditorAction>,
    ) {
        let Some(editor) = self.editor.as_mut() else {
            ui.heading(catalog.text("editor_new_title"));
            ui.add_space(8.0);
            ui.label(RichText::new(catalog.text("editor_empty_hint")).weak());
            return;
        };
        ui.horizontal(|ui| {
            ui.heading(if editor.original.alias.is_empty() {
                catalog.text("editor_new_title")
            } else {
                catalog.text("editor_edit_title")
            });
            if editor.connection_changed() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let color = ui.visuals().warn_fg_color;
                    egui::Frame::new()
                        .stroke(egui::Stroke::new(1.0, color))
                        .corner_radius(12.0)
                        .inner_margin(egui::Margin::symmetric(10, 4))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(catalog.text("unsaved_changes_short"))
                                    .small()
                                    .color(color),
                            );
                        })
                        .response
                        .on_hover_text(catalog.text("unsaved_changes"));
                });
            }
        });
        ui.label(RichText::new(catalog.text("editor_subtitle")).weak());
        ui.add_space(12.0);
        if codex_edit {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.label(RichText::new(catalog.text("codex_draft")).strong());
                ui.label(catalog.text("secret_not_exported"));
            });
            ui.add_space(12.0);
        }
        basic_section(ui, editor, catalog);
        connection_section(ui, editor, catalog, hosts);
        auth_section(ui, editor, catalog, action);
        if editor.profile.protocol == Protocol::Ssh {
            host_key_section(ui, editor, catalog);
        } else {
            warning_callout(ui, catalog.text("telnet_warning"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{find_text_rect, metadata_test_app};
    use super::*;

    fn run_form(app: &mut HostsApp, context: &egui::Context, width: f32) -> egui::FullOutput {
        let mut output = None;
        for frame in 0..3 {
            output = Some(context.run_ui(
                egui::RawInput {
                    // Tall enough that every section is inside the scroll viewport.
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1040.0, 1600.0),
                    )),
                    time: Some(frame as f64 * 0.1),
                    ..Default::default()
                },
                |ui| {
                    ui.set_max_width(width);
                    app.editor_panel(ui, context);
                },
            ));
        }
        output.unwrap()
    }

    fn metadata_profile() -> HostProfile {
        HostProfile {
            alias: "example".into(),
            description: "metadata notes\nsecond line".into(),
            tags: vec!["prod".into(), "web".into()],
            ..Default::default()
        }
    }

    #[test]
    fn metadata_controls_render_in_all_locales_without_overlapping() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            let context = egui::Context::default();
            theme::configure_fonts(&context, locale);
            let mut app = metadata_test_app(&context, locale, metadata_profile());
            let output = run_form(&mut app, &context, 690.0);
            let rect = |text: &str| {
                find_text_rect(&output.shapes, text)
                    .unwrap_or_else(|| panic!("missing {locale} label {text}"))
            };
            let description = rect(app.catalog.text("description"));
            let tags = rect(app.catalog.text("tags"));
            let chip = rect("prod ×");
            assert!(tags.top() > description.bottom());
            assert!(chip.left() > tags.right());
            assert!(chip.right() < 690.0);
        }
    }

    #[test]
    fn metadata_controls_stack_labels_when_narrow() {
        let context = egui::Context::default();
        theme::configure_fonts(&context, "en");
        let mut app = metadata_test_app(&context, "en", metadata_profile());
        let output = run_form(&mut app, &context, 420.0);
        let rect = |text: &str| {
            find_text_rect(&output.shapes, text).unwrap_or_else(|| panic!("missing {text}"))
        };
        let tags = rect(app.catalog.text("tags"));
        let chip = rect("prod ×");
        assert!(chip.top() > tags.bottom());
        assert!(chip.right() < 420.0);
    }

    #[test]
    fn form_sections_render_titles_in_order_for_all_locales() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            let context = egui::Context::default();
            theme::configure_fonts(&context, locale);
            let mut app = metadata_test_app(&context, locale, metadata_profile());
            let output = run_form(&mut app, &context, 760.0);
            let mut last_top = f32::MIN;
            for key in [
                "section_basic",
                "section_connection",
                "section_auth",
                "section_host_key",
            ] {
                let rect = find_text_rect(&output.shapes, app.catalog.text(key))
                    .unwrap_or_else(|| panic!("missing {locale} section {key}"));
                assert!(
                    rect.top() > last_top,
                    "{key} must follow the previous section"
                );
                last_top = rect.top();
            }
        }
    }

    #[test]
    fn layout_switches_to_stacked_below_breakpoint() {
        assert!(matches!(
            form_layout_for(420.0, 8.0),
            FormLayout::Stacked { .. }
        ));
        match form_layout_for(700.0, 8.0) {
            FormLayout::TwoColumn {
                label_width,
                field_width,
            } => {
                assert_eq!(label_width, LABEL_WIDTH);
                assert!(field_width > 400.0);
            }
            other => panic!("expected two columns, got {other:?}"),
        }
    }
}
