//! Settings shared by both password-manager login caches.

use eframe::egui;

use super::HostsApp;
use crate::i18n::Catalog;
use crate::model::{MAX_LOGIN_CACHE_MINUTES, VaultProvider};
use crate::vault_hosts::cache;

pub(super) fn controls(ui: &mut egui::Ui, catalog: &Catalog, minutes: &mut [u32; 2]) -> [bool; 2] {
    let mut clear = [false; 2];
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.strong(catalog.text("vault_cache_title"));
        ui.add(egui::Label::new(catalog.text("vault_cache_hint")).wrap());
        egui::Grid::new("vault_cache_settings").show(ui, |ui| {
            for (index, service) in ["Bitwarden", "1Password"].into_iter().enumerate() {
                ui.label(service);
                ui.add(
                    egui::DragValue::new(&mut minutes[index])
                        .range(0..=MAX_LOGIN_CACHE_MINUTES)
                        .speed(1),
                );
                ui.label(catalog.text("vault_cache_minutes"));
                clear[index] = ui.button(catalog.text("vault_cache_clear")).clicked();
                ui.end_row();
            }
        });
    });
    clear
}

impl HostsApp {
    pub(super) fn apply_vault_cache_settings(&mut self, minutes: [u32; 2], clear: [bool; 2]) {
        let before = [
            self.store.bitwarden.login_cache_minutes,
            self.store.onepassword.login_cache_minutes,
        ];
        if before != minutes {
            self.store.bitwarden.login_cache_minutes = minutes[0];
            self.store.onepassword.login_cache_minutes = minutes[1];
            if let Err(error) = self.store.save() {
                self.store.bitwarden.login_cache_minutes = before[0];
                self.store.onepassword.login_cache_minutes = before[1];
                self.set_status_format("storage_error", &[("error", &error.to_string())]);
                return;
            }
        }
        for (index, provider) in [VaultProvider::Bitwarden, VaultProvider::OnePassword]
            .into_iter()
            .enumerate()
        {
            if (before[index] != minutes[index] || clear[index])
                && let Err(error) = cache::reset(provider)
            {
                self.set_status_format("storage_error", &[("error", &error)]);
                return;
            }
        }
        if clear.iter().any(|clear| *clear) {
            self.set_status_key("vault_cache_cleared");
        } else if before != minutes {
            self.set_status_key("vault_cache_saved");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BitwardenSettings, HostProfile, OnePasswordSettings};
    use crate::storage::HostStore;

    #[test]
    fn old_settings_default_to_sixty_minutes_and_disabled_values_round_trip() {
        let bw: BitwardenSettings = serde_json::from_str("{}").unwrap();
        let mut op: OnePasswordSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(bw.login_cache_minutes, 60);
        assert_eq!(op.login_cache_minutes, 60);
        assert!(
            serde_json::from_str::<OnePasswordSettings>("{\"login_cache_minutes\":1441}").is_err()
        );
        assert!(
            serde_json::from_str::<BitwardenSettings>("{\"login_cache_minutes\":1441}").is_err()
        );
        op.login_cache_minutes = 0;
        assert_eq!(
            serde_json::from_value::<OnePasswordSettings>(serde_json::to_value(op).unwrap())
                .unwrap()
                .login_cache_minutes,
            0
        );
    }

    #[test]
    fn cache_settings_are_saved_and_manual_clear_invalidates_other_instances() {
        if !super::super::tests::in_isolated_store(
            "ui::vault_cache::tests::cache_settings_are_saved_and_manual_clear_invalidates_other_instances",
        ) {
            return;
        }
        let context = egui::Context::default();
        let mut app = super::super::tests::metadata_test_app(
            &context,
            "en",
            HostProfile::new("local".into()),
        );
        app.apply_vault_cache_settings([15, 0], [false, false]);
        let stored = HostStore::load().unwrap();
        assert_eq!(stored.bitwarden.login_cache_minutes, 15);
        assert_eq!(stored.onepassword.login_cache_minutes, 0);
        let bw = cache::Epoch::current(VaultProvider::Bitwarden).unwrap();
        let op = cache::Epoch::current(VaultProvider::OnePassword).unwrap();
        app.apply_vault_cache_settings([15, 0], [true, false]);
        assert!(!bw.is_current());
        assert!(
            op.is_current(),
            "clearing one service leaves the other cache alone"
        );
        assert_eq!(app.status.text, app.catalog.text("vault_cache_cleared"));
    }

    #[test]
    fn cache_controls_are_visible_in_all_four_locales() {
        for locale in ["en", "ja", "zh-CN", "zh-TW"] {
            let context = egui::Context::default();
            super::super::theme::configure_fonts(&context, locale);
            let catalog = Catalog::for_locale(Some(locale));
            let mut minutes = [60, 60];
            let output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(740.0, 220.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    controls(ui, &catalog, &mut minutes);
                },
            );
            let texts = super::super::tests::painted_texts(&output.shapes);
            assert!(
                texts
                    .iter()
                    .any(|(text, _, _)| text == catalog.text("vault_cache_title"))
            );
            assert_eq!(
                texts
                    .iter()
                    .filter(|(text, _, _)| text == catalog.text("vault_cache_clear"))
                    .count(),
                2
            );
            assert_eq!(minutes, [60, 60]);
        }
    }
}
