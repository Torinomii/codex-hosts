//! The places a host can be kept (the built-in store and each password
//! manager) and what the window says or does for each. The host list, the
//! "New host" menu, the editor, the toolbar and the status bar ask this
//! module instead of naming a service, so a further password manager is a
//! variant here, its catalog keys, and its own adapter and setup page.

use eframe::egui::{self, Color32, RichText};

use super::HostsApp;
use super::bitwarden::VaultState;
use super::host_list::VaultAction;
use super::theme;
use crate::i18n::Catalog;
use crate::model::{HostProfile, VaultProvider};

/// Which store defines a host; the list shows one section per store once a
/// password manager is set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HostGroup {
    Local,
    Bitwarden,
    OnePassword,
}

impl HostGroup {
    pub(super) const ALL: [Self; 3] = [Self::Local, Self::Bitwarden, Self::OnePassword];
    /// The password managers, in the order the window lists them.
    pub(super) const VAULTS: [Self; 2] = [Self::Bitwarden, Self::OnePassword];

    pub(super) fn of(host: &HostProfile) -> Self {
        Self::of_provider(host.vault_provider())
    }

    pub(super) fn of_provider(provider: Option<VaultProvider>) -> Self {
        match provider {
            None => Self::Local,
            Some(VaultProvider::Bitwarden) => Self::Bitwarden,
            Some(VaultProvider::OnePassword) => Self::OnePassword,
        }
    }

    pub(super) fn provider(self) -> Option<VaultProvider> {
        match self {
            Self::Local => None,
            Self::Bitwarden => Some(VaultProvider::Bitwarden),
            Self::OnePassword => Some(VaultProvider::OnePassword),
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    /// The section header and "New host" entry.
    pub(super) fn label_key(self) -> &'static str {
        match self {
            Self::Local => "group_local",
            Self::Bitwarden => "group_bitwarden",
            Self::OnePassword => "group_onepassword",
        }
    }

    /// The name in the host count of the status bar.
    fn count_key(self) -> &'static str {
        match self {
            Self::Local => "host_count_local",
            other => other.label_key(),
        }
    }

    /// What the window says about a password manager's hosts; `None` for
    /// the built-in store.
    pub(super) fn texts(self) -> Option<&'static VaultTexts> {
        match self {
            Self::Local => None,
            Self::Bitwarden => Some(&BITWARDEN_TEXTS),
            Self::OnePassword => Some(&ONEPASSWORD_TEXTS),
        }
    }
}

/// Catalog keys for one password manager's hosts.
pub(super) struct VaultTexts {
    pub(super) new_host_hint: &'static str,
    pub(super) host_title: &'static str,
    pub(super) host_hint: &'static str,
    pub(super) password_hint: &'static str,
    pub(super) saving: &'static str,
    pub(super) saved: &'static str,
    pub(super) deleted: &'static str,
    pub(super) delete_message: &'static str,
    pub(super) conflict: &'static str,
    pub(super) chip_hint: &'static str,
    pub(super) done_title: &'static str,
    /// Takes `{account}` and `{place}` (the folder or vault).
    pub(super) done_text: &'static str,
    pub(super) done_next: [&'static str; 2],
    pub(super) done_add_host: &'static str,
}

static BITWARDEN_TEXTS: VaultTexts = VaultTexts {
    new_host_hint: "new_vault_host_hint",
    host_title: "vault_host_title",
    host_hint: "vault_host_hint",
    password_hint: "vault_password_hint",
    saving: "vault_saving",
    saved: "vault_saved",
    deleted: "vault_deleted",
    delete_message: "vault_delete_message",
    conflict: "vault_conflict",
    chip_hint: "vault_chip_hint",
    done_title: "bw_done_title",
    done_text: "bw_done_text",
    done_next: ["bw_done_next_add", "bw_done_next_edit"],
    done_add_host: "bw_done_add_host",
};

static ONEPASSWORD_TEXTS: VaultTexts = VaultTexts {
    new_host_hint: "op_new_host_hint",
    host_title: "op_host_title",
    host_hint: "op_host_hint",
    password_hint: "op_password_hint",
    saving: "op_saving",
    saved: "op_saved",
    deleted: "op_deleted",
    delete_message: "op_delete_message",
    conflict: "op_conflict",
    chip_hint: "op_chip_hint",
    done_title: "op_done_title",
    done_text: "op_done_text",
    done_next: ["op_done_next_add", "op_done_next_edit"],
    done_add_host: "op_done_add_host",
};

/// Which sections the user folded away.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct GroupFolds([bool; HostGroup::ALL.len()]);

impl GroupFolds {
    pub(super) fn collapsed(self, group: HostGroup) -> bool {
        self.0[group.index()]
    }

    pub(super) fn toggle(&mut self, group: HostGroup) {
        self.0[group.index()] = !self.0[group.index()];
    }

    pub(super) fn unfold(&mut self, group: HostGroup) {
        self.0[group.index()] = false;
    }
}

/// One service's card on the credential-services page.
pub(super) struct ServiceCard<'a> {
    pub(super) name: &'a str,
    pub(super) text: &'a str,
    pub(super) hosts: usize,
    /// The on/off switch, once the service is set up.
    pub(super) enabled: Option<&'a mut bool>,
    pub(super) status: String,
    pub(super) busy: bool,
    /// "Start setup…" or "Manage…"; `None` for the built-in store.
    pub(super) button: Option<&'a str>,
}

/// Draws the card; `true` when its button was clicked.
pub(super) fn service_card(ui: &mut egui::Ui, catalog: &Catalog, card: ServiceCard<'_>) -> bool {
    let mut clicked = false;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(card.name).strong());
            if let Some(enabled) = card.enabled {
                ui.checkbox(enabled, catalog.text("service_enabled"));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(
                        catalog.format("service_hosts", &[("count", &card.hosts.to_string())]),
                    )
                    .weak(),
                );
            });
        });
        ui.add(egui::Label::new(RichText::new(card.text).weak().small()).wrap());
        ui.horizontal(|ui| {
            ui.label(RichText::new(card.status).small());
            if card.busy {
                ui.spinner();
            }
            if let Some(button) = card.button {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    clicked = ui.add(theme::primary_button(ui, button)).clicked();
                });
            }
        });
    });
    clicked
}

/// A password manager's section in the host list: the line under it while
/// it is empty, and its header button.
pub(super) struct Section {
    pub(super) note: String,
    pub(super) action: VaultAction,
}

impl HostsApp {
    /// Set up and switched on, so its hosts are listed and served.
    pub(super) fn service_ready(&self, group: HostGroup) -> bool {
        match group.provider() {
            None => true,
            Some(provider) => {
                self.store.provider_enabled(provider) && self.service_configured(group)
            }
        }
    }

    /// Set up, whether switched on or not.
    pub(super) fn service_configured(&self, group: HostGroup) -> bool {
        match group {
            HostGroup::Local => true,
            HostGroup::Bitwarden => {
                let settings = &self.store.bitwarden;
                settings.profile.is_some() && settings.cli_path.is_some()
            }
            HostGroup::OnePassword => {
                let settings = &self.store.onepassword;
                settings.account.is_some() && settings.cli_path.is_some()
            }
        }
    }

    /// The password managers whose sections and hosts the window shows.
    pub(super) fn ready_vaults(&self) -> Vec<HostGroup> {
        HostGroup::VAULTS
            .into_iter()
            .filter(|group| self.service_ready(*group))
            .collect()
    }

    pub(super) fn service_section(&self, group: HostGroup) -> Option<Section> {
        if group == HostGroup::Local || !self.service_ready(group) {
            return None;
        }
        Some(match group {
            HostGroup::Local => unreachable!("checked above"),
            HostGroup::Bitwarden => Section {
                note: if self.vault_state == Some(VaultState::SignedOut) {
                    self.catalog.text("vault_section_signed_out").to_owned()
                } else {
                    self.vault_note
                        .clone()
                        .unwrap_or_else(|| self.catalog.text("vault_section_unloaded").to_owned())
                },
                action: VaultAction::for_state(self.vault_state, self.vault_job_running()),
            },
            HostGroup::OnePassword => Section {
                note: self.onepassword_section_note(),
                action: if self.onepassword.reading() {
                    VaultAction::Reading
                } else {
                    VaultAction::Refresh
                },
            },
        })
    }

    /// The header button of a section was clicked.
    pub(super) fn service_header_act(&mut self, group: HostGroup, action: VaultAction) {
        match (group, action) {
            (_, VaultAction::Reading) | (HostGroup::Local, _) => {}
            (HostGroup::Bitwarden, VaultAction::LogIn) => self.open_service_page(group),
            (HostGroup::Bitwarden, _) => self.refresh_vault_hosts(),
            (HostGroup::OnePassword, _) => self.refresh_onepassword_hosts(),
        }
    }

    /// Opens the form on a new host of `group`; nothing is written until Save.
    pub(super) fn new_service_host(&mut self, group: HostGroup) {
        match group.provider() {
            None => self.new_host(),
            Some(provider) => self.new_vault_host(provider),
        }
        self.collapsed_groups.unfold(group);
    }

    /// Opens the credential services on the page of `group`.
    pub(super) fn open_service_page(&mut self, group: HostGroup) {
        match group {
            HostGroup::Local => self.open_credential_manager(),
            HostGroup::Bitwarden => self.open_bitwarden_page(),
            HostGroup::OnePassword => self.open_onepassword_page(),
        }
    }

    /// The toolbar's state word for a password manager that is set up.
    pub(super) fn service_chip(
        &self,
        group: HostGroup,
        visuals: &egui::Visuals,
    ) -> Option<(String, Color32)> {
        if group == HostGroup::Local || !self.service_configured(group) {
            return None;
        }
        let provider = group.provider()?;
        let (key, color) = if !self.store.provider_enabled(provider) {
            ("service_status_disabled", visuals.weak_text_color())
        } else {
            match group {
                HostGroup::Local => return None,
                HostGroup::Bitwarden => match self.vault_state {
                    Some(VaultState::Unlocked) => {
                        ("bw_status_unlocked", theme::success_color(visuals))
                    }
                    Some(VaultState::Locked) => ("bw_status_locked", visuals.warn_fg_color),
                    Some(VaultState::SignedOut) => ("bw_status_signed_out", visuals.warn_fg_color),
                    None => ("bw_status_unknown", visuals.weak_text_color()),
                },
                HostGroup::OnePassword => self.onepassword_chip(visuals),
            }
        };
        Some((
            format!(
                "{} · {}",
                self.catalog.text(group.label_key()),
                self.catalog.text(key)
            ),
            color,
        ))
    }

    /// The store of the host open in the form.
    pub(super) fn editor_group(&self) -> HostGroup {
        self.editor
            .as_ref()
            .map_or(HostGroup::Local, |editor| HostGroup::of(&editor.profile))
    }

    /// The catalog key `pick` chooses among a password manager's texts.
    pub(super) fn vault_key(
        group: HostGroup,
        pick: fn(&'static VaultTexts) -> &'static str,
    ) -> &'static str {
        pick(group.texts().unwrap_or(&BITWARDEN_TEXTS))
    }

    pub(super) fn set_vault_status(
        &mut self,
        group: HostGroup,
        pick: fn(&'static VaultTexts) -> &'static str,
    ) {
        self.set_status_key(Self::vault_key(group, pick));
    }

    /// "Local 12 · Bitwarden 1 · 1Password 0" while the list is split.
    pub(super) fn host_count_summary(&self) -> Option<String> {
        let vaults = self.ready_vaults();
        if vaults.is_empty() {
            return None;
        }
        let parts: Vec<String> = std::iter::once(HostGroup::Local)
            .chain(vaults)
            .map(|group| {
                let count = self
                    .store
                    .hosts
                    .iter()
                    .filter(|host| HostGroup::of(host) == group)
                    .count();
                self.catalog.format(
                    "host_count_part",
                    &[
                        ("name", self.catalog.text(group.count_key())),
                        ("count", &count.to_string()),
                    ],
                )
            })
            .collect();
        Some(parts.join(" · "))
    }
}
