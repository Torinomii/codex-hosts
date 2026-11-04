use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::i18n::Catalog;

#[cfg(windows)]
mod ipc;

const MAX_FIELDS: usize = 64;
const MAX_VALUE_BYTES: usize = 32 * 1024;
const MAX_OPERATIONS: usize = 16;
const APPROVAL_LIFETIME: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    #[serde(rename = "request")]
    Declare {
        fields: Vec<String>,
    },
    Status,
    Show {
        temporary: bool,
    },
    Clear {
        fields: Vec<String>,
    },
    Execute(Execution),
    ReviewSsh {
        id: Uuid,
        review: crate::approval::Review,
    },
    TakeSshApproval {
        id: Uuid,
    },
    CancelSshApproval {
        id: Uuid,
    },
    UnlockBitwarden {
        id: Uuid,
    },
    TakeBitwardenUnlock {
        id: Uuid,
    },
    CancelBitwardenUnlock {
        id: Uuid,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    program: PathBuf,
    #[serde(default)]
    args: Vec<String>,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
}

fn default_timeout() -> u64 {
    60_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldStatus {
    name: String,
    ready: bool,
    #[serde(default = "default_approval_required")]
    requires_approval: bool,
}

fn default_approval_required() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationStatus {
    id: Uuid,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    schema_version: u32,
    pub status: String,
    pub(crate) session: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    fields: Vec<FieldStatus>,
    operations: Vec<OperationStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) operation_id: Option<Uuid>,
    /// Set by the launcher when the window it started shares the caller's
    /// lifetime; older holders never send it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
}

impl Response {
    fn error(session: Uuid, code: &'static str) -> Self {
        Self {
            schema_version: 1,
            status: "error".into(),
            session,
            code: Some(code.into()),
            fields: vec![],
            operations: vec![],
            operation_id: None,
            warning: None,
        }
    }
}

const HOLDER_SHARES_JOB_WARNING: &str = "The secret window was started inside the caller's job object and will close when that process (usually Codex) exits. Start the codex-hosts GUI by hand before opening secrets to keep the window independent.";

// Deliberately no Serialize or Debug on secret-bearing types.
struct Field {
    name: String,
    draft: Zeroizing<String>,
    value: Option<Zeroizing<String>>,
    requires_approval: bool,
}

impl Field {
    fn clear(&mut self) {
        self.draft = Zeroizing::new(String::new());
        self.value = None;
    }
    fn save(&mut self) -> Result<(), &'static str> {
        if self.draft.is_empty() || self.draft.len() > MAX_VALUE_BYTES || self.draft.contains('\0')
        {
            return Err("SECRET_VALUE_INVALID");
        }
        self.value = Some(std::mem::replace(
            &mut self.draft,
            Zeroizing::new(String::new()),
        ));
        Ok(())
    }
}

#[derive(Default)]
struct Vault {
    fields: Vec<Field>,
}

fn validate_names(names: &[String], allow_empty: bool) -> Result<(), &'static str> {
    let mut unique = HashSet::new();
    if (!allow_empty && names.is_empty()) || names.len() > MAX_FIELDS {
        return Err("FIELD_NAMES_INVALID");
    }
    for name in names {
        if name.trim().is_empty()
            || name != name.trim()
            || name.len() > 256
            || name.chars().any(char::is_control)
            || !unique.insert(name)
        {
            return Err("FIELD_NAMES_INVALID");
        }
    }
    Ok(())
}

impl Vault {
    fn request(&mut self, names: &[String]) -> Result<(), &'static str> {
        validate_names(names, false)?;
        let added = names
            .iter()
            .filter(|name| !self.fields.iter().any(|f| &f.name == *name))
            .count();
        if self.fields.len() + added > MAX_FIELDS {
            return Err("FIELD_LIMIT");
        }
        for name in names {
            if !self.fields.iter().any(|f| &f.name == name) {
                self.fields.push(Field {
                    name: name.clone(),
                    draft: Zeroizing::new(String::new()),
                    value: None,
                    requires_approval: true,
                });
            }
        }
        Ok(())
    }
    fn clear(&mut self, names: &[String]) -> Result<(), &'static str> {
        validate_names(names, true)?;
        if names
            .iter()
            .any(|n| !self.fields.iter().any(|f| &f.name == n))
        {
            return Err("FIELD_NOT_FOUND");
        }
        for field in &mut self.fields {
            if names.is_empty() || names.contains(&field.name) {
                field.clear();
            }
        }
        Ok(())
    }
    fn status(&self) -> Vec<FieldStatus> {
        self.fields
            .iter()
            .map(|f| FieldStatus {
                name: f.name.clone(),
                ready: f.value.is_some(),
                requires_approval: f.requires_approval,
            })
            .collect()
    }
    fn environment(
        &self,
        execution: &Execution,
    ) -> Result<Vec<(String, Zeroizing<String>)>, &'static str> {
        execution.validate()?;
        execution
            .env
            .iter()
            .map(|(env, name)| {
                let value = self
                    .fields
                    .iter()
                    .find(|f| &f.name == name)
                    .and_then(|f| f.value.as_ref())
                    .ok_or("SECRET_MISSING")?;
                Ok((env.clone(), value.clone()))
            })
            .collect()
    }
    fn requires_approval(&self, execution: &Execution) -> bool {
        execution.env.values().any(|name| {
            self.fields
                .iter()
                .find(|field| &field.name == name)
                .is_none_or(|field| field.requires_approval)
        })
    }
}

impl Execution {
    fn validate(&self) -> Result<(), &'static str> {
        if !self.program.is_absolute()
            || !self.cwd.is_absolute()
            || self.args.len() > 128
            || self.args.iter().any(|a| a.len() > 8192 || a.contains('\0'))
            || self.args.iter().map(String::len).sum::<usize>() > 32 * 1024
            || !(1..=600_000).contains(&self.timeout_ms)
            || self.env.is_empty()
            || self.env.len() > MAX_FIELDS
        {
            return Err("EXECUTION_INVALID");
        }
        let mut env_names = HashSet::new();
        for name in self.env.keys() {
            if name.is_empty()
                || name.len() > 256
                || !name.bytes().enumerate().all(|(i, b)| {
                    b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
                })
                || !env_names.insert(name.to_ascii_uppercase())
            {
                return Err("ENVIRONMENT_INVALID");
            }
        }
        validate_names(
            &self
                .env
                .values()
                .cloned()
                .collect::<HashSet<_>>()
                .into_iter()
                .collect::<Vec<_>>(),
            false,
        )
    }
}

struct Operation {
    result: OperationStatus,
    execution: Execution,
    expires: Instant,
}

pub(super) struct Envelope {
    request: Request,
    reply: tokio::sync::oneshot::Sender<Response>,
}

struct Running {
    id: Uuid,
    cancel: Arc<AtomicBool>,
    result: mpsc::Receiver<OperationStatus>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_child(
    id: Uuid,
    execution: Execution,
    environment: Vec<(String, Zeroizing<String>)>,
    cancel: &AtomicBool,
) -> OperationStatus {
    let outcome = |status: &str, exit_code| OperationStatus {
        id,
        status: status.into(),
        exit_code,
    };
    if cancel.load(Ordering::Relaxed) {
        return outcome("cancelled", None);
    }
    let child = {
        let mut command = Command::new(&execution.program);
        command
            .args(&execution.args)
            .current_dir(&execution.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW, no startup registration.
        }
        for (name, value) in &environment {
            command.env(name, value.as_str());
        }
        command.spawn()
    };
    drop(environment);
    let Ok(mut child) = child else {
        return outcome("spawn_failed", None);
    };
    let start = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed)
            || start.elapsed() >= Duration::from_millis(execution.timeout_ms)
        {
            let status = if cancel.load(Ordering::Relaxed) {
                "cancelled"
            } else {
                "timed_out"
            };
            let _ = child.kill();
            let _ = child.wait();
            return outcome(status, None);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return outcome(
                    if status.success() {
                        "completed"
                    } else {
                        "failed"
                    },
                    status.code(),
                );
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return outcome("wait_failed", None);
            }
        }
    }
}

pub(crate) struct SecretsApp {
    session: Uuid,
    catalog: Catalog,
    vault: Vault,
    inbox: mpsc::Receiver<Envelope>,
    #[cfg(windows)]
    _server: ipc::Server,
    #[cfg(windows)]
    _discovery: Option<ipc::Server>,
    pub(crate) visible: bool,
    restore_requested: bool,
    operations: VecDeque<Operation>,
    running: Option<Running>,
    new_name: String,
    delete_all_prompt: Option<Vec<String>>,
    notice: Option<&'static str>,
    input_status: &'static str,
    context: egui::Context,
    ssh_approvals: crate::approval::Queue,
    bitwarden_unlocks: crate::bitwarden::prompt::UnlockQueue,
    detail: Option<Uuid>,
    displayed_approval: Option<Uuid>,
    approval_ready_at: f64,
    feedback: crate::approval_ui::Feedback,
}

impl SecretsApp {
    fn response(&self) -> Response {
        Response {
            schema_version: 1,
            status: self.input_status.into(),
            session: self.session,
            code: None,
            fields: self.vault.status(),
            operations: self.operations.iter().map(|o| o.result.clone()).collect(),
            operation_id: None,
            warning: None,
        }
    }
    fn handle(&mut self, request: Request) -> Response {
        let request = match request {
            Request::ReviewSsh { id, review } => {
                let status = self.ssh_approvals.submit(id, review);
                self.restore_requested |= status == "pending";
                return self.approval_response(id, status);
            }
            Request::TakeSshApproval { id } => {
                let status = self.ssh_approvals.take(id);
                return self.approval_response(id, status);
            }
            Request::CancelSshApproval { id } => {
                self.ssh_approvals.cancel(id);
                return self.approval_response(id, "cancelled");
            }
            Request::UnlockBitwarden { id } => {
                let status = self.bitwarden_unlocks.submit(id);
                self.restore_requested |= status == "pending";
                return self.approval_response(id, status);
            }
            Request::TakeBitwardenUnlock { id } => {
                let status = self.bitwarden_unlocks.take(id);
                return self.approval_response(id, status);
            }
            Request::CancelBitwardenUnlock { id } => {
                self.bitwarden_unlocks.cancel(id);
                return self.approval_response(id, "cancelled");
            }
            request => request,
        };
        if !matches!(&request, Request::Status | Request::Show { .. }) {
            self.input_status = "ok";
        }
        let mut operation_id = None;
        let result = match request {
            Request::Declare { fields } => {
                let result = self.vault.request(&fields);
                if result.is_ok() {
                    self.visible = true;
                    self.restore_requested = true;
                }
                result
            }
            Request::Show { temporary } => {
                if temporary {
                    self.visible = true;
                }
                self.restore_requested = true;
                Ok(())
            }
            Request::Status => Ok(()),
            Request::Clear { fields } => {
                // Clear also revokes pending approvals; running child already owns its environment.
                let result = self.vault.clear(&fields);
                if result.is_ok() {
                    self.cancel_pending();
                }
                result
            }
            Request::Execute(execution) => match self.vault.environment(&execution) {
                Err(code) => Err(code),
                Ok(_) => {
                    if self.operations.len() >= MAX_OPERATIONS {
                        if let Some(index) = self.operations.iter().position(|o| {
                            !matches!(o.result.status.as_str(), "pending" | "queued" | "running")
                        }) {
                            self.operations.remove(index);
                        } else {
                            return Response::error(self.session, "OPERATION_LIMIT");
                        }
                    }
                    let id = Uuid::new_v4();
                    let requires_approval = self.vault.requires_approval(&execution);
                    if requires_approval {
                        self.visible = true;
                        self.restore_requested = true;
                    }
                    self.operations.push_back(Operation {
                        result: OperationStatus {
                            id,
                            status: if requires_approval {
                                "pending"
                            } else {
                                "queued"
                            }
                            .into(),
                            exit_code: None,
                        },
                        execution,
                        expires: Instant::now() + APPROVAL_LIFETIME,
                    });
                    operation_id = Some(id);
                    self.start_queued();
                    Ok(())
                }
            },
            Request::ReviewSsh { .. }
            | Request::TakeSshApproval { .. }
            | Request::CancelSshApproval { .. }
            | Request::UnlockBitwarden { .. }
            | Request::TakeBitwardenUnlock { .. }
            | Request::CancelBitwardenUnlock { .. } => unreachable!(),
        };
        if let Err(code) = result {
            return Response::error(self.session, code);
        }
        let mut response = self.response();
        response.operation_id = operation_id;
        response
    }
    /// The Bitwarden unlock prompt a tool call is waiting on, if any.
    pub(crate) fn bitwarden_unlock_pending(&self) -> Option<Uuid> {
        self.bitwarden_unlocks.pending()
    }

    /// The master password an SSH call approval was allowed with, for the
    /// window to unlock the vault; handed over once.
    pub(crate) fn take_approval_unlock(&mut self) -> Option<Zeroizing<String>> {
        self.ssh_approvals.take_unlock()
    }

    /// The window's unlock result for [`Self::take_approval_unlock`]: the
    /// calls are allowed on success, else stay pending with the reason.
    pub(crate) fn finish_approval_unlock(&mut self, result: Result<(), String>) {
        let catalog = self.catalog.clone();
        self.ssh_approvals.finish_unlock(result, &catalog);
    }

    /// The window's answer to an unlock prompt: `unlocked` or `cancelled`.
    pub(crate) fn finish_bitwarden_unlock(&mut self, id: Uuid, status: &'static str) {
        self.bitwarden_unlocks.finish(id, status);
    }

    fn approval_response(&self, id: Uuid, status: &str) -> Response {
        Response {
            schema_version: 1,
            status: status.into(),
            session: self.session,
            code: None,
            fields: vec![],
            operations: vec![],
            operation_id: Some(id),
            warning: None,
        }
    }
    fn record_input_status(&mut self, status: &'static str) {
        self.input_status = status;
        self.cancel_pending();
    }
    #[cfg(test)]
    fn save_field(&mut self, index: usize) -> Result<(), &'static str> {
        self.vault
            .fields
            .get_mut(index)
            .ok_or("FIELD_NOT_FOUND")?
            .save()?;
        self.record_input_status("saved");
        Ok(())
    }
    fn cancel_pending(&mut self) {
        let mut count = 0;
        for operation in &mut self.operations {
            if matches!(operation.result.status.as_str(), "pending" | "queued") {
                operation.result.status = "cancelled".into();
                count += 1;
            }
        }
        if count > 0 {
            self.feedback.set(
                self.catalog
                    .format("temp_input_revoked", &[("count", &count.to_string())]),
            );
        }
    }
    fn expire_pending(&mut self) {
        let mut count = 0;
        for operation in &mut self.operations {
            if operation.result.status == "pending" && Instant::now() >= operation.expires {
                operation.result.status = "expired".into();
                count += 1;
            }
        }
        if count > 0 {
            self.feedback.set(
                self.catalog
                    .format("temp_expired_feedback", &[("count", &count.to_string())]),
            );
        }
    }
    fn result_feedback(&mut self, id: Uuid) {
        if let Some(operation) = self.operations.iter().find(|o| o.result.id == id) {
            let program = operation
                .execution
                .program
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            let mut message = self.catalog.format(
                "temp_result",
                &[
                    ("program", &program),
                    (
                        "status",
                        self.catalog
                            .text(&format!("temp_op_{}", operation.result.status)),
                    ),
                ],
            );
            if let Some(code) = operation.result.exit_code {
                message.push_str(&format!(
                    " · {}",
                    self.catalog
                        .format("temp_exit", &[("code", &code.to_string())])
                ));
            }
            self.feedback.set(message);
        }
    }
    fn poll(&mut self) {
        self.ssh_approvals.expire();
        self.bitwarden_unlocks.expire();
        if let Some(running) = &self.running {
            let result = match running.result.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(OperationStatus {
                    id: running.id,
                    status: "worker_failed".into(),
                    exit_code: None,
                }),
            };
            if let Some(result) = result {
                let id = result.id;
                if let Some(operation) = self
                    .operations
                    .iter_mut()
                    .find(|o| o.result.id == result.id)
                {
                    operation.result = result;
                }
                self.running = None;
                self.result_feedback(id);
            }
        }
        self.expire_pending();
        for _ in 0..8 {
            let Ok(envelope) = self.inbox.try_recv() else {
                break;
            };
            if !envelope.reply.is_closed() {
                let response = self.handle(envelope.request);
                let _ = envelope.reply.send(response);
            }
        }
        self.start_queued();
    }
    fn approve(&mut self, id: Uuid, context: &egui::Context) {
        self.expire_pending();
        if self
            .operations
            .iter()
            .any(|o| o.result.id == id && o.result.status == "pending")
        {
            self.start_operation(id, context);
            if self
                .running
                .as_ref()
                .is_some_and(|running| running.id == id)
            {
                self.feedback.set(self.catalog.format(
                    "temp_decided",
                    &[("decision", self.catalog.text("approval_allowed"))],
                ));
            }
        }
    }
    fn decide_approval(&mut self, id: Uuid, allow: bool, context: &egui::Context) {
        self.expire_pending();
        if !self
            .operations
            .iter()
            .any(|o| o.result.id == id && o.result.status == "pending")
        {
            self.feedback
                .set(self.catalog.text("approval_detail_invalid").to_owned());
            return;
        }
        if allow {
            self.approve(id, context);
        } else {
            let operation = self
                .operations
                .iter_mut()
                .find(|o| o.result.id == id)
                .expect("validated pending operation");
            operation.result.status = "cancelled".into();
            self.feedback.set(self.catalog.format(
                "temp_decided",
                &[("decision", self.catalog.text("approval_rejected"))],
            ));
        }
    }
    fn start_queued(&mut self) {
        if let Some(id) = self
            .operations
            .iter()
            .find(|o| o.result.status == "queued")
            .map(|o| o.result.id)
        {
            self.start_operation(id, &self.context.clone());
        }
    }
    fn start_operation(&mut self, id: Uuid, context: &egui::Context) {
        if self.running.is_some() {
            return;
        }
        let Some(operation) = self.operations.iter_mut().find(|o| {
            o.result.id == id && matches!(o.result.status.as_str(), "pending" | "queued")
        }) else {
            return;
        };
        let environment = match self.vault.environment(&operation.execution) {
            Ok(environment) => environment,
            Err(_) => {
                operation.result.status = "secret_missing".into();
                return;
            }
        };
        let execution = operation.execution.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let (sender, result) = mpsc::channel();
        let context = context.clone();
        let worker = thread::spawn(move || {
            let _ = sender.send(run_child(id, execution, environment, &flag));
            context.request_repaint();
        });
        operation.result.status = "running".into();
        self.running = Some(Running {
            id,
            cancel,
            result,
            worker: Some(worker),
        });
    }
}

impl SecretsApp {
    pub(crate) fn new(context: egui::Context) -> std::io::Result<Self> {
        let session = Uuid::new_v4();
        let (sender, inbox) = mpsc::sync_channel(8);
        let discovery = ipc::Server::start_discovery(session, sender.clone(), context.clone())?;
        let server = ipc::Server::start(session, sender, context.clone())?;
        Ok(Self {
            session,
            catalog: Catalog::for_locale(None),
            vault: Vault::default(),
            inbox,
            _server: server,
            _discovery: Some(discovery),
            visible: false,
            restore_requested: false,
            operations: VecDeque::new(),
            running: None,
            new_name: String::new(),
            delete_all_prompt: None,
            notice: None,
            input_status: "ok",
            context,
            ssh_approvals: crate::approval::Queue::default(),
            bitwarden_unlocks: crate::bitwarden::prompt::UnlockQueue::default(),
            detail: None,
            displayed_approval: None,
            approval_ready_at: 0.0,
            feedback: crate::approval_ui::Feedback::default(),
        })
    }
    pub(crate) fn logic(&mut self, context: &egui::Context) -> bool {
        self.poll();
        if self.running.is_some()
            || self.ssh_approvals.pending()
            || self
                .operations
                .iter()
                .any(|o| matches!(o.result.status.as_str(), "pending" | "queued"))
        {
            context.request_repaint_after(Duration::from_secs(1));
        }
        std::mem::take(&mut self.restore_requested)
    }
    pub(crate) fn show(&mut self, context: &egui::Context, catalog: &Catalog) {
        self.catalog = catalog.clone();
        self.expire_pending();
        if self.detail.is_some_and(|id| {
            !self
                .operations
                .iter()
                .any(|o| o.result.id == id && o.result.status == "pending")
        }) {
            self.detail = None;
            self.approval_ready_at =
                context.input(|input| input.time) + crate::approval_ui::ARM_SECONDS;
            self.feedback
                .set(self.catalog.text("approval_detail_invalid").to_owned());
        }
        if !self.visible {
            self.ssh_approvals.show(context, catalog);
            if !self.ssh_approvals.pending() {
                self.feedback.show(context, "temporary_feedback");
            }
            return;
        }
        let approval_pending = self.operations.iter().any(|o| o.result.status == "pending");
        let ssh_pending = self.ssh_approvals.pending();
        let mut open = true;
        let blocked = approval_pending || ssh_pending || self.delete_all_prompt.is_some();
        let frame =
            egui::Frame::window(&context.global_style()).inner_margin(egui::Margin::same(18));
        let window = egui::Window::new(catalog.text("temp_title"))
            .id(egui::Id::new("temporary_secrets_window"))
            .open(&mut open)
            .enabled(!blocked)
            .collapsible(false)
            .resizable(true)
            .default_width((context.content_rect().width() - 56.0).min(760.0))
            .max_width((context.content_rect().width() - 56.0).max(280.0))
            .min_width(280.0)
            .min_height(320.0)
            .frame(frame);
        let show = || window.show(context, |ui| self.contents(ui));
        let window = if blocked {
            crate::approval_ui::opaque_when_disabled(context, show)
        } else {
            show()
        };
        self.visible = open;
        if let Some(window) = window
            && !ssh_pending
            && self.delete_all_prompt.is_none()
        {
            self.show_approval(context, window.response.rect);
        } else if ssh_pending {
            self.approval_ready_at =
                context.input(|input| input.time) + crate::approval_ui::ARM_SECONDS;
        }
        self.ssh_approvals.show(context, catalog);
        if !ssh_pending {
            self.show_delete_all(context);
        }
        if !approval_pending && !ssh_pending {
            self.feedback.show(context, "temporary_feedback");
        }
    }
    fn show_approval(&mut self, context: &egui::Context, parent_rect: egui::Rect) {
        let Some(operation) = self
            .operations
            .iter()
            .find(|o| o.result.status == "pending")
        else {
            self.displayed_approval = None;
            return;
        };
        let id = operation.result.id;
        if self.displayed_approval != Some(id) {
            self.displayed_approval = Some(id);
            self.approval_ready_at =
                context.input(|input| input.time) + crate::approval_ui::ARM_SECONDS;
        }
        let ready = context.input(|input| input.time) >= self.approval_ready_at;
        if !ready {
            context.request_repaint_after(Duration::from_millis(50));
        }
        let pending = self
            .operations
            .iter()
            .filter(|o| o.result.status == "pending")
            .count();
        let bounds = parent_rect.intersect(context.content_rect()).shrink(12.0);
        let modal_id = egui::Id::new(("temporary_secret_approval", id));
        let mut decision = None;
        let mut open_detail = false;
        let modal = egui::Modal::new(modal_id)
            .area(
                // Center within the editor, but cover the viewport with the
                // backdrop so clicks cannot reach the underlying main window.
                egui::Modal::default_area(modal_id)
                    .default_size(bounds.size())
                    .anchor(
                        egui::Align2::CENTER_CENTER,
                        bounds.center() - context.content_rect().center(),
                    )
                    .enabled(self.detail.is_none())
                    .fade_in(false),
            )
            .frame(crate::approval_ui::frame(context))
            .show(context, |ui| {
                crate::approval_ui::style(ui);
                ui.set_width((bounds.width() - 40.0).min(600.0));
                let fields = operation
                    .execution
                    .env
                    .values()
                    .collect::<HashSet<_>>()
                    .len();
                crate::approval_ui::header(
                    ui,
                    self.catalog.text("temp_approval_title"),
                    &self
                        .catalog
                        .format("temp_approval_subtitle", &[("fields", &fields.to_string())]),
                );
                let buttons = ui
                    .add_enabled_ui(ready, |ui| {
                        crate::approval_ui::buttons(ui, &self.catalog, self.running.is_none())
                    })
                    .inner;
                if ready {
                    decision = buttons.decision;
                }
                #[cfg(test)]
                context.data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new(("temporary_approval_buttons", id)),
                        (buttons.confirm, buttons.cancel),
                    )
                });
                if let Some(running) = &self.running {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            self.catalog.text("temp_blocked_running"),
                        );
                        if ui.small_button(self.catalog.text("temp_stop")).clicked() {
                            running.cancel.store(true, Ordering::Relaxed);
                        }
                    });
                }
                if pending > 1 {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        self.catalog.format(
                            "temp_more_waiting",
                            &[("count", &(pending - 1).to_string())],
                        ),
                    );
                }
                if let Some(text) = self.feedback.text() {
                    ui.add(egui::Label::new(text).wrap());
                }
                crate::approval_ui::key_hint(ui, self.catalog.text("temp_key_hint"));
                ui.separator();
                let height = (bounds.height() - ui.min_rect().height() - 48.0).clamp(20.0, 260.0);
                egui::ScrollArea::vertical()
                    .id_salt(("temporary_approval_summary", id))
                    .auto_shrink([false, true])
                    .min_scrolled_height(20.0)
                    .max_height(height)
                    .show(ui, |ui| {
                        egui::Frame::group(ui.style())
                            .inner_margin(egui::Margin::same(10))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                crate::approval_ui::countdown_label(
                                    ui,
                                    &self.catalog,
                                    operation.expires,
                                );
                                crate::approval_ui::section(ui, self.catalog.text("temp_program"));
                                let command = format!(
                                    "{} {}",
                                    operation.execution.program.display(),
                                    serde_json::to_string(&operation.execution.args)
                                        .unwrap_or_default()
                                );
                                let response = crate::approval_ui::code_line(
                                    ui,
                                    &command,
                                    self.catalog.text("approval_view_details"),
                                );
                                if response.clicked() {
                                    open_detail = true;
                                }
                                #[cfg(test)]
                                context.data_mut(|data| {
                                    data.insert_temp(
                                        egui::Id::new(("temporary_command_preview", id)),
                                        response.rect,
                                    )
                                });
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(format!(
                                            "{}: {}",
                                            self.catalog.text("temp_directory"),
                                            crate::approval_ui::preview(
                                                &operation.execution.cwd.display().to_string()
                                            )
                                        ))
                                        .color(ui.visuals().weak_text_color()),
                                    )
                                    .wrap(),
                                );
                                crate::approval_ui::section(ui, self.catalog.text("temp_mappings"));
                                for (env, field) in operation
                                    .execution
                                    .env
                                    .iter()
                                    .take(crate::approval_ui::PREVIEW_COMMANDS)
                                {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(crate::approval_ui::preview(
                                                &format!("{env} ← {field}"),
                                            ))
                                            .monospace(),
                                        )
                                        .wrap(),
                                    );
                                }
                                let hidden = operation
                                    .execution
                                    .env
                                    .len()
                                    .saturating_sub(crate::approval_ui::PREVIEW_COMMANDS);
                                if hidden > 0 {
                                    ui.label(self.catalog.format(
                                        "temp_more_mappings",
                                        &[("count", &hidden.to_string())],
                                    ));
                                }
                                ui.add_space(4.0);
                                if ui
                                    .button(self.catalog.text("approval_view_details"))
                                    .clicked()
                                {
                                    open_detail = true;
                                }
                            });
                    });
            });
        if modal.response.rect.center().distance(bounds.center()) > 1.0 {
            context.request_discard("Center the temporary secret approval after measuring it");
        }
        #[cfg(test)]
        context.data_mut(|data| {
            data.insert_temp(
                egui::Id::new(("temporary_approval_bounds", id)),
                (parent_rect, modal.response.rect),
            )
        });
        if self.detail.is_none()
            && modal.is_top_modal
            && !modal.any_popup_open
            && crate::approval_ui::escape(context)
            && ready
        {
            decision = Some(false);
        }
        if let Some(allow) = decision {
            self.decide_approval(id, allow, context);
            self.approval_ready_at =
                context.input(|input| input.time) + crate::approval_ui::ARM_SECONDS;
        } else if open_detail && ready {
            self.detail = Some(id);
            self.approval_ready_at =
                context.input(|input| input.time) + crate::approval_ui::ARM_SECONDS;
        }
        self.show_detail(context);
    }

    fn show_detail(&mut self, context: &egui::Context) {
        let Some(id) = self.detail else {
            return;
        };
        let Some(operation) = self
            .operations
            .iter()
            .find(|o| o.result.id == id && o.result.status == "pending")
        else {
            self.detail = None;
            return;
        };
        let bounds = context.content_rect().shrink(24.0);
        let ready = context.input(|input| input.time) >= self.approval_ready_at;
        if !ready {
            context.request_repaint_after(Duration::from_millis(50));
        }
        let modal_id = egui::Id::new(("temporary_execution_detail", id));
        let mut decision = None;
        let mut back = false;
        let modal = egui::Modal::new(modal_id)
            .area(
                egui::Modal::default_area(modal_id)
                    .default_size(bounds.size())
                    .fade_in(false),
            )
            .frame(crate::approval_ui::frame(context))
            .show(context, |ui| {
                crate::approval_ui::style(ui);
                ui.set_width((bounds.width() - 40.0).min(760.0));
                crate::approval_ui::header(
                    ui,
                    self.catalog.text("temp_detail_title"),
                    self.catalog.text("temp_detail_scope"),
                );
                let buttons = ui
                    .add_enabled_ui(ready, |ui| {
                        crate::approval_ui::buttons(ui, &self.catalog, self.running.is_none())
                    })
                    .inner;
                if ready {
                    decision = buttons.decision;
                }
                #[cfg(test)]
                context.data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new(("temporary_detail_buttons", id)),
                        (buttons.confirm, buttons.cancel),
                    )
                });
                ui.horizontal_wrapped(|ui| {
                    let response = ui.small_button(self.catalog.text("approval_back"));
                    if response.clicked() {
                        back = true;
                    }
                    #[cfg(test)]
                    context.data_mut(|data| {
                        data.insert_temp(
                            egui::Id::new(("temporary_detail_back", id)),
                            response.rect,
                        )
                    });
                    crate::approval_ui::countdown_label(ui, &self.catalog, operation.expires);
                });
                crate::approval_ui::key_hint(ui, self.catalog.text("approval_detail_key_hint"));
                if let Some(running) = &self.running {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(self.catalog.text("temp_blocked_running"))
                                .color(ui.visuals().warn_fg_color),
                        )
                        .wrap(),
                    );
                    if ui.button(self.catalog.text("temp_stop")).clicked() {
                        running.cancel.store(true, Ordering::Relaxed);
                    }
                }
                if let Some(text) = self.feedback.text() {
                    ui.add(egui::Label::new(text).wrap());
                }
                ui.separator();
                let height = (bounds.height() - ui.min_rect().height() - 48.0).max(24.0);
                egui::ScrollArea::vertical()
                    .id_salt(("temporary_full_details", id))
                    .auto_shrink([false, true])
                    .max_height(height)
                    .show(ui, |ui| {
                        for (key, value) in [
                            (
                                "temp_program",
                                operation.execution.program.display().to_string(),
                            ),
                            (
                                "temp_directory",
                                operation.execution.cwd.display().to_string(),
                            ),
                            (
                                "temp_args",
                                serde_json::to_string_pretty(&operation.execution.args)
                                    .unwrap_or_default(),
                            ),
                        ] {
                            ui.strong(self.catalog.text(key));
                            crate::approval_ui::full_text(
                                ui,
                                &self.catalog,
                                egui::Id::new(("temporary_execution_text", id, key)),
                                &value,
                            );
                            ui.separator();
                        }
                        ui.label(self.catalog.format(
                            "temp_timeout",
                            &[("timeout", &operation.execution.timeout_ms.to_string())],
                        ));
                        ui.strong(self.catalog.text("temp_mappings"));
                        for (env, field) in &operation.execution.env {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!("{env} ← {field}")).monospace(),
                                )
                                .selectable(true)
                                .wrap(),
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
            context.request_discard("Center complete local execution details");
        }
        #[cfg(test)]
        context.data_mut(|data| {
            data.insert_temp(
                egui::Id::new(("temporary_detail_bounds", id)),
                modal.response.rect,
            )
        });
        if modal.is_top_modal && !modal.any_popup_open && crate::approval_ui::escape(context) {
            back = true;
        }
        if let Some(allow) = decision {
            self.decide_approval(id, allow, context);
            self.detail = None;
            self.approval_ready_at =
                context.input(|input| input.time) + crate::approval_ui::ARM_SECONDS;
        } else if back {
            self.detail = None;
            self.approval_ready_at =
                context.input(|input| input.time) + crate::approval_ui::ARM_SECONDS;
        }
    }
    fn contents(&mut self, ui: &mut egui::Ui) {
        let context = ui.ctx().clone();
        ui.label(
            egui::RichText::new(self.catalog.text("temp_memory_notice"))
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(12.0);
        self.field_toolbar(ui);
        if let Some(notice) = self.notice {
            ui.colored_label(ui.visuals().warn_fg_color, self.catalog.text(notice));
        }
        ui.add_space(16.0);
        egui::ScrollArea::vertical()
            .id_salt("temporary_secret_scroll")
            .auto_shrink([false, true])
            .max_height((context.content_rect().height() - 240.0).clamp(120.0, 460.0))
            .show(ui, |ui| {
                self.field_rows(ui);
                if !self.operations.is_empty() {
                    ui.separator();
                    ui.strong(self.catalog.text("temp_operations"));
                }
                let mut history = 0;
                for operation in self.operations.iter().rev().filter(|operation| {
                    if matches!(
                        operation.result.status.as_str(),
                        "pending" | "queued" | "running"
                    ) {
                        true
                    } else {
                        history += 1;
                        history <= 4
                    }
                }) {
                    ui.group(|ui| {
                        ui.label(format!(
                            "{} · {}",
                            operation
                                .execution
                                .program
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy(),
                            self.catalog
                                .text(&format!("temp_op_{}", operation.result.status))
                        ));
                        if let Some(code) = operation.result.exit_code {
                            ui.label(
                                self.catalog
                                    .format("temp_exit", &[("code", &code.to_string())]),
                            );
                        }
                        if operation.result.status == "running"
                            && ui.button(self.catalog.text("temp_stop")).clicked()
                            && let Some(running) = &self.running
                        {
                            running.cancel.store(true, Ordering::Relaxed);
                        }
                    });
                }
            });
        if self.running.is_some()
            || self
                .operations
                .iter()
                .any(|o| matches!(o.result.status.as_str(), "pending" | "queued"))
        {
            context.request_repaint_after(Duration::from_millis(100));
        }
    }
}

/// The current process user's SID as text, shared with the store owner check.
pub fn current_user_sid() -> std::io::Result<String> {
    #[cfg(windows)]
    {
        ipc::current_user_sid()
    }
    #[cfg(not(windows))]
    {
        Err(std::io::Error::other("PLATFORM_UNSUPPORTED"))
    }
}

pub fn restore_existing(temporary: bool) -> bool {
    #[cfg(windows)]
    {
        ipc::discover(Request::Show { temporary }).is_ok()
    }
    #[cfg(not(windows))]
    {
        let _ = temporary;
        false
    }
}

pub fn open(fields: Vec<String>) -> Result<Response, &'static str> {
    validate_names(&fields, false)?;
    request_gui(Request::Declare { fields })
}

pub(crate) fn request_gui(request: Request) -> Result<Response, &'static str> {
    if let Ok(response) = ipc::discover(request.clone()) {
        return Ok(response);
    }
    let exe = std::env::current_exe().map_err(|_| "TEMPORARY_WINDOW_FAILED")?;
    let (mut child, shares_job) = spawn_holder(&exe).map_err(|_| "TEMPORARY_WINDOW_FAILED")?;
    let start = Instant::now();
    loop {
        if let Ok(mut response) = ipc::discover(request.clone()) {
            if shares_job {
                response.warning = Some(HOLDER_SHARES_JOB_WARNING.into());
            }
            return Ok(response);
        }
        // A concurrent launcher may exit after handing off to the winning instance.
        let _ = child.try_wait();
        if start.elapsed() >= Duration::from_secs(20) {
            return Err("TEMPORARY_WINDOW_TIMEOUT");
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Starts the tray holder outside the caller's job object, so a window first
/// opened from Codex's MCP server outlives Codex. A job that forbids breakaway
/// gets a plain child instead; the flag says whether that child dies with the
/// job.
fn spawn_holder(exe: &Path) -> std::io::Result<(Child, bool)> {
    let plain = |exe: &Path| {
        let mut command = Command::new(exe);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        if let Ok(child) = plain(exe).creation_flags(CREATE_BREAKAWAY_FROM_JOB).spawn() {
            return Ok((child, false));
        }
        let child = plain(exe).spawn()?;
        Ok((child, job_kills_on_close()))
    }
    #[cfg(not(windows))]
    {
        Ok((plain(exe).spawn()?, false))
    }
}

#[cfg(windows)]
fn job_kills_on_close() -> bool {
    use windows_sys::Win32::System::JobObjects::{
        IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, QueryInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: plain Win32 queries on the current process with a zeroed,
    // correctly sized output buffer; a null job handle means "the job of the
    // calling process".
    unsafe {
        let mut in_job = 0;
        if IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) == 0
            || in_job == 0
        {
            return false;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        let queried = QueryInformationJobObject(
            std::ptr::null_mut(),
            JobObjectExtendedLimitInformation,
            &mut info as *mut _ as *mut _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            std::ptr::null_mut(),
        );
        queried != 0
            && info.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE != 0
    }
}

pub fn call(session: Uuid, request: Request) -> Result<Response, &'static str> {
    #[cfg(windows)]
    {
        ipc::call(session, request).map_err(|_| "TEMPORARY_SESSION_UNAVAILABLE")
    }
    #[cfg(not(windows))]
    {
        let _ = (session, request);
        Err("PLATFORM_UNSUPPORTED")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    fn vault() -> Vault {
        let mut vault = Vault::default();
        vault
            .request(&["project-local-qwen-apikey".into()])
            .unwrap();
        vault
    }
    fn execution() -> Execution {
        Execution {
            program: Path::new("C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe")
                .to_owned(),
            args: vec![],
            cwd: Path::new("C:/Windows").to_owned(),
            env: BTreeMap::from([("API_KEY".into(), "project-local-qwen-apikey".into())]),
            timeout_ms: 5000,
        }
    }
    #[test]
    fn field_approval_defaults_on_and_only_referenced_fields_require_review() {
        let mut vault = vault();
        vault.request(&["another-project-token".into()]).unwrap();
        assert!(vault.status().iter().all(|field| field.requires_approval));
        let mut call = execution();
        assert!(vault.requires_approval(&call));
        vault.fields[0].requires_approval = false;
        assert!(!vault.requires_approval(&call));
        call.env
            .insert("OTHER_TOKEN".into(), "another-project-token".into());
        assert!(vault.requires_approval(&call));
        vault.fields[1].requires_approval = false;
        assert!(!vault.requires_approval(&call));
        vault
            .request(&["project-local-qwen-apikey".into()])
            .unwrap();
        assert!(!vault.fields[0].requires_approval); // Reopening preserves the user's choice.
        vault.clear(&[]).unwrap();
        assert!(!vault.fields[0].requires_approval);
        let old: FieldStatus =
            serde_json::from_str(r#"{"name":"old-holder","ready":true}"#).unwrap();
        assert!(old.requires_approval);
        assert_eq!(
            serde_json::to_value(vault.status()).unwrap()[0]["requires_approval"],
            false
        );
    }

    #[test]
    fn names_are_bounded_unique_and_requests_atomic() {
        let mut vault = vault();
        assert!(vault.request(&["new".into(), "\n".into()]).is_err());
        assert_eq!(vault.fields.len(), 1);
        assert!(validate_names(&["x".into(), "x".into()], false).is_err());
        assert!(validate_names(&["x".repeat(257)], false).is_err());
        assert!(validate_names(&vec!["x".into(); 65], false).is_err());
        assert!(validate_names(&[], false).is_err());
        vault
            .request(&[
                "project-local-qwen-apikey".into(),
                "project-prod-token".into(),
            ])
            .unwrap();
        assert_eq!(vault.fields.len(), 2);
    }
    #[test]
    fn save_preserves_whitespace_and_clear_drops_both_buffers() {
        let mut vault = vault();
        vault.fields[0].draft.push_str("  sentinel\nvalue \n");
        vault.fields[0].save().unwrap();
        assert!(vault.fields[0].draft.is_empty());
        assert_eq!(
            vault.fields[0].value.as_deref().map(|s| s.as_str()),
            Some("  sentinel\nvalue \n")
        );
        assert!(
            !serde_json::to_string(&vault.status())
                .unwrap()
                .contains("sentinel")
        );
        vault.fields[0].draft.push_str("replacement");
        assert!(vault.clear(&["unknown".into()]).is_err());
        assert!(vault.status()[0].ready);
        vault.clear(&[]).unwrap();
        assert!(!vault.status()[0].ready);
        assert!(vault.fields[0].draft.is_empty());
        assert!(Vault::default().fields.is_empty());
    }
    #[test]
    fn invalid_replacement_preserves_saved_value() {
        let mut vault = vault();
        let field = &mut vault.fields[0];
        field.draft.push_str("old");
        field.save().unwrap();
        field.draft.push('\0');
        assert!(field.save().is_err());
        assert_eq!(field.value.as_ref().unwrap().as_str(), "old");
        field.draft = Zeroizing::new("x".repeat(MAX_VALUE_BYTES + 1));
        assert!(field.save().is_err());
    }
    #[test]
    fn protocol_has_no_plaintext_set_or_get() {
        for json in [
            r#"{"action":"get","field":"x"}"#,
            r#"{"action":"request","fields":["x"],"secret":"sentinel"}"#,
        ] {
            assert!(serde_json::from_str::<Request>(json).is_err());
        }
    }
    #[test]
    #[cfg(windows)]
    fn execution_validation_rejects_ambiguous_environment_and_missing_secrets() {
        let mut execution = execution();
        let vault = vault();
        assert_eq!(vault.environment(&execution).err(), Some("SECRET_MISSING"));
        execution
            .env
            .insert("api_key".into(), "project-local-qwen-apikey".into());
        assert_eq!(execution.validate(), Err("ENVIRONMENT_INVALID"));
        execution.env.remove("api_key");
        execution.program = PathBuf::from("cmd.exe");
        assert_eq!(execution.validate(), Err("EXECUTION_INVALID"));
    }
    #[test]
    #[cfg(windows)]
    fn child_consumes_exact_environment_but_response_contains_no_output() {
        let mut execution = execution();
        execution.args = vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), "if ($env:API_KEY -eq 'temporary-sentinel') { Write-Output $env:API_KEY; exit 0 } else { exit 9 }".into()];
        let result = run_child(
            Uuid::new_v4(),
            execution,
            vec![(
                "API_KEY".into(),
                Zeroizing::new("temporary-sentinel".into()),
            )],
            &AtomicBool::new(false),
        );
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.status, "completed");
        assert!(!serde_json::to_string(&result).unwrap().contains("sentinel"));
        assert!(std::env::var("API_KEY").ok().as_deref() != Some("temporary-sentinel"));
    }
    #[test]
    #[cfg(windows)]
    fn child_timeout_and_cancellation_are_bounded() {
        let mut execution = execution();
        execution.args = vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            "Start-Sleep -Seconds 30".into(),
        ];
        execution.timeout_ms = 50;
        let start = Instant::now();
        assert_eq!(
            run_child(
                Uuid::new_v4(),
                execution.clone(),
                vec![],
                &AtomicBool::new(false)
            )
            .status,
            "timed_out"
        );
        assert!(start.elapsed() < Duration::from_secs(3));
        assert_eq!(
            run_child(Uuid::new_v4(), execution, vec![], &AtomicBool::new(true)).status,
            "cancelled"
        );
    }
    #[cfg(windows)]
    fn app() -> SecretsApp {
        let session = Uuid::new_v4();
        let (sender, inbox) = mpsc::sync_channel(8);
        let server = ipc::Server::start(session, sender, egui::Context::default()).unwrap();
        let mut vault = vault();
        vault.fields[0].draft.push_str("temporary-sentinel");
        vault.fields[0].save().unwrap();
        SecretsApp {
            session,
            catalog: Catalog::for_locale(Some("en")),
            vault,
            inbox,
            _server: server,
            _discovery: None,
            visible: false,
            restore_requested: false,
            operations: VecDeque::new(),
            running: None,
            new_name: String::new(),
            delete_all_prompt: None,
            notice: None,
            input_status: "ok",
            context: egui::Context::default(),
            ssh_approvals: crate::approval::Queue::default(),
            bitwarden_unlocks: crate::bitwarden::prompt::UnlockQueue::default(),
            detail: None,
            displayed_approval: None,
            approval_ready_at: 0.0,
            feedback: crate::approval_ui::Feedback::default(),
        }
    }
    #[test]
    #[cfg(windows)]
    fn ssh_call_approval_transport_consumes_decisions_and_cancels_abandoned_calls() {
        let mut app = app();
        let mut host = crate::model::HostProfile::new("ssh-fixture".into());
        host.address = "127.0.0.1".into();
        host.username = "fixture".into();
        host.advanced.require_call_approval = true;
        let context = egui::Context::default();
        crate::ui::configure_fonts(&context, "en");
        let catalog = Catalog::for_locale(Some("en"));
        for outcome in ["approved", "cancelled", "abandoned"] {
            let review = crate::approval::Review::new(
                std::slice::from_ref(&host),
                std::slice::from_ref(&host),
                vec!["printf first".into(), "printf second".into()],
                Some("fixture-input"),
                None,
            )
            .unwrap();
            let task = crate::ssh::runtime()
                .spawn(crate::approval::authorize_session(app.session, review));
            let deadline = Instant::now() + Duration::from_secs(5);
            while !app.ssh_approvals.pending() && !task.is_finished() && Instant::now() < deadline {
                app.poll();
                thread::sleep(Duration::from_millis(5));
            }
            if !app.ssh_approvals.pending() {
                task.abort();
                panic!("private GUI IPC did not receive the SSH review");
            }
            assert!(app.running.is_none());
            assert!(app.restore_requested);
            let id = app.ssh_approvals.active_id().unwrap();
            if outcome == "abandoned" {
                task.abort();
            } else {
                // A fresh call can arrive before an empty UI frame clears the
                // completed snapshot; it then replaces that batch on its own,
                // after the longer pause for following calls.
                for _ in 0..8 {
                    approval_frame(
                        &mut app,
                        &context,
                        &catalog,
                        egui::vec2(820.0, 620.0),
                        vec![],
                    );
                }
                let (confirm, cancel) = context
                    .data(|data| {
                        data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new((
                            "ssh_approval_buttons",
                            id,
                        )))
                    })
                    .unwrap();
                let button = if outcome == "approved" {
                    confirm
                } else {
                    cancel
                };
                approval_click(&mut app, &context, &catalog, button.center());
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            while !task.is_finished() && Instant::now() < deadline {
                app.poll();
                thread::sleep(Duration::from_millis(5));
            }
            if !task.is_finished() {
                task.abort();
                panic!("SSH authorization did not complete after its GUI decision");
            }
            let result = crate::ssh::runtime().block_on(task);
            if outcome == "abandoned" {
                assert!(result.unwrap_err().is_cancelled());
            } else if outcome == "approved" {
                assert!(result.unwrap().is_ok());
            } else {
                assert_eq!(result.unwrap().unwrap_err().code, "APPROVAL_CANCELLED");
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.ssh_approvals.pending() && Instant::now() < deadline {
                app.poll();
                thread::sleep(Duration::from_millis(5));
            }
            assert!(!app.ssh_approvals.pending());
            assert_eq!(
                app.ssh_approvals.take(id),
                if outcome == "abandoned" {
                    "cancelled"
                } else {
                    "unavailable"
                }
            );
        }
    }

    #[test]
    #[cfg(windows)]
    fn opted_out_runs_start_directly_and_queued_runs_are_not_evicted() {
        let mut app = app();
        app.vault.fields[0].requires_approval = false;
        let mut call = execution();
        call.program = std::env::temp_dir().join(format!("missing-{}.exe", Uuid::new_v4()));
        let first = app.handle(Request::Execute(call.clone()));
        let first_id = first.operation_id.unwrap();
        assert_eq!(app.running.as_ref().unwrap().id, first_id);
        assert_eq!(first.operations[0].status, "running");
        assert!(!app.visible && !app.restore_requested);
        for _ in 1..MAX_OPERATIONS {
            let next = app.handle(Request::Execute(call.clone()));
            assert_eq!(next.operations.last().unwrap().status, "queued");
        }
        let ids: Vec<_> = app.operations.iter().map(|o| o.result.id).collect();
        assert_eq!(
            app.handle(Request::Execute(call)).code.as_deref(),
            Some("OPERATION_LIMIT")
        );
        assert_eq!(
            app.operations
                .iter()
                .map(|o| o.result.id)
                .collect::<Vec<_>>(),
            ids
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.running.is_some() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
            app.poll();
        }
        assert!(app.running.is_none());
        assert!(
            app.operations
                .iter()
                .all(|o| o.result.status == "spawn_failed")
        );
    }

    #[test]
    #[cfg(windows)]
    fn changes_revoke_queued_runs_without_restarting_the_running_child() {
        for clear in [false, true] {
            let mut app = app();
            app.vault.fields[0].requires_approval = false;
            let mut call = execution();
            call.program = std::env::temp_dir().join(format!("missing-{}.exe", Uuid::new_v4()));
            let first = app
                .handle(Request::Execute(call.clone()))
                .operation_id
                .unwrap();
            app.handle(Request::Execute(call));
            assert_eq!(app.operations[1].result.status, "queued");
            if clear {
                app.handle(Request::Clear { fields: vec![] });
            } else {
                app.vault.fields[0].requires_approval = true;
                app.record_input_status("ok");
            }
            assert_eq!(app.operations[1].result.status, "cancelled");
            assert_eq!(app.running.as_ref().unwrap().id, first);
            assert_eq!(app.operations[0].result.status, "running");
        }
    }

    #[test]
    #[cfg(windows)]
    fn successful_save_returns_saved_status_without_plaintext() {
        let mut app = app();
        app.vault.clear(&[]).unwrap();
        app.vault.fields[0].draft.push_str("replacement-sentinel");

        app.save_field(0).unwrap();

        let response = app.handle(Request::Status);
        assert_eq!(response.status, "saved");
        assert!(response.fields[0].ready);
        assert!(
            !serde_json::to_string(&response)
                .unwrap()
                .contains("replacement-sentinel")
        );

        let cleared = app.handle(Request::Clear { fields: vec![] });
        assert_eq!(cleared.status, "ok");
        assert!(!cleared.fields[0].ready);
    }

    #[test]
    fn saved_fields_can_be_reused_after_reopen_and_execute_without_new_input() {
        let mut app = app();
        let session = app.session;
        let first = app.handle(Request::Declare {
            fields: vec!["project-local-qwen-apikey".into()],
        });
        assert_eq!(first.status, "ok");
        assert_eq!(first.session, session);
        assert!(first.fields[0].ready);
        assert!(app.vault.fields[0].draft.is_empty());
        let first_op = app.handle(Request::Execute(execution()));
        assert!(first_op.operation_id.is_some());
        assert_eq!(first_op.status, "ok");
        let second_op = app.handle(Request::Execute(execution()));
        assert!(second_op.operation_id.is_some());
        assert_ne!(first_op.operation_id, second_op.operation_id);
        assert!(app.running.is_none()); // Saved fields still require approval by default.
        let more = app.handle(Request::Declare {
            fields: vec!["new-project-key".into()],
        });
        assert!(more.fields[0].ready);
        assert!(!more.fields[1].ready);
        app.handle(Request::Clear {
            fields: vec!["project-local-qwen-apikey".into()],
        });
        assert_eq!(
            app.handle(Request::Execute(execution())).code.as_deref(),
            Some("SECRET_MISSING")
        );
    }

    #[test]
    #[cfg(windows)]
    fn approval_is_required_and_clear_revokes_pending_operations() {
        let mut app = app();
        let response = app.handle(Request::Execute(execution()));
        assert!(response.operation_id.is_some());
        assert!(app.running.is_none());
        assert_eq!(response.operations[0].status, "pending");
        app.handle(Request::Clear { fields: vec![] });
        let id = response.operation_id.unwrap();
        app.approve(id, &egui::Context::default());
        assert!(app.running.is_none());
        assert_eq!(app.operations[0].result.status, "cancelled");
        assert_eq!(
            app.handle(Request::Execute(execution())).code.as_deref(),
            Some("SECRET_MISSING")
        );
    }
    #[test]
    #[cfg(windows)]
    fn approval_executes_then_reports_only_exit_metadata() {
        let mut app = app();
        let mut execution = execution();
        execution.args = vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(),
            "if ($env:API_KEY -eq 'temporary-sentinel') { Write-Output $env:API_KEY; exit 0 } else { exit 9 }".into()];
        let id = app
            .handle(Request::Execute(execution))
            .operation_id
            .unwrap();
        app.approve(id, &egui::Context::default());
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.running.is_some() && Instant::now() < deadline {
            app.poll();
            thread::sleep(Duration::from_millis(10));
        }
        assert!(app.running.is_none());
        assert_eq!(app.operations[0].result.status, "completed");
        assert_eq!(app.operations[0].result.exit_code, Some(0));
        assert!(
            !serde_json::to_string(&app.response())
                .unwrap()
                .contains("temporary-sentinel")
        );
    }
    #[test]
    #[cfg(windows)]
    fn pending_approvals_expire_and_operation_count_is_bounded() {
        let mut app = app();
        for _ in 0..MAX_OPERATIONS {
            assert_eq!(app.handle(Request::Execute(execution())).status, "ok");
        }
        assert_eq!(
            app.handle(Request::Execute(execution())).code.as_deref(),
            Some("OPERATION_LIMIT")
        );
        app.operations[0].expires = Instant::now();
        app.poll();
        assert_eq!(app.operations[0].result.status, "expired");
        assert_eq!(app.handle(Request::Execute(execution())).status, "ok");
        assert_eq!(app.operations.len(), MAX_OPERATIONS);
    }
    #[test]
    #[cfg(windows)]
    fn separate_sessions_and_new_fields_do_not_mix_values() {
        let mut first = app();
        let mut second = app();
        assert_ne!(first.session, second.session);
        first.handle(Request::Declare {
            fields: vec!["project-two-token".into()],
        });
        assert!(first.vault.status()[0].ready);
        assert!(!first.vault.status()[1].ready);
        second.handle(Request::Clear { fields: vec![] });
        assert!(first.vault.status()[0].ready);
        assert!(!second.vault.status()[0].ready);
    }
    #[test]
    #[cfg(windows)]
    fn dropping_running_guard_cancels_direct_child() {
        let mut app = app();
        let mut execution = execution();
        execution.timeout_ms = 60_000;
        execution.args = vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            "Start-Sleep -Seconds 30".into(),
        ];
        let id = app
            .handle(Request::Execute(execution))
            .operation_id
            .unwrap();
        app.approve(id, &egui::Context::default());
        let start = Instant::now();
        drop(app);
        assert!(start.elapsed() < Duration::from_secs(3));
    }
    #[test]
    fn all_operation_statuses_have_translations() {
        for language in Catalog::available() {
            let catalog = Catalog::for_locale(Some(language.locale));
            for status in [
                "pending",
                "queued",
                "running",
                "completed",
                "failed",
                "cancelled",
                "timed_out",
                "spawn_failed",
                "wait_failed",
                "worker_failed",
                "secret_missing",
                "expired",
            ] {
                assert!(!catalog.text(&format!("temp_op_{status}")).is_empty());
            }
        }
    }
    #[test]
    fn execution_protocol_round_trip_contains_only_field_references() {
        let request = Request::Execute(execution());
        let encoded = serde_json::to_string(&request).unwrap();
        assert!(encoded.contains("\"action\":\"execute\""));
        assert!(matches!(
            serde_json::from_str::<Request>(&encoded).unwrap(),
            Request::Execute(_)
        ));
        let invalid = encoded.replacen('{', "{\"value\":\"forbidden\",", 1);
        assert!(serde_json::from_str::<Request>(&invalid).is_err());
    }

    #[test]
    fn hiding_editor_retains_saved_values_and_same_session() {
        let mut app = app();
        let session = app.session;
        app.visible = true;
        app.visible = false;
        let response = app.handle(Request::Status);
        assert_eq!(response.session, session);
        assert!(response.fields[0].ready);
        assert!(!app.visible);
        app.handle(Request::Show { temporary: true });
        assert!(app.visible);
        assert!(app.restore_requested);
        assert_eq!(
            app.vault.fields[0].value.as_ref().unwrap().as_str(),
            "temporary-sentinel"
        );
        app.handle(Request::Declare {
            fields: vec!["another-project-token".into()],
        });
        assert_eq!(app.session, session);
        assert!(app.vault.fields[0].value.is_some());
    }
    #[test]
    fn internal_editor_has_no_native_viewport_or_session_label() {
        let mut app = app();
        app.visible = true;
        let context = egui::Context::default();
        crate::ui::configure_fonts(&context, "en");
        let output = context.run_ui(egui::RawInput::default(), |ui| {
            app.show(ui.ctx(), &Catalog::for_locale(Some("en")))
        });
        assert_eq!(output.viewport_output.len(), 1);
        assert!(output.viewport_output.contains_key(&egui::ViewportId::ROOT));
        fn texts(shape: &egui::Shape, values: &mut String) {
            match shape {
                egui::Shape::Text(text) => {
                    values.push_str(&text.galley.job.text);
                    values.push('\n');
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        texts(shape, values);
                    }
                }
                _ => {}
            }
        }
        let mut rendered = String::new();
        for shape in &output.shapes {
            texts(&shape.shape, &mut rendered);
        }
        assert!(!rendered.contains(&app.session.to_string()));
        assert!(!rendered.contains("Secret-consuming operations"));
        assert!(!rendered.contains("temporary-sentinel"));
    }

    #[test]
    fn secret_fields_keep_form_width_across_locales_and_frames() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            for dark in [false, true] {
                for width in [380.0, 540.0, 774.0, 820.0, 1040.0] {
                    let mut app = app();
                    app.vault = Vault::default();
                    let names = [
                        "0202",
                        "myproject-staging-qwen-apikey-with-a-long-name",
                        "项目测试用的长字段名称用于检查自动换行与左对齐",
                    ];
                    app.vault.request(&names.map(str::to_owned)).unwrap();
                    app.vault.fields[0]
                        .draft
                        .push_str("plaintext-layout-sentinel");
                    app.catalog = Catalog::for_locale(Some(locale));
                    let context = egui::Context::default();
                    context.set_visuals(if dark {
                        egui::Visuals::dark()
                    } else {
                        egui::Visuals::light()
                    });
                    crate::ui::configure_fonts(&context, locale);
                    for frame in 0..4 {
                        let output = context.run_ui(
                            egui::RawInput {
                                screen_rect: Some(egui::Rect::from_min_size(
                                    egui::Pos2::ZERO,
                                    egui::vec2(width, 2400.0),
                                )),
                                time: Some(frame as f64 * 0.1),
                                ..Default::default()
                            },
                            |ui| {
                                ui.set_width(width);
                                app.field_rows(ui);
                            },
                        );
                        let rect = |kind, name| {
                            context
                                .data(|d| d.get_temp::<egui::Rect>(egui::Id::new((kind, name))))
                                .unwrap()
                        };
                        let wide = width - 26.0 >= 600.0;
                        let mut previous = None::<egui::Rect>;
                        let first = rect("temporary_layout_input", names[0]);
                        for name in names {
                            let label = rect("temporary_layout_name", name);
                            let input = rect("temporary_layout_input", name);
                            let approval = rect("temporary_layout_approval", name);
                            for bounds in [label, input, approval] {
                                assert!(
                                    bounds.left() >= 0.0 && bounds.right() <= width + 1.0,
                                    "{locale} {dark} {width}: {bounds:?}"
                                );
                            }
                            assert!(input.height() >= 64.0 && input.width() >= 220.0);
                            assert!(
                                (input.left() - first.left()).abs() < 1.0
                                    && (input.width() - first.width()).abs() < 1.0
                            );
                            if let Some(previous) = previous {
                                assert!(label.top() > previous.bottom());
                            }
                            if wide {
                                for (bounds, key) in [
                                    (label, "temp_field"),
                                    (input, "temp_secret"),
                                    (approval, "temp_field_approval"),
                                ] {
                                    assert!(
                                        (bounds.left() - rect("temporary_header", key).left())
                                            .abs()
                                            < 1.0,
                                        "{locale} {width}: header alignment {key}"
                                    );
                                }
                                assert!(
                                    label.right() < input.left() && input.right() < approval.left()
                                );
                                assert!((label.top() - input.top()).abs() < 2.0);
                            } else {
                                assert!((label.left() - input.left()).abs() < 1.0);
                                assert!(
                                    input.top() > label.bottom() && approval.top() > input.bottom()
                                );
                            }
                            previous = Some(label.union(input).union(approval));
                        }
                        fn no_plaintext(shape: &egui::Shape) {
                            match shape {
                                egui::Shape::Text(text) => assert!(
                                    !text.galley.job.text.contains("plaintext-layout-sentinel")
                                ),
                                egui::Shape::Vec(shapes) => shapes.iter().for_each(no_plaintext),
                                _ => {}
                            }
                        }
                        for shape in &output.shapes {
                            no_plaintext(&shape.shape);
                        }
                    }
                }
            }
        }
    }

    fn approval_frame(
        app: &mut SecretsApp,
        context: &egui::Context,
        catalog: &Catalog,
        size: egui::Vec2,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, bool) {
        let mut background_clicked = false;
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                time: Some(context.input(|i| i.time) + 0.1),
                events,
                ..Default::default()
            },
            |ui| {
                let background = ui.button("Background action");
                background_clicked = background.clicked();
                context.data_mut(|data| {
                    data.insert_temp(egui::Id::new("approval_test_background"), background.rect);
                });
                app.show(ui.ctx(), catalog);
            },
        );
        (output, background_clicked)
    }

    fn approval_click(
        app: &mut SecretsApp,
        context: &egui::Context,
        catalog: &Catalog,
        position: egui::Pos2,
    ) -> bool {
        let mut background_clicked = false;
        for pressed in [true, false] {
            background_clicked |= approval_frame(
                app,
                context,
                catalog,
                egui::vec2(820.0, 620.0),
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            )
            .1;
        }
        background_clicked
    }

    fn warm_approval(app: &mut SecretsApp, context: &egui::Context, catalog: &Catalog) {
        for _ in 0..4 {
            approval_frame(app, context, catalog, egui::vec2(820.0, 620.0), vec![]);
        }
    }

    #[test]
    #[cfg(windows)]
    fn local_detail_navigation_expiry_and_rejection_apply_only_to_the_selected_run() {
        let mut app = app();
        let mut execution = execution();
        execution.program = std::env::temp_dir().join(format!("missing-{}.exe", Uuid::new_v4()));
        execution.args = vec!["完整 argument\n".repeat(400); 4];
        let first = app
            .handle(Request::Execute(execution.clone()))
            .operation_id
            .unwrap();
        let second = app
            .handle(Request::Execute(execution))
            .operation_id
            .unwrap();
        let context = egui::Context::default();
        crate::ui::configure_fonts(&context, "en");
        let catalog = Catalog::for_locale(Some("en"));
        for escape in [false, true] {
            warm_approval(&mut app, &context, &catalog);
            let preview = context
                .data(|data| {
                    data.get_temp::<egui::Rect>(egui::Id::new(("temporary_command_preview", first)))
                })
                .unwrap();
            approval_click(&mut app, &context, &catalog, preview.center());
            warm_approval(&mut app, &context, &catalog);
            assert_eq!(app.detail, Some(first));
            if escape {
                approval_frame(
                    &mut app,
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
                        data.get_temp::<egui::Rect>(egui::Id::new(("temporary_detail_back", first)))
                    })
                    .unwrap();
                approval_click(&mut app, &context, &catalog, back.center());
            }
            assert!(app.detail.is_none());
            assert!(app.operations.iter().all(|o| o.result.status == "pending"));
            assert!(app.running.is_none());
        }
        warm_approval(&mut app, &context, &catalog);
        let preview = context
            .data(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new(("temporary_command_preview", first)))
            })
            .unwrap();
        approval_click(&mut app, &context, &catalog, preview.center());
        warm_approval(&mut app, &context, &catalog);
        let (confirm, _) = context
            .data(|data| {
                data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new((
                    "temporary_detail_buttons",
                    first,
                )))
            })
            .unwrap();
        app.operations[0].expires = Instant::now();
        approval_click(&mut app, &context, &catalog, confirm.center());
        assert!(app.detail.is_none());
        assert_eq!(app.operations[0].result.status, "expired");
        assert_eq!(app.operations[1].result.status, "pending");
        assert!(app.running.is_none());
        warm_approval(&mut app, &context, &catalog);
        let preview = context
            .data(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new(("temporary_command_preview", second)))
            })
            .unwrap();
        approval_click(&mut app, &context, &catalog, preview.center());
        warm_approval(&mut app, &context, &catalog);
        let (_, reject) = context
            .data(|data| {
                data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new((
                    "temporary_detail_buttons",
                    second,
                )))
            })
            .unwrap();
        approval_click(&mut app, &context, &catalog, reject.center());
        assert_eq!(app.operations[1].result.status, "cancelled");
        assert!(app.running.is_none());
        assert!(app.feedback.text().unwrap().starts_with("Rejected"));
        let third = app
            .handle(Request::Execute(app.operations[0].execution.clone()))
            .operation_id
            .unwrap();
        let fourth = app
            .handle(Request::Execute(app.operations[0].execution.clone()))
            .operation_id
            .unwrap();
        warm_approval(&mut app, &context, &catalog);
        app.detail = Some(third);
        warm_approval(&mut app, &context, &catalog);
        let (confirm, _) = context
            .data(|data| {
                data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new((
                    "temporary_detail_buttons",
                    third,
                )))
            })
            .unwrap();
        approval_click(&mut app, &context, &catalog, confirm.center());
        assert_eq!(app.running.as_ref().unwrap().id, third);
        assert_eq!(
            app.operations
                .iter()
                .find(|o| o.result.id == fourth)
                .unwrap()
                .result
                .status,
            "pending"
        );
        assert!(app.feedback.text().unwrap().starts_with("Allowed"));
    }

    #[test]
    #[cfg(windows)]
    fn local_full_details_fit_small_windows_keep_top_actions_and_never_render_secret_values() {
        fn text(shape: &egui::Shape, values: &mut String) {
            match shape {
                egui::Shape::Text(text) => values.push_str(&text.galley.job.text),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        text(shape, values);
                    }
                }
                _ => {}
            }
        }
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            let mut app = app();
            let mut execution = execution();
            execution.args = vec!["long argument\n".repeat(500); 4];
            let id = app
                .handle(Request::Execute(execution))
                .operation_id
                .unwrap();
            let context = egui::Context::default();
            crate::ui::configure_fonts(&context, locale);
            let catalog = Catalog::for_locale(Some(locale));
            warm_approval(&mut app, &context, &catalog);
            app.detail = Some(id);
            let size = egui::vec2(774.0, 584.0);
            for pass in 0..4 {
                let (output, _) = approval_frame(&mut app, &context, &catalog, size, vec![]);
                if pass == 0 {
                    continue;
                }
                let bounds = context
                    .data(|data| {
                        data.get_temp::<egui::Rect>(egui::Id::new(("temporary_detail_bounds", id)))
                    })
                    .unwrap();
                let (confirm, reject) = context
                    .data(|data| {
                        data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new((
                            "temporary_detail_buttons",
                            id,
                        )))
                    })
                    .unwrap();
                assert!(
                    egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains_rect(bounds),
                    "{locale} {bounds:?}"
                );
                assert!(bounds.contains_rect(confirm) && bounds.contains_rect(reject));
                assert!(confirm.bottom() < bounds.top() + bounds.height() / 2.0);
                crate::approval_ui::assert_painted_palette(&output, confirm, reject);
                let mut rendered = String::new();
                for shape in &output.shapes {
                    text(&shape.shape, &mut rendered);
                }
                assert!(!rendered.contains("temporary-sentinel"));
                assert_eq!(app.operations[0].result.status, "pending");
                assert!(app.running.is_none());
            }
        }
    }

    #[test]
    #[cfg(windows)]
    fn local_expired_overview_and_escape_transitions_preserve_the_next_run() {
        let mut app = app();
        let first = app
            .handle(Request::Execute(execution()))
            .operation_id
            .unwrap();
        let second = app
            .handle(Request::Execute(execution()))
            .operation_id
            .unwrap();
        let context = egui::Context::default();
        crate::ui::configure_fonts(&context, "en");
        let catalog = Catalog::for_locale(Some("en"));
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
        warm_approval(&mut app, &context, &catalog);
        let (old_confirm, _) = context
            .data(|data| {
                data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new((
                    "temporary_approval_buttons",
                    first,
                )))
            })
            .unwrap();
        app.operations[0].expires = Instant::now();
        approval_click(&mut app, &context, &catalog, old_confirm.center());
        assert_eq!(app.operations[0].result.status, "expired");
        assert_eq!(app.operations[1].result.status, "pending");
        assert!(app.running.is_none());
        warm_approval(&mut app, &context, &catalog);
        app.detail = Some(second);
        warm_approval(&mut app, &context, &catalog);
        approval_frame(
            &mut app,
            &context,
            &catalog,
            egui::vec2(820.0, 620.0),
            escape(false),
        );
        assert!(app.detail.is_none());
        approval_frame(
            &mut app,
            &context,
            &catalog,
            egui::vec2(820.0, 620.0),
            escape(false),
        );
        assert_eq!(app.operations[1].result.status, "pending");
        warm_approval(&mut app, &context, &catalog);
        approval_frame(
            &mut app,
            &context,
            &catalog,
            egui::vec2(820.0, 620.0),
            escape(true),
        );
        assert_eq!(app.operations[1].result.status, "pending");
        assert!(app.running.is_none());
        let third = app
            .handle(Request::Execute(execution()))
            .operation_id
            .unwrap();
        app.detail = Some(second);
        warm_approval(&mut app, &context, &catalog);
        app.operations[1].expires = Instant::now();
        approval_frame(
            &mut app,
            &context,
            &catalog,
            egui::vec2(820.0, 620.0),
            escape(false),
        );
        assert!(app.detail.is_none());
        assert_eq!(app.operations[1].result.status, "expired");
        assert_eq!(
            app.operations
                .iter()
                .find(|o| o.result.id == third)
                .unwrap()
                .result
                .status,
            "pending"
        );
    }

    #[test]
    #[cfg(windows)]
    fn field_approval_checkbox_changes_only_its_row_and_revokes_waiting_calls() {
        let mut app = app();
        app.vault.request(&["other-field".into()]).unwrap();
        app.visible = true;
        let context = egui::Context::default();
        crate::ui::configure_fonts(&context, "zh-CN");
        let catalog = Catalog::for_locale(Some("zh-CN"));
        for _ in 0..4 {
            approval_frame(
                &mut app,
                &context,
                &catalog,
                egui::vec2(820.0, 620.0),
                vec![],
            );
        }
        let rect = context
            .data(|d| {
                d.get_temp::<egui::Rect>(egui::Id::new((
                    "temporary_layout_approval",
                    "project-local-qwen-apikey",
                )))
            })
            .unwrap();
        let standard = context.run_ui(egui::RawInput::default(), |ui| {
            let response = ui.checkbox(&mut true, "");
            context.data_mut(|d| d.insert_temp(egui::Id::new("standard_checkbox"), response.rect));
        });
        assert!(!standard.shapes.is_empty());
        let expected = context
            .data(|d| d.get_temp::<egui::Rect>(egui::Id::new("standard_checkbox")))
            .unwrap();
        assert_eq!(rect.size(), expected.size());
        for _ in 0..3 {
            approval_frame(
                &mut app,
                &context,
                &catalog,
                egui::vec2(820.0, 620.0),
                vec![],
            );
        }
        approval_click(&mut app, &context, &catalog, rect.center());
        assert!(!app.vault.fields[0].requires_approval);
        assert!(app.vault.fields[1].requires_approval);
        // Model a run queued behind an existing worker; the checkbox must revoke it.
        app.operations.push_back(Operation {
            execution: execution(),
            expires: Instant::now() + APPROVAL_LIFETIME,
            result: OperationStatus {
                id: Uuid::new_v4(),
                status: "queued".into(),
                exit_code: None,
            },
        });
        approval_click(&mut app, &context, &catalog, rect.center());
        assert!(app.vault.fields[0].requires_approval);
        assert!(app.vault.fields[1].requires_approval);
        assert_eq!(app.operations[0].result.status, "cancelled");
    }

    #[test]
    fn delete_all_removes_fields_and_drafts_revokes_waiters_but_keeps_running() {
        let mut app = app();
        let name = app.vault.fields[0].name.clone();
        app.vault.fields[0]
            .draft
            .push_str("unsaved-delete-sentinel");
        app.new_name = "unfinished-field".into();
        let (sender, result) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let running_id = Uuid::new_v4();
        app.running = Some(Running {
            id: running_id,
            cancel: cancel.clone(),
            result,
            worker: None,
        });
        for (id, status) in [
            (running_id, "running"),
            (Uuid::new_v4(), "queued"),
            (Uuid::new_v4(), "pending"),
        ] {
            app.operations.push_back(Operation {
                execution: execution(),
                expires: Instant::now() + APPROVAL_LIFETIME,
                result: OperationStatus {
                    id,
                    status: status.into(),
                    exit_code: None,
                },
            });
        }
        app.delete_all_fields();
        assert!(app.vault.fields.is_empty() && app.new_name.is_empty());
        assert_eq!(app.operations[0].result.status, "running");
        assert!(
            app.operations
                .iter()
                .skip(1)
                .all(|op| op.result.status == "cancelled")
        );
        assert_eq!(app.running.as_ref().unwrap().id, running_id);
        assert!(!cancel.load(Ordering::Relaxed));
        assert!(app.response().fields.is_empty());
        app.handle(Request::Declare { fields: vec![name] });
        assert!(app.vault.fields[0].requires_approval);
        assert!(app.vault.fields[0].value.is_none() && app.vault.fields[0].draft.is_empty());
        assert_eq!(
            app.handle(Request::Execute(execution())).code.as_deref(),
            Some("SECRET_MISSING")
        );
        drop(sender);
    }

    #[test]
    fn delete_all_requires_confirmation_and_cancel_preserves_values_in_every_locale() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            let mut app = app();
            app.visible = true;
            let context = egui::Context::default();
            crate::ui::configure_fonts(&context, locale);
            let catalog = Catalog::for_locale(Some(locale));
            for confirm in [false, true] {
                warm_approval(&mut app, &context, &catalog);
                let (button, enabled) = context
                    .data(|d| {
                        d.get_temp::<(egui::Rect, bool)>(egui::Id::new(
                            "temporary_delete_all_button",
                        ))
                    })
                    .unwrap();
                assert!(enabled);
                approval_click(&mut app, &context, &catalog, button.center());
                assert!(app.delete_all_prompt.is_some());
                // A late request must not replace the deletion confirmation with
                // an approval dialog or start a process.
                if confirm {
                    app.handle(Request::Execute(execution()));
                }
                for _ in 0..4 {
                    approval_frame(
                        &mut app,
                        &context,
                        &catalog,
                        egui::vec2(820.0, 620.0),
                        vec![],
                    );
                }
                assert_eq!(app.vault.fields.len(), 1);
                let buttons = context
                    .data(|d| {
                        d.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new(
                            "temporary_delete_buttons",
                        ))
                    })
                    .unwrap();
                approval_click(
                    &mut app,
                    &context,
                    &catalog,
                    if confirm { buttons.0 } else { buttons.1 }.center(),
                );
                assert!(app.delete_all_prompt.is_none());
                if confirm {
                    assert!(app.vault.fields.is_empty());
                    assert_eq!(app.operations[0].result.status, "cancelled");
                    assert!(app.running.is_none());
                } else {
                    assert_eq!(
                        app.vault.fields[0].value.as_ref().unwrap().as_str(),
                        "temporary-sentinel"
                    );
                }
            }
            warm_approval(&mut app, &context, &catalog);
            let (button, enabled) = context
                .data(|d| {
                    d.get_temp::<(egui::Rect, bool)>(egui::Id::new("temporary_delete_all_button"))
                })
                .unwrap();
            assert!(!enabled);
            approval_click(&mut app, &context, &catalog, button.center());
            assert!(app.delete_all_prompt.is_none());
        }
    }

    #[test]
    fn changed_field_list_invalidates_delete_confirmation_and_mcp_clear_keeps_fields() {
        let mut app = app();
        app.delete_all_prompt = Some(app.vault.fields.iter().map(|f| f.name.clone()).collect());
        app.handle(Request::Declare {
            fields: vec!["new-arrival".into()],
        });
        let context = egui::Context::default();
        let _ = context.run_ui(egui::RawInput::default(), |_| app.show_delete_all(&context));
        assert!(app.delete_all_prompt.is_none());
        assert_eq!(app.notice, Some("temp_fields_changed"));
        assert_eq!(app.vault.fields.len(), 2);
        let response = app.handle(Request::Clear { fields: vec![] });
        assert_eq!(response.fields.len(), 2);
        assert!(response.fields.iter().all(|f| !f.ready));
    }

    #[test]
    #[cfg(windows)]
    fn nested_approval_fits_small_editors_and_keeps_buttons_visible() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            for size in [egui::vec2(774.0, 584.0), egui::vec2(1040.0, 760.0)] {
                for field_count in [1, MAX_FIELDS] {
                    let mut app = app();
                    if field_count > 1 {
                        app.vault
                            .request(
                                &(1..field_count)
                                    .map(|i| format!("project-local-token-{i}"))
                                    .collect::<Vec<_>>(),
                            )
                            .unwrap();
                    }
                    let mut execution = execution();
                    execution.args = vec!["long-argument".repeat(300); 6];
                    let id = app
                        .handle(Request::Execute(execution))
                        .operation_id
                        .unwrap();
                    let context = egui::Context::default();
                    crate::ui::configure_fonts(&context, locale);
                    let catalog = Catalog::for_locale(Some(locale));
                    for frame in 0..4 {
                        let (output, _) =
                            approval_frame(&mut app, &context, &catalog, size, vec![]);
                        if frame == 0 {
                            continue; // New areas first perform an invisible sizing pass.
                        }
                        let (parent, dialog) = context
                            .data(|data| {
                                data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new((
                                    "temporary_approval_bounds",
                                    id,
                                )))
                            })
                            .unwrap();
                        let (approve, reject) = context
                            .data(|data| {
                                data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new((
                                    "temporary_approval_buttons",
                                    id,
                                )))
                            })
                            .unwrap();
                        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
                        assert!(
                            screen.contains_rect(parent),
                            "{locale} frame={frame} fields={field_count}: {parent:?}, input={:?}, toggle={:?}",
                            context.data(|data| data.get_temp::<egui::Rect>(egui::Id::new((
                                "temporary_layout_input",
                                "project-local-qwen-apikey"
                            )))),
                            context.data(|data| data.get_temp::<egui::Rect>(egui::Id::new((
                                "temporary_layout_approval",
                                "project-local-qwen-apikey"
                            ))))
                        );
                        assert!(
                            parent.contains_rect(dialog),
                            "{locale}: {parent:?}, {dialog:?}"
                        );
                        assert!(dialog.contains_rect(approve) && dialog.contains_rect(reject));
                        if frame == 3 {
                            crate::approval_ui::assert_painted_palette(&output, approve, reject);
                        }
                        assert!(
                            parent.center().distance(dialog.center()) < 2.0,
                            "{locale} frame={frame} fields={field_count}: {parent:?}, {dialog:?}"
                        );
                        assert!(app.running.is_none());
                        assert_eq!(app.operations[0].result.status, "pending");
                        assert_eq!(output.viewport_output.len(), 1);
                        fn check_text(
                            shape: &egui::Shape,
                            clip: egui::Rect,
                            catalog: &Catalog,
                            seen: &mut [bool; 3],
                        ) {
                            match shape {
                                egui::Shape::Text(text) => {
                                    for (index, key) in
                                        ["temp_approval_title", "temp_approve", "temp_reject"]
                                            .iter()
                                            .enumerate()
                                    {
                                        if text.galley.job.text == catalog.text(key) {
                                            let rect =
                                                text.galley.rect.translate(text.pos.to_vec2());
                                            assert!(
                                                clip.contains_rect(rect),
                                                "{key}: {rect:?}, {clip:?}"
                                            );
                                            seen[index] = true;
                                        }
                                    }
                                    assert!(!text.galley.job.text.contains("temporary-sentinel"));
                                }
                                egui::Shape::Vec(shapes) => {
                                    for shape in shapes {
                                        check_text(shape, clip, catalog, seen);
                                    }
                                }
                                _ => {}
                            }
                        }
                        let mut seen = [false; 3];
                        for shape in &output.shapes {
                            check_text(&shape.shape, shape.clip_rect, &catalog, &mut seen);
                        }
                        assert_eq!(seen, [true; 3], "{locale} frame={frame}");
                    }
                }
            }
        }
    }

    #[test]
    #[cfg(windows)]
    fn nested_approval_blocks_background_and_decides_each_queued_run_separately() {
        let mut app = app();
        let mut execution = execution();
        execution.program = std::env::temp_dir().join(format!("missing-{}.exe", Uuid::new_v4()));
        let first = app
            .handle(Request::Execute(execution.clone()))
            .operation_id
            .unwrap();
        let second = app
            .handle(Request::Execute(execution))
            .operation_id
            .unwrap();
        let context = egui::Context::default();
        crate::ui::configure_fonts(&context, "en");
        let catalog = Catalog::for_locale(Some("en"));
        for _ in 0..4 {
            approval_frame(
                &mut app,
                &context,
                &catalog,
                egui::vec2(820.0, 620.0),
                vec![],
            );
        }
        let background = context
            .data(|data| data.get_temp::<egui::Rect>(egui::Id::new("approval_test_background")))
            .unwrap();
        assert!(!approval_click(
            &mut app,
            &context,
            &catalog,
            background.center()
        ));
        let (parent, _) = context
            .data(|data| {
                data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new((
                    "temporary_approval_bounds",
                    first,
                )))
            })
            .unwrap();
        approval_click(
            &mut app,
            &context,
            &catalog,
            parent.left_bottom() + egui::vec2(4.0, -4.0),
        );
        assert!(app.visible);
        assert!(app.running.is_none());
        assert!(app.operations.iter().all(|o| o.result.status == "pending"));
        let (_, reject) = context
            .data(|data| {
                data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new((
                    "temporary_approval_buttons",
                    first,
                )))
            })
            .unwrap();
        approval_click(&mut app, &context, &catalog, reject.center());
        assert_eq!(app.operations[0].result.status, "cancelled");
        assert_eq!(app.operations[1].result.status, "pending");
        assert!(app.running.is_none());
        for _ in 0..4 {
            approval_frame(
                &mut app,
                &context,
                &catalog,
                egui::vec2(820.0, 620.0),
                vec![],
            );
        }
        let (approve, _) = context
            .data(|data| {
                data.get_temp::<(egui::Rect, egui::Rect)>(egui::Id::new((
                    "temporary_approval_buttons",
                    second,
                )))
            })
            .unwrap();
        approval_click(&mut app, &context, &catalog, approve.center());
        assert_eq!(app.running.as_ref().unwrap().id, second);
        assert_eq!(app.operations[0].result.status, "cancelled");
        assert_eq!(app.operations[1].result.status, "running");
    }
}

mod editor;
