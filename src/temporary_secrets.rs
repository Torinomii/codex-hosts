use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::process::{Command, Stdio};
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
    session: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    fields: Vec<FieldStatus>,
    operations: Vec<OperationStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_id: Option<Uuid>,
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
        }
    }
}

// Deliberately no Serialize or Debug on secret-bearing types.
struct Field {
    name: String,
    draft: Zeroizing<String>,
    value: Option<Zeroizing<String>>,
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
    requested: Instant,
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
    notice: Option<&'static str>,
    input_status: &'static str,
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
        }
    }
    fn handle(&mut self, request: Request) -> Response {
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
                            !matches!(o.result.status.as_str(), "pending" | "running")
                        }) {
                            self.operations.remove(index);
                        } else {
                            return Response::error(self.session, "OPERATION_LIMIT");
                        }
                    }
                    let id = Uuid::new_v4();
                    self.visible = true;
                    self.restore_requested = true;
                    self.operations.push_back(Operation {
                        result: OperationStatus {
                            id,
                            status: "pending".into(),
                            exit_code: None,
                        },
                        execution,
                        requested: Instant::now(),
                    });
                    operation_id = Some(id);
                    Ok(())
                }
            },
        };
        if let Err(code) = result {
            return Response::error(self.session, code);
        }
        let mut response = self.response();
        response.operation_id = operation_id;
        response
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
        for operation in &mut self.operations {
            if operation.result.status == "pending" {
                operation.result.status = "cancelled".into();
            }
        }
    }
    fn poll(&mut self) {
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
                if let Some(operation) = self
                    .operations
                    .iter_mut()
                    .find(|o| o.result.id == result.id)
                {
                    operation.result = result;
                }
                self.running = None;
            }
        }
        for operation in &mut self.operations {
            if operation.result.status == "pending"
                && operation.requested.elapsed() > Duration::from_secs(300)
            {
                operation.result.status = "expired".into();
            }
        }
        for _ in 0..8 {
            let Ok(envelope) = self.inbox.try_recv() else {
                break;
            };
            if !envelope.reply.is_closed() {
                let response = self.handle(envelope.request);
                let _ = envelope.reply.send(response);
            }
        }
    }
    fn approve(&mut self, id: Uuid, context: &egui::Context) {
        if self.running.is_some() {
            return;
        }
        let Some(operation) = self
            .operations
            .iter_mut()
            .find(|o| o.result.id == id && o.result.status == "pending")
        else {
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
        let server = ipc::Server::start(session, sender, context)?;
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
            notice: None,
            input_status: "ok",
        })
    }
    pub(crate) fn logic(&mut self, context: &egui::Context) -> bool {
        self.poll();
        if self.running.is_some() || self.operations.iter().any(|o| o.result.status == "pending") {
            context.request_repaint_after(Duration::from_secs(1));
        }
        std::mem::take(&mut self.restore_requested)
    }
    pub(crate) fn show(&mut self, context: &egui::Context, catalog: &Catalog) {
        if !self.visible {
            return;
        }
        self.catalog = catalog.clone();
        let mut open = true;
        egui::Window::new(catalog.text("temp_title"))
            .id(egui::Id::new("temporary_secrets_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(710.0)
            .min_width(640.0)
            .frame(
                egui::Frame::window(&context.global_style()).inner_margin(egui::Margin::same(18)),
            )
            .show(context, |ui| {
                self.contents(ui);
            });
        self.visible = open;
    }
    fn contents(&mut self, ui: &mut egui::Ui) {
        let context = ui.ctx().clone();
        ui.label(
            egui::RichText::new(self.catalog.text("temp_memory_notice"))
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let name_width = (ui.available_width() - 224.0).max(220.0);
            ui.add_sized(
                [name_width, 34.0],
                egui::TextEdit::singleline(&mut self.new_name)
                    .hint_text("project-local-qwen-apikey")
                    .desired_width(name_width)
                    .char_limit(256),
            );
            if ui
                .add_sized(
                    [104.0, 34.0],
                    egui::Button::new(self.catalog.text("temp_add")),
                )
                .clicked()
            {
                match self.vault.request(std::slice::from_ref(&self.new_name)) {
                    Ok(()) => {
                        self.new_name.clear();
                        self.notice = None;
                        self.input_status = "ok";
                    }
                    Err(_) => self.notice = Some("temp_invalid_field"),
                }
            }
            if ui
                .add_sized(
                    [104.0, 34.0],
                    egui::Button::new(self.catalog.text("temp_clear_all")),
                )
                .clicked()
            {
                let _ = self.vault.clear(&[]);
                self.input_status = "ok";
                self.cancel_pending();
            }
        });
        if let Some(notice) = self.notice {
            ui.colored_label(egui::Color32::YELLOW, self.catalog.text(notice));
        }
        ui.add_space(16.0);
        egui::ScrollArea::vertical()
            .id_salt("temporary_secret_scroll")
            .auto_shrink([false, true])
            .max_height(460.0)
            .show(ui, |ui| {
                egui::Frame::group(ui.style())
                    .inner_margin(egui::Margin::same(18))
                    .show(ui, |ui| {
                        let secret_width = (ui.available_width() - 160.0 - 24.0).max(320.0);
                        // Grid cells are vertically centered by egui; explicit top-aligned rows
                        // keep short field names aligned with the first line of multiline inputs.
                        ui.spacing_mut().item_spacing = egui::vec2(24.0, 14.0);
                        ui.vertical(|ui| {
                            ui.horizontal_top(|ui| {
                                ui.add_sized(
                                    [160.0, 24.0],
                                    egui::Label::new(
                                        egui::RichText::new(self.catalog.text("temp_field"))
                                            .strong(),
                                    )
                                    .halign(egui::Align::Min),
                                );
                                ui.add_sized(
                                    [secret_width, 24.0],
                                    egui::Label::new(
                                        egui::RichText::new(self.catalog.text("temp_secret"))
                                            .strong(),
                                    )
                                    .halign(egui::Align::Min),
                                );
                            });
                            let mut input_status_change = None;
                            for field in &mut self.vault.fields {
                                ui.horizontal_top(|ui| {
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(160.0, 24.0),
                                        egui::Layout::top_down(egui::Align::Min),
                                        |ui| {
                                            ui.set_width(160.0);
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(&field.name).strong(),
                                                )
                                                .halign(egui::Align::Min)
                                                .wrap(),
                                            );
                                        },
                                    );
                                    // TextEdit's desired_width is clamped to the Grid's available width.
                                    // Reserve the entire column explicitly before measuring the multiline edit.
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(secret_width, 88.0),
                                        egui::Layout::top_down(egui::Align::Min),
                                        |ui| {
                                            ui.set_width(secret_width);
                                            let mut output =
                                                egui::TextEdit::multiline(&mut *field.draft)
                                                    .id(egui::Id::new((
                                                        "temporary_secret_input",
                                                        &field.name,
                                                    )))
                                                    .password(true)
                                                    .desired_rows(3)
                                                    .desired_width(secret_width)
                                                    .min_size(egui::vec2(secret_width, 48.0))
                                                    .char_limit(MAX_VALUE_BYTES)
                                                    .hint_text(self.catalog.text(
                                                        if field.value.is_some() {
                                                            "temp_replace_hint"
                                                        } else {
                                                            "temp_empty_hint"
                                                        },
                                                    ))
                                                    .show(ui);
                                            if output.response.changed() {
                                                input_status_change = Some("ok");
                                            }
                                            #[cfg(test)]
                                            context.data_mut(|data| {
                                                data.insert_temp(
                                                    egui::Id::new((
                                                        "temporary_layout_input",
                                                        &field.name,
                                                    )),
                                                    output.response.rect,
                                                )
                                            });
                                            output.state.clear_undoer();
                                            output.state.store(&context, output.response.id);
                                            ui.add_space(4.0);
                                            ui.spacing_mut().item_spacing = egui::vec2(8.0, 4.0);
                                            ui.horizontal(|ui| {
                                                if ui
                                                    .add_sized(
                                                        [80.0, 34.0],
                                                        egui::Button::new(
                                                            self.catalog.text("temp_save"),
                                                        ),
                                                    )
                                                    .clicked()
                                                {
                                                    match field.save() {
                                                        Ok(()) => {
                                                            self.notice = None;
                                                            input_status_change = Some("saved");
                                                        }
                                                        Err(_) => {
                                                            self.notice = Some("temp_invalid_value")
                                                        }
                                                    }
                                                }
                                                if ui
                                                    .add_sized(
                                                        [80.0, 34.0],
                                                        egui::Button::new(
                                                            self.catalog.text("temp_clear"),
                                                        ),
                                                    )
                                                    .clicked()
                                                {
                                                    field.clear();
                                                    input_status_change = Some("ok");
                                                }
                                                ui.label(
                                                    egui::RichText::new(self.catalog.text(
                                                        if field.value.is_some() {
                                                            "temp_ready"
                                                        } else {
                                                            "temp_missing"
                                                        },
                                                    ))
                                                    .color(ui.visuals().weak_text_color()),
                                                );
                                            });
                                        },
                                    );
                                });
                            }
                            if let Some(status) = input_status_change {
                                self.record_input_status(status);
                            }
                        });
                    });
                if self
                    .operations
                    .iter()
                    .any(|o| matches!(o.result.status.as_str(), "pending" | "running"))
                {
                    ui.separator();
                    ui.strong(self.catalog.text("temp_operations"));
                }
                let mut approve = None;
                for operation in self
                    .operations
                    .iter_mut()
                    .filter(|o| matches!(o.result.status.as_str(), "pending" | "running"))
                {
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
                            ui.label(format!("Exit: {code}"));
                        }
                        if operation.result.status == "pending" {
                            ui.label(format!(
                                "Program: {}",
                                operation.execution.program.display()
                            ));
                            ui.label(format!("Directory: {}", operation.execution.cwd.display()));
                            ui.label(format!("Timeout: {} ms", operation.execution.timeout_ms));
                            ui.label(format!(
                                "Args: {}",
                                serde_json::to_string(&operation.execution.args)
                                    .unwrap_or_default()
                            ));
                            for (env, field) in &operation.execution.env {
                                ui.monospace(format!("{env} ← {field}"));
                            }
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(
                                        self.running.is_none(),
                                        egui::Button::new(self.catalog.text("temp_approve")),
                                    )
                                    .clicked()
                                {
                                    approve = Some(operation.result.id);
                                }
                                if ui.button(self.catalog.text("temp_reject")).clicked() {
                                    operation.result.status = "cancelled".into();
                                }
                            });
                        }
                        if operation.result.status == "running"
                            && ui.button(self.catalog.text("temp_stop")).clicked()
                            && let Some(running) = &self.running
                        {
                            running.cancel.store(true, Ordering::Relaxed);
                        }
                    });
                }
                if let Some(id) = approve {
                    self.approve(id, &context);
                }
            });
        if self.running.is_some() || self.operations.iter().any(|o| o.result.status == "pending") {
            context.request_repaint_after(Duration::from_millis(100));
        }
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
    let request = Request::Declare { fields };
    if let Ok(response) = ipc::discover(request.clone()) {
        return Ok(response);
    }
    let exe = std::env::current_exe().map_err(|_| "TEMPORARY_WINDOW_FAILED")?;
    let mut child = Command::new(exe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "TEMPORARY_WINDOW_FAILED")?;
    let start = Instant::now();
    loop {
        if let Ok(response) = ipc::discover(request.clone()) {
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
            notice: None,
            input_status: "ok",
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
        assert!(app.running.is_none()); // Reuse never bypasses user approval.
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
        app.operations[0].requested = Instant::now() - Duration::from_secs(301);
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
            for width in [820.0, 1040.0] {
                let mut app = app();
                app.vault = Vault::default();
                app.vault
                    .request(&["0202".into(), "myproject-staging-qwen-apikey".into()])
                    .unwrap();
                app.visible = true;
                let context = egui::Context::default();
                crate::ui::configure_fonts(&context, locale);
                let catalog = Catalog::for_locale(Some(locale));
                for frame in 0..4 {
                    let input = egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 760.0),
                        )),
                        time: Some(frame as f64 * 0.1),
                        ..Default::default()
                    };
                    let output = context.run_ui(input, |ui| app.show(ui.ctx(), &catalog));
                    // Capture this pass's actual TextEdit output: read_response can return the
                    // previous pass's hit-test geometry after run_ui finishes.
                    let first = context
                        .data(|data| {
                            data.get_temp::<egui::Rect>(egui::Id::new((
                                "temporary_layout_input",
                                "0202",
                            )))
                        })
                        .unwrap();
                    let second = context
                        .data(|data| {
                            data.get_temp::<egui::Rect>(egui::Id::new((
                                "temporary_layout_input",
                                "myproject-staging-qwen-apikey",
                            )))
                        })
                        .unwrap();
                    assert!(
                        first.width() >= 320.0,
                        "{locale} width={width} frame={frame}: {first:?}"
                    );
                    assert!(
                        first.height() >= 48.0,
                        "{locale} frame={frame} rect={first:?}"
                    );
                    assert!((first.left() - second.left()).abs() < 1.0);
                    assert!((first.width() - second.width()).abs() < 1.0);
                    assert!(
                        second.top() >= first.bottom() + 34.0,
                        "row controls must not overlap the next input"
                    );
                    if frame > 0 {
                        fn label_position(shape: &egui::Shape) -> Option<egui::Pos2> {
                            match shape {
                                egui::Shape::Text(text) if text.galley.job.text == "0202" => {
                                    Some(text.pos + text.galley.rect.min.to_vec2())
                                }
                                egui::Shape::Vec(shapes) => shapes.iter().find_map(label_position),
                                _ => None,
                            }
                        }
                        let label = output
                            .shapes
                            .iter()
                            .find_map(|shape| label_position(&shape.shape))
                            .expect("field label must actually be painted");
                        assert!(
                            (first.top() - label.y).abs() <= 8.0,
                            "label must align with input top: {label:?}, {first:?}"
                        );
                        assert!(
                            (first.left() - label.x - 184.0).abs() <= 8.0,
                            "label must be left aligned in the 160px column"
                        );
                        assert!(
                            first.right() <= width,
                            "input must fit the viewport: {first:?}"
                        );
                    }
                }
            }
        }
    }
}
