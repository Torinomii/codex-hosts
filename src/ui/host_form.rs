use eframe::egui::{self, RichText};

use super::theme::{self, control_height, input_margin};
use super::{EditorAction, HostEditor, HostsApp, PasswordMode};
use crate::i18n::Catalog;
use crate::model::{
    AuthPersistence, DEFAULT_CHANNELS_PER_HOST, DEFAULT_KEEPALIVE_SECONDS, HostProfile,
    MAX_CHANNELS_PER_HOST, MAX_IDLE_PERSISTENCE_MINUTES, MAX_KEEPALIVE_SECONDS,
    MIN_KEEPALIVE_SECONDS, Protocol, RemoteEnv, SshAuth, TelnetPrompts, can_use_as_jump,
    normalize_tags,
};

const STACKED_BREAKPOINT: f32 = 520.0;
const LABEL_WIDTH: f32 = 168.0;
const ROW_MARGIN: egui::Margin = egui::Margin::symmetric(10, 7);
const MAX_FORM_WIDTH: f32 = 1100.0;
/// Stands in for a saved login password while the empty field is not focused.
const SAVED_PASSWORD_MASK: &str = "******";

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

/// How a row announces itself: optional, required, or required and currently empty.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Requirement {
    Optional,
    Required,
    Missing,
}

pub(super) fn required_color(visuals: &egui::Visuals) -> egui::Color32 {
    visuals.warn_fg_color
}

/// Stable id for a required text field so a failed save can move focus to it.
fn required_field_id(label_key: &str) -> egui::Id {
    egui::Id::new(("required_field", label_key))
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
        self.row_with(ui, label_key, Requirement::Optional, hint, add_field);
    }

    fn required_row(
        &mut self,
        ui: &mut egui::Ui,
        label_key: &str,
        missing: bool,
        hint: Option<String>,
        add_field: impl FnOnce(&mut egui::Ui, f32),
    ) {
        let requirement = if missing {
            Requirement::Missing
        } else {
            Requirement::Required
        };
        self.row_with(ui, label_key, requirement, hint, add_field);
    }

    fn row_with(
        &mut self,
        ui: &mut egui::Ui,
        label_key: &str,
        requirement: Requirement,
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
        let accent = required_color(ui.visuals());
        let stroke = if requirement == Requirement::Missing {
            egui::Stroke::new(1.0, accent)
        } else {
            egui::Stroke::NONE
        };
        let label = RichText::new(self.catalog.text(label_key)).strong();
        let star = (requirement != Requirement::Optional)
            .then(|| RichText::new("*").strong().color(accent));
        let missing_hint = (requirement == Requirement::Missing)
            .then(|| self.catalog.text("required_hint").to_owned());
        // The label wraps inside its column (the star takes the rest of the
        // width) instead of being cut with an ellipsis.
        let label_cell = |ui: &mut egui::Ui, wrap: bool| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let mut label = egui::Label::new(label);
            if wrap {
                let star_width = ui.spacing().interact_size.y * 0.5;
                ui.set_max_width(ui.available_width());
                label = label.wrap();
                ui.scope(|ui| {
                    ui.set_max_width((ui.available_width() - star_width).max(40.0));
                    ui.add(label);
                });
            } else {
                ui.add(label);
            }
            if let Some(star) = star {
                ui.label(star);
            }
        };
        let hints = |ui: &mut egui::Ui| {
            if let Some(missing_hint) = missing_hint {
                ui.label(RichText::new(missing_hint).small().color(accent));
            }
            if let Some(hint) = hint {
                ui.label(RichText::new(hint).small().weak());
            }
        };
        egui::Frame::new()
            .fill(fill)
            .stroke(stroke)
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
                                    label_cell(ui, true);
                                },
                            );
                            ui.vertical(|ui| {
                                add_field(ui, field_width);
                                hints(ui);
                            });
                        });
                    }
                    FormLayout::Stacked { field_width } => {
                        ui.horizontal(|ui| label_cell(ui, false));
                        add_field(ui, field_width);
                        hints(ui);
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

/// A required text field keeps a stable id so a failed save can focus it.
fn required_text_field(
    ui: &mut egui::Ui,
    label_key: &str,
    value: &mut String,
    width: f32,
) -> egui::Response {
    let edit = single_line(ui, value, width).id(required_field_id(label_key));
    ui.add(edit)
}

/// Checkbox plus number: unchecked means "use the default", which the field then shows greyed.
fn optional_number<T: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    value: &mut Option<T>,
    default: T,
    range: std::ops::RangeInclusive<T>,
    suffix: &str,
) {
    ui.horizontal(|ui| {
        let mut custom = value.is_some();
        if ui
            .checkbox(&mut custom, catalog.text("custom_value"))
            .changed()
        {
            *value = custom.then_some(value.unwrap_or(default));
        }
        let mut shown = value.unwrap_or(default);
        let response = ui.add_enabled(
            custom,
            egui::DragValue::new(&mut shown).range(range).suffix(suffix),
        );
        if custom && response.changed() {
            *value = Some(shown);
        }
    });
}

fn basic_section(
    ui: &mut egui::Ui,
    editor: &mut HostEditor,
    catalog: &Catalog,
    testing_all: bool,
    action: &mut Option<EditorAction>,
) {
    section(ui, catalog.text("section_basic"), |ui| {
        let mut form = Form::new(ui, catalog);
        let missing = editor.show_required && editor.profile.alias.trim().is_empty();
        form.required_row(ui, "alias", missing, None, |ui, width| {
            required_text_field(ui, "alias", &mut editor.profile.alias, width);
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
            // Test All writes the store when it finishes, so tags wait like Save does.
            ui.add_enabled_ui(!testing_all, |ui| {
                tags_field(ui, editor, catalog, width, action);
            });
        });
    });
}

/// Adding or removing a tag asks for the tags alone to be saved at once.
fn tags_field(
    ui: &mut egui::Ui,
    editor: &mut HostEditor,
    catalog: &Catalog,
    width: f32,
    action: &mut Option<EditorAction>,
) {
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
            let enter = input.lost_focus() && ui.input(|state| state.key_pressed(egui::Key::Enter));
            if add.clicked() || enter {
                let tags = normalize_tags(
                    editor
                        .profile
                        .tags
                        .iter()
                        .chain(std::iter::once(&editor.tag_input)),
                );
                editor.tag_input.clear();
                if tags != editor.profile.tags {
                    editor.profile.tags = tags;
                    *action = Some(EditorAction::SaveTags);
                }
            }
        },
    );
    if !editor.profile.tags.is_empty() {
        let mut remove = None;
        ui.horizontal_wrapped(|ui| {
            for (index, tag) in editor.profile.tags.iter().enumerate() {
                if tag_chip(ui, tag).clicked() {
                    remove = Some(index);
                }
            }
        });
        if let Some(index) = remove {
            editor.profile.tags.remove(index);
            *action = Some(EditorAction::SaveTags);
        }
    }
}

/// A removable tag pill. The text is centred on its painted glyph bounds rather
/// than on the font's line box: the CJK fallback fonts have tall line boxes,
/// which left a small button's text sitting visibly below the middle.
fn tag_chip(ui: &mut egui::Ui, tag: &str) -> egui::Response {
    let padding = egui::vec2(8.0, 3.0);
    let font = egui::TextStyle::Body.resolve(ui.style());
    let max_width = (ui.available_width() - 2.0 * padding.x).max(40.0);
    let galley = ui.painter().layout_job(egui::text::LayoutJob {
        wrap: egui::text::TextWrapping::truncate_at_width(max_width),
        ..egui::text::LayoutJob::simple_singleline(
            format!("{tag} ×"),
            font,
            ui.visuals().text_color(),
        )
    });
    let glyphs = galley.mesh_bounds;
    let height = glyphs.height().max(ui.spacing().interact_size.y * 0.6) + 2.0 * padding.y;
    let size = egui::vec2(galley.rect.width() + 2.0 * padding.x, height);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        ui.painter().rect(
            rect,
            visuals.corner_radius,
            visuals.weak_bg_fill,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
        let pos = rect.center() - glyphs.center().to_vec2();
        ui.painter().galley(pos, galley, visuals.text_color());
    }
    response
}

fn connection_section(
    ui: &mut egui::Ui,
    editor: &mut HostEditor,
    catalog: &Catalog,
    hosts: &[HostProfile],
) {
    section(ui, catalog.text("section_connection"), |ui| {
        let mut form = Form::new(ui, catalog);
        form.required_row(ui, "protocol", false, None, |ui, _| {
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
        let address_missing = editor.show_required
            && (editor.profile.address.trim().is_empty() || editor.profile.port == 0);
        form.required_row(ui, "address", address_missing, None, |ui, width| {
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
                required_text_field(ui, "address", &mut editor.profile.address, address_width);
                ui.add_space(spacing);
                ui.label(RichText::new(catalog.text("port")).strong());
                ui.label(
                    RichText::new("*")
                        .strong()
                        .color(required_color(ui.visuals())),
                );
                ui.add_sized(
                    [port_width, control_height(ui)],
                    egui::DragValue::new(&mut editor.profile.port).range(1..=65535),
                );
            });
        });
        let username_missing = editor.show_required && editor.profile.username.trim().is_empty();
        form.required_row(ui, "username", username_missing, None, |ui, width| {
            required_text_field(ui, "username", &mut editor.profile.username, width);
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
            form.required_row(ui, "auth_method", false, None, |ui, width| {
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
            // The same choice for both stores; a vault host writes the
            // outcome into its item instead of Credential Manager.
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
            } else if let Some(texts) = editor.vault_texts().filter(|_| !editor.draft) {
                catalog.text(texts.password_hint).to_owned()
            } else if !editor.draft && editor.login_user_changed() {
                catalog.text("login_password_user_changed").to_owned()
            } else if let Some(error) = editor.password_read_error.as_deref() {
                catalog.format("credential_error", &[("error", error)])
            } else if editor.login_password_saved() {
                catalog.text("password_saved").to_owned()
            } else {
                catalog.text("password_required").to_owned()
            };
            form.row(ui, "password", Some(password_hint), |ui, width| {
                let id = egui::Id::new("login_password");
                // The mask leaves on focus, so a new password is typed into an empty field.
                let masked =
                    editor.login_password_saved() && !ui.memory(|memory| memory.has_focus(id));
                let enabled = editor.password_mode == PasswordMode::Password && !editor.saving;
                ui.horizontal(|ui| {
                    // Per host, so another host's form starts masked.
                    let key = egui::Id::new(("login_password", editor.profile.id));
                    let revealed = theme::password_revealed(ui, key);
                    let field_width = width - control_height(ui) - ui.spacing().item_spacing.x;
                    let mut edit = single_line(ui, &mut editor.password, field_width)
                        .password(!revealed)
                        .id(id);
                    if masked {
                        edit = edit.hint_text(SAVED_PASSWORD_MASK);
                    }
                    let response = ui.add_enabled(enabled, edit);
                    theme::forget_password_history(ui, &response);
                    ui.add_enabled_ui(enabled, |ui| theme::reveal_button(ui, catalog, key));
                    if masked && response.has_focus() {
                        ui.ctx().request_repaint();
                    }
                });
            });
        } else if editor.profile.ssh_auth == SshAuth::PrivateKey {
            let key_missing =
                editor.show_required && editor.profile.private_key_path.trim().is_empty();
            form.required_row(
                ui,
                "private_key",
                key_missing,
                Some(format!(
                    "{}\n{}",
                    catalog.text("private_key_hint"),
                    catalog.text("fido_direct_hint")
                )),
                |ui, width| {
                    required_text_field(
                        ui,
                        "private_key",
                        &mut editor.profile.private_key_path,
                        width,
                    );
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
                ui.horizontal(|ui| {
                    let key = egui::Id::new(("key_passphrase", editor.profile.id));
                    let revealed = theme::password_revealed(ui, key);
                    let field_width = width - control_height(ui) - ui.spacing().item_spacing.x;
                    let edit = single_line(ui, &mut editor.key_passphrase, field_width)
                        .password(!revealed);
                    let response = ui.add(edit);
                    theme::forget_password_history(ui, &response);
                    theme::reveal_button(ui, catalog, key);
                });
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
        persistence_rows(ui, &mut form, editor, catalog);
    });
}

/// The mandatory three-way choice. Telnet has no session layer yet, so its
/// profiles show the choice greyed out at "every operation".
fn persistence_rows(
    ui: &mut egui::Ui,
    form: &mut Form<'_>,
    editor: &mut HostEditor,
    catalog: &Catalog,
) {
    let telnet = editor.profile.protocol == Protocol::Telnet;
    let hint = telnet.then(|| catalog.text("persistence_telnet_note").to_owned());
    form.required_row(ui, "auth_persistence", false, hint, |ui, width| {
        ui.set_max_width(width);
        let current = if telnet {
            AuthPersistence::PerCall
        } else {
            editor.profile.auth_persistence
        };
        // Telnet always re-authenticates, so the fixed option shows as chosen.
        let mut selection = Some(if telnet {
            AuthPersistence::PerCall
        } else {
            current
        });
        let idle_minutes = match current {
            AuthPersistence::Idle { minutes } => minutes,
            _ => 15,
        };
        ui.add_enabled_ui(!telnet, |ui| {
            ui.vertical(|ui| {
                let options = [
                    (AuthPersistence::PerCall, "persistence_per_call"),
                    (AuthPersistence::Session, "persistence_session"),
                    (
                        AuthPersistence::Idle {
                            minutes: idle_minutes,
                        },
                        "persistence_idle",
                    ),
                ];
                for (option, key) in options {
                    let selected = selection.is_some_and(|value| {
                        std::mem::discriminant(&value) == std::mem::discriminant(&option)
                    });
                    if ui.radio(selected, catalog.text(key)).clicked() {
                        selection = Some(option);
                    }
                }
            });
        });
        if let Some(choice) = selection {
            editor.profile.auth_persistence = choice;
        }
        if let AuthPersistence::Idle { minutes } = &mut editor.profile.auth_persistence
            && !telnet
        {
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(minutes)
                        .range(1..=MAX_IDLE_PERSISTENCE_MINUTES)
                        .suffix(format!(" {}", catalog.text("persistence_minutes"))),
                );
            });
        }
    });
    if !telnet && editor.profile.auth_persistence != AuthPersistence::PerCall {
        warning_callout(ui, catalog.text("persistence_warning_ssh"));
    }
}

/// Collapsed by default; the header summarises anything set away from its default so a
/// tuned host is recognisable without expanding the card.
fn advanced_section(
    ui: &mut egui::Ui,
    editor: &mut HostEditor,
    catalog: &Catalog,
    action: &mut Option<EditorAction>,
) {
    let summary = advanced_summary(&editor.profile, catalog);
    let customised = !editor.profile.advanced.is_default();
    egui::Frame::new()
        .fill(theme::section_fill(ui.visuals()))
        .stroke(theme::section_stroke(ui.visuals()))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let title = RichText::new(catalog.text("section_advanced"))
                .strong()
                .size(15.0);
            egui::CollapsingHeader::new(title)
                .id_salt("advanced_section")
                .default_open(false)
                .show_unindented(ui, |ui| {
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(4.0);
                    advanced_rows(ui, editor, catalog);
                    sudo_rows(ui, editor, catalog, action);
                });
            let summary_text = if customised {
                RichText::new(summary).small().strong()
            } else {
                RichText::new(summary).small().weak()
            };
            ui.add(egui::Label::new(summary_text).wrap());
        });
    ui.add_space(12.0);
}

fn advanced_summary(profile: &HostProfile, catalog: &Catalog) -> String {
    let advanced = &profile.advanced;
    let mut parts = Vec::new();
    if let Some(value) = advanced.max_channels {
        parts.push(format!("{} {value}", catalog.text("max_channels")));
    }
    if let Some(value) = advanced.connect_timeout_s {
        parts.push(format!(
            "{} {value}s",
            catalog.text("connect_timeout_default")
        ));
    }
    if let Some(value) = advanced.command_timeout_s {
        parts.push(format!(
            "{} {value}s",
            catalog.text("command_timeout_default")
        ));
    }
    if let Some(value) = advanced.keepalive_s {
        parts.push(format!("{} {value}s", catalog.text("keepalive_interval")));
    }
    if advanced.remote_env != RemoteEnv::Auto {
        parts.push(catalog.text(remote_env_key(advanced.remote_env)).to_owned());
    }
    if advanced.codex_hidden {
        parts.push(catalog.text("codex_hidden").to_owned());
    }
    if profile.protocol == Protocol::Ssh && advanced.require_call_approval {
        parts.push(catalog.text("ssh_call_approval").to_owned());
    }
    if advanced.telnet_prompts.is_some() {
        parts.push(catalog.text("telnet_prompts").to_owned());
    }
    if advanced
        .sudo
        .as_ref()
        .is_some_and(|settings| settings.enabled)
    {
        parts.push("sudo".into());
    }
    if advanced
        .su
        .as_ref()
        .is_some_and(|settings| settings.enabled)
    {
        parts.push("su".into());
    }
    let count = advanced
        .sudo
        .as_ref()
        .map_or(0, |settings| settings.credentials.len());
    if count != 0 {
        parts.push(catalog.format("privilege_user_count", &[("count", &count.to_string())]));
    }
    if parts.is_empty() {
        catalog.text("advanced_default_summary").to_owned()
    } else {
        parts.join(" · ")
    }
}

fn sudo_rows(
    ui: &mut egui::Ui,
    editor: &mut HostEditor,
    catalog: &Catalog,
    _action: &mut Option<EditorAction>,
) {
    if editor.profile.protocol != Protocol::Ssh {
        return;
    }
    let mut form = Form::new(ui, catalog);
    let mut sudo_enabled = editor
        .profile
        .advanced
        .sudo
        .as_ref()
        .is_some_and(|s| s.enabled);
    form.row(
        ui,
        "sudo_enabled",
        Some(catalog.text("sudo_hint").to_owned()),
        |ui, _| {
            let response = ui.checkbox(&mut sudo_enabled, "");
            #[cfg(test)]
            ui.ctx()
                .data_mut(|d| d.insert_temp(egui::Id::new("sudo_enabled_rect"), response.rect));
            if response.changed() {
                editor
                    .profile
                    .advanced
                    .sudo
                    .get_or_insert_with(Default::default)
                    .enabled = sudo_enabled;
            }
        },
    );
    let mut su_enabled = editor
        .profile
        .advanced
        .su
        .as_ref()
        .is_some_and(|s| s.enabled);
    form.row(
        ui,
        "su_enabled",
        Some(catalog.text("su_hint").to_owned()),
        |ui, _| {
            let response = ui.checkbox(&mut su_enabled, "");
            #[cfg(test)]
            ui.ctx()
                .data_mut(|d| d.insert_temp(egui::Id::new("su_enabled_rect"), response.rect));
            if response.changed() {
                editor
                    .profile
                    .advanced
                    .su
                    .get_or_insert_with(Default::default)
                    .enabled = su_enabled;
            }
        },
    );
    let current_id = current_user_id(editor);
    let has_override = editor
        .profile
        .advanced
        .sudo
        .as_ref()
        .is_some_and(|s| s.credentials.iter().any(|c| c.id == current_id));
    if (sudo_enabled || su_enabled)
        && (editor.profile.ssh_auth != SshAuth::Password || has_override)
    {
        let response = egui::CollapsingHeader::new(catalog.text("sudo_system_password"))
            .id_salt(("sudo_system_password", editor.profile.id))
            .default_open(false)
            .show_unindented(ui, |ui| {
                current_user_password(ui, editor, catalog, true);
            });
        #[cfg(test)]
        ui.ctx().data_mut(|d| {
            d.insert_temp(
                egui::Id::new("sudo_system_password_header"),
                response.header_response.id,
            )
        });
        let _ = response;
    }
    let has_accounts = editor
        .profile
        .advanced
        .sudo
        .as_ref()
        .is_some_and(|s| s.credentials.iter().any(|c| c.id != current_id));
    if sudo_enabled || su_enabled || has_accounts {
        let response = egui::CollapsingHeader::new(catalog.text("privilege_other_passwords"))
            .id_salt(("privilege_other_passwords", editor.profile.id))
            .default_open(false)
            .show_unindented(ui, |ui| {
                ui.label(
                    RichText::new(catalog.text("su_accounts_hint"))
                        .small()
                        .weak(),
                );
                su_accounts(ui, editor, catalog, current_id);
            });
        #[cfg(test)]
        ui.ctx().data_mut(|d| {
            d.insert_temp(
                egui::Id::new("privilege_other_passwords_header"),
                response.header_response.id,
            )
        });
        let _ = response;
    }
}

fn su_accounts(
    ui: &mut egui::Ui,
    editor: &mut HostEditor,
    catalog: &Catalog,
    current_id: uuid::Uuid,
) {
    let Some(sudo) = &mut editor.profile.advanced.sudo else {
        if ui.button(catalog.text("sudo_add_user")).clicked() {
            editor
                .profile
                .advanced
                .sudo
                .get_or_insert_with(Default::default)
                .credentials
                .push(crate::model::SudoCredential {
                    id: uuid::Uuid::new_v4(),
                    username: String::new(),
                });
        }
        return;
    };
    let mut removed = None;
    for (index, credential) in sudo.credentials.iter_mut().enumerate() {
        if credential.id == current_id {
            continue;
        }
        ui.push_id(credential.id, |ui| {
            let renamed = editor.original.advanced.sudo.as_ref().is_some_and(|old| {
                old.credentials.iter().any(|old| {
                    old.id == credential.id && old.username.trim() != credential.username.trim()
                })
            });
            let hint = if renamed {
                catalog.text("privilege_password_renamed").to_owned()
            } else if editor.sudo.cleared.contains(&credential.id) {
                catalog.text("sudo_password_cleared").to_owned()
            } else {
                match editor.sudo.stored.get(&credential.id) {
                    Some(Ok(true)) => catalog.text("password_saved").to_owned(),
                    Some(Err(error)) => catalog.format("credential_error", &[("error", error)]),
                    _ => catalog.text("sudo_password_missing").to_owned(),
                }
            };
            let placeholder = if renamed {
                catalog.text("privilege_password_optional")
            } else if editor.sudo.cleared.contains(&credential.id) {
                catalog.text("sudo_password_cleared")
            } else if editor
                .sudo
                .stored
                .get(&credential.id)
                .is_some_and(|value| matches!(value, Ok(true)))
            {
                catalog.text("sudo_password_saved")
            } else {
                catalog.text("privilege_password_optional")
            };
            egui::Frame::new()
                .fill(if index % 2 == 1 {
                    ui.visuals().faint_bg_color
                } else {
                    egui::Color32::TRANSPARENT
                })
                .corner_radius(4.0)
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let height = control_height(ui);
                    let gap = 6.0;
                    let fields_width = (ui.available_width() - 2.0 * height - 3.0 * gap).max(100.0);
                    let username_width = (fields_width * 0.36).clamp(48.0, 220.0);
                    let password_width = fields_width - username_width;
                    let password = editor
                        .sudo
                        .passwords
                        .entry(credential.id)
                        .or_insert_with(|| zeroize::Zeroizing::new(String::new()));
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        ui.spacing_mut().button_padding.x = 4.0;
                        ui.spacing_mut().interact_size.x = height;
                        ui.add_sized(
                            [username_width, height],
                            single_line(ui, &mut credential.username, username_width)
                                .id(sudo_field_id(credential.id, "username"))
                                .hint_text(catalog.text("su_target_user")),
                        )
                        .on_hover_text(catalog.text("su_target_user"));
                        let key = sudo_field_id(credential.id, "password");
                        let revealed = theme::password_revealed(ui, key);
                        let response = ui
                            .add_sized(
                                [password_width, height],
                                single_line(ui, password, password_width)
                                    .id(key)
                                    .password(!revealed)
                                    .hint_text(placeholder),
                            )
                            .on_hover_text(hint);
                        theme::forget_password_history(ui, &response);
                        theme::reveal_button(ui, catalog, key);
                        ui.menu_button("⋯", |ui| {
                            if ui.button(catalog.text("sudo_clear_password")).clicked() {
                                password.clear();
                                editor.sudo.cleared.insert(credential.id);
                                ui.close();
                            }
                            if ui.button(catalog.text("sudo_delete_user")).clicked() {
                                removed = Some(credential.id);
                                ui.close();
                            }
                        })
                        .response
                        .on_hover_text(catalog.text("sudo_user_actions"));
                    });
                });
        });
    }
    if let Some(id) = removed {
        sudo.credentials.retain(|credential| credential.id != id);
        editor.sudo.passwords.remove(&id);
        editor.sudo.cleared.remove(&id);
        editor.sudo.stored.remove(&id);
    }
    ui.horizontal_wrapped(|ui| {
        if ui.button(catalog.text("sudo_add_user")).clicked() {
            sudo.credentials.push(crate::model::SudoCredential {
                id: uuid::Uuid::new_v4(),
                username: String::new(),
            });
        }
    });
}

fn current_user_id(editor: &mut HostEditor) -> uuid::Uuid {
    let username = editor.profile.username.trim().to_owned();
    let id = editor
        .profile
        .advanced
        .sudo
        .as_ref()
        .and_then(|s| {
            s.credentials
                .iter()
                .find(|c| c.username.trim() == username)
                .map(|c| c.id)
        })
        .or_else(|| {
            editor
                .sudo
                .current_user
                .as_ref()
                .filter(|(name, _)| name == &username)
                .map(|(_, id)| *id)
        })
        .unwrap_or_else(uuid::Uuid::new_v4);
    editor.sudo.current_user = Some((username, id));
    id
}

/// The fixed SSH login identity supplies sudo's ordinary authentication. No
/// authentication-user selector or persistent execution target is offered.
fn current_user_password(
    ui: &mut egui::Ui,
    editor: &mut HostEditor,
    catalog: &Catalog,
    ssh: bool,
) -> uuid::Uuid {
    let username = editor.profile.username.trim().to_owned();
    let existing = editor.profile.advanced.sudo.as_ref().and_then(|s| {
        s.credentials
            .iter()
            .find(|c| c.username.trim() == username)
            .map(|c| c.id)
    });
    let id = current_user_id(editor);
    let renamed = editor.original.advanced.sudo.as_ref().is_some_and(|s| {
        s.credentials
            .iter()
            .any(|c| c.id == id && c.username.trim() != username)
    });
    let can_reuse = editor.profile.ssh_auth == SshAuth::Password
        && editor.password_mode == PasswordMode::Password
        && (!editor.login_user_changed() || !editor.password.is_empty());
    let placeholder = if renamed {
        catalog.text("current_user_password")
    } else if editor.sudo.cleared.contains(&id) {
        catalog.text("sudo_password_cleared")
    } else if editor
        .sudo
        .stored
        .get(&id)
        .is_some_and(|v| matches!(v, Ok(true)))
    {
        catalog.text("sudo_password_saved")
    } else if can_reuse {
        catalog.text("sudo_password_login")
    } else {
        catalog.text("current_user_password")
    };
    let hint = if renamed {
        catalog.text("privilege_password_renamed").to_owned()
    } else if let Some(Err(error)) = editor.sudo.stored.get(&id) {
        catalog.format("credential_error", &[("error", error)])
    } else {
        placeholder.to_owned()
    };
    let mut changed = false;
    let password = editor
        .sudo
        .passwords
        .entry(id)
        .or_insert_with(|| zeroize::Zeroizing::new(String::new()));
    let mut form = Form::new(ui, catalog);
    form.row(
        ui,
        "current_user_password",
        Some(catalog.format("sudo_system_password_hint", &[("user", &username)])),
        |ui, width| {
            ui.horizontal(|ui| {
                let menu_width = control_height(ui);
                let password_width =
                    (width - 2.0 * (menu_width + ui.spacing().item_spacing.x)).max(60.0);
                let enabled = ssh && crate::model::valid_sudo_username(&username);
                let key = sudo_field_id(id, "password");
                let revealed = theme::password_revealed(ui, key);
                let response = ui
                    .add_enabled(
                        enabled,
                        single_line(ui, password, password_width)
                            .id(key)
                            .password(!revealed)
                            .hint_text(placeholder),
                    )
                    .on_hover_text(hint);
                theme::forget_password_history(ui, &response);
                ui.add_enabled_ui(enabled, |ui| theme::reveal_button(ui, catalog, key));
                changed = response.changed();
                ui.menu_button("⋯", |ui| {
                    if ui.button(catalog.text("sudo_clear_password")).clicked() {
                        password.clear();
                        editor.sudo.cleared.insert(id);
                        ui.close();
                    }
                });
            });
        },
    );
    if changed && !password.is_empty() && existing.is_none() {
        editor
            .profile
            .advanced
            .sudo
            .get_or_insert_with(Default::default)
            .credentials
            .push(crate::model::SudoCredential { id, username });
    }
    id
}

fn sudo_field_id(record: uuid::Uuid, field: &str) -> egui::Id {
    egui::Id::new(("sudo_account", record, field))
}

fn remote_env_key(value: RemoteEnv) -> &'static str {
    match value {
        RemoteEnv::Auto => "remote_env_auto",
        RemoteEnv::Posix => "remote_env_posix",
        RemoteEnv::Windows => "remote_env_windows",
    }
}

fn advanced_rows(ui: &mut egui::Ui, editor: &mut HostEditor, catalog: &Catalog) {
    let mut form = Form::new(ui, catalog);
    let telnet = editor.profile.protocol == Protocol::Telnet;
    let retained = !telnet && editor.profile.auth_persistence != AuthPersistence::PerCall;
    let advanced = &mut editor.profile.advanced;
    form.row(
        ui,
        "max_channels",
        Some(catalog.text("max_channels_hint").to_owned()),
        |ui, _| {
            optional_number(
                ui,
                catalog,
                &mut advanced.max_channels,
                DEFAULT_CHANNELS_PER_HOST,
                1..=MAX_CHANNELS_PER_HOST,
                "",
            );
        },
    );
    form.row(
        ui,
        "connect_timeout_default",
        Some(catalog.text("timeout_default_hint").to_owned()),
        |ui, _| {
            optional_number(
                ui,
                catalog,
                &mut advanced.connect_timeout_s,
                120,
                1..=86_400,
                " s",
            );
        },
    );
    form.row(ui, "command_timeout_default", None, |ui, _| {
        optional_number(
            ui,
            catalog,
            &mut advanced.command_timeout_s,
            600,
            1..=86_400,
            " s",
        );
    });
    form.row(
        ui,
        "keepalive_interval",
        Some(catalog.text("keepalive_hint").to_owned()),
        |ui, _| {
            ui.add_enabled_ui(retained, |ui| {
                optional_number(
                    ui,
                    catalog,
                    &mut advanced.keepalive_s,
                    DEFAULT_KEEPALIVE_SECONDS,
                    MIN_KEEPALIVE_SECONDS..=MAX_KEEPALIVE_SECONDS,
                    " s",
                );
            });
        },
    );
    form.row(
        ui,
        "remote_env",
        Some(catalog.text("remote_env_hint").to_owned()),
        |ui, width| {
            egui::ComboBox::from_id_salt("remote_env")
                .selected_text(catalog.text(remote_env_key(advanced.remote_env)))
                .width(width.min(240.0))
                .show_ui(ui, |ui| {
                    for option in [RemoteEnv::Auto, RemoteEnv::Posix, RemoteEnv::Windows] {
                        ui.selectable_value(
                            &mut advanced.remote_env,
                            option,
                            catalog.text(remote_env_key(option)),
                        );
                    }
                });
        },
    );
    form.row(
        ui,
        "codex_hidden",
        Some(catalog.text("codex_hidden_hint").to_owned()),
        |ui, _| {
            ui.checkbox(&mut advanced.codex_hidden, "");
        },
    );
    if !telnet {
        form.row(
            ui,
            "ssh_call_approval",
            Some(catalog.text("ssh_call_approval_hint").to_owned()),
            |ui, _| {
                ui.checkbox(&mut advanced.require_call_approval, "");
            },
        );
    }
    if telnet {
        form.row(
            ui,
            "telnet_prompts",
            Some(catalog.text("telnet_prompts_hint").to_owned()),
            |ui, width| {
                let mut custom = advanced.telnet_prompts.is_some();
                if ui
                    .checkbox(&mut custom, catalog.text("custom_value"))
                    .changed()
                {
                    advanced.telnet_prompts = custom.then(TelnetPrompts::default);
                }
                if let Some(prompts) = &mut advanced.telnet_prompts {
                    ui.horizontal(|ui| {
                        ui.label(catalog.text("telnet_login_prompt"));
                        text_field(ui, &mut prompts.login, (width / 2.0 - 80.0).max(80.0));
                    });
                    ui.horizontal(|ui| {
                        ui.label(catalog.text("telnet_password_prompt"));
                        text_field(ui, &mut prompts.password, (width / 2.0 - 80.0).max(80.0));
                    });
                }
            },
        );
    }
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
                for (key, stamp) in [
                    (
                        "host_key_first_seen",
                        editor.profile.host_key_first_seen_unix,
                    ),
                    (
                        "host_key_last_verified",
                        editor.profile.host_key_last_verified_unix,
                    ),
                ] {
                    if let Some(stamp) = stamp {
                        ui.label(
                            RichText::new(format!(
                                "{}: {}",
                                catalog.text(key),
                                super::format_unix_utc(stamp)
                            ))
                            .small()
                            .weak(),
                        );
                    }
                }
            }
            None => {
                ui.label(RichText::new(catalog.text("host_key_unverified")).weak());
            }
        }
    });
}

/// Tells the user the host is its password-manager item: name, endpoint,
/// account, password, description and tags are written to the item on
/// save, the rest of the form stays in this app.
fn vault_banner(ui: &mut egui::Ui, editor: &HostEditor, catalog: &Catalog) {
    let Some(texts) = editor.vault_texts() else {
        return;
    };
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(catalog.text(texts.host_title)).strong());
            if editor.saving {
                ui.spinner();
                ui.label(RichText::new(catalog.text(texts.saving)).weak());
            }
        });
        ui.add(egui::Label::new(catalog.text(texts.host_hint)).wrap());
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
        let testing =
            self.testing_all || self.selected.is_some_and(|id| self.host_test_running(id));
        let codex_edit = self.launch.codex_edit;
        let draft = self.editor.as_ref().is_some_and(|editor| editor.draft);
        let saving = self.editor.as_ref().is_some_and(|editor| editor.saving);
        let unsaved = self
            .editor
            .as_ref()
            .is_some_and(HostEditor::has_unsaved_changes);
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
                            .add_enabled(!testing, theme::primary_button(ui, catalog.text("save")))
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
                        if draft && ui.button(catalog.text("discard_draft")).clicked() {
                            action = Some(EditorAction::DiscardDraft);
                        }
                        if !draft
                            && unsaved
                            && ui
                                .add_enabled(
                                    !saving,
                                    egui::Button::new(catalog.text("revert_changes")),
                                )
                                .on_hover_text(catalog.text("revert_changes_hint"))
                                .clicked()
                        {
                            action = Some(EditorAction::Revert);
                        }
                        if testing {
                            ui.spinner();
                        }
                        // Deleting sits apart, at the far left, so it is
                        // found without being hit by mistake.
                        if !draft && !codex_edit {
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add_enabled(
                                            !testing && !saving,
                                            theme::danger_button(ui, catalog.text("delete")),
                                        )
                                        .on_hover_text(catalog.text("delete_host_hint"))
                                        .clicked()
                                    {
                                        action = Some(EditorAction::Delete);
                                    }
                                },
                            );
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
            Some(EditorAction::SaveTags) => self.save_tags(),
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
            Some(EditorAction::DiscardDraft) => self.discard_draft(),
            Some(EditorAction::Revert) => self.revert_editor(),
            Some(EditorAction::Delete) => {
                if self.selected.is_some() {
                    self.delete_prompt = true;
                }
            }
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
        // Where local secrets live; a vault host says so in its banner.
        if !editor.is_vault_host() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(catalog.text("editor_subtitle")).weak());
            });
        }
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("*")
                    .strong()
                    .color(required_color(ui.visuals())),
            );
            ui.label(
                RichText::new(catalog.text("required_legend"))
                    .small()
                    .weak(),
            );
        });
        ui.add_space(12.0);
        if editor.focus_first_missing {
            editor.focus_first_missing = false;
            if let Some(key) = editor.first_missing_field() {
                ui.memory_mut(|memory| memory.request_focus(required_field_id(key)));
            }
        }
        if codex_edit {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.label(RichText::new(catalog.text("codex_draft")).strong());
                ui.label(catalog.text("secret_not_exported"));
            });
            ui.add_space(12.0);
        }
        if editor.is_vault_host() {
            vault_banner(ui, editor, catalog);
            ui.add_space(12.0);
        }
        basic_section(ui, editor, catalog, self.testing_all, action);
        connection_section(ui, editor, catalog, hosts);
        auth_section(ui, editor, catalog, action);
        if editor.profile.protocol == Protocol::Ssh {
            host_key_section(ui, editor, catalog);
        } else {
            warning_callout(ui, catalog.text("telnet_warning"));
        }
        advanced_section(ui, editor, catalog, action);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{find_text_rect, in_isolated_store, metadata_test_app};
    use super::*;
    use crate::model::VaultHostRef;

    fn panel_frame(
        app: &mut HostsApp,
        context: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1040.0, 1600.0),
                )),
                events,
                ..Default::default()
            },
            |ui| app.editor_panel(ui, context),
        )
    }

    /// Clicks the text in the editor panel on a frame of its own.
    fn click_text(app: &mut HostsApp, context: &egui::Context, text: &str) {
        let shown = panel_frame(app, context, vec![]);
        let target = find_text_rect(&shown.shapes, text)
            .unwrap_or_else(|| panic!("missing {text}"))
            .center();
        panel_frame(app, context, click_events(target));
        panel_frame(app, context, vec![]);
    }

    fn stored_host() -> HostProfile {
        crate::storage::HostStore::load().unwrap().hosts[0].clone()
    }

    #[test]
    fn tag_changes_save_at_once_and_leave_the_rest_of_the_form_as_a_draft() {
        if !in_isolated_store(
            "ui::host_form::tests::tag_changes_save_at_once_and_leave_the_rest_of_the_form_as_a_draft",
        ) {
            return;
        }
        let _guard = crate::credentials::test_lock();
        let context = egui::Context::default();
        theme::configure_fonts(&context, "en");
        let mut app = metadata_test_app(&context, "en", metadata_profile());
        app.store.save().unwrap();
        panel_frame(&mut app, &context, vec![]);
        let editor = app.editor.as_mut().unwrap();
        editor.profile.description = "unsaved draft".into();
        editor.tag_input = "db".into();
        let add_tag = app.catalog.text("add_tag").to_owned();
        click_text(&mut app, &context, &add_tag);
        let stored = stored_host();
        assert_eq!(stored.tags, ["prod", "web", "db"]);
        assert_eq!(stored.description, "metadata notes\nsecond line");
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.profile.description, "unsaved draft");
        assert!(editor.has_unsaved_changes());
        assert!(editor.matches_stored_original(&app.store));
        assert_eq!(app.status.text, app.catalog.text("status_tags_saved"));

        // A duplicate changes nothing, so it writes nothing and reports nothing.
        app.status.text = "untouched".into();
        app.editor.as_mut().unwrap().tag_input = "DB".into();
        click_text(&mut app, &context, &add_tag);
        assert_eq!(stored_host().tags, ["prod", "web", "db"]);
        assert_eq!(app.status.text, "untouched");

        click_text(&mut app, &context, "db ×");
        assert_eq!(stored_host().tags, ["prod", "web"]);
        assert_eq!(stored_host().description, "metadata notes\nsecond line");

        // With nothing else edited, the host is clean after a tag save.
        app.editor.as_mut().unwrap().profile.description = "metadata notes\nsecond line".into();
        assert!(!app.editor.as_ref().unwrap().has_unsaved_changes());
    }

    #[test]
    fn a_tag_save_keeps_the_first_save_persistence_confirmation_pending() {
        if !in_isolated_store(
            "ui::host_form::tests::a_tag_save_keeps_the_first_save_persistence_confirmation_pending",
        ) {
            return;
        }
        let _guard = crate::credentials::test_lock();
        let context = egui::Context::default();
        theme::configure_fonts(&context, "en");
        let mut app = metadata_test_app(&context, "en", metadata_profile());
        app.store.save().unwrap();
        let profile = app.store.hosts[0].clone();
        app.editor = Some(HostEditor::fresh(profile));
        app.editor.as_mut().unwrap().tag_input = "db".into();
        let add_tag = app.catalog.text("add_tag").to_owned();
        click_text(&mut app, &context, &add_tag);
        let stored = stored_host();
        assert!(stored.tags.contains(&"db".to_owned()));
        assert_eq!(stored.auth_persistence, AuthPersistence::PerCall);
        let editor = app.editor.as_ref().unwrap();
        assert!(editor.first_save);
        assert_eq!(editor.profile.auth_persistence, AuthPersistence::Session);
        assert!(editor.persistence_change().is_some());
    }

    #[test]
    fn codex_drafts_keep_tags_unsaved_until_their_own_save() {
        if !in_isolated_store(
            "ui::host_form::tests::codex_drafts_keep_tags_unsaved_until_their_own_save",
        ) {
            return;
        }
        let _guard = crate::credentials::test_lock();
        let context = egui::Context::default();
        theme::configure_fonts(&context, "en");
        let mut app = metadata_test_app(&context, "en", metadata_profile());
        app.store.save().unwrap();
        app.launch.codex_edit = true;
        app.editor.as_mut().unwrap().tag_input = "db".into();
        let add_tag = app.catalog.text("add_tag").to_owned();
        click_text(&mut app, &context, &add_tag);
        assert_eq!(stored_host().tags, ["prod", "web"]);
        assert!(
            app.editor
                .as_ref()
                .unwrap()
                .profile
                .tags
                .contains(&"db".to_owned())
        );
    }

    #[test]
    fn tags_cannot_change_while_test_all_owns_the_store() {
        let context = egui::Context::default();
        theme::configure_fonts(&context, "en");
        let mut app = metadata_test_app(&context, "en", metadata_profile());
        app.testing_all = true;
        app.editor.as_mut().unwrap().tag_input = "db".into();
        let add_tag = app.catalog.text("add_tag").to_owned();
        click_text(&mut app, &context, &add_tag);
        assert_eq!(
            app.editor.as_ref().unwrap().profile.tags,
            ["prod", "web"],
            "the disabled button did nothing"
        );
    }

    #[test]
    fn saved_password_shows_a_mask_only_until_the_field_has_focus() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            let context = egui::Context::default();
            theme::configure_fonts(&context, locale);
            let mut profile = metadata_profile();
            profile.username = "alice".into();
            let mut app = metadata_test_app(&context, locale, profile);
            let masked = |output: &egui::FullOutput| {
                find_text_rect(&output.shapes, SAVED_PASSWORD_MASK).is_some()
            };

            let output = panel_frame(&mut app, &context, vec![]);
            assert!(!masked(&output), "nothing saved, nothing to mask");

            app.editor.as_mut().unwrap().saved_password_mode = Some(PasswordMode::Password);
            let output = panel_frame(&mut app, &context, vec![]);
            assert!(masked(&output), "a saved password is masked");

            let field = egui::Id::new("login_password");
            context.memory_mut(|memory| memory.request_focus(field));
            let output = panel_frame(&mut app, &context, vec![]);
            assert!(!masked(&output), "the focused field is empty");
            assert!(app.editor.as_ref().unwrap().password.is_empty());

            panel_frame(
                &mut app,
                &context,
                vec![egui::Event::Text("new secret".into())],
            );
            assert_eq!(app.editor.as_ref().unwrap().password.as_str(), "new secret");

            // Clearing the field and leaving it again still means "keep the saved one".
            app.editor.as_mut().unwrap().password.clear();
            context.memory_mut(|memory| memory.surrender_focus(field));
            let output = panel_frame(&mut app, &context, vec![]);
            assert!(masked(&output), "the mask returns on blur");

            app.editor.as_mut().unwrap().password_mode = PasswordMode::NoPassword;
            let output = panel_frame(&mut app, &context, vec![]);
            assert!(!masked(&output), "no-password mode has nothing to mask");
            app.editor.as_mut().unwrap().password_mode = PasswordMode::Password;

            app.editor.as_mut().unwrap().profile.username = "bob".into();
            let output = panel_frame(&mut app, &context, vec![]);
            assert!(
                !masked(&output),
                "the saved password belongs to the old user"
            );
        }
    }

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

    fn auth_frame(
        app: &mut HostsApp,
        context: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        privilege_frame(app, context, 690.0, true, events)
    }

    fn privilege_frame(
        app: &mut HostsApp,
        context: &egui::Context,
        width: f32,
        expanded: bool,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            context.style_mut_of(theme, |style| style.animation_time = 0.0);
        }
        if expanded {
            privilege_frame(app, context, width, false, vec![]);
            for key in [
                "sudo_system_password_header",
                "privilege_other_passwords_header",
            ] {
                if let Some(id) = context.data(|d| d.get_temp::<egui::Id>(egui::Id::new(key))) {
                    let mut state =
                        egui::collapsing_header::CollapsingState::load_with_default_open(
                            context, id, true,
                        );
                    state.set_open(true);
                    state.store(context);
                }
            }
        }
        context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1040.0, 2400.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                ui.set_max_width(width);
                let editor = app.editor.as_mut().unwrap();
                let mut action = None;
                sudo_rows(ui, editor, &app.catalog, &mut action);
                assert!(
                    action.is_none(),
                    "editing permissions must not execute or save"
                );
            },
        )
    }

    fn click_events(pos: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            },
        ]
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
    fn host_key_card_lists_trust_history_and_telnet_shows_the_fixed_option() {
        let context = egui::Context::default();
        theme::configure_fonts(&context, "en");
        let mut profile = metadata_profile();
        profile.host_fingerprint = Some("SHA256:abcdefghijklmnop".into());
        profile.host_key_algorithm = Some("ssh-ed25519".into());
        profile.host_key_first_seen_unix = Some(1_758_500_000);
        profile.host_key_last_verified_unix = Some(1_758_586_400);
        let mut app = metadata_test_app(&context, "en", profile);
        let output = run_form(&mut app, &context, 760.0);
        for (key, stamp) in [
            ("host_key_first_seen", "2025-09-22 00:13 UTC"),
            ("host_key_last_verified", "2025-09-23 00:13 UTC"),
        ] {
            let line = format!("{}: {stamp}", app.catalog.text(key));
            assert!(
                find_text_rect(&output.shapes, &line).is_some(),
                "missing {line}"
            );
        }

        app.editor.as_mut().unwrap().profile.protocol = Protocol::Telnet;
        let output = run_form(&mut app, &context, 760.0);
        let note = app.catalog.text("persistence_telnet_note");
        assert!(
            find_text_rect(&output.shapes, note).is_some(),
            "telnet note is shown"
        );
        assert!(
            find_text_rect(&output.shapes, app.catalog.text("persistence_per_call")).is_some(),
            "the fixed per-call option is listed"
        );
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
                "section_advanced",
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
    fn required_markers_and_persistence_choice_render_in_all_locales() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            let context = egui::Context::default();
            theme::configure_fonts(&context, locale);
            let mut app = metadata_test_app(&context, locale, metadata_profile());
            let output = run_form(&mut app, &context, 760.0);
            let rect = |text: &str| {
                find_text_rect(&output.shapes, text)
                    .unwrap_or_else(|| panic!("missing {locale} text {text}"))
            };
            let alias = rect(app.catalog.text("alias"));
            let legend = rect(app.catalog.text("required_legend"));
            assert!(legend.bottom() <= alias.top(), "legend sits above the form");
            rect(app.catalog.text("auth_persistence"));
            rect(app.catalog.text("persistence_per_call"));
            rect(app.catalog.text("persistence_session"));
            rect(app.catalog.text("persistence_idle"));
            rect(app.catalog.text("section_advanced"));
            rect(app.catalog.text("advanced_default_summary"));
        }
    }

    #[test]
    fn missing_required_fields_show_hints_after_a_failed_save() {
        let context = egui::Context::default();
        theme::configure_fonts(&context, "en");
        let mut profile = metadata_profile();
        profile.alias.clear();
        profile.username.clear();
        let mut app = metadata_test_app(&context, "en", profile);
        if let Some(editor) = app.editor.as_mut() {
            editor.show_required = true;
        }
        let output = run_form(&mut app, &context, 760.0);
        let hint = app.catalog.text("required_hint");
        let hints = output
            .shapes
            .iter()
            .filter(|shape| find_text_rect(std::slice::from_ref(shape), hint).is_some())
            .count();
        assert!(hints >= 2, "alias and username are flagged, got {hints}");
        assert_eq!(
            app.editor.as_ref().unwrap().first_missing_field(),
            Some("alias")
        );
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

    fn vault_profile() -> HostProfile {
        let mut profile = metadata_profile();
        profile.address = "10.0.0.5".into();
        profile.username = "deploy".into();
        profile.vault = Some(VaultHostRef {
            item_id: profile.id,
            item_name: profile.alias.clone(),
            revision_date: "rev-1".into(),
            ..Default::default()
        });
        profile
    }

    #[test]
    fn a_onepassword_host_saves_through_its_item_like_any_vault_host() {
        if !in_isolated_store(
            "ui::host_form::tests::a_onepassword_host_saves_through_its_item_like_any_vault_host",
        ) {
            return;
        }
        let _guard = crate::credentials::test_lock();
        let context = egui::Context::default();
        theme::configure_fonts(&context, "en");
        let mut profile = vault_profile();
        let vault = profile.vault.as_mut().unwrap();
        vault.provider = crate::model::VaultProvider::OnePassword;
        vault.external_id = Some("testitem000000000000000000".into());
        let mut app = metadata_test_app(&context, "en", profile);
        app.store.onepassword.cli_path = Some(std::path::PathBuf::from("C:/missing/op.exe"));
        app.store.onepassword.account = Some(crate::model::OnePasswordAccount::default());
        app.store.save().unwrap();
        app.editor = Some(HostEditor::load(app.store.hosts[0].clone()));
        {
            let editor = app.editor.as_mut().unwrap();
            editor.profile.address = "10.0.0.6".into();
            editor.profile.alias = "renamed".into();
            editor.profile.advanced.require_call_approval = true;
        }

        // The item is written first; the store waits for it.
        assert!(!app.persist_editor().unwrap());
        assert!(app.vault_job.is_some() && app.pending_vault_save.is_some());
        assert_eq!(stored_host().address, "10.0.0.5");
        let write = app.editor.as_ref().unwrap().vault_write();
        assert_eq!(write.provider, crate::model::VaultProvider::OnePassword);
        assert_eq!(
            write.external_id.as_deref(),
            Some("testitem000000000000000000")
        );
        assert_eq!(write.name, "renamed");
        assert_eq!(write.host_uri, "ssh://10.0.0.6:22");
        assert_eq!(
            app.persist_editor().unwrap_err(),
            app.catalog.text("op_saving"),
            "a second save waits, in 1Password's words"
        );
    }

    #[test]
    fn vault_hosts_show_the_vault_banner_and_the_usual_choices() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            let context = egui::Context::default();
            theme::configure_fonts(&context, locale);
            let mut app = metadata_test_app(&context, locale, vault_profile());
            assert!(app.editor.as_ref().unwrap().is_vault_host());
            let output = run_form(&mut app, &context, 760.0);
            let rect = |text: &str| {
                find_text_rect(&output.shapes, text)
                    .unwrap_or_else(|| panic!("missing {locale} text {text}"))
            };
            rect(app.catalog.text("vault_host_title"));
            rect(app.catalog.text("vault_password_hint"));
            // The same authentication and password choices as a local host.
            rect(app.catalog.text("password_mode"));
            rect(app.catalog.text("no_password"));
            let editor = app.editor.as_ref().unwrap();
            assert!(
                !editor.password_value_missing(),
                "the item holds a password"
            );
            assert!(!editor.has_unsaved_changes());
        }
    }

    #[test]
    fn the_form_offers_delete_for_saved_hosts_and_names_the_right_store() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            let context = egui::Context::default();
            theme::configure_fonts(&context, locale);
            let has = |app: &mut HostsApp, key: &str| {
                panel_frame(app, &context, vec![]);
                let output = panel_frame(app, &context, vec![]);
                find_text_rect(&output.shapes, app.catalog.text(key)).is_some()
            };
            let mut app = metadata_test_app(&context, locale, metadata_profile());
            assert!(
                has(&mut app, "delete"),
                "{locale}: delete in the action bar"
            );
            assert!(
                has(&mut app, "editor_subtitle"),
                "{locale}: local storage note"
            );
            let target = find_text_rect(
                &panel_frame(&mut app, &context, vec![]).shapes,
                app.catalog.text("delete"),
            )
            .unwrap()
            .center();
            panel_frame(&mut app, &context, click_events(target));
            assert!(app.delete_prompt, "{locale}: delete asks first");

            assert!(
                !has(&mut app, "revert_changes"),
                "{locale}: nothing to revert"
            );
            app.delete_prompt = false;
            app.editor.as_mut().unwrap().profile.description = "typed".into();
            assert!(
                has(&mut app, "revert_changes"),
                "{locale}: revert once edited"
            );
            let target = find_text_rect(
                &panel_frame(&mut app, &context, vec![]).shapes,
                app.catalog.text("revert_changes"),
            )
            .unwrap()
            .center();
            panel_frame(&mut app, &context, click_events(target));
            assert!(
                !app.editor.as_ref().unwrap().has_unsaved_changes(),
                "{locale}: revert reloads the saved host"
            );

            let mut app = metadata_test_app(&context, locale, vault_profile());
            assert!(
                !has(&mut app, "editor_subtitle"),
                "{locale}: a vault host does not claim Credential Manager"
            );
            assert!(has(&mut app, "delete"), "{locale}: a Bitwarden item can go");
            let vault = app.editor.as_mut().unwrap().profile.vault.as_mut().unwrap();
            vault.provider = crate::model::VaultProvider::OnePassword;
            assert!(has(&mut app, "delete"), "{locale}: so can a 1Password item");
            assert!(has(&mut app, "op_host_title"), "{locale}: named as such");

            app.new_host();
            assert!(
                !has(&mut app, "delete"),
                "{locale}: a draft is discarded, not deleted"
            );
        }
    }

    #[test]
    fn saving_a_vault_host_writes_the_item_first_and_the_store_after() {
        use codex_hosts_bw::FieldEdit;

        use crate::credentials::{self, CredentialKind};

        if !in_isolated_store(
            "ui::host_form::tests::saving_a_vault_host_writes_the_item_first_and_the_store_after",
        ) {
            return;
        }
        let _guard = crate::credentials::test_lock();
        let context = egui::Context::default();
        theme::configure_fonts(&context, "en");
        let mut app = metadata_test_app(&context, "en", vault_profile());
        app.store.bitwarden.enabled = true;
        app.store.bitwarden.cli_path = Some(std::path::PathBuf::from("C:/missing/bw.exe"));
        app.store.bitwarden.profile = Some(crate::model::BitwardenProfile::default());
        app.store.save().unwrap();
        let id = app.store.hosts[0].id;
        app.editor = Some(HostEditor::load(app.store.hosts[0].clone()));
        {
            let editor = app.editor.as_mut().unwrap();
            editor.profile.address = "10.0.0.6".into();
            editor.profile.description = "front".into();
            editor.profile.advanced.require_call_approval = true;
            editor.password.push_str("new-pw");
        }

        // The save is deferred to the item write; nothing is stored yet.
        assert!(!app.persist_editor().unwrap());
        assert!(app.editor.as_ref().unwrap().saving);
        assert!(app.pending_vault_save.is_some());
        assert!(app.vault_job.is_some());
        assert_eq!(stored_host().address, "10.0.0.5");
        let write = app.editor.as_ref().unwrap().vault_write();
        assert_eq!(write.host_uri, "ssh://10.0.0.6:22");
        assert_eq!(write.name, "example");
        assert_eq!(write.username, "deploy");
        assert_eq!(write.description, "front");
        assert_eq!(write.expected_revision, "rev-1");
        assert!(matches!(write.password, FieldEdit::Set(_)));
        assert_eq!(
            app.persist_editor().unwrap_err(),
            app.catalog.text("vault_saving"),
            "a second save waits for the first item write"
        );

        // The item write lands: the store takes what the item now says plus
        // the local settings from the form; Credential Manager stays empty.
        app.vault_job = None;
        let facts = crate::vault_hosts::HostFacts {
            alias: "example".into(),
            protocol: Protocol::Ssh,
            address: "10.0.0.6".into(),
            port: 22,
            username: "deploy".into(),
            description: "front".into(),
            tags: Vec::new(),
            revision: "rev-2".into(),
        };
        app.finish_vault_save(&crate::vault_hosts::ItemKey::bitwarden(id), &facts);
        let stored = stored_host();
        assert_eq!(stored.address, "10.0.0.6");
        assert_eq!(stored.description, "front");
        assert_eq!(stored.vault.as_ref().unwrap().revision_date, "rev-2");
        assert!(
            stored.advanced.require_call_approval,
            "local settings saved"
        );
        assert!(!credentials::has(id, CredentialKind::Password).unwrap());
        let editor = app.editor.as_ref().unwrap();
        assert!(!editor.saving);
        assert!(editor.password.is_empty());
        assert!(!editor.has_unsaved_changes());
        let listed = crate::tool::list_hosts(&app.store, vec![]).unwrap();
        assert!(listed.hosts[0].has_required_secret);
        assert_eq!(listed.hosts[0].credential_source, Some("bitwarden"));

        // Switching Bitwarden off hides the host from Codex and refuses it.
        app.store.bitwarden.enabled = false;
        assert!(
            crate::tool::list_hosts(&app.store, vec![])
                .unwrap()
                .hosts
                .is_empty()
        );
        assert_eq!(
            crate::tool::find_host(&app.store, "example")
                .unwrap_err()
                .code,
            "BITWARDEN_DISABLED"
        );
    }

    #[test]
    fn testing_a_vault_host_writes_only_a_changed_form_and_tests_after_the_write() {
        if !in_isolated_store(
            "ui::host_form::tests::testing_a_vault_host_writes_only_a_changed_form_and_tests_after_the_write",
        ) {
            return;
        }
        let _guard = crate::credentials::test_lock();
        let context = egui::Context::default();
        theme::configure_fonts(&context, "en");
        let mut profile = vault_profile();
        // Nothing listens here, so the probe ends at once.
        profile.address = "127.0.0.1".into();
        profile.port = 1;
        let mut app = metadata_test_app(&context, "en", profile);
        app.store.bitwarden.enabled = true;
        app.store.bitwarden.cli_path = Some(std::path::PathBuf::from("C:/missing/bw.exe"));
        app.store.bitwarden.profile = Some(crate::model::BitwardenProfile::default());
        app.store.save().unwrap();
        let id = app.store.hosts[0].id;
        app.editor = Some(HostEditor::load(app.store.hosts[0].clone()));

        // Unchanged: tested as stored, with no item write.
        app.start_test();
        assert!(app.vault_job.is_none() && app.pending_vault_save.is_none());
        assert!(app.host_test_running(id));
        app.test_operations.clear();
        app.test_states.clear();

        // Changed: the item is written first and the test waits for it,
        // even when asked again meanwhile.
        app.editor.as_mut().unwrap().profile.description = "front".into();
        app.start_test();
        assert!(app.vault_job.is_some());
        assert!(!app.host_test_running(id));
        app.start_test();
        assert!(
            app.pending_vault_save
                .as_ref()
                .is_some_and(|plan| plan.then_test)
        );
        assert!(!app.host_test_running(id));

        app.vault_job = None;
        let facts = crate::vault_hosts::HostFacts {
            alias: "example".into(),
            protocol: Protocol::Ssh,
            address: "127.0.0.1".into(),
            port: 1,
            username: "deploy".into(),
            description: "front".into(),
            tags: Vec::new(),
            revision: "rev-2".into(),
        };
        app.finish_vault_save(&crate::vault_hosts::ItemKey::bitwarden(id), &facts);
        assert_eq!(stored_host().description, "front");
        assert!(
            app.host_test_running(id),
            "the test runs once the write lands"
        );
        assert!(!app.editor.as_ref().unwrap().saving);
    }

    #[test]
    fn a_new_local_host_is_a_draft_until_its_first_save() {
        if !in_isolated_store(
            "ui::host_form::tests::a_new_local_host_is_a_draft_until_its_first_save",
        ) {
            return;
        }
        let context = egui::Context::default();
        theme::configure_fonts(&context, "en");
        let mut app = metadata_test_app(&context, "en", metadata_profile());
        app.store.save().unwrap();
        app.new_host();
        let editor = app.editor.as_ref().unwrap();
        assert!(editor.draft && !editor.is_vault_host());
        assert!(
            editor.profile.alias.is_empty(),
            "the alias is typed, not generated"
        );
        assert_eq!(app.selected, None);
        assert_eq!(app.store.hosts.len(), 1, "nothing is stored before Save");
        // Tags of a draft wait for its Save instead of writing at once.
        app.editor.as_mut().unwrap().profile.tags.push("lab".into());
        app.save_tags();
        assert_eq!(app.store.hosts.len(), 1);
        {
            let editor = app.editor.as_mut().unwrap();
            editor.profile.alias = "fresh".into();
            editor.profile.address = "10.0.0.7".into();
            editor.profile.username = "ops".into();
            editor.profile.ssh_auth = SshAuth::SshAgent;
        }
        assert!(app.persist_editor().unwrap());
        let stored = app.store.find_alias("fresh").unwrap();
        assert_eq!(stored.tags, ["lab"]);
        assert_eq!(app.selected, Some(stored.id));
        let editor = app.editor.as_ref().unwrap();
        assert!(!editor.draft);
        assert!(!editor.has_unsaved_changes());

        // Discarding a draft leaves the store untouched.
        app.new_host();
        app.discard_draft();
        assert!(app.editor.is_none());
        assert_eq!(app.store.hosts.len(), 2);
    }

    #[test]
    fn a_vault_draft_creates_the_item_and_takes_its_id() {
        if !in_isolated_store(
            "ui::host_form::tests::a_vault_draft_creates_the_item_and_takes_its_id",
        ) {
            return;
        }
        let _guard = crate::credentials::test_lock();
        let context = egui::Context::default();
        theme::configure_fonts(&context, "en");
        let mut app = metadata_test_app(&context, "en", metadata_profile());
        app.store.bitwarden.enabled = true;
        app.store.bitwarden.cli_path = Some(std::path::PathBuf::from("C:/missing/bw.exe"));
        app.store.bitwarden.profile = Some(crate::model::BitwardenProfile::default());
        app.store.save().unwrap();
        app.new_vault_host(crate::model::VaultProvider::Bitwarden);
        let editor = app.editor.as_ref().unwrap();
        assert!(editor.draft && editor.is_vault_host());
        assert!(
            editor.has_unsaved_changes(),
            "a draft is never dropped by a refresh"
        );
        assert!(
            editor.password_value_missing(),
            "a new item needs a password"
        );
        {
            let editor = app.editor.as_mut().unwrap();
            editor.profile.alias = "new-box".into();
            editor.profile.address = "10.0.0.9".into();
            editor.profile.username = "ops".into();
            editor.password.push_str("pw");
        }
        assert!(!app.persist_editor().unwrap());
        let write = app.editor.as_ref().unwrap().vault_write();
        assert_eq!(write.item_id, None, "the item does not exist yet");
        assert_eq!(write.expected_revision, "");
        app.vault_job = None;
        let item_id = uuid::Uuid::new_v4();
        let facts = crate::vault_hosts::HostFacts {
            alias: "new-box".into(),
            protocol: Protocol::Ssh,
            address: "10.0.0.9".into(),
            port: 22,
            username: "ops".into(),
            description: String::new(),
            tags: Vec::new(),
            revision: "rev-1".into(),
        };
        app.finish_vault_save(&crate::vault_hosts::ItemKey::bitwarden(item_id), &facts);
        let created = app.store.find_alias("new-box").unwrap();
        assert_eq!(created.id, item_id, "the profile id is the item id");
        assert_eq!(created.vault.as_ref().unwrap().item_id, item_id);
        assert_eq!(app.selected, Some(item_id));
        let editor = app.editor.as_ref().unwrap();
        assert!(!editor.draft);
        assert_eq!(editor.profile.id, item_id);
        assert_eq!(app.store.hosts.len(), 2);
    }

    #[test]
    fn sudo_controls_fit_narrow_and_wide_forms_in_every_locale() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            for dark in [false, true] {
                for width in [260.0, 360.0, 690.0, 1030.0] {
                    let context = egui::Context::default();
                    context.set_visuals(if dark {
                        egui::Visuals::dark()
                    } else {
                        egui::Visuals::light()
                    });
                    theme::configure_fonts(&context, locale);
                    let mut profile = metadata_profile();
                    profile.username = "alice".into();
                    let accounts: Vec<_> = ["alice", "bob", "root"]
                        .into_iter()
                        .map(|username| crate::model::SudoCredential {
                            id: uuid::Uuid::new_v4(),
                            username: username.into(),
                        })
                        .collect();
                    profile.advanced.sudo = Some(crate::model::SudoSettings {
                        enabled: true,
                        credentials: accounts.clone(),
                    });
                    profile.advanced.su = Some(crate::model::SuSettings { enabled: true });
                    let mut app = metadata_test_app(&context, locale, profile);
                    for _ in 0..3 {
                        privilege_frame(&mut app, &context, width, false, vec![]);
                    }
                    let output = privilege_frame(&mut app, &context, width, false, vec![]);
                    let label =
                        |key| find_text_rect(&output.shapes, app.catalog.text(key)).unwrap();
                    let sudo = label("sudo_enabled");
                    let su = label("su_enabled");
                    assert!((sudo.left() - su.left()).abs() < 1.0);
                    let check = |key| {
                        context
                            .data(|d| d.get_temp::<egui::Rect>(egui::Id::new(key)))
                            .unwrap()
                    };
                    let a = check("sudo_enabled_rect");
                    let b = check("su_enabled_rect");
                    assert!((a.left() - b.left()).abs() < 1.0);
                    assert!(sudo.bottom() < su.top() && a.bottom() < b.top());
                    for rect in [
                        sudo,
                        su,
                        a,
                        b,
                        label("sudo_system_password"),
                        label("privilege_other_passwords"),
                    ] {
                        assert!(
                            rect.right() <= width + 1.0,
                            "{locale} {dark} {width}: {rect:?}"
                        );
                    }
                    if width < STACKED_BREAKPOINT {
                        assert!(a.top() >= sudo.bottom());
                    } else {
                        assert!(a.left() > sudo.right());
                    }
                    for account in &accounts {
                        assert!(
                            context
                                .read_response(sudo_field_id(account.id, "password"))
                                .is_none()
                        );
                    }
                    for _ in 0..3 {
                        privilege_frame(&mut app, &context, width, true, vec![]);
                    }
                    let mut previous = None::<egui::Rect>;
                    for account in &accounts {
                        let password = context
                            .read_response(sudo_field_id(account.id, "password"))
                            .unwrap()
                            .rect;
                        assert!(
                            password.right() <= width + 1.0,
                            "{locale} {width}: {password:?}"
                        );
                        if let Some(previous) = previous {
                            assert!(previous.bottom() < password.top());
                        }
                        if account.username != "alice" {
                            let username = context
                                .read_response(sudo_field_id(account.id, "username"))
                                .unwrap()
                                .rect;
                            assert!((username.center().y - password.center().y).abs() < 1.0);
                            assert!(username.right() < password.left());
                        } else {
                            assert!(
                                context
                                    .read_response(sudo_field_id(account.id, "username"))
                                    .is_none()
                            );
                        }
                        previous = Some(password);
                    }
                }
            }
        }
    }

    #[test]
    fn current_system_password_follows_the_login_account_without_rebinding_secrets() {
        let context = egui::Context::default();
        let mut profile = metadata_profile();
        profile.username = "alice".into();
        profile.ssh_auth = SshAuth::PrivateKey;
        profile.advanced.sudo = Some(crate::model::SudoSettings {
            enabled: true,
            ..Default::default()
        });
        let mut app = metadata_test_app(&context, "en", profile);
        auth_frame(&mut app, &context, vec![]);
        let editor = app.editor.as_ref().unwrap();
        assert!(
            editor
                .profile
                .advanced
                .sudo
                .as_ref()
                .unwrap()
                .credentials
                .is_empty()
        );
        assert!(editor.profile.advanced.su.is_none());
        assert!(!editor.has_unsaved_changes());
        let alice_id = editor.sudo.current_user.as_ref().unwrap().1;
        assert!(
            context
                .read_response(sudo_field_id(alice_id, "username"))
                .is_none()
        );

        context.memory_mut(|m| m.request_focus(sudo_field_id(alice_id, "password")));
        auth_frame(
            &mut app,
            &context,
            vec![egui::Event::Text("alice test token".into())],
        );
        let editor = app.editor.as_ref().unwrap();
        let (settings, updates) = editor.sudo_changes().unwrap();
        let settings = settings.unwrap();
        assert!(settings.enabled);
        assert_eq!(settings.credentials.len(), 1);
        assert_eq!(settings.credentials[0].username, "alice");
        assert_eq!(settings.credentials[0].id, alice_id);
        assert!(
            matches!(&updates[..], [(crate::credentials::CredentialKind::SudoPassword(id), Some(value))]
            if *id == alice_id && value.as_str() == "alice test token")
        );
        assert!(
            !serde_json::to_string(&editor.profile)
                .unwrap()
                .contains("alice test token")
        );
        assert!(editor.profile.advanced.su.is_none());

        app.editor.as_mut().unwrap().profile.username = "bob".into();
        auth_frame(&mut app, &context, vec![]);
        let editor = app.editor.as_ref().unwrap();
        let bob_id = editor.sudo.current_user.as_ref().unwrap().1;
        assert_ne!(alice_id, bob_id);
        assert!(editor.sudo.passwords[&bob_id].is_empty());
        assert_eq!(
            editor.sudo.passwords[&alice_id].as_str(),
            "alice test token"
        );
        assert_eq!(
            editor
                .profile
                .advanced
                .sudo
                .as_ref()
                .unwrap()
                .credentials
                .len(),
            1
        );
        assert!(
            context
                .read_response(sudo_field_id(alice_id, "username"))
                .is_some()
        );

        context.memory_mut(|m| m.request_focus(sudo_field_id(bob_id, "password")));
        auth_frame(
            &mut app,
            &context,
            vec![egui::Event::Text("bob test token".into())],
        );
        let editor = app.editor.as_ref().unwrap();
        let records = &editor.profile.advanced.sudo.as_ref().unwrap().credentials;
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].username, "alice");
        assert_eq!(records[1].username, "bob");
        assert_eq!(
            editor.sudo.passwords[&alice_id].as_str(),
            "alice test token"
        );
        assert_eq!(editor.sudo.passwords[&bob_id].as_str(), "bob test token");
        assert!(
            context
                .read_response(sudo_field_id(bob_id, "username"))
                .is_none()
        );

        app.editor.as_mut().unwrap().profile.username = "alice".into();
        auth_frame(&mut app, &context, vec![]);
        auth_frame(&mut app, &context, vec![]);
        assert_eq!(
            app.editor
                .as_ref()
                .unwrap()
                .sudo
                .current_user
                .as_ref()
                .unwrap()
                .1,
            alice_id
        );
        assert!(
            context
                .read_response(sudo_field_id(alice_id, "username"))
                .is_none()
        );
        assert!(
            context
                .read_response(sudo_field_id(bob_id, "username"))
                .is_some()
        );
    }

    #[test]
    fn renamed_record_cannot_advertise_the_previous_accounts_password_for_sudo() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            let context = egui::Context::default();
            theme::configure_fonts(&context, locale);
            let id = uuid::Uuid::new_v4();
            let mut profile = metadata_profile();
            profile.username = "alice".into();
            profile.ssh_auth = SshAuth::PrivateKey;
            profile.advanced.sudo = Some(crate::model::SudoSettings {
                enabled: true,
                credentials: vec![crate::model::SudoCredential {
                    id,
                    username: "bob".into(),
                }],
            });
            let mut app = metadata_test_app(&context, locale, profile);
            let editor = app.editor.as_mut().unwrap();
            editor.sudo.stored.insert(id, Ok(true));
            editor.profile.advanced.sudo.as_mut().unwrap().credentials[0].username = "alice".into();
            let output = auth_frame(&mut app, &context, vec![]);
            assert!(
                find_text_rect(&output.shapes, app.catalog.text("current_user_password")).is_some()
            );
            assert!(
                find_text_rect(&output.shapes, app.catalog.text("sudo_password_saved")).is_none()
            );
            let (settings, updates) = app.editor.as_ref().unwrap().sudo_changes().unwrap();
            assert_ne!(settings.unwrap().credentials[0].id, id);
            assert!(
                matches!(&updates[..], [(crate::credentials::CredentialKind::SudoPassword(old), None)] if *old == id)
            );
        }
    }

    #[test]
    fn saved_current_password_tracks_actual_accounts_across_login_changes() {
        use crate::credentials::{self, CredentialKind};
        use crate::storage::HostStore;
        use std::{env, fs, process::Command};

        // Run the real editor save path in a child so no process-wide test
        // environment or user's actual host list is modified. The credential
        // lock is taken only after the child finished: it needs it itself.
        if env::var("CODEX_HOSTS_AUTH_FLOW_TEST_CHILD").as_deref() != Ok("1") {
            let directory =
                env::temp_dir().join(format!("codex-hosts-auth-flow-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&directory).unwrap();
            let output = Command::new(env::current_exe().unwrap())
                .args(["--exact", "ui::host_form::tests::saved_current_password_tracks_actual_accounts_across_login_changes", "--test-threads=1"])
                .env("CODEX_HOSTS_AUTH_FLOW_TEST_CHILD", "1")
                .env("CODEX_HOSTS_DATA_DIR", &directory)
                .output().unwrap();
            let _guard = crate::credentials::test_lock();
            let fixture_path = directory.join("hosts.json");
            if fixture_path.exists() {
                let store: HostStore =
                    serde_json::from_slice(&fs::read(&fixture_path).unwrap()).unwrap();
                for host in &store.hosts {
                    assert!(!credentials::has(host.id, CredentialKind::Password).unwrap());
                    assert!(
                        !credentials::has(host.id, credentials::login_password_kind(host)).unwrap()
                    );
                    for record in &host.advanced.sudo.as_ref().unwrap().credentials {
                        assert!(
                            !credentials::has(host.id, CredentialKind::SudoPassword(record.id))
                                .unwrap()
                        );
                    }
                }
            }
            for name in ["hosts.json", "hosts.json.bak", "hosts.lock"] {
                let path = directory.join(name);
                if path.exists() {
                    fs::remove_file(path).unwrap();
                }
            }
            fs::remove_dir(&directory).unwrap();
            assert!(
                output.status.success(),
                "isolated editor save failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let _guard = crate::credentials::test_lock();
        let directory = std::path::PathBuf::from(
            env::var_os("CODEX_HOSTS_DATA_DIR").expect("isolated data directory"),
        );
        let directory = directory.canonicalize().unwrap();
        assert_eq!(
            directory.parent().unwrap(),
            env::temp_dir().canonicalize().unwrap()
        );
        assert!(
            directory
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("codex-hosts-auth-flow-")
        );
        struct FixtureCredentials(Vec<HostProfile>);
        impl Drop for FixtureCredentials {
            fn drop(&mut self) {
                for host in &self.0 {
                    credentials::delete_host(host).unwrap();
                    assert!(
                        !credentials::has(host.id, credentials::login_password_kind(host)).unwrap()
                    );
                }
            }
        }
        let context = egui::Context::default();
        let alice = uuid::Uuid::new_v4();
        let bob = uuid::Uuid::new_v4();
        let mut profile = metadata_profile();
        profile.username = "alice".into();
        profile.address = "127.0.0.1".into();
        profile.advanced.sudo = Some(crate::model::SudoSettings {
            enabled: true,
            credentials: vec![
                crate::model::SudoCredential {
                    id: alice,
                    username: "alice".into(),
                },
                crate::model::SudoCredential {
                    id: bob,
                    username: "bob".into(),
                },
            ],
        });
        profile.advanced.su = Some(crate::model::SuSettings { enabled: true });
        let mut cleanup = FixtureCredentials(vec![profile.clone()]);
        credentials::store(
            profile.id,
            CredentialKind::Password,
            "alice login test token",
        )
        .unwrap();
        credentials::store(
            profile.id,
            CredentialKind::SudoPassword(bob),
            "bob system test token",
        )
        .unwrap();
        let mut app = metadata_test_app(&context, "en", profile);
        app.store.save().unwrap();
        app.editor.as_mut().unwrap().saved_password_mode = Some(PasswordMode::Password);
        assert_eq!(
            crate::sudo::password_for(&app.editor.as_ref().unwrap().profile, "alice")
                .unwrap()
                .as_str(),
            "alice login test token"
        );
        auth_frame(&mut app, &context, vec![]);
        context.memory_mut(|m| m.request_focus(sudo_field_id(alice, "password")));
        auth_frame(
            &mut app,
            &context,
            vec![egui::Event::Text("alice system test token".into())],
        );
        app.persist_editor().unwrap();
        let saved = HostStore::load().unwrap().hosts[0].clone();
        assert_eq!(
            crate::sudo::password_for(&saved, "alice").unwrap().as_str(),
            "alice system test token"
        );
        assert_eq!(
            crate::su::password_for(&saved, "bob").unwrap().as_str(),
            "bob system test token"
        );

        // Freeze the profile held by an already-running MCP call. Remove named
        // overrides from this snapshot so both methods exercise login fallback.
        let mut stale_alice = saved.clone();
        stale_alice
            .advanced
            .sudo
            .as_mut()
            .unwrap()
            .credentials
            .clear();
        app.editor.as_mut().unwrap().profile.username = "bob".into();
        assert!(app.persist_editor().is_err());
        assert_eq!(HostStore::load().unwrap().hosts[0].username, "alice");
        assert_eq!(
            credentials::load(saved.id, credentials::login_password_kind(&saved))
                .unwrap()
                .unwrap()
                .as_str(),
            "alice login test token"
        );
        app.editor
            .as_mut()
            .unwrap()
            .password
            .push_str("bob login test token");
        app.persist_editor().unwrap();
        let saved = HostStore::load().unwrap().hosts[0].clone();
        cleanup.0.push(saved.clone());
        assert_ne!(saved.login_password_id, stale_alice.login_password_id);
        assert_eq!(
            crate::sudo::password_for(&stale_alice, "alice")
                .unwrap_err()
                .code,
            "SUDO_CREDENTIAL_MISSING"
        );
        assert_eq!(
            crate::su::password_for(&stale_alice, "alice")
                .unwrap_err()
                .code,
            "SU_CREDENTIAL_MISSING"
        );
        let mut stale_bob = saved.clone();
        stale_bob
            .advanced
            .sudo
            .as_mut()
            .unwrap()
            .credentials
            .clear();
        assert_eq!(saved.username, "bob");
        assert_eq!(
            credentials::load(saved.id, credentials::login_password_kind(&saved))
                .unwrap()
                .unwrap()
                .as_str(),
            "bob login test token"
        );
        assert_eq!(
            crate::sudo::password_for(&saved, "bob").unwrap().as_str(),
            "bob system test token"
        );
        assert_eq!(
            crate::su::password_for(&saved, "alice").unwrap().as_str(),
            "alice system test token"
        );

        let editor = app.editor.as_mut().unwrap();
        editor.profile.username = "carol".into();
        editor.profile.ssh_auth = SshAuth::PrivateKey;
        editor.profile.private_key_path = "fixture-key-not-used-for-connection".into();
        app.persist_editor().unwrap();
        let saved = HostStore::load().unwrap().hosts[0].clone();
        cleanup.0.push(saved.clone());
        assert_ne!(saved.login_password_id, stale_bob.login_password_id);
        assert_eq!(
            crate::sudo::password_for(&stale_bob, "bob")
                .unwrap_err()
                .code,
            "SUDO_CREDENTIAL_MISSING"
        );
        assert_eq!(
            crate::su::password_for(&stale_bob, "bob").unwrap_err().code,
            "SU_CREDENTIAL_MISSING"
        );
        assert!(
            credentials::load(saved.id, credentials::login_password_kind(&saved))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            crate::sudo::password_for(&saved, "carol")
                .err()
                .unwrap()
                .code,
            "SUDO_CREDENTIAL_MISSING"
        );
        assert_eq!(
            crate::su::password_for(&saved, "bob").unwrap().as_str(),
            "bob system test token"
        );
        assert_eq!(
            crate::su::password_for(&saved, "alice").unwrap().as_str(),
            "alice system test token"
        );
        // Agent-only identity changes must rotate even a currently empty slot.
        let carol_id = saved.login_password_id;
        let editor = app.editor.as_mut().unwrap();
        editor.profile.username = "dave".into();
        editor.profile.ssh_auth = SshAuth::SshAgent;
        app.persist_editor().unwrap();
        let saved = HostStore::load().unwrap().hosts[0].clone();
        cleanup.0.push(saved.clone());
        assert_ne!(saved.login_password_id, carol_id);
        let editor = app.editor.as_mut().unwrap();
        editor.profile.ssh_auth = SshAuth::Password;
        editor.password.push_str("dave login test token");
        app.persist_editor().unwrap();
        let dave = HostStore::load().unwrap().hosts[0].clone();
        cleanup.0.push(dave.clone());
        assert_eq!(
            crate::sudo::password_for(&dave, "dave").unwrap().as_str(),
            "dave login test token"
        );
        assert_eq!(
            crate::su::password_for(&dave, "dave").unwrap().as_str(),
            "dave login test token"
        );
        assert!(crate::sudo::password_for(&stale_bob, "bob").is_err());
        assert!(crate::su::password_for(&stale_alice, "alice").is_err());
        let metadata = fs::read_to_string(directory.join("hosts.json")).unwrap();
        assert!(!metadata.contains("test token"));
        drop(cleanup);
    }

    #[test]
    fn default_privilege_ui_does_not_duplicate_login_inputs_or_change_profiles() {
        for auth in [SshAuth::Password, SshAuth::PrivateKey, SshAuth::SshAgent] {
            for enabled in [false, true] {
                let context = egui::Context::default();
                let mut profile = metadata_profile();
                profile.username = "alice".into();
                profile.ssh_auth = auth;
                if enabled {
                    profile.advanced.sudo = Some(crate::model::SudoSettings {
                        enabled: true,
                        ..Default::default()
                    });
                }
                let before = serde_json::to_value(&profile).unwrap();
                let mut app = metadata_test_app(&context, "en", profile);
                let output = privilege_frame(&mut app, &context, 690.0, false, vec![]);
                let editor = app.editor.as_ref().unwrap();
                let id = editor.sudo.current_user.as_ref().unwrap().1;
                for field in ["username", "password", "login_identity"] {
                    assert!(context.read_response(sudo_field_id(id, field)).is_none());
                }
                for retired in ["Save and test sudo", "Save and test su", "alice"] {
                    assert!(find_text_rect(&output.shapes, retired).is_none());
                }
                assert_eq!(serde_json::to_value(&editor.profile).unwrap(), before);
                assert!(!editor.has_unsaved_changes());
                assert!(app.tests_idle() && app.persistence_prompt.is_none());
            }
        }
    }

    #[test]
    fn renamed_user_shows_optional_password_instead_of_previous_saved_status() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            let context = egui::Context::default();
            theme::configure_fonts(&context, locale);
            let mut profile = metadata_profile();
            let id = uuid::Uuid::new_v4();
            profile.advanced.sudo = Some(crate::model::SudoSettings {
                enabled: true,
                credentials: vec![crate::model::SudoCredential {
                    id,
                    username: "alice".into(),
                }],
            });
            let mut app = metadata_test_app(&context, locale, profile);
            let editor = app.editor.as_mut().unwrap();
            editor.sudo.stored.insert(id, Ok(true));
            editor.profile.advanced.sudo.as_mut().unwrap().credentials[0].username = "bob".into();
            let output = auth_frame(&mut app, &context, vec![]);
            assert!(
                find_text_rect(
                    &output.shapes,
                    app.catalog.text("privilege_password_optional")
                )
                .is_some()
            );
            assert!(
                find_text_rect(&output.shapes, app.catalog.text("sudo_password_saved")).is_none()
            );
        }
    }

    #[test]
    fn privilege_switches_only_change_their_own_supply_permission() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            for width in [260.0, 690.0] {
                for key in ["sudo_enabled_rect", "su_enabled_rect"] {
                    let context = egui::Context::default();
                    theme::configure_fonts(&context, locale);
                    let mut app = metadata_test_app(&context, locale, metadata_profile());
                    for _ in 0..3 {
                        privilege_frame(&mut app, &context, width, false, vec![]);
                    }
                    let rect = context
                        .data(|d| d.get_temp::<egui::Rect>(egui::Id::new(key)))
                        .unwrap();
                    for enabled in [true, false] {
                        privilege_frame(
                            &mut app,
                            &context,
                            width,
                            false,
                            click_events(rect.center()),
                        );
                        let editor = app.editor.as_ref().unwrap();
                        let settings = &editor.profile.advanced;
                        assert_eq!(
                            settings.sudo.as_ref().is_some_and(|s| s.enabled),
                            key == "sudo_enabled_rect" && enabled
                        );
                        assert_eq!(
                            settings.su.as_ref().is_some_and(|s| s.enabled),
                            key == "su_enabled_rect" && enabled
                        );
                        assert!(
                            settings
                                .sudo
                                .as_ref()
                                .is_none_or(|s| s.credentials.is_empty())
                        );
                        assert!(editor.sudo.passwords.is_empty());
                        assert!(app.tests_idle() && app.persistence_prompt.is_none());
                    }
                }
            }
        }
    }
}
