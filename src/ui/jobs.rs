//! Credential-worker lifecycle, progress and user-controlled cancellation.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use codex_hosts_bw::operation::{Control, Snapshot, Stage, Stop};
use eframe::egui;

use crate::i18n::Catalog;

const JOB_TIMEOUT: Duration = Duration::from_secs(120);

pub(super) struct Job<T> {
    receiver: Receiver<Result<T, String>>,
    control: Control,
    catalog: Catalog,
}

fn stage_key(stage: Stage) -> &'static str {
    match stage {
        Stage::Preparing => "vault_job_preparing",
        Stage::Authorizing => "vault_job_authorizing",
        Stage::Syncing => "vault_job_syncing",
        Stage::Reading => "vault_job_reading",
        Stage::Writing => "vault_job_writing",
    }
}

fn stopped_text(catalog: &Catalog, snapshot: &Snapshot) -> Option<String> {
    let key = match snapshot.stopped? {
        Stop::Cancelled => "vault_job_cancelled",
        Stop::TimedOut => "vault_job_timeout",
    };
    let mut text = catalog.format(
        key,
        &[
            ("stage", catalog.text(stage_key(snapshot.stage))),
            ("elapsed", &snapshot.elapsed.as_secs().to_string()),
        ],
    );
    if snapshot.may_have_written {
        text.push(' ');
        text.push_str(catalog.text("vault_job_write_uncertain"));
    }
    Some(text)
}

impl<T> Job<T> {
    pub(super) fn cancel(&self) {
        self.control.stop(Stop::Cancelled);
    }
    pub(super) fn poll(&self) -> Option<Result<T, String>> {
        self.control.check_timeout();
        match self.receiver.try_recv() {
            Ok(result) => Some(
                match stopped_text(&self.catalog, &self.control.snapshot()) {
                    Some(error) => Err(error),
                    None => result,
                },
            ),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(self
                .catalog
                .text("vault_job_worker_stopped")
                .to_owned())),
        }
    }

    pub(super) fn show(&self, ui: &mut egui::Ui, catalog: &Catalog, service: &str) {
        let snapshot = self.control.snapshot();
        ui.ctx().request_repaint_after(Duration::from_millis(200));
        ui.horizontal(|ui| {
            ui.spinner();
            let stage = if snapshot.stopped.is_some() {
                catalog.text("vault_job_stopping")
            } else {
                catalog.text(stage_key(snapshot.stage))
            };
            let text = catalog.format(
                "vault_job_progress",
                &[
                    ("service", service),
                    ("stage", stage),
                    ("elapsed", &snapshot.elapsed.as_secs().to_string()),
                    (
                        "stage_elapsed",
                        &snapshot.stage_elapsed.as_secs().to_string(),
                    ),
                ],
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        snapshot.stopped.is_none(),
                        egui::Button::new(catalog.text("cancel")),
                    )
                    .clicked()
                {
                    self.cancel();
                }
                ui.add(egui::Label::new(&text).truncate())
                    .on_hover_text(&text);
            });
        });
    }
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        self.control.stop(Stop::Cancelled);
    }
}

pub(super) fn spawn_job<T: Send + 'static>(
    repaint: egui::Context,
    catalog: Catalog,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Job<T> {
    let (sender, receiver) = mpsc::channel();
    let control = Control::new(JOB_TIMEOUT);
    let worker = control.clone();
    std::thread::spawn(move || {
        let _scope = worker.enter();
        let result = work();
        worker.check_timeout();
        worker.finish();
        if sender.send(result).is_ok() {
            repaint.request_repaint();
        }
    });
    Job {
        receiver,
        control,
        catalog,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopped_writes_are_localized_and_never_claim_to_have_rolled_back() {
        for locale in ["en", "ja", "zh-CN", "zh-TW"] {
            let catalog = Catalog::for_locale(Some(locale));
            let control = Control::new(JOB_TIMEOUT);
            control.stage(Stage::Writing);
            control.stop(Stop::Cancelled);
            let text = stopped_text(&catalog, &control.snapshot()).unwrap();
            assert!(text.contains(catalog.text("vault_job_write_uncertain")));
            assert!(!text.contains("vault_job_"));
        }
    }

    #[test]
    fn progress_and_cancel_are_visible_and_keep_waiting_for_worker_cleanup() {
        for locale in ["en", "ja", "zh-CN", "zh-TW"] {
            let context = egui::Context::default();
            super::super::theme::configure_fonts(&context, locale);
            let catalog = Catalog::for_locale(Some(locale));
            let (sender, receiver) = mpsc::channel::<Result<(), String>>();
            let control = Control::new(JOB_TIMEOUT);
            control.stage(Stage::Syncing);
            let job = Job {
                receiver,
                control: control.clone(),
                catalog: catalog.clone(),
            };
            let input = |events| egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(700.0, 100.0),
                )),
                events,
                ..Default::default()
            };
            let mut output =
                context.run_ui(input(Vec::new()), |ui| job.show(ui, &catalog, "Bitwarden"));
            for _ in 0..2 {
                output =
                    context.run_ui(input(Vec::new()), |ui| job.show(ui, &catalog, "Bitwarden"));
            }
            let texts = super::super::tests::painted_texts(&output.shapes);
            assert!(
                texts
                    .iter()
                    .any(|(text, _, _)| text.contains(catalog.text("vault_job_syncing")))
            );
            let cancel = texts
                .iter()
                .find(|(text, _, _)| text == catalog.text("cancel"))
                .unwrap()
                .1;
            assert!(
                cancel.right() <= 700.0,
                "the cancel control must stay visible"
            );
            let pos = cancel.center();
            for pressed in [true, false] {
                let _ = context.run_ui(
                    input(vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: Default::default(),
                        },
                    ]),
                    |ui| job.show(ui, &catalog, "Bitwarden"),
                );
            }
            assert_eq!(control.snapshot().stopped, Some(Stop::Cancelled));
            assert!(
                job.poll().is_none(),
                "cancellation does not permit another writer before cleanup"
            );
            control.finish();
            sender.send(Ok(())).unwrap();
            assert!(job.poll().unwrap().is_err());
        }
    }
}
