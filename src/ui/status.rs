use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};

use super::HostsApp;
use super::theme;

pub(super) const TOAST_DURATION: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatusKind {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub(super) struct StatusMessage {
    pub kind: StatusKind,
    pub text: String,
    pub shown_at: Option<Instant>,
}

impl StatusMessage {
    pub fn new(kind: StatusKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            shown_at: (kind != StatusKind::Info).then(Instant::now),
        }
    }

    pub fn info(text: impl Into<String>) -> Self {
        Self::new(StatusKind::Info, text)
    }

    pub fn toast_visible(&self, now: Instant) -> bool {
        self.shown_at
            .is_some_and(|shown_at| now.duration_since(shown_at) < TOAST_DURATION)
    }
}

pub(super) fn status_kind_for_key(key: &str) -> StatusKind {
    match key {
        "status_test_all_done" => StatusKind::Info,
        "import_file_kept_warning"
        | "batch_nothing_selected"
        | "test_all_empty"
        | "tray_unavailable"
        | "host_changed"
        | "chain_in_use" => StatusKind::Warning,
        "status_saved"
        | "status_test_ok"
        | "batch_deleted"
        | "batch_export_succeeded"
        | "import_succeeded"
        | "import_succeeded_with_credentials"
        | "template_saved"
        | "import_file_deleted"
        | "fido_found"
        | "hosts_refreshed" => StatusKind::Success,
        _ if key.ends_with("_error")
            || key.ends_with("_failed")
            || key.ends_with("_timeout")
            || key.starts_with("validation_")
            || key == "fido_not_found" =>
        {
            StatusKind::Error
        }
        _ => StatusKind::Info,
    }
}

pub(super) fn status_color(kind: StatusKind, visuals: &egui::Visuals) -> Color32 {
    match kind {
        StatusKind::Info => visuals.weak_text_color(),
        StatusKind::Success => theme::success_color(visuals),
        StatusKind::Warning => visuals.warn_fg_color,
        StatusKind::Error => visuals.error_fg_color,
    }
}

impl HostsApp {
    pub(super) fn set_status(&mut self, kind: StatusKind, text: impl Into<String>) {
        self.status = StatusMessage::new(kind, text);
        if kind != StatusKind::Info {
            self.repaint_context.request_repaint_after(TOAST_DURATION);
        }
    }

    pub(super) fn set_status_key(&mut self, key: &str) {
        let text = self.catalog.text(key).to_owned();
        self.set_status(status_kind_for_key(key), text);
    }

    pub(super) fn set_status_format(&mut self, key: &str, values: &[(&str, &str)]) {
        let text = self.catalog.format(key, values);
        self.set_status(status_kind_for_key(key), text);
    }

    pub(super) fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let color = status_color(self.status.kind, ui.visuals());
            ui.label(RichText::new("●").color(color).small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(12.0);
                let summary = if self.testing_all {
                    let running = self.test_operations.len() + self.pending_tests.len();
                    self.catalog.format(
                        "status_testing_progress",
                        &[("remaining", &running.to_string())],
                    )
                } else {
                    self.catalog.format(
                        "host_count",
                        &[("count", &self.store.hosts.len().to_string())],
                    )
                };
                ui.label(RichText::new(summary).weak());
                ui.separator();
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(&self.status.text).truncate())
                        .on_hover_text(&self.status.text);
                });
            });
        });
    }

    pub(super) fn toasts(&mut self, context: &egui::Context) {
        let now = Instant::now();
        if !self.status.toast_visible(now) {
            return;
        }
        context.request_repaint_after(Duration::from_millis(250));
        let kind = self.status.kind;
        let mut dismiss = false;
        egui::Area::new(egui::Id::new("status_toast"))
            .anchor(egui::Align2::RIGHT_TOP, [-20.0, 64.0])
            .order(egui::Order::Foreground)
            .interactable(true)
            .show(context, |ui| {
                let color = status_color(kind, ui.visuals());
                egui::Frame::popup(ui.style())
                    .stroke(egui::Stroke::new(1.5, color))
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_max_width(380.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("●").color(color));
                            ui.add(egui::Label::new(&self.status.text).wrap());
                            if ui.small_button("×").clicked() {
                                dismiss = true;
                            }
                        });
                    });
            });
        if dismiss {
            self.status.shown_at = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_keys_map_to_severity() {
        assert_eq!(status_kind_for_key("storage_error"), StatusKind::Error);
        assert_eq!(status_kind_for_key("import_failed"), StatusKind::Error);
        assert_eq!(status_kind_for_key("validation_alias"), StatusKind::Error);
        assert_eq!(
            status_kind_for_key("status_test_timeout"),
            StatusKind::Error
        );
        assert_eq!(
            status_kind_for_key("import_file_kept_warning"),
            StatusKind::Warning
        );
        assert_eq!(status_kind_for_key("status_saved"), StatusKind::Success);
        assert_eq!(status_kind_for_key("status_ready"), StatusKind::Info);
        assert_eq!(status_kind_for_key("testing"), StatusKind::Info);
    }

    #[test]
    fn info_messages_never_toast() {
        let now = Instant::now();
        assert!(!StatusMessage::info("ready").toast_visible(now));
        assert!(StatusMessage::new(StatusKind::Success, "saved").toast_visible(now));
        assert!(
            !StatusMessage::new(StatusKind::Error, "old")
                .toast_visible(now + TOAST_DURATION + Duration::from_millis(1))
        );
    }
}
