use eframe::egui::{self, RichText};

use super::{HostsApp, retain_visible_selection, theme};
use crate::i18n::Catalog;

impl HostsApp {
    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        retain_visible_selection(
            &self.store.hosts,
            &self.host_filter,
            &mut self.batch_selected,
        );
        let mut open_import = false;
        let mut test_all = false;
        let mut begin_batch = false;
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if !self.launch.codex_edit {
                ui.add_enabled_ui(!self.batch_mode, |ui| {
                    if ui.button(self.catalog.text("import_hosts")).clicked() {
                        open_import = true;
                    }
                    if ui
                        .add_enabled(
                            self.tests_idle(),
                            egui::Button::new(self.catalog.text("test_all")),
                        )
                        .clicked()
                    {
                        test_all = true;
                    }
                    if ui.button(self.catalog.text("batch_manage")).clicked() {
                        begin_batch = true;
                    }
                });
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                self.language_selector(ui, context);
                ui.label(self.catalog.text("language"));
                if !self.launch.codex_edit
                    && !self.tray_available
                    && ui.button(self.catalog.text("tray_exit")).clicked()
                {
                    self.exiting = true;
                    context.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if self.temporary.is_some()
                    && ui.button(self.catalog.text("temp_title")).clicked()
                    && let Some(temporary) = &mut self.temporary
                {
                    temporary.visible = true;
                }
            });
        });
        if open_import {
            self.import_window_open = true;
        }
        if test_all {
            self.start_all_tests();
        }
        if begin_batch {
            self.begin_batch_mode();
        }
    }

    fn language_selector(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        let current = self.catalog.locale().to_owned();
        let selected_name = Catalog::available()
            .iter()
            .find(|language| language.locale == current)
            .map(|language| language.display_name)
            .unwrap_or(current.as_str());
        let mut chosen = None;
        egui::ComboBox::from_id_salt("language_selector")
            .selected_text(selected_name)
            .width(130.0)
            .show_ui(ui, |ui| {
                for language in Catalog::available() {
                    if ui
                        .selectable_label(language.locale == current, language.display_name)
                        .clicked()
                    {
                        chosen = Some(language.locale);
                    }
                }
            });
        if let Some(locale) = chosen {
            self.catalog = Catalog::for_locale(Some(locale));
            theme::configure_fonts(context, locale);
            self.store.preferred_locale = Some(locale.to_owned());
            context.send_viewport_cmd(egui::ViewportCommand::Title(
                self.catalog.text("app_title").to_owned(),
            ));
            match self.store.save() {
                Ok(()) => self.set_status_key("status_ready"),
                Err(error) => {
                    self.set_status_format("storage_error", &[("error", &error.to_string())])
                }
            }
        }
    }

    /// Contextual action bar shown under the toolbar while batch mode is active.
    pub(super) fn selection_bar(&mut self, ui: &mut egui::Ui) {
        let visible = self
            .store
            .hosts
            .iter()
            .filter(|host| self.host_filter.matches(host))
            .count();
        let all_selected = visible > 0
            && self
                .store
                .hosts
                .iter()
                .filter(|host| self.host_filter.matches(host))
                .all(|host| self.batch_selected.contains(&host.id));
        let count = self.batch_selected.len();
        let mut toggle_selection = false;
        let mut delete_batch = false;
        let mut export_batch = false;
        let mut cancel_batch = ui.input(|input| input.key_pressed(egui::Key::Escape));
        egui::Frame::new()
            .fill(theme::accent_bar_fill(ui.visuals()))
            .inner_margin(egui::Margin::symmetric(12, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(
                            self.catalog
                                .format("batch_selected_count", &[("count", &count.to_string())]),
                        )
                        .strong(),
                    );
                    ui.separator();
                    if ui
                        .add_enabled(
                            visible > 0,
                            egui::Button::new(self.catalog.text(if all_selected {
                                "deselect_all"
                            } else {
                                "select_all"
                            })),
                        )
                        .clicked()
                    {
                        toggle_selection = true;
                    }
                    if ui
                        .add_enabled(count > 0, egui::Button::new(self.catalog.text("export")))
                        .clicked()
                    {
                        export_batch = true;
                    }
                    if ui
                        .add_enabled(count > 0, egui::Button::new(self.catalog.text("delete")))
                        .clicked()
                    {
                        delete_batch = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button(format!("{}  (Esc)", self.catalog.text("cancel")))
                            .clicked()
                        {
                            cancel_batch = true;
                        }
                    });
                });
            });
        if toggle_selection {
            self.toggle_batch_selection();
        }
        if export_batch {
            self.request_batch_export();
        }
        if delete_batch {
            self.request_batch_delete();
        }
        if cancel_batch && !self.any_dialog_open() {
            self.cancel_batch_mode();
        }
    }
}
