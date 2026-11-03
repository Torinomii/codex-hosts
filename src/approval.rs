//! One-use authorization of a Codex SSH call, held by the existing GUI process.
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use eframe::egui;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::i18n::Catalog;
use crate::model::{HostProfile, Protocol, resolve_ssh_chain};
use crate::ssh::RemoteFailure;
use crate::temporary_secrets::{self, Request};

const LIFETIME: Duration = Duration::from_secs(300);
const MAX_REVIEWS: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewHost {
    alias: String,
    address: String,
    port: u16,
    username: String,
    requires_approval: bool,
    protocol: Protocol,
    is_target: bool,
    is_jump: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    hosts: Vec<ReviewHost>,
    commands: Vec<String>,
    stdin: Option<String>,
    sudo_user: Option<String>,
    /// The same dialog asks for the Bitwarden master password: a vault host
    /// is in the call and reading it would otherwise prompt right after.
    #[serde(default)]
    unlock_vault: bool,
    #[serde(skip)]
    uses_vault: bool,
}

impl Review {
    pub(crate) fn new(
        targets: &[HostProfile],
        all: &[HostProfile],
        commands: Vec<String>,
        stdin: Option<&str>,
        sudo_user: Option<String>,
    ) -> Result<Self, RemoteFailure> {
        let target_ids: HashSet<_> = targets.iter().map(|host| host.id).collect();
        let mut seen: HashMap<Uuid, usize> = HashMap::new();
        let mut hosts: Vec<ReviewHost> = Vec::new();
        let mut uses_vault = false;
        for target in targets {
            let chain = if target.protocol == Protocol::Ssh {
                resolve_ssh_chain(target, all).map_err(|_| {
                    RemoteFailure::new("INVALID_HOST_CHAIN", "The SSH host chain is invalid.")
                })?
            } else {
                vec![target]
            };
            for host in chain {
                uses_vault |= host.is_vault_host();
                let is_jump = host.id != target.id;
                if let Some(&index) = seen.get(&host.id) {
                    hosts[index].is_jump |= is_jump;
                } else {
                    seen.insert(host.id, hosts.len());
                    hosts.push(ReviewHost {
                        alias: host.alias.clone(),
                        address: host.address.clone(),
                        port: host.port,
                        username: host.username.clone(),
                        requires_approval: host.protocol == Protocol::Ssh
                            && host.advanced.require_call_approval,
                        protocol: host.protocol,
                        is_target: target_ids.contains(&host.id),
                        is_jump,
                    });
                }
            }
        }
        Ok(Self {
            hosts,
            commands,
            stdin: stdin.map(str::to_owned),
            sudo_user,
            unlock_vault: false,
            uses_vault,
        })
    }

    fn required(&self) -> bool {
        self.hosts.iter().any(|host| host.requires_approval)
    }

    fn summary(&self, catalog: &Catalog) -> String {
        catalog.format(
            "approval_call_summary",
            &[
                (
                    "hosts",
                    &self
                        .hosts
                        .iter()
                        .filter(|host| host.is_target)
                        .count()
                        .to_string(),
                ),
                (
                    "jumps",
                    &self
                        .hosts
                        .iter()
                        .filter(|host| host.is_jump)
                        .count()
                        .to_string(),
                ),
                ("commands", &self.commands.len().to_string()),
            ],
        )
    }
}

/// Each tool handler calls this once before credentials, connections or workers.
/// Nothing is cached across calls, including calls that reuse an SSH session.
/// When the call's vault hosts would ask for the master password next, the
/// approval asks for it too, so the user answers one dialog.
pub(crate) async fn authorize(mut review: Review) -> Result<(), RemoteFailure> {
    if !review.required() {
        return Ok(());
    }
    review.unlock_vault = review.uses_vault && crate::bitwarden::unlock_due();
    let response = tokio::task::spawn_blocking(|| {
        temporary_secrets::request_gui(Request::Show { temporary: false })
    })
    .await
    .map_err(|_| failure("APPROVAL_UNAVAILABLE"))?
    .map_err(|_| failure("APPROVAL_UNAVAILABLE"))?;
    let session = response.session;
    authorize_session(session, review).await
}

pub(crate) async fn authorize_session(session: Uuid, review: Review) -> Result<(), RemoteFailure> {
    let id = Uuid::new_v4();
    let mut guard = CancelOnDrop {
        session,
        id,
        active: true,
    };
    let started = Instant::now();
    let response = call(session, Request::ReviewSsh { id, review }).await?;
    if response.status != "pending" || response.operation_id != Some(id) {
        return Err(failure("APPROVAL_UNAVAILABLE"));
    }
    loop {
        if started.elapsed() >= LIFETIME {
            return Err(failure("APPROVAL_EXPIRED"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let response = call(session, Request::TakeSshApproval { id }).await?;
        if response.operation_id != Some(id) {
            return Err(failure("APPROVAL_UNAVAILABLE"));
        }
        match response.status.as_str() {
            "pending" => {}
            "approved" => {
                guard.active = false;
                return Ok(());
            }
            "expired" => return Err(failure("APPROVAL_EXPIRED")),
            "cancelled" => return Err(failure("APPROVAL_CANCELLED")),
            _ => return Err(failure("APPROVAL_UNAVAILABLE")),
        }
    }
}

pub(crate) fn authorize_sync(review: Review) -> Result<(), RemoteFailure> {
    crate::ssh::runtime().block_on(authorize(review))
}

async fn call(
    session: Uuid,
    request: Request,
) -> Result<temporary_secrets::Response, RemoteFailure> {
    tokio::task::spawn_blocking(move || temporary_secrets::call(session, request))
        .await
        .map_err(|_| failure("APPROVAL_UNAVAILABLE"))?
        .map_err(|_| failure("APPROVAL_UNAVAILABLE"))
}

fn failure(code: &'static str) -> RemoteFailure {
    RemoteFailure::new(
        code,
        "SSH call authorization was not granted; no remote command was started.",
    )
}

struct CancelOnDrop {
    session: Uuid,
    id: Uuid,
    active: bool,
}
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.active
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let session = self.session;
            let id = self.id;
            runtime.spawn_blocking(move || {
                let _ = temporary_secrets::call(session, Request::CancelSshApproval { id });
            });
        }
    }
}

struct Pending {
    id: Uuid,
    review: Option<Review>,
    status: &'static str,
    expires: Instant,
}

#[derive(Default)]
pub(crate) struct Queue {
    calls: VecDeque<Pending>,
    batch: Option<Batch>,
    serial: u64,
    detail: Option<Detail>,
    feedback: crate::approval_ui::Feedback,
    expired_notices: usize,
    /// The master password typed into a dialog whose calls read the vault.
    master_password: Zeroizing<String>,
    unlock: Unlock,
    unlock_error: Option<String>,
}

/// An allowed decision that waits for the window to unlock the vault: the
/// calls stay pending until it succeeds, and stay open for another try when
/// it fails.
#[derive(Default)]
enum Unlock {
    #[default]
    Idle,
    Requested(Vec<Uuid>, Zeroizing<String>),
    Running(Vec<Uuid>),
}

impl Unlock {
    fn busy(&self) -> bool {
        !matches!(self, Self::Idle)
    }
}

/// The master-password row of a dialog whose calls read the vault; true
/// when Enter submits it.
fn unlock_row(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    password: &mut Zeroizing<String>,
    error: Option<&str>,
    busy: bool,
) -> bool {
    ui.add(egui::Label::new(catalog.text("approval_unlock_text")).wrap());
    if let Some(error) = error {
        ui.colored_label(ui.visuals().warn_fg_color, error);
    }
    ui.horizontal(|ui| {
        ui.label(catalog.text("master_password"));
        // Shared by the batch and the detail view, which show together.
        let key = egui::Id::new("approval_master_password");
        let revealed = crate::ui::theme::password_revealed(ui, key);
        let response = ui.add_enabled(
            !busy,
            egui::TextEdit::singleline(&mut **password)
                .password(!revealed)
                .desired_width(240.0),
        );
        crate::ui::forget_password_history(ui, &response);
        crate::ui::theme::reveal_button(ui, catalog, key);
        if busy {
            ui.spinner();
        }
        response.lost_focus()
            && ui.input(|input| input.key_pressed(egui::Key::Enter))
            && !password.is_empty()
    })
    .inner
}

struct Batch {
    ids: Vec<Uuid>,
    serial: u64,
    ready_at: f64,
}

#[derive(Clone, Copy)]
struct Detail {
    id: Uuid,
    command: Option<usize>,
    focus: bool,
}
impl Queue {
    #[cfg(test)]
    pub(crate) fn active_id(&self) -> Option<Uuid> {
        self.calls
            .iter()
            .find(|call| call.status == "pending")
            .map(|call| call.id)
    }

    pub(crate) fn pending(&self) -> bool {
        self.calls.iter().any(|call| call.status == "pending")
    }
    pub(crate) fn expire(&mut self) {
        let now = Instant::now();
        for call in &mut self.calls {
            if call.status == "pending" && now >= call.expires {
                call.status = "expired";
                self.expired_notices += 1;
            }
        }
        self.calls
            .retain(|call| now < call.expires + Duration::from_secs(30));
    }
    fn room(&mut self) -> bool {
        self.expire();
        if self.calls.len() >= MAX_REVIEWS {
            if let Some(index) = self.calls.iter().position(|call| call.status != "pending") {
                self.calls.remove(index);
            } else {
                return false;
            }
        }
        true
    }
    pub(crate) fn submit(&mut self, id: Uuid, review: Review) -> &'static str {
        // A cancellation tombstone prevents a cancelled create request that was
        // still in transit from opening an orphaned authorization window.
        if self.calls.iter().any(|call| call.id == id) || !self.room() {
            return "unavailable";
        }
        self.calls.push_back(Pending {
            id,
            review: Some(review),
            status: "pending",
            expires: Instant::now() + LIFETIME,
        });
        "pending"
    }
    pub(crate) fn take(&mut self, id: Uuid) -> &'static str {
        self.expire();
        let Some(index) = self.calls.iter().position(|call| call.id == id) else {
            return "unavailable";
        };
        let status = self.calls[index].status;
        if status != "pending" {
            self.calls.remove(index);
        }
        status
    }
    pub(crate) fn cancel(&mut self, id: Uuid) {
        if let Some(call) = self.calls.iter_mut().find(|call| call.id == id) {
            call.status = "cancelled";
        } else if self.room() {
            self.calls.push_back(Pending {
                id,
                review: None,
                status: "cancelled",
                expires: Instant::now() + LIFETIME,
            });
        }
    }
    fn start_batch(&mut self, now: f64) {
        let ids: Vec<_> = self
            .calls
            .iter()
            .filter(|call| call.status == "pending")
            .map(|call| call.id)
            .collect();
        if ids.is_empty() {
            self.batch = None;
            return;
        }
        self.serial = self.serial.wrapping_add(1);
        self.batch = Some(Batch {
            ids,
            serial: self.serial,
            ready_at: now + 0.25,
        });
    }

    fn active(&self, id: Uuid) -> bool {
        self.calls
            .iter()
            .any(|call| call.id == id && call.status == "pending")
    }
    fn rearm(&mut self, now: f64) {
        if let Some(batch) = &mut self.batch {
            batch.ready_at = now + 0.25;
        }
    }

    fn needs_unlock(&self, ids: &[Uuid]) -> bool {
        self.calls.iter().any(|call| {
            ids.contains(&call.id)
                && call.status == "pending"
                && call
                    .review
                    .as_ref()
                    .is_some_and(|review| review.unlock_vault)
        })
    }

    /// Allows `ids`, through the vault unlock first when one of them reads
    /// the vault.
    fn allow(&mut self, ids: &[Uuid], catalog: &Catalog) {
        if !self.needs_unlock(ids) {
            self.decide(ids, true, catalog);
        } else if !self.unlock.busy() && !self.master_password.is_empty() {
            self.unlock_error = None;
            self.unlock =
                Unlock::Requested(ids.to_vec(), std::mem::take(&mut self.master_password));
        }
    }

    /// The master password of an allowed decision, for the window to unlock
    /// the vault with; handed over once.
    pub(crate) fn take_unlock(&mut self) -> Option<Zeroizing<String>> {
        match std::mem::take(&mut self.unlock) {
            Unlock::Requested(ids, password) => {
                self.unlock = Unlock::Running(ids);
                Some(password)
            }
            other => {
                self.unlock = other;
                None
            }
        }
    }

    /// The window's unlock result: the calls are allowed once the vault is
    /// open (the key is stored for them by then); otherwise they stay
    /// pending with the reason shown.
    pub(crate) fn finish_unlock(&mut self, result: Result<(), String>, catalog: &Catalog) {
        let Unlock::Running(ids) = std::mem::take(&mut self.unlock) else {
            return;
        };
        if result.is_ok() && self.detail.is_some_and(|detail| ids.contains(&detail.id)) {
            self.detail = None;
        }
        match result {
            Ok(()) if ids.iter().any(|id| self.active(*id)) => self.decide(&ids, true, catalog),
            Ok(()) => {}
            Err(error) => self.unlock_error = Some(error),
        }
    }

    fn decide(&mut self, ids: &[Uuid], allow: bool, catalog: &Catalog) {
        self.expire();
        let mut decided = 0;
        for call in &mut self.calls {
            if ids.contains(&call.id) && call.status == "pending" {
                call.status = if allow { "approved" } else { "cancelled" };
                decided += 1;
            }
        }
        let mut message = catalog.format(
            "approval_decided",
            &[
                ("count", &decided.to_string()),
                (
                    "decision",
                    catalog.text(if allow {
                        "approval_allowed"
                    } else {
                        "approval_rejected"
                    }),
                ),
            ],
        );
        if decided < ids.len() {
            message.push_str(&catalog.format(
                "approval_skipped",
                &[("count", &(ids.len() - decided).to_string())],
            ));
        }
        self.feedback.set(message);
    }

    pub(crate) fn show(&mut self, context: &egui::Context, catalog: &Catalog) {
        let now = context.input(|input| input.time);
        self.expire();
        let expired = std::mem::take(&mut self.expired_notices);
        if expired > 0 {
            self.feedback.set(catalog.format(
                "approval_expired_feedback",
                &[("count", &expired.to_string())],
            ));
        }
        if self.detail.is_some_and(|detail| !self.active(detail.id)) {
            self.detail = None;
            self.rearm(now);
            self.feedback
                .set(catalog.text("approval_detail_invalid").to_owned());
        }
        if self.batch.is_none() {
            self.start_batch(now);
        }
        if !self.pending() {
            self.batch = None;
            self.detail = None;
            self.master_password = Zeroizing::default();
            self.unlock_error = None;
            self.feedback.show(context, "ssh_approval_feedback");
            return;
        }
        let batch = self.batch.as_ref().expect("pending calls have a batch");
        let ids = batch.ids.clone();
        let ready = now >= batch.ready_at;
        if !ready {
            context.request_repaint_after(Duration::from_millis(50));
        }
        let active: Vec<_> = self
            .calls
            .iter()
            .filter(|call| ids.contains(&call.id) && call.status == "pending")
            .collect();
        let active_ids: Vec<_> = active.iter().map(|call| call.id).collect();
        let unlock = active.iter().any(|call| {
            call.review
                .as_ref()
                .is_some_and(|review| review.unlock_vault)
        });
        let busy = self.unlock.busy();
        let next = self
            .calls
            .iter()
            .filter(|call| call.status == "pending" && !ids.contains(&call.id))
            .count();
        let bounds = context.content_rect().shrink(24.0);
        let mut decision = None;
        let mut open_detail = None;
        let mut next_batch = false;
        let id = egui::Id::new(("ssh_call_batch", batch.serial));
        let modal = egui::Modal::new(id)
            .area(
                egui::Modal::default_area(id)
                    .default_size(egui::vec2(680.0, bounds.height()))
                    .enabled(self.detail.is_none() && ready)
                    .fade_in(false),
            )
            .frame(crate::approval_ui::frame(context))
            .show(context, |ui| {
                crate::approval_ui::style(ui);
                ui.set_width((bounds.width() - 40.0).min(640.0));
                ui.heading(catalog.text("ssh_approval_title"));
                ui.add(
                    egui::Label::new(catalog.format(
                        "approval_batch_scope",
                        &[
                            ("count", &active.len().to_string()),
                            ("next", &next.to_string()),
                        ],
                    ))
                    .wrap(),
                );
                if !active.is_empty() {
                    let mut submit = false;
                    if unlock {
                        submit = unlock_row(
                            ui,
                            catalog,
                            &mut self.master_password,
                            self.unlock_error.as_deref(),
                            busy,
                        );
                    }
                    let can_allow = !unlock || (!busy && !self.master_password.is_empty());
                    let buttons = ui
                        .add_enabled_ui(ready, |ui| {
                            crate::approval_ui::actions(
                                ui,
                                catalog,
                                can_allow,
                                if unlock {
                                    "approval_unlock_allow_all"
                                } else {
                                    "approval_allow_all"
                                },
                                "approval_reject_all",
                            )
                        })
                        .inner;
                    if ready {
                        decision = buttons.decision;
                        if submit && decision.is_none() {
                            decision = Some(true);
                        }
                    }
                    #[cfg(test)]
                    for call in &active {
                        context.data_mut(|data| {
                            data.insert_temp(
                                egui::Id::new(("ssh_approval_buttons", call.id)),
                                (buttons.confirm, buttons.cancel),
                            )
                        });
                    }
                } else {
                    ui.strong(catalog.text("approval_batch_finished"));
                    let response = ui.button(catalog.text("approval_next_batch"));
                    if response.clicked() {
                        next_batch = true;
                    }
                    #[cfg(test)]
                    context.data_mut(|data| {
                        data.insert_temp(egui::Id::new("ssh_next_batch"), response.rect)
                    });
                }
                if let Some(text) = self.feedback.text() {
                    ui.add(egui::Label::new(text).wrap());
                }
                ui.separator();
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .id_salt(("ssh_batch_list", batch.serial))
                    .auto_shrink([false, true])
                    .max_height((bounds.height() - 230.0).clamp(70.0, 400.0))
                    .show(ui, |ui| {
                        for (index, call) in active.iter().enumerate() {
                            let review = call.review.as_ref().expect("pending review has details");
                            ui.group(|ui| {
                                ui.strong(catalog.format(
                                    "approval_call_number",
                                    &[("number", &(index + 1).to_string())],
                                ));
                                ui.add(egui::Label::new(review.summary(catalog)).wrap());
                                ui.label(crate::approval_ui::countdown(catalog, call.expires));
                                let targets = review
                                    .hosts
                                    .iter()
                                    .filter(|host| host.is_target)
                                    .map(|host| host.alias.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                ui.add(
                                    egui::Label::new(crate::approval_ui::preview(&targets)).wrap(),
                                );
                                if ui.button(catalog.text("approval_view_details")).clicked() {
                                    open_detail = Some(Detail {
                                        id: call.id,
                                        command: None,
                                        focus: true,
                                    });
                                }
                                for (index, command) in review.commands.iter().enumerate() {
                                    let response = ui.add(
                                        egui::Button::new(
                                            egui::RichText::new(format!(
                                                "{}  {}",
                                                index + 1,
                                                crate::approval_ui::preview(command)
                                            ))
                                            .monospace(),
                                        )
                                        .wrap(),
                                    );
                                    if response.clicked() {
                                        open_detail = Some(Detail {
                                            id: call.id,
                                            command: Some(index),
                                            focus: true,
                                        });
                                    }
                                    #[cfg(test)]
                                    context.data_mut(|data| {
                                        data.insert_temp(
                                            egui::Id::new(("ssh_command_preview", call.id, index)),
                                            response.rect,
                                        )
                                    });
                                }
                            });
                            ui.add_space(8.0);
                        }
                    });
            });
        if modal
            .response
            .rect
            .center()
            .distance(context.content_rect().center())
            > 1.0
        {
            context.request_discard("Center the SSH call approval after measuring it");
        }
        #[cfg(test)]
        for call in &active {
            context.data_mut(|data| {
                data.insert_temp(
                    egui::Id::new(("ssh_approval_bounds", call.id)),
                    modal.response.rect,
                )
            });
        }
        if self.detail.is_none()
            && modal.is_top_modal
            && !modal.any_popup_open
            && crate::approval_ui::escape(context)
            && ready
            && !active_ids.is_empty()
        {
            decision = Some(false);
        }
        match decision {
            Some(true) => self.allow(&active_ids, catalog),
            Some(false) => self.decide(&active_ids, false, catalog),
            None => {}
        }
        if decision.is_some() {
            self.rearm(now);
        }
        if next_batch {
            self.start_batch(now);
        }
        if let Some(detail) = open_detail {
            self.detail = Some(detail);
            self.rearm(now);
        }
        self.show_detail(context, catalog);
        context.request_repaint_after(Duration::from_secs(1));
    }

    fn show_detail(&mut self, context: &egui::Context, catalog: &Catalog) {
        let Some(detail) = self.detail else {
            return;
        };
        let Some(call) = self
            .calls
            .iter()
            .find(|call| call.id == detail.id && call.status == "pending")
        else {
            self.detail = None;
            return;
        };
        let review = call.review.as_ref().expect("pending review has details");
        let bounds = context.content_rect().shrink(24.0);
        let now = context.input(|input| input.time);
        let ready = self
            .batch
            .as_ref()
            .is_none_or(|batch| now >= batch.ready_at);
        if !ready {
            context.request_repaint_after(Duration::from_millis(50));
        }
        let mut decision = None;
        let mut back = false;
        let unlock = review.unlock_vault;
        let busy = self.unlock.busy();
        let id = egui::Id::new(("ssh_call_detail", detail.id));
        let modal = egui::Modal::new(id)
            .area(
                egui::Modal::default_area(id)
                    .default_size(bounds.size())
                    .enabled(ready)
                    .fade_in(false),
            )
            .frame(crate::approval_ui::frame(context))
            .show(context, |ui| {
                crate::approval_ui::style(ui);
                ui.set_width((bounds.width() - 40.0).min(760.0));
                ui.heading(catalog.text("approval_detail_title"));
                ui.add(egui::Label::new(catalog.text("approval_detail_scope")).wrap());
                ui.add(egui::Label::new(review.summary(catalog)).wrap());
                let mut submit = false;
                if unlock {
                    submit = unlock_row(
                        ui,
                        catalog,
                        &mut self.master_password,
                        self.unlock_error.as_deref(),
                        busy,
                    );
                }
                let buttons = crate::approval_ui::actions(
                    ui,
                    catalog,
                    !unlock || (!busy && !self.master_password.is_empty()),
                    if unlock {
                        "approval_unlock_allow_call"
                    } else {
                        "approval_allow_call"
                    },
                    "approval_reject_call",
                );
                decision = buttons.decision;
                if submit && decision.is_none() {
                    decision = Some(true);
                }
                #[cfg(test)]
                context.data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new(("ssh_detail_buttons", detail.id)),
                        (buttons.confirm, buttons.cancel),
                    )
                });
                let response = ui.small_button(catalog.text("approval_back"));
                if response.clicked() {
                    back = true;
                }
                #[cfg(test)]
                context.data_mut(|data| {
                    data.insert_temp(egui::Id::new(("ssh_detail_back", detail.id)), response.rect)
                });
                ui.label(crate::approval_ui::countdown(catalog, call.expires));
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt(("ssh_full_details", detail.id))
                    .auto_shrink([false, true])
                    .max_height((bounds.height() - 235.0).max(40.0))
                    .show(ui, |ui| {
                        for (role, key) in [
                            (true, "approval_execution_hosts"),
                            (false, "approval_jump_hosts"),
                        ] {
                            ui.strong(catalog.text(key));
                            for host in review
                                .hosts
                                .iter()
                                .filter(|host| if role { host.is_target } else { host.is_jump })
                            {
                                ui.add(
                                    egui::Label::new(format!(
                                        "{} · {}@{}:{} · {}",
                                        host.alias,
                                        host.username,
                                        host.address,
                                        host.port,
                                        host.protocol.stable_name()
                                    ))
                                    .selectable(true)
                                    .wrap(),
                                );
                            }
                        }
                        if let Some(user) = &review.sudo_user {
                            ui.label(format!("sudo → {user}"));
                        }
                        ui.strong(catalog.text("ssh_approval_commands"));
                        for (index, command) in review.commands.iter().enumerate() {
                            let heading = ui.strong(catalog.format(
                                "approval_command_number",
                                &[("number", &(index + 1).to_string())],
                            ));
                            if detail.focus && detail.command == Some(index) {
                                heading.scroll_to_me(Some(egui::Align::TOP));
                            }
                            crate::approval_ui::full_text(
                                ui,
                                catalog,
                                egui::Id::new(("ssh_command_text", detail.id, index)),
                                command,
                            );
                            ui.separator();
                        }
                        if let Some(stdin) = &review.stdin {
                            ui.strong(catalog.text("ssh_approval_stdin"));
                            crate::approval_ui::full_text(
                                ui,
                                catalog,
                                egui::Id::new(("ssh_stdin_text", detail.id)),
                                stdin,
                            );
                        }
                    });
            });
        if modal
            .response
            .rect
            .center()
            .distance(context.content_rect().center())
            > 1.0
        {
            context.request_discard("Center complete SSH call details");
        }
        #[cfg(test)]
        context.data_mut(|data| {
            data.insert_temp(
                egui::Id::new(("ssh_detail_bounds", detail.id)),
                modal.response.rect,
            )
        });
        if modal.is_top_modal && !modal.any_popup_open && crate::approval_ui::escape(context) {
            back = true;
        }
        if let Some(allow) = decision {
            if allow {
                self.allow(&[detail.id], catalog);
            } else {
                self.decide(&[detail.id], false, catalog);
            }
            if !self.unlock.busy() {
                self.detail = None;
            }
            self.rearm(now);
        } else if back {
            self.detail = None;
            self.rearm(now);
        } else if let Some(detail) = &mut self.detail {
            detail.focus = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn review() -> Review {
        Review {
            hosts: vec![ReviewHost {
                alias: "fixture".into(),
                address: "127.0.0.1".into(),
                port: 22,
                username: "tester".into(),
                requires_approval: true,
                protocol: Protocol::Ssh,
                is_target: true,
                is_jump: false,
            }],
            commands: vec!["printf test".into()],
            stdin: None,
            sudo_user: None,
            unlock_vault: false,
            uses_vault: false,
        }
    }
    #[test]
    fn decisions_are_single_use_cancelled_creates_stay_cancelled_and_requests_expire() {
        let mut queue = Queue::default();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        assert_eq!(queue.submit(first, review()), "pending");
        assert_eq!(queue.submit(second, review()), "pending");
        queue.calls[0].status = "approved";
        assert_eq!(queue.take(first), "approved");
        assert_eq!(queue.take(first), "unavailable");
        assert_eq!(queue.take(second), "pending");
        queue.calls[0].expires = Instant::now();
        assert_eq!(queue.take(second), "expired");
        let cancelled = Uuid::new_v4();
        queue.cancel(cancelled);
        assert_eq!(queue.submit(cancelled, review()), "unavailable");
        assert!(!queue.pending());
    }
    #[test]
    fn jump_hosts_can_require_approval_and_telnet_cannot() {
        let mut jump = HostProfile::new("jump".into());
        let mut target = HostProfile::new("target".into());
        target.jump_host = Some(jump.id);
        let hosts = vec![jump.clone(), target.clone()];
        assert!(
            !Review::new(&[target.clone()], &hosts, vec![], None, None)
                .unwrap()
                .required()
        );
        jump.advanced.require_call_approval = true;
        let hosts = vec![jump.clone(), target.clone()];
        let review = Review::new(&[target.clone(), target], &hosts, vec![], None, None).unwrap();
        assert!(review.required());
        assert_eq!(review.hosts.len(), 2);
        jump.protocol = Protocol::Telnet;
        assert!(
            !Review::new(&[jump], &[], vec![], None, None)
                .unwrap()
                .required()
        );
    }

    #[test]
    fn a_vault_host_anywhere_in_the_chain_marks_the_review_and_only_the_ask_crosses_the_pipe() {
        let mut jump = HostProfile::new("jump".into());
        let mut target = HostProfile::new("target".into());
        target.jump_host = Some(jump.id);
        target.advanced.require_call_approval = true;
        let plain = Review::new(
            std::slice::from_ref(&target),
            &[jump.clone(), target.clone()],
            vec![],
            None,
            None,
        )
        .unwrap();
        assert!(!plain.uses_vault);
        jump.vault = Some(crate::model::VaultHostRef {
            item_id: Uuid::new_v4(),
            item_name: "jump".into(),
            revision_date: String::new(),
        });
        let hosts = [jump, target.clone()];
        let mut review =
            Review::new(std::slice::from_ref(&target), &hosts, vec![], None, None).unwrap();
        assert!(review.uses_vault && !review.unlock_vault);
        review.unlock_vault = true;
        let sent: Review = serde_json::from_str(&serde_json::to_string(&review).unwrap()).unwrap();
        assert!(sent.unlock_vault);
        assert!(!sent.uses_vault, "the window never needs the host scan");
    }

    #[test]
    fn a_locked_vault_is_unlocked_and_allowed_in_the_same_dialog() {
        for detail_view in [false, true] {
            let mut queue = Queue::default();
            let id = Uuid::new_v4();
            let mut asks = review();
            asks.unlock_vault = true;
            queue.submit(id, asks);
            let context = egui::Context::default();
            crate::ui::configure_fonts(&context, "en");
            let catalog = Catalog::for_locale(Some("en"));
            warm(&mut queue, &context, &catalog);
            if detail_view {
                queue.detail = Some(Detail {
                    id,
                    command: None,
                    focus: false,
                });
                queue.rearm(context.input(|input| input.time));
                warm(&mut queue, &context, &catalog);
            }
            let key = if detail_view {
                egui::Id::new(("ssh_detail_buttons", id))
            } else {
                egui::Id::new(("ssh_approval_buttons", id))
            };
            let confirm = || {
                context
                    .data(|data| data.get_temp::<(egui::Rect, egui::Rect)>(key))
                    .unwrap()
                    .0
                    .center()
            };
            click(&mut queue, &context, confirm());
            assert_eq!(queue.take(id), "pending", "no password, nothing to allow");
            assert!(queue.take_unlock().is_none());

            queue.master_password = Zeroizing::new("wrong".into());
            warm(&mut queue, &context, &catalog);
            click(&mut queue, &context, confirm());
            assert_eq!(
                queue.take(id),
                "pending",
                "allowed only once the vault opens"
            );
            assert_eq!(
                queue.take_unlock().as_deref().map(String::as_str),
                Some("wrong")
            );
            assert!(queue.take_unlock().is_none(), "handed over once");
            assert!(queue.master_password.is_empty(), "the field keeps no copy");
            queue.finish_unlock(Err("Invalid master password.".into()), &catalog);
            assert_eq!(
                queue.take(id),
                "pending",
                "a failed unlock leaves the call open"
            );
            assert_eq!(
                queue.unlock_error.as_deref(),
                Some("Invalid master password.")
            );

            queue.master_password = Zeroizing::new("right".into());
            warm(&mut queue, &context, &catalog);
            click(&mut queue, &context, confirm());
            assert_eq!(
                queue.take_unlock().as_deref().map(String::as_str),
                Some("right")
            );
            queue.finish_unlock(Ok(()), &catalog);
            assert_eq!(queue.take(id), "approved");
            assert!(queue.detail.is_none());
            warm(&mut queue, &context, &catalog);
            assert!(queue.unlock_error.is_none());
        }
    }

    #[test]
    fn rejecting_while_the_vault_unlocks_never_allows_the_call() {
        let mut queue = Queue::default();
        let id = Uuid::new_v4();
        let mut asks = review();
        asks.unlock_vault = true;
        queue.submit(id, asks);
        let catalog = Catalog::for_locale(Some("en"));
        queue.master_password = Zeroizing::new("secret".into());
        queue.allow(&[id], &catalog);
        assert!(queue.take_unlock().is_some());
        queue.decide(&[id], false, &catalog);
        queue.finish_unlock(Ok(()), &catalog);
        assert_eq!(queue.take(id), "cancelled");
    }

    #[test]
    fn mixed_review_distinguishes_execution_targets_from_shared_jump_hosts() {
        let mut jump = HostProfile::new("shared-jump".into());
        jump.advanced.require_call_approval = true;
        let mut first = HostProfile::new("first".into());
        let mut second = HostProfile::new("second".into());
        first.jump_host = Some(jump.id);
        second.jump_host = Some(jump.id);
        let mut telnet = HostProfile::new("telnet-target".into());
        telnet.protocol = Protocol::Telnet;
        telnet.advanced.require_call_approval = true;
        let hosts = vec![jump, first, second, telnet];
        let review =
            Review::new(&hosts, &hosts, vec!["one".into(), "two".into()], None, None).unwrap();
        assert!(review.required());
        assert_eq!(review.hosts.len(), 4);
        assert_eq!(review.hosts.iter().filter(|h| h.is_target).count(), 4);
        assert_eq!(review.hosts.iter().filter(|h| h.is_jump).count(), 1);
        let shared = review
            .hosts
            .iter()
            .find(|h| h.alias == "shared-jump")
            .unwrap();
        assert!(shared.is_target && shared.is_jump);
        let telnet = review
            .hosts
            .iter()
            .find(|h| h.protocol == Protocol::Telnet)
            .unwrap();
        assert!(telnet.is_target && !telnet.is_jump && !telnet.requires_approval);
    }

    fn warm(queue: &mut Queue, context: &egui::Context, catalog: &Catalog) {
        for _ in 0..4 {
            frame(queue, context, catalog, egui::vec2(820.0, 620.0), vec![]);
        }
    }

    #[test]
    fn bulk_decisions_freeze_the_displayed_batch_and_require_opening_the_next_batch() {
        for allow in [true, false] {
            let mut queue = Queue::default();
            let first = Uuid::new_v4();
            let second = Uuid::new_v4();
            let later = Uuid::new_v4();
            queue.submit(first, review());
            queue.submit(second, review());
            let context = egui::Context::default();
            crate::ui::configure_fonts(&context, "en");
            let catalog = Catalog::for_locale(Some("en"));
            warm(&mut queue, &context, &catalog);
            queue.submit(later, review());
            let (confirm, reject) = context
                .data(|data| {
                    data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new((
                        "ssh_approval_buttons",
                        first,
                    )))
                })
                .unwrap();
            let at = if allow {
                confirm.center()
            } else {
                reject.center()
            };
            click(&mut queue, &context, at);
            let status = if allow { "approved" } else { "cancelled" };
            assert_eq!(queue.take(first), status);
            assert_eq!(queue.take(second), status);
            assert_eq!(queue.take(later), "pending");
            // Repeating the old bulk click cannot act on newly arrived calls.
            click(&mut queue, &context, at);
            assert_eq!(queue.take(later), "pending");
            warm(&mut queue, &context, &catalog);
            let next = context
                .data(|data| data.get_temp::<egui::Rect>(egui::Id::new("ssh_next_batch")))
                .unwrap();
            click(&mut queue, &context, next.center());
            frame(
                &mut queue,
                &context,
                &catalog,
                egui::vec2(820.0, 620.0),
                vec![],
            );
            let (confirm, _) = context
                .data(|data| {
                    data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new((
                        "ssh_approval_buttons",
                        later,
                    )))
                })
                .unwrap();
            click(&mut queue, &context, confirm.center());
            assert_eq!(
                queue.take(later),
                "pending",
                "a click begun during the transition must not authorize the next batch"
            );
            warm(&mut queue, &context, &catalog);
            click(&mut queue, &context, confirm.center());
            assert_eq!(queue.take(later), "approved");
            assert_eq!(queue.take(later), "unavailable");
        }
    }

    #[test]
    fn detail_navigation_and_expiry_never_authorize_another_call() {
        let mut queue = Queue::default();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        queue.submit(first, review());
        queue.submit(second, review());
        let context = egui::Context::default();
        crate::ui::configure_fonts(&context, "en");
        let catalog = Catalog::for_locale(Some("en"));
        for escape in [false, true] {
            warm(&mut queue, &context, &catalog);
            let preview = context
                .data(|data| {
                    data.get_temp::<egui::Rect>(egui::Id::new((
                        "ssh_command_preview",
                        first,
                        0_usize,
                    )))
                })
                .unwrap();
            click(&mut queue, &context, preview.center());
            warm(&mut queue, &context, &catalog);
            assert_eq!(queue.detail.unwrap().id, first);
            if escape {
                frame(
                    &mut queue,
                    &context,
                    &catalog,
                    egui::vec2(820.0, 620.0),
                    vec![egui::Event::Key {
                        key: egui::Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                );
            } else {
                let back = context
                    .data(|data| {
                        data.get_temp::<egui::Rect>(egui::Id::new(("ssh_detail_back", first)))
                    })
                    .unwrap();
                click(&mut queue, &context, back.center());
            }
            assert!(queue.detail.is_none());
            assert_eq!(queue.take(first), "pending");
            assert_eq!(queue.take(second), "pending");
        }
        warm(&mut queue, &context, &catalog);
        let preview = context
            .data(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new(("ssh_command_preview", first, 0_usize)))
            })
            .unwrap();
        click(&mut queue, &context, preview.center());
        warm(&mut queue, &context, &catalog);
        let (confirm, _) = context
            .data(|data| {
                data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new((
                    "ssh_detail_buttons",
                    first,
                )))
            })
            .unwrap();
        queue.calls[0].expires = Instant::now();
        click(&mut queue, &context, confirm.center());
        assert!(queue.detail.is_none());
        assert_eq!(queue.take(first), "expired");
        assert_eq!(queue.take(second), "pending");
        assert!(
            queue
                .feedback
                .text()
                .unwrap()
                .contains("without granting authorization")
        );
    }

    #[test]
    fn full_call_details_fit_small_windows_in_every_locale() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            let mut queue = Queue::default();
            let id = Uuid::new_v4();
            let mut review = review();
            review.hosts[0].alias = "Long execution host name ".repeat(16);
            review.commands = vec!["完整コマンド α\n".repeat(1800), "second command".into()];
            review.stdin = Some("input\n".repeat(3000));
            review.sudo_user = Some("root".into());
            queue.submit(id, review);
            let context = egui::Context::default();
            crate::ui::configure_fonts(&context, locale);
            let catalog = Catalog::for_locale(Some(locale));
            warm(&mut queue, &context, &catalog);
            queue.detail = Some(Detail {
                id,
                command: Some(0),
                focus: true,
            });
            let size = egui::vec2(774.0, 584.0);
            for pass in 0..4 {
                let (output, _) = frame(&mut queue, &context, &catalog, size, vec![]);
                if pass == 0 {
                    continue;
                }
                let bounds = context
                    .data(|data| {
                        data.get_temp::<egui::Rect>(egui::Id::new(("ssh_detail_bounds", id)))
                    })
                    .unwrap();
                let (confirm, cancel) = context
                    .data(|data| {
                        data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new((
                            "ssh_detail_buttons",
                            id,
                        )))
                    })
                    .unwrap();
                assert!(
                    egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains_rect(bounds),
                    "{locale} {bounds:?}"
                );
                assert!(bounds.contains_rect(confirm) && bounds.contains_rect(cancel));
                assert!(
                    confirm.bottom() < bounds.top() + bounds.height() / 2.0,
                    "actions remain at the top"
                );
                crate::approval_ui::assert_painted_palette(&output, confirm, cancel);
                assert_eq!(queue.take(id), "pending");
            }
        }
    }

    #[test]
    fn escape_during_stale_detail_or_transition_does_not_reject_other_calls() {
        let escape = |repeat| {
            let key = |pressed| egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed,
                repeat,
                modifiers: egui::Modifiers::NONE,
            };
            // A fresh key press includes releasing the previous physical key.
            if repeat {
                vec![key(true)]
            } else {
                vec![key(false), key(true)]
            }
        };
        for cancelled in [false, true] {
            let mut queue = Queue::default();
            let first = Uuid::new_v4();
            let second = Uuid::new_v4();
            queue.submit(first, review());
            queue.submit(second, review());
            let context = egui::Context::default();
            crate::ui::configure_fonts(&context, "en");
            let catalog = Catalog::for_locale(Some("en"));
            warm(&mut queue, &context, &catalog);
            queue.detail = Some(Detail {
                id: first,
                command: None,
                focus: false,
            });
            warm(&mut queue, &context, &catalog);
            if cancelled {
                queue.cancel(first);
            } else {
                queue.calls[0].expires = Instant::now();
            }
            frame(
                &mut queue,
                &context,
                &catalog,
                egui::vec2(820.0, 620.0),
                escape(false),
            );
            assert!(queue.detail.is_none());
            assert_eq!(queue.take(second), "pending");
            warm(&mut queue, &context, &catalog);
            frame(
                &mut queue,
                &context,
                &catalog,
                egui::vec2(820.0, 620.0),
                escape(true),
            );
            assert_eq!(
                queue.take(second),
                "pending",
                "holding Escape must not reject the remaining batch"
            );
            queue.detail = Some(Detail {
                id: second,
                command: None,
                focus: false,
            });
            warm(&mut queue, &context, &catalog);
            frame(
                &mut queue,
                &context,
                &catalog,
                egui::vec2(820.0, 620.0),
                escape(false),
            );
            assert!(queue.detail.is_none());
            frame(
                &mut queue,
                &context,
                &catalog,
                egui::vec2(820.0, 620.0),
                escape(false),
            );
            assert_eq!(queue.take(second), "pending");
        }
    }

    fn frame(
        queue: &mut Queue,
        context: &egui::Context,
        catalog: &Catalog,
        size: egui::Vec2,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, bool) {
        let mut clicked = false;
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                time: Some(context.input(|i| i.time) + 0.1),
                events,
                ..Default::default()
            },
            |ui| {
                let button = ui.button("Background action");
                clicked = button.clicked();
                context.data_mut(|data| {
                    data.insert_temp(egui::Id::new("ssh_test_background"), button.rect);
                });
                queue.show(ui.ctx(), catalog);
            },
        );
        (output, clicked)
    }

    fn click(queue: &mut Queue, context: &egui::Context, at: egui::Pos2) -> bool {
        let mut clicked = false;
        for pressed in [true, false] {
            clicked |= frame(
                queue,
                context,
                &Catalog::for_locale(Some("en")),
                egui::vec2(820.0, 620.0),
                vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            )
            .1;
        }
        clicked
    }

    #[test]
    fn ssh_review_fits_small_windows_and_paints_the_shared_palette() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            for size in [egui::vec2(774.0, 584.0), egui::vec2(1040.0, 760.0)] {
                let mut queue = Queue::default();
                let id = Uuid::new_v4();
                let mut call = review();
                call.commands = vec!["long-command ".repeat(300); 64];
                call.stdin = Some("input\n".repeat(500));
                queue.submit(id, call);
                let context = egui::Context::default();
                crate::ui::configure_fonts(&context, locale);
                let catalog = Catalog::for_locale(Some(locale));
                for pass in 0..4 {
                    let (output, _) = frame(&mut queue, &context, &catalog, size, vec![]);
                    if pass == 0 {
                        continue;
                    }
                    let (confirm, cancel) = context
                        .data(|data| {
                            data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new((
                                "ssh_approval_buttons",
                                id,
                            )))
                        })
                        .unwrap();
                    let bounds = context
                        .data(|data| {
                            data.get_temp::<egui::Rect>(egui::Id::new(("ssh_approval_bounds", id)))
                        })
                        .unwrap();
                    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
                    assert!(
                        screen.contains_rect(bounds),
                        "{locale} pass={pass}: {bounds:?}"
                    );
                    assert!(bounds.contains_rect(confirm) && bounds.contains_rect(cancel));
                    assert!(
                        bounds.center().distance(screen.center()) < 2.0,
                        "{locale} pass={pass} size={size:?}: {bounds:?}"
                    );
                    if pass == 3 {
                        crate::approval_ui::assert_painted_palette(&output, confirm, cancel);
                    }
                    assert_eq!(queue.take(id), "pending");
                }
            }
        }
    }

    #[test]
    fn ssh_review_blocks_background_and_grants_only_the_displayed_call() {
        let mut queue = Queue::default();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        queue.submit(first, review());
        queue.submit(second, review());
        let context = egui::Context::default();
        crate::ui::configure_fonts(&context, "en");
        let catalog = Catalog::for_locale(Some("en"));
        for _ in 0..4 {
            frame(
                &mut queue,
                &context,
                &catalog,
                egui::vec2(820.0, 620.0),
                vec![],
            );
        }
        let background = context
            .data(|data| data.get_temp::<egui::Rect>(egui::Id::new("ssh_test_background")))
            .unwrap();
        assert!(!click(&mut queue, &context, background.center()));
        assert_eq!(queue.take(first), "pending");
        let preview = context
            .data(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new(("ssh_command_preview", first, 0_usize)))
            })
            .unwrap();
        click(&mut queue, &context, preview.center());
        assert_eq!(queue.detail.unwrap().id, first);
        for _ in 0..4 {
            frame(
                &mut queue,
                &context,
                &catalog,
                egui::vec2(820.0, 620.0),
                vec![],
            );
        }
        let (confirm, _) = context
            .data(|data| {
                data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new((
                    "ssh_detail_buttons",
                    first,
                )))
            })
            .unwrap();
        click(&mut queue, &context, confirm.center());
        assert_eq!(queue.take(first), "approved");
        assert_eq!(queue.take(first), "unavailable");
        assert_eq!(queue.take(second), "pending");
        for _ in 0..4 {
            frame(
                &mut queue,
                &context,
                &catalog,
                egui::vec2(820.0, 620.0),
                vec![],
            );
        }
        let (_, cancel) = context
            .data(|data| {
                data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new((
                    "ssh_approval_buttons",
                    second,
                )))
            })
            .unwrap();
        click(&mut queue, &context, cancel.center());
        assert_eq!(queue.take(second), "cancelled");
        assert!(!queue.pending());
    }
}
