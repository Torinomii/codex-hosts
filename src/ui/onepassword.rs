//! 1Password in the window: its setup wizard (find `op.exe`, let the app
//! authorize codex-hosts, choose or create the vault), its page and card on
//! the credential services, its state in the toolbar and the host list, and
//! the refresh that mirrors the vault into the list. Its hosts are created,
//! edited and archived through the shared vault jobs, like Bitwarden's.

use std::path::PathBuf;

use eframe::egui::{self, Color32, RichText};

use super::HostsApp;
use super::bitwarden::{Job, SetupDone, back_link, block_on, close_row, problem_key, spawn_job};
use super::services::{HostGroup, ServiceCard, service_card};
use super::theme;
use crate::i18n::Catalog;
use crate::model::{OnePasswordAccount, OnePasswordSettings, VaultProvider};
use crate::onepassword::cli::{self, OpAccount, OpError, OpExecutable, OpItemSummary, OpVault};
use crate::onepassword::{self, Op};
use crate::ssh::RemoteFailure;
use crate::storage::HostStore;
use crate::vault_hosts::ItemProblem;

const INSTALL_COMMAND: &str = "winget install AgileBits.1Password.CLI";
/// The vault the wizard proposes to create: hosts are best kept apart.
const DEFAULT_VAULT: &str = "codex-hosts";

/// What the last `op` call of this window found out about the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OpState {
    Connected,
    NotAuthorized,
    AppClosed,
    IntegrationOff,
    Failed,
}

impl OpState {
    fn of_error(error: &OpError) -> Self {
        match error {
            OpError::NotAuthorized => Self::NotAuthorized,
            OpError::AppUnavailable => Self::AppClosed,
            OpError::IntegrationOff => Self::IntegrationOff,
            // The app answered; only the item was not there.
            OpError::ItemMissing => Self::Connected,
            _ => Self::Failed,
        }
    }

    /// The state a tool-protocol code tells; `None` for codes that say
    /// nothing about the app.
    fn of_code(code: &str) -> Option<Self> {
        match code {
            "ONEPASSWORD_LOCKED" => Some(Self::NotAuthorized),
            "ONEPASSWORD_NOT_CONFIGURED" => Some(Self::IntegrationOff),
            "ONEPASSWORD_ITEM_MISSING" | "ONEPASSWORD_ITEM_MISMATCH" => Some(Self::Connected),
            "ONEPASSWORD_DISABLED" => None,
            code if code.starts_with("ONEPASSWORD_") => Some(Self::Failed),
            _ => None,
        }
    }

    fn status_key(self) -> &'static str {
        match self {
            Self::Connected => "op_status_connected",
            Self::NotAuthorized => "op_status_not_authorized",
            Self::AppClosed => "op_status_app_closed",
            Self::IntegrationOff => "op_status_integration_off",
            Self::Failed => "op_status_failed",
        }
    }

    fn color(self, visuals: &egui::Visuals) -> Color32 {
        match self {
            Self::Connected => theme::success_color(visuals),
            Self::Failed => visuals.error_fg_color,
            _ => visuals.warn_fg_color,
        }
    }
}

/// A failed `op` step as the window words it.
struct Failure {
    state: OpState,
    text: String,
}

impl Failure {
    fn of(catalog: &Catalog, error: &OpError) -> Self {
        Self {
            state: OpState::of_error(error),
            text: explain(catalog, error),
        }
    }
}

/// The window's wording for an `op` failure; CLI output never appears.
fn explain(catalog: &Catalog, error: &OpError) -> String {
    let key = match error {
        OpError::NotOp => "op_err_not_cli",
        OpError::IntegrationOff => "op_err_integration",
        OpError::AppUnavailable => "op_err_app",
        OpError::NotAuthorized => "op_err_not_authorized",
        OpError::ItemMissing => "op_err_item_missing",
        OpError::BadName => "op_err_vault_name",
        other => return catalog.format("op_err_failed", &[("error", &other.to_string())]),
    };
    catalog.text(key).to_owned()
}

/// The window's wording for a failed 1Password host operation; `None` for
/// a failure of another service.
pub(super) fn explain_code(catalog: &Catalog, error: &RemoteFailure) -> Option<String> {
    let key = match error.code {
        "ONEPASSWORD_LOCKED" => "op_err_not_authorized",
        "ONEPASSWORD_NOT_CONFIGURED" => "op_err_integration",
        "ONEPASSWORD_ITEM_MISSING" => "op_err_item_missing",
        "ONEPASSWORD_CLI_UNSUPPORTED" => "op_err_not_cli",
        "ONEPASSWORD_DISABLED" => "op_err_disabled",
        code if code.starts_with("ONEPASSWORD_") => {
            return Some(catalog.format("op_err_failed", &[("error", &error.message)]));
        }
        _ => return None,
    };
    Some(catalog.text(key).to_owned())
}

enum Event {
    Checked {
        path: PathBuf,
        version: String,
        accounts: Vec<OpAccount>,
    },
    Vaults(Vec<OpVault>),
    Created(OpVault),
    Listed(Vec<OpItemSummary>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Cli,
    Authorize,
    Vault,
}

const STEPS: [(Step, &str); 3] = [
    (Step::Cli, "op_step_cli"),
    (Step::Authorize, "op_step_authorize"),
    (Step::Vault, "op_step_vault"),
];

/// The setup wizard's answers so far.
struct Wizard {
    step: Step,
    cli_path: String,
    /// The path was found by the search, not typed.
    found: bool,
    version: Option<String>,
    accounts: Vec<OpAccount>,
    account: usize,
    vaults: Vec<OpVault>,
    vault: usize,
    new_vault: bool,
    vault_name: String,
    error: Option<String>,
}

impl Wizard {
    fn new(settings: &OnePasswordSettings) -> Self {
        let mut wizard = Self {
            step: Step::Cli,
            cli_path: String::new(),
            found: false,
            version: None,
            accounts: Vec::new(),
            account: 0,
            vaults: Vec::new(),
            vault: 0,
            new_vault: false,
            vault_name: DEFAULT_VAULT.to_owned(),
            error: None,
        };
        match &settings.cli_path {
            Some(path) if path.is_file() => wizard.cli_path = path.display().to_string(),
            _ => wizard.search(),
        }
        wizard
    }

    /// Fills in the path of the CLI where it is installed.
    fn search(&mut self) {
        let found = cli::detect();
        self.found = found.is_some();
        self.cli_path = found
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        self.version = None;
        self.accounts.clear();
    }

    fn account(&self) -> Option<&OpAccount> {
        self.accounts.get(self.account)
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum View {
    #[default]
    Closed,
    Wizard,
    Page,
}

/// 1Password's part of the window.
#[derive(Default)]
pub(super) struct OnePasswordUi {
    view: View,
    wizard: Option<Wizard>,
    job: Option<Job<Result<Event, Failure>>>,
    /// What the last call found; `None` before this window's first.
    state: Option<OpState>,
    /// The line under the empty section, from the last refresh.
    note: Option<String>,
    problems: Vec<(String, ItemProblem)>,
    /// The page's last result: (is_error, text).
    message: Option<(bool, String)>,
    confirm_remove: bool,
}

impl OnePasswordUi {
    pub(super) fn reading(&self) -> bool {
        self.job.is_some()
    }

    /// The wizard or the page is open in the credential services.
    pub(super) fn showing(&self) -> bool {
        self.view != View::Closed
    }

    pub(super) fn title<'a>(&self, catalog: &'a Catalog) -> &'a str {
        match self.view {
            View::Wizard => catalog.text("op_setup_title"),
            _ => "1Password",
        }
    }

    pub(super) fn close(&mut self) {
        self.view = View::Closed;
        self.wizard = None;
        self.confirm_remove = false;
    }

    /// Records what a host operation found: `None` when it succeeded, else
    /// its failure code.
    pub(super) fn note_result(&mut self, code: Option<&str>) {
        match code {
            None => self.state = Some(OpState::Connected),
            Some(code) => {
                if let Some(state) = OpState::of_code(code) {
                    self.state = Some(state);
                }
            }
        }
    }
}

/// What the user did on 1Password's card, page or wizard this frame.
#[derive(Clone, Copy)]
pub(super) enum PageAction {
    StartSetup,
    OpenPage,
    SetEnabled(bool),
    Back,
    Refresh,
    RemoveSetup,
    Search,
    Check,
    Authorize,
    Finish,
    StepBack,
    CancelSetup,
    Close,
}

impl HostsApp {
    fn start_op_job(
        &mut self,
        work: impl FnOnce(&Catalog) -> Result<Event, Failure> + Send + 'static,
    ) {
        if self.onepassword.job.is_some() {
            return;
        }
        self.onepassword.message = None;
        if let Some(wizard) = self.onepassword.wizard.as_mut() {
            wizard.error = None;
        }
        let catalog = self.catalog.clone();
        self.onepassword.job = Some(spawn_job(self.repaint_context.clone(), move || {
            Ok(work(&catalog))
        }));
    }

    /// Lists the vault on a worker; the window mirrors it when it lands.
    /// The first `op` call of this process makes 1Password ask the user.
    pub(super) fn refresh_onepassword_hosts(&mut self) {
        let Ok(op) = Op::from_settings(&self.store.onepassword) else {
            return;
        };
        self.start_op_job(move |catalog| {
            block_on(op.list_items())
                .map_err(|text| Failure {
                    state: OpState::Failed,
                    text,
                })?
                .map(Event::Listed)
                .map_err(|error| Failure::of(catalog, &error))
        });
    }

    /// Opens the credential services on 1Password: its page once set up,
    /// else the wizard.
    pub(super) fn open_onepassword_page(&mut self) {
        self.open_credential_manager();
        if self.service_configured(HostGroup::OnePassword) {
            self.onepassword.view = View::Page;
        } else {
            self.onepassword_act(PageAction::StartSetup);
        }
    }

    pub(super) fn poll_onepassword(&mut self) {
        let Some(result) = self.onepassword.job.as_ref().and_then(|job| job.poll()) else {
            return;
        };
        self.onepassword.job = None;
        let failure = match result {
            Ok(Ok(event)) => {
                self.onepassword_event(event);
                return;
            }
            Ok(Err(failure)) => failure,
            Err(text) => Failure {
                state: OpState::Failed,
                text,
            },
        };
        let panel = &mut self.onepassword;
        panel.state = Some(failure.state);
        match panel.view {
            View::Wizard => {
                if let Some(wizard) = panel.wizard.as_mut() {
                    wizard.error = Some(failure.text);
                }
            }
            View::Page => panel.message = Some((true, failure.text)),
            View::Closed => self.set_status(super::status::StatusKind::Error, failure.text),
        }
    }

    fn onepassword_event(&mut self, event: Event) {
        let configured = self.store.onepassword.account.clone();
        let panel = &mut self.onepassword;
        match event {
            Event::Checked {
                path,
                version,
                accounts,
            } => {
                let Some(wizard) = panel.wizard.as_mut() else {
                    return;
                };
                wizard.cli_path = path.display().to_string();
                wizard.version = Some(version);
                wizard.account = configured
                    .as_ref()
                    .and_then(|known| {
                        accounts
                            .iter()
                            .position(|account| account.account_uuid == known.account_id)
                    })
                    .unwrap_or_default();
                wizard.accounts = accounts;
                wizard.step = Step::Authorize;
            }
            Event::Vaults(vaults) => {
                panel.state = Some(OpState::Connected);
                let Some(wizard) = panel.wizard.as_mut() else {
                    return;
                };
                let known = configured.as_ref().map(|known| known.vault_id.as_str());
                let chosen = vaults
                    .iter()
                    .position(|vault| Some(vault.id.as_str()) == known)
                    .or_else(|| vaults.iter().position(|vault| vault.name == DEFAULT_VAULT));
                wizard.new_vault = chosen.is_none();
                wizard.vault = chosen.unwrap_or_default();
                wizard.vaults = vaults;
                wizard.step = Step::Vault;
            }
            Event::Created(vault) => {
                panel.state = Some(OpState::Connected);
                self.finish_onepassword_setup(vault);
            }
            Event::Listed(items) => {
                panel.state = Some(OpState::Connected);
                self.finish_onepassword_listing(&items);
            }
        }
    }

    fn finish_onepassword_listing(&mut self, items: &[OpItemSummary]) {
        let Ok(op) = Op::from_settings(&self.store.onepassword) else {
            return;
        };
        let mut store = self.store.clone();
        let result = onepassword::reconcile(&op, &mut store, items);
        if result.changed
            && let Err(error) = store.save()
        {
            self.set_status_format("storage_error", &[("error", &error.to_string())]);
            return;
        }
        self.store = store;
        self.forget_missing_selection();
        let mut text = self
            .catalog
            .format("op_refreshed", &[("count", &result.hosts.to_string())]);
        if !result.problems.is_empty() {
            let names: Vec<&str> = result
                .problems
                .iter()
                .map(|(name, _)| name.as_str())
                .collect();
            text.push_str("  ");
            text.push_str(&self.catalog.format(
                "op_problems",
                &[
                    ("count", &result.problems.len().to_string()),
                    ("names", &names.join(", ")),
                ],
            ));
        }
        let panel = &mut self.onepassword;
        panel.note = Some(if result.hosts == 0 {
            self.catalog.text("op_section_empty").to_owned()
        } else {
            String::new()
        });
        panel.problems = result.problems;
        panel.message = Some((false, text.clone()));
        self.set_status(super::status::StatusKind::Success, text);
    }

    fn forget_missing_selection(&mut self) {
        if self
            .selected
            .is_some_and(|id| !self.store.hosts.iter().any(|host| host.id == id))
        {
            self.selected = None;
        }
    }

    fn save_onepassword_settings(&mut self) {
        if let Err(error) = self.store.save() {
            self.set_status_format("storage_error", &[("error", &error.to_string())]);
        }
    }

    /// The hosts of another account or vault mean nothing any more.
    fn drop_onepassword_mirrors(&mut self) {
        self.store
            .hosts
            .retain(|host| host.vault_provider() != Some(VaultProvider::OnePassword));
        self.onepassword.note = None;
        self.onepassword.problems.clear();
        self.forget_missing_selection();
    }

    /// Saves the wizard's choice, closes the dialog and says what is next.
    fn finish_onepassword_setup(&mut self, vault: OpVault) {
        let Some(wizard) = self.onepassword.wizard.take() else {
            return;
        };
        let Some(chosen) = wizard.account().cloned() else {
            return;
        };
        let account = OnePasswordAccount {
            account_id: chosen.account_uuid,
            url: chosen.url,
            email: chosen.email,
            vault_id: vault.id,
            vault_name: vault.name,
        };
        let same_place = self
            .store
            .onepassword
            .account
            .as_ref()
            .is_some_and(|known| {
                known.account_id == account.account_id && known.vault_id == account.vault_id
            });
        if !same_place {
            self.drop_onepassword_mirrors();
        }
        let done = SetupDone {
            service: HostGroup::OnePassword,
            account: if account.email.is_empty() {
                account.url.clone()
            } else {
                account.email.clone()
            },
            place: account.vault_name.clone(),
            store_error: None,
        };
        self.store.onepassword = OnePasswordSettings {
            enabled: true,
            cli_path: Some(PathBuf::from(wizard.cli_path.trim())),
            account: Some(account),
        };
        self.save_onepassword_settings();
        self.close_credential_manager();
        self.set_status_key("op_setup_done");
        self.setup_done = Some(done);
        // The vault's items become hosts right away.
        self.refresh_onepassword_hosts();
    }

    pub(super) fn onepassword_act(&mut self, act: PageAction) {
        let panel = &mut self.onepassword;
        match act {
            PageAction::StartSetup => {
                panel.wizard = Some(Wizard::new(&self.store.onepassword));
                panel.view = View::Wizard;
                panel.confirm_remove = false;
            }
            PageAction::OpenPage => {
                panel.view = View::Page;
                panel.message = None;
            }
            PageAction::SetEnabled(enabled) => {
                self.store.onepassword.enabled = enabled;
                self.save_onepassword_settings();
            }
            PageAction::Back => panel.close(),
            PageAction::Refresh => self.refresh_onepassword_hosts(),
            PageAction::RemoveSetup => {
                panel.confirm_remove = false;
                panel.state = None;
                panel.message = None;
                self.store.onepassword = OnePasswordSettings::default();
                self.drop_onepassword_mirrors();
                self.save_onepassword_settings();
                self.onepassword.close();
            }
            PageAction::Search => {
                if let Some(wizard) = panel.wizard.as_mut() {
                    wizard.search();
                }
            }
            PageAction::Check => {
                let Some(wizard) = panel.wizard.as_ref() else {
                    return;
                };
                let path = PathBuf::from(wizard.cli_path.trim());
                self.start_op_job(move |catalog| {
                    let failed = |error: OpError| Failure::of(catalog, &error);
                    let exe = OpExecutable::new(path).map_err(failed)?;
                    let (version, accounts) = block_on(onepassword::check_cli(&exe))
                        .map_err(|text| Failure {
                            state: OpState::Failed,
                            text,
                        })?
                        .map_err(failed)?;
                    Ok(Event::Checked {
                        path: exe.path().to_path_buf(),
                        version,
                        accounts,
                    })
                });
            }
            PageAction::Authorize | PageAction::Finish => {
                let Some(wizard) = panel.wizard.as_ref() else {
                    return;
                };
                let (Some(account), Ok(exe)) = (
                    wizard.account().map(|account| account.account_uuid.clone()),
                    OpExecutable::new(wizard.cli_path.trim()),
                ) else {
                    return;
                };
                if matches!(act, PageAction::Finish) && !wizard.new_vault {
                    if let Some(vault) = wizard.vaults.get(wizard.vault).cloned() {
                        self.finish_onepassword_setup(vault);
                    }
                    return;
                }
                let new_vault =
                    matches!(act, PageAction::Finish).then(|| wizard.vault_name.clone());
                self.start_op_job(move |catalog| {
                    let event = match new_vault {
                        None => block_on(onepassword::list_vaults(&exe, &account))
                            .map(|listed| listed.map(Event::Vaults)),
                        Some(name) => block_on(onepassword::create_vault(&exe, &account, &name))
                            .map(|created| created.map(Event::Created)),
                    };
                    event
                        .map_err(|text| Failure {
                            state: OpState::Failed,
                            text,
                        })?
                        .map_err(|error| Failure::of(catalog, &error))
                });
            }
            PageAction::StepBack => {
                if let Some(wizard) = panel.wizard.as_mut() {
                    wizard.error = None;
                    wizard.step = match wizard.step {
                        Step::Vault => Step::Authorize,
                        _ => Step::Cli,
                    };
                }
            }
            PageAction::CancelSetup => {
                let configured = self.service_configured(HostGroup::OnePassword);
                let panel = &mut self.onepassword;
                panel.wizard = None;
                panel.view = if configured { View::Page } else { View::Closed };
            }
            PageAction::Close => self.close_credential_manager(),
        }
    }

    /// The toolbar's state word for 1Password once it is set up and on.
    pub(super) fn onepassword_chip(&self, visuals: &egui::Visuals) -> (&'static str, Color32) {
        match self.onepassword.state {
            None => ("op_status_unknown", visuals.weak_text_color()),
            Some(state) => (state.status_key(), state.color(visuals)),
        }
    }

    /// The line under the 1Password section while it is empty.
    pub(super) fn onepassword_section_note(&self) -> String {
        let key = match self.onepassword.state {
            Some(OpState::NotAuthorized) => "op_section_not_authorized",
            Some(OpState::AppClosed) => "op_section_app_closed",
            Some(OpState::IntegrationOff) => "op_err_integration",
            _ => match &self.onepassword.note {
                Some(note) => return note.clone(),
                None => "op_section_unloaded",
            },
        };
        self.catalog.text(key).to_owned()
    }
}

fn host_count(store: &HostStore) -> usize {
    store
        .hosts
        .iter()
        .filter(|host| host.vault_provider() == Some(VaultProvider::OnePassword))
        .count()
}

/// The state line of the card and the page.
fn state_text(catalog: &Catalog, settings: &OnePasswordSettings, panel: &OnePasswordUi) -> String {
    if !settings.enabled {
        return catalog.text("service_status_disabled").to_owned();
    }
    catalog
        .text(panel.state.map_or("op_status_unknown", OpState::status_key))
        .to_owned()
}

/// 1Password's card on the credential services overview.
pub(super) fn card(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    store: &HostStore,
    panel: &mut OnePasswordUi,
) -> Option<PageAction> {
    let settings = &store.onepassword;
    let configured = settings.account.is_some() && settings.cli_path.is_some();
    let mut enabled = settings.enabled;
    let status = match settings.account.as_ref().filter(|_| configured) {
        None => catalog.text("op_not_configured").to_owned(),
        Some(account) => format!(
            "{}  ·  {}  ·  {}",
            state_text(catalog, settings, panel),
            account.url,
            account.vault_name
        ),
    };
    let clicked = service_card(
        ui,
        catalog,
        ServiceCard {
            name: "1Password",
            text: catalog.text("service_onepassword_text"),
            hosts: host_count(store),
            enabled: configured.then_some(&mut enabled),
            status,
            busy: panel.reading(),
            button: Some(catalog.text(if configured {
                "service_manage"
            } else {
                "bw_start_setup"
            })),
        },
    );
    if enabled != settings.enabled {
        return Some(PageAction::SetEnabled(enabled));
    }
    clicked.then_some(if configured {
        PageAction::OpenPage
    } else {
        PageAction::StartSetup
    })
}

/// The wizard or the page, whichever is open.
pub(super) fn page(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    store: &HostStore,
    panel: &mut OnePasswordUi,
) -> Option<PageAction> {
    let busy = panel.reading();
    if panel.view == View::Wizard
        && let Some(wizard) = panel.wizard.as_mut()
    {
        return wizard_body(ui, catalog, wizard, busy);
    }
    let mut action = None;
    if back_link(ui, catalog) {
        action = Some(PageAction::Back);
    }
    ui.add_space(4.0);
    let settings = &store.onepassword;
    if let Some(account) = &settings.account {
        egui::Grid::new("op_page")
            .num_columns(2)
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                ui.label(catalog.text("op_account"));
                if account.email.is_empty() {
                    ui.label(&account.url);
                } else {
                    ui.label(format!("{}  ({})", account.url, account.email));
                }
                ui.end_row();
                ui.label(catalog.text("op_vault"));
                ui.label(&account.vault_name);
                ui.end_row();
                ui.label(catalog.text("op_cli_path"));
                ui.label(
                    settings
                        .cli_path
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_default(),
                );
                ui.end_row();
                ui.label(catalog.text("op_state"));
                let color = match panel.state.filter(|_| settings.enabled) {
                    Some(state) => state.color(ui.visuals()),
                    None => ui.visuals().weak_text_color(),
                };
                ui.colored_label(color, state_text(catalog, settings, panel));
                ui.end_row();
                ui.label(catalog.text("op_hosts"));
                ui.label(host_count(store).to_string());
                ui.end_row();
            });
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                settings.enabled && !busy,
                theme::primary_button(ui, catalog.text("vault_action_refresh")),
            )
            .clicked()
        {
            action = Some(PageAction::Refresh);
        }
        if ui
            .add_enabled(!busy, egui::Button::new(catalog.text("op_reconfigure")))
            .clicked()
        {
            action = Some(PageAction::StartSetup);
        }
        if busy {
            ui.spinner();
        }
    });
    if busy {
        prompt_callout(ui, catalog);
    }
    if let Some((is_error, text)) = &panel.message {
        let color = if *is_error {
            ui.visuals().error_fg_color
        } else {
            ui.visuals().strong_text_color()
        };
        ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
    }
    if !panel.problems.is_empty() {
        ui.add_space(6.0);
        ui.label(RichText::new(catalog.text("op_problems_title")).strong());
        for (name, problem) in &panel.problems {
            ui.label(format!("•  {name}: {}", catalog.text(problem_key(problem))));
        }
    }
    ui.add_space(12.0);
    if panel.confirm_remove {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(catalog.text("op_confirm_remove")).wrap());
            ui.horizontal(|ui| {
                if ui
                    .add(theme::danger_confirm_button(
                        ui,
                        catalog.text("bw_remove_setup"),
                    ))
                    .clicked()
                {
                    action = Some(PageAction::RemoveSetup);
                }
                if ui.button(catalog.text("cancel")).clicked() {
                    panel.confirm_remove = false;
                }
            });
        });
    } else if ui
        .add_enabled(
            !busy,
            theme::danger_button(ui, catalog.text("bw_remove_setup")),
        )
        .clicked()
    {
        panel.confirm_remove = true;
    }
    if close_row(ui, catalog) {
        action = Some(PageAction::Close);
    }
    action
}

/// Says that 1Password is about to ask, and where to answer.
fn prompt_callout(ui: &mut egui::Ui, catalog: &Catalog) {
    egui::Frame::group(ui.style())
        .fill(theme::section_fill(ui.visuals()))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(catalog.text("op_prompt_title")).strong());
            ui.add(egui::Label::new(catalog.text("op_prompt_text")).wrap());
        });
}

fn wizard_body(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    wizard: &mut Wizard,
    busy: bool,
) -> Option<PageAction> {
    let keys: Vec<&str> = STEPS.iter().map(|(_, key)| *key).collect();
    let current = STEPS
        .iter()
        .position(|(step, _)| *step == wizard.step)
        .unwrap_or_default();
    let busy_text = busy.then(|| {
        catalog.text(if wizard.step == Step::Cli {
            "busy"
        } else {
            "op_waiting_prompt"
        })
    });
    super::dialogs::wizard_header(
        ui,
        catalog,
        &keys,
        current,
        wizard.error.as_deref(),
        busy_text,
    );
    let mut action = None;
    match wizard.step {
        Step::Cli => {
            ui.add(egui::Label::new(catalog.text("op_cli_hint")).wrap());
            ui.horizontal(|ui| {
                let field = ui.add_enabled(
                    !busy,
                    egui::TextEdit::singleline(&mut wizard.cli_path)
                        .desired_width(400.0)
                        .hint_text(catalog.text("op_cli_missing")),
                );
                if field.changed() {
                    wizard.found = false;
                    wizard.version = None;
                }
                if ui
                    .add_enabled(!busy, egui::Button::new(catalog.text("wizard_browse_cli")))
                    .clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("op", &["exe"])
                        .pick_file()
                {
                    wizard.cli_path = path.display().to_string();
                    wizard.found = false;
                }
                if ui
                    .add_enabled(!busy, egui::Button::new(catalog.text("op_search_again")))
                    .clicked()
                {
                    action = Some(PageAction::Search);
                }
            });
            if wizard.found {
                ui.label(
                    RichText::new(catalog.text("op_cli_found_auto"))
                        .small()
                        .color(theme::success_color(ui.visuals())),
                );
            }
            if wizard.cli_path.trim().is_empty() {
                ui.add_space(4.0);
                ui.add(egui::Label::new(catalog.text("op_install_hint")).wrap());
                ui.horizontal(|ui| {
                    ui.code(INSTALL_COMMAND);
                    if ui.small_button(catalog.text("op_copy_command")).clicked() {
                        ui.ctx().copy_text(INSTALL_COMMAND.to_owned());
                    }
                });
            }
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(
                    RichText::new(catalog.text("op_integration_hint"))
                        .small()
                        .weak(),
                )
                .wrap(),
            );
            let ready = !busy && std::path::Path::new(wizard.cli_path.trim()).is_file();
            action = action.or(
                wizard_buttons(ui, catalog, busy, ready, "wizard_next", false).map(
                    |act| match act {
                        PageAction::Finish => PageAction::Check,
                        other => other,
                    },
                ),
            );
        }
        Step::Authorize => {
            if let Some(version) = &wizard.version {
                ui.label(
                    RichText::new(catalog.format("op_cli_found", &[("version", version)]))
                        .small()
                        .weak(),
                );
            }
            ui.horizontal(|ui| {
                ui.label(catalog.text("op_account"));
                let label = |account: &OpAccount| {
                    if account.email.is_empty() {
                        account.url.clone()
                    } else {
                        format!("{} ({})", account.url, account.email)
                    }
                };
                if wizard.accounts.len() <= 1 {
                    ui.label(wizard.account().map(label).unwrap_or_default());
                } else {
                    let selected = wizard.account().map(label).unwrap_or_default();
                    ui.add_enabled_ui(!busy, |ui| {
                        egui::ComboBox::from_id_salt("op_account")
                            .selected_text(selected)
                            .show_ui(ui, |ui| {
                                for (index, account) in wizard.accounts.iter().enumerate() {
                                    if ui
                                        .selectable_label(index == wizard.account, label(account))
                                        .clicked()
                                    {
                                        wizard.account = index;
                                    }
                                }
                            });
                    });
                }
            });
            ui.add_space(6.0);
            prompt_callout(ui, catalog);
            let ready = !busy && wizard.account().is_some();
            action =
                wizard_buttons(ui, catalog, busy, ready, "op_authorize", true).map(
                    |act| match act {
                        PageAction::Finish => PageAction::Authorize,
                        other => other,
                    },
                );
        }
        Step::Vault => {
            ui.add(egui::Label::new(catalog.text("op_vault_text")).wrap());
            ui.add_space(4.0);
            ui.add_enabled_ui(!busy, |ui| {
                ui.radio_value(&mut wizard.new_vault, true, catalog.text("op_vault_new"));
                if wizard.new_vault {
                    ui.indent("op_vault_new", |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut wizard.vault_name).desired_width(320.0),
                        );
                    });
                }
                if !wizard.vaults.is_empty() {
                    ui.radio_value(
                        &mut wizard.new_vault,
                        false,
                        catalog.text("op_vault_existing"),
                    );
                    if !wizard.new_vault {
                        ui.indent("op_vault_existing", |ui| {
                            let selected = wizard
                                .vaults
                                .get(wizard.vault)
                                .map(|vault| vault.name.clone())
                                .unwrap_or_default();
                            egui::ComboBox::from_id_salt("op_vault")
                                .selected_text(selected)
                                .show_ui(ui, |ui| {
                                    for (index, vault) in wizard.vaults.iter().enumerate() {
                                        if ui
                                            .selectable_label(index == wizard.vault, &vault.name)
                                            .clicked()
                                        {
                                            wizard.vault = index;
                                        }
                                    }
                                });
                        });
                    }
                }
            });
            let ready = !busy
                && if wizard.new_vault {
                    !wizard.vault_name.trim().is_empty()
                } else {
                    wizard.vaults.get(wizard.vault).is_some()
                };
            action = wizard_buttons(ui, catalog, busy, ready, "op_finish", true);
        }
    }
    action
}

/// Primary, Back (after the first step) and Cancel; the primary click is
/// reported as `Finish` for the caller to name.
fn wizard_buttons(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    busy: bool,
    ready: bool,
    primary: &str,
    back: bool,
) -> Option<PageAction> {
    let mut action = None;
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(ready, theme::primary_button(ui, catalog.text(primary)))
            .clicked()
        {
            action = Some(PageAction::Finish);
        }
        if back
            && ui
                .add_enabled(!busy, egui::Button::new(catalog.text("wizard_back")))
                .clicked()
        {
            action = Some(PageAction::StepBack);
        }
        if ui.button(catalog.text("cancel")).clicked() {
            action = Some(PageAction::CancelSetup);
        }
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_tell_the_state_of_the_app() {
        assert_eq!(
            OpState::of_code("ONEPASSWORD_LOCKED"),
            Some(OpState::NotAuthorized)
        );
        assert_eq!(
            OpState::of_code("ONEPASSWORD_ITEM_MISSING"),
            Some(OpState::Connected)
        );
        assert_eq!(OpState::of_code("ONEPASSWORD_DISABLED"), None);
        assert_eq!(OpState::of_code("BITWARDEN_LOCKED"), None);
        assert_eq!(
            OpState::of_error(&OpError::AppUnavailable),
            OpState::AppClosed
        );
    }

    fn window_texts(app: &mut HostsApp, context: &egui::Context) -> Vec<String> {
        let mut texts = Vec::new();
        for _ in 0..3 {
            let output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1100.0, 760.0),
                    )),
                    ..Default::default()
                },
                |ui| app.render(ui),
            );
            texts = super::super::tests::painted_texts(&output.shapes)
                .into_iter()
                .map(|(text, _, _)| text)
                .collect();
        }
        texts
    }

    #[test]
    fn the_wizard_fills_in_the_cli_and_warns_before_the_prompt() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            let context = egui::Context::default();
            super::super::theme::configure_fonts(&context, locale);
            let mut app = super::super::tests::metadata_test_app(
                &context,
                locale,
                crate::model::HostProfile::new("local".into()),
            );
            let catalog = app.catalog.clone();
            app.open_onepassword_page();
            assert!(
                app.onepassword.showing(),
                "{locale}: not set up, so the wizard"
            );
            {
                let wizard = app.onepassword.wizard.as_mut().unwrap();
                wizard.cli_path = "C:/op/op.exe".into();
                wizard.found = true;
            }
            let texts = window_texts(&mut app, &context);
            let has = |texts: &[String], key: &str| texts.iter().any(|t| t == catalog.text(key));
            assert!(has(&texts, "op_setup_title"), "{locale}: wizard title");
            assert!(has(&texts, "op_cli_found_auto"), "{locale}: found path");
            assert!(
                texts.iter().any(|t| t == "C:/op/op.exe"),
                "{locale}: the path is filled in"
            );

            // While the authorization call runs, the window says where the
            // 1Password prompt is and what to press.
            {
                let wizard = app.onepassword.wizard.as_mut().unwrap();
                wizard.step = Step::Authorize;
                wizard.accounts = vec![OpAccount {
                    url: "my.1password.com".into(),
                    email: "me@example.com".into(),
                    account_uuid: "TESTACCOUNTAAAAAAAAAAAAAAA".into(),
                }];
            }
            let (hold, wait) = std::sync::mpsc::channel::<()>();
            app.onepassword.job = Some(spawn_job(context.clone(), move || {
                let _ = wait.recv();
                Err("stopped".to_owned())
            }));
            let texts = window_texts(&mut app, &context);
            assert!(has(&texts, "op_prompt_title"), "{locale}: prompt callout");
            assert!(has(&texts, "op_waiting_prompt"), "{locale}: waiting line");
            drop(hold);
        }
    }

    #[test]
    fn a_set_up_1password_gets_a_menu_entry_a_section_and_a_chip() {
        let context = egui::Context::default();
        super::super::theme::configure_fonts(&context, "en");
        let mut app = super::super::tests::metadata_test_app(
            &context,
            "en",
            crate::model::HostProfile::new("local".into()),
        );
        assert!(app.ready_vaults().is_empty());
        app.store.onepassword.cli_path = Some(PathBuf::from("C:/op/op.exe"));
        app.store.onepassword.account = Some(OnePasswordAccount::default());
        assert_eq!(app.ready_vaults(), [HostGroup::OnePassword]);
        let visuals = egui::Visuals::default();
        let chip = |app: &HostsApp| {
            app.service_chip(HostGroup::OnePassword, &visuals)
                .unwrap()
                .0
        };
        assert_eq!(chip(&app), "1Password · Not checked yet");
        app.onepassword.note_result(Some("ONEPASSWORD_LOCKED"));
        assert_eq!(chip(&app), "1Password · Not authorized");
        assert_eq!(
            app.onepassword_section_note(),
            app.catalog.text("op_section_not_authorized")
        );
        app.onepassword.note_result(None);
        assert_eq!(chip(&app), "1Password · Connected");
        assert!(app.service_chip(HostGroup::Bitwarden, &visuals).is_none());
        assert_eq!(
            app.host_count_summary().as_deref(),
            Some("Local 1 · 1Password 0")
        );
    }

    #[test]
    fn only_1password_failures_are_worded_here() {
        let catalog = Catalog::for_locale(Some("en"));
        let locked = RemoteFailure::new("ONEPASSWORD_LOCKED", "x");
        assert_eq!(
            explain_code(&catalog, &locked).as_deref(),
            Some(catalog.text("op_err_not_authorized"))
        );
        let other = RemoteFailure::new("BITWARDEN_LOCKED", "x");
        assert!(explain_code(&catalog, &other).is_none());
    }
}
