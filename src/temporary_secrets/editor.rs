use super::*;
use crate::ui::theme;

const GAP: f32 = 16.0;
const CONTROL_HEIGHT: f32 = 34.0;

#[derive(Clone, Copy)]
struct Columns {
    name: f32,
    secret: f32,
    approval: f32,
}

impl Columns {
    fn for_width(width: f32) -> Option<Self> {
        (width >= 600.0).then(|| {
            let name = (width * 0.27).clamp(150.0, 240.0);
            let approval = 120.0;
            Self {
                name,
                secret: width - name - approval - 2.0 * GAP,
                approval,
            }
        })
    }
}

fn cell(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, CONTROL_HEIGHT),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_width(width);
            add(ui);
        },
    );
}

fn header(ui: &mut egui::Ui, catalog: &Catalog, key: &str, width: f32) {
    cell(ui, width, |ui| {
        let response =
            ui.add(egui::Label::new(egui::RichText::new(catalog.text(key)).strong()).wrap());
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(egui::Id::new(("temporary_header", key)), response.rect)
        });
        let _ = response;
    });
}

fn name_cell(ui: &mut egui::Ui, field: &Field) {
    // A left-aligned, bounded reference name, not a centered faux input.
    let response = ui
        .add(
            egui::Label::new(egui::RichText::new(&field.name).monospace().strong())
                .wrap()
                .halign(egui::Align::Min),
        )
        .on_hover_text(&field.name);
    #[cfg(test)]
    ui.ctx().data_mut(|data| {
        data.insert_temp(
            egui::Id::new(("temporary_layout_name", &field.name)),
            response.rect,
        )
    });
    let _ = response;
}

fn secret_cell(
    ui: &mut egui::Ui,
    field: &mut Field,
    catalog: &Catalog,
    notice: &mut Option<&'static str>,
    change: &mut Option<&'static str>,
) {
    let key = egui::Id::new(("temporary_secret_input", &field.name));
    let revealed = crate::ui::theme::password_revealed(ui, key);
    let reveal_width = CONTROL_HEIGHT + ui.spacing().item_spacing.x;
    let width = (ui.available_width() - reveal_width).max(80.0);
    let mut output = ui
        .horizontal_top(|ui| {
            let output = egui::TextEdit::multiline(&mut *field.draft)
                .id(key)
                .password(!revealed)
                .desired_rows((64.0 / ui.text_style_height(&egui::TextStyle::Body)).ceil() as usize)
                .desired_width(width)
                .min_size(egui::vec2(width, 64.0))
                .char_limit(MAX_VALUE_BYTES)
                .hint_text(catalog.text(if field.value.is_some() {
                    "temp_replace_hint"
                } else {
                    "temp_empty_hint"
                }))
                .show(ui);
            crate::ui::theme::reveal_button(ui, catalog, key);
            output
        })
        .inner;
    crate::ui::forget_password_history(ui, &output.response);
    if output.response.changed() {
        *change = Some("ok");
    }
    #[cfg(test)]
    ui.ctx().data_mut(|data| {
        data.insert_temp(
            egui::Id::new(("temporary_layout_input", &field.name)),
            output.response.rect,
        )
    });
    output.state.clear_undoer();
    output.state.store(ui.ctx(), output.response.id);
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 4.0);
        if ui
            .add_sized(
                [72.0, CONTROL_HEIGHT],
                egui::Button::new(catalog.text("temp_save")),
            )
            .clicked()
        {
            match field.save() {
                Ok(()) => {
                    *notice = None;
                    *change = Some("saved");
                }
                Err(_) => *notice = Some("temp_invalid_value"),
            }
        }
        if ui
            .add_sized(
                [72.0, CONTROL_HEIGHT],
                egui::Button::new(catalog.text("temp_clear")),
            )
            .clicked()
        {
            field.clear();
            *change = Some("ok");
        }
    });
    ui.label(
        egui::RichText::new(catalog.text(if field.value.is_some() {
            "temp_ready"
        } else {
            "temp_missing"
        }))
        .small()
        .weak(),
    );
}

fn approval_cell(
    ui: &mut egui::Ui,
    field: &mut Field,
    catalog: &Catalog,
    change: &mut Option<&'static str>,
    labelled: bool,
) {
    // The exact standard Checkbox used by the main host form (not a separate slider).
    let response = ui
        .checkbox(
            &mut field.requires_approval,
            if labelled {
                catalog.text("temp_field_approval")
            } else {
                ""
            },
        )
        .on_hover_text(catalog.text("temp_field_approval_hint"));
    if response.changed() {
        *change = Some("ok");
    }
    #[cfg(test)]
    ui.ctx().data_mut(|data| {
        data.insert_temp(
            egui::Id::new(("temporary_layout_approval", &field.name)),
            response.rect,
        )
    });
}

fn delete_button(ui: &mut egui::Ui, catalog: &Catalog, enabled: bool, width: f32) -> bool {
    let response = ui
        .add_enabled_ui(enabled, |ui| {
            ui.add_sized(
                [width, CONTROL_HEIGHT],
                egui::Button::new(catalog.text("temp_delete_all")),
            )
        })
        .inner;
    #[cfg(test)]
    ui.ctx().data_mut(|d| {
        d.insert_temp(
            egui::Id::new("temporary_delete_all_button"),
            (response.rect, response.enabled()),
        )
    });
    response.clicked()
}

impl SecretsApp {
    pub(super) fn field_toolbar(&mut self, ui: &mut egui::Ui) {
        let wide = ui.available_width() >= 600.0;
        let button_width = |key: &str| {
            ui.painter()
                .layout_no_wrap(
                    self.catalog.text(key).to_owned(),
                    egui::TextStyle::Button.resolve(ui.style()),
                    ui.visuals().text_color(),
                )
                .size()
                .x
                + 2.0 * ui.spacing().button_padding.x
        };
        let add_width = button_width("temp_add").max(96.0);
        let delete_width = button_width("temp_delete_all").max(112.0);
        let mut add = false;
        let mut delete = false;
        let name = |ui: &mut egui::Ui, value: &mut String, width: f32| {
            ui.add_sized(
                [width, CONTROL_HEIGHT],
                egui::TextEdit::singleline(value)
                    .hint_text(self.catalog.text("temp_name_hint"))
                    .desired_width(width)
                    .char_limit(256),
            );
        };
        if wide {
            ui.horizontal(|ui| {
                let width = (ui.available_width()
                    - add_width
                    - delete_width
                    - 2.0 * ui.spacing().item_spacing.x)
                    .max(120.0);
                name(ui, &mut self.new_name, width);
                add = ui
                    .add_sized(
                        [add_width, CONTROL_HEIGHT],
                        egui::Button::new(self.catalog.text("temp_add")),
                    )
                    .clicked();
                delete = delete_button(
                    ui,
                    &self.catalog,
                    !self.vault.fields.is_empty(),
                    delete_width,
                );
            });
        } else {
            name(ui, &mut self.new_name, ui.available_width());
            ui.horizontal_wrapped(|ui| {
                add = ui
                    .add_sized(
                        [add_width, CONTROL_HEIGHT],
                        egui::Button::new(self.catalog.text("temp_add")),
                    )
                    .clicked();
                delete = delete_button(
                    ui,
                    &self.catalog,
                    !self.vault.fields.is_empty(),
                    delete_width,
                );
            });
        }
        if add {
            match self.vault.request(std::slice::from_ref(&self.new_name)) {
                Ok(()) => {
                    self.new_name.clear();
                    self.notice = None;
                    self.input_status = "ok";
                }
                Err(_) => self.notice = Some("temp_invalid_field"),
            }
        }
        if delete {
            self.delete_all_prompt = Some(
                self.vault
                    .fields
                    .iter()
                    .map(|field| field.name.clone())
                    .collect(),
            );
        }
    }

    pub(super) fn delete_all_fields(&mut self) {
        // Dropping Field zeroizes both the stored secret and the unsaved draft.
        self.vault.fields.clear();
        self.new_name.clear();
        self.notice = None;
        self.record_input_status("ok");
        self.detail = None;
        self.displayed_approval = None;
    }

    pub(super) fn show_delete_all(&mut self, context: &egui::Context) {
        let Some(names) = &self.delete_all_prompt else {
            return;
        };
        // An IPC declaration while the dialog is open must not silently expand
        // the set of items the user agreed to delete.
        if !self
            .vault
            .fields
            .iter()
            .map(|field| &field.name)
            .eq(names.iter())
        {
            self.delete_all_prompt = None;
            self.notice = Some("temp_fields_changed");
            return;
        }
        let message = self.catalog.format(
            "temp_delete_all_confirm",
            &[("count", &names.len().to_string())],
        );
        let mut confirm = false;
        let mut cancel = false;
        let modal =
            egui::Modal::new(egui::Id::new("temporary_delete_all_dialog")).show(context, |ui| {
                ui.set_width((context.content_rect().width() - 80.0).clamp(220.0, 430.0));
                ui.heading(self.catalog.text("temp_delete_all"));
                ui.label(message);
                ui.label(
                    egui::RichText::new(self.catalog.text("temp_delete_all_running"))
                        .small()
                        .weak(),
                );
                ui.add_space(10.0);
                ui.horizontal_wrapped(|ui| {
                    let cancel_button = ui.button(self.catalog.text("cancel"));
                    let confirm_button = ui.button(
                        egui::RichText::new(self.catalog.text("temp_delete_confirm"))
                            .color(ui.visuals().error_fg_color),
                    );
                    cancel = cancel_button.clicked();
                    confirm = confirm_button.clicked();
                    #[cfg(test)]
                    context.data_mut(|data| {
                        data.insert_temp(
                            egui::Id::new("temporary_delete_buttons"),
                            (confirm_button.rect, cancel_button.rect),
                        )
                    });
                });
            });
        if confirm {
            self.delete_all_fields();
        }
        if confirm || cancel || modal.should_close() {
            self.delete_all_prompt = None;
        }
    }

    pub(super) fn field_rows(&mut self, ui: &mut egui::Ui) {
        if self.vault.fields.is_empty() {
            ui.label(egui::RichText::new(self.catalog.text("temp_no_fields")).weak());
            return;
        }
        let mut change = None;
        egui::Frame::new()
            .fill(theme::section_fill(ui.visuals()))
            .stroke(theme::section_stroke(ui.visuals()))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing = egui::vec2(GAP, 12.0);
                let columns = Columns::for_width(ui.available_width());
                if let Some(c) = columns {
                    ui.horizontal_top(|ui| {
                        header(ui, &self.catalog, "temp_field", c.name);
                        header(ui, &self.catalog, "temp_secret", c.secret);
                        header(ui, &self.catalog, "temp_field_approval", c.approval);
                    });
                    ui.separator();
                }
                for (index, field) in self.vault.fields.iter_mut().enumerate() {
                    if index > 0 {
                        ui.separator();
                    }
                    if let Some(c) = columns {
                        ui.horizontal_top(|ui| {
                            cell(ui, c.name, |ui| name_cell(ui, field));
                            cell(ui, c.secret, |ui| {
                                secret_cell(ui, field, &self.catalog, &mut self.notice, &mut change)
                            });
                            cell(ui, c.approval, |ui| {
                                approval_cell(ui, field, &self.catalog, &mut change, false)
                            });
                        });
                    } else {
                        name_cell(ui, field);
                        secret_cell(ui, field, &self.catalog, &mut self.notice, &mut change);
                        approval_cell(ui, field, &self.catalog, &mut change, true);
                    }
                }
            });
        if let Some(status) = change {
            self.record_input_status(status);
        }
    }
}
