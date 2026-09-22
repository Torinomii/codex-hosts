use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use crate::connection;
use crate::credentials::{self, CredentialKind};
use crate::fido::{self, FidoKeyInfo};
use crate::model::{
    AuthPersistence, HostFilter, HostProfile, Protocol, RemoteEnv, SshAuth, normalize_tags,
};
use crate::ssh::{self, AgentKeyInfo, OperationLimits, RemoteFailure, VerifiedHostKey};
use crate::storage::HostStore;

pub(crate) const SCHEMA_VERSION: u32 = 1;
pub(crate) const DEFAULT_BATCH_CONCURRENCY: usize = 8;
pub(crate) const MAX_BATCH_CONCURRENCY: usize = 16;
pub(crate) const MAX_BATCH_HOSTS: usize = 256;
pub(crate) const MAX_BATCH_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_SINGLE_RESULT_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_EXEC_MANY_COMMANDS: usize = 64;
const MAX_ALIAS_BYTES: usize = 256;
pub(crate) const MAX_TOOL_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum ToolRequest {
    Capabilities,
    TemporarySecretsOpen {
        fields: Vec<String>,
    },
    TemporarySecrets {
        session: uuid::Uuid,
        request: crate::temporary_secrets::Request,
    },
    AgentIdentities,
    FidoIdentities,
    ListHosts {
        #[serde(default)]
        tags: Vec<String>,
    },
    Probe {
        alias: String,
        #[serde(default)]
        connect_timeout_ms: Option<u64>,
        #[serde(default)]
        command_timeout_ms: Option<u64>,
    },
    Exec {
        alias: String,
        command: String,
        #[serde(default)]
        stdin: Option<String>,
        #[serde(default)]
        connect_timeout_ms: Option<u64>,
        #[serde(default)]
        command_timeout_ms: Option<u64>,
    },
    ExecMany {
        alias: String,
        commands: Vec<String>,
        #[serde(default)]
        stdin: Option<String>,
        #[serde(default)]
        max_concurrency: Option<usize>,
        #[serde(default)]
        connect_timeout_ms: Option<u64>,
        #[serde(default)]
        command_timeout_ms: Option<u64>,
    },
    BatchProbe {
        aliases: Vec<String>,
        #[serde(default)]
        max_concurrency: Option<usize>,
        #[serde(default)]
        connect_timeout_ms: Option<u64>,
        #[serde(default)]
        command_timeout_ms: Option<u64>,
        #[serde(default)]
        batch_timeout_ms: Option<u64>,
        #[serde(default = "default_continue_on_error")]
        continue_on_error: bool,
    },
    BatchExec {
        aliases: Vec<String>,
        command: String,
        #[serde(default)]
        stdin: Option<String>,
        #[serde(default)]
        max_concurrency: Option<usize>,
        #[serde(default)]
        connect_timeout_ms: Option<u64>,
        #[serde(default)]
        command_timeout_ms: Option<u64>,
        #[serde(default)]
        batch_timeout_ms: Option<u64>,
        #[serde(default = "default_continue_on_error")]
        continue_on_error: bool,
    },
}

fn default_continue_on_error() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub(crate) struct HostSummary {
    pub(crate) alias: String,
    pub(crate) description: String,
    pub(crate) tags: Vec<String>,
    pub(crate) address: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) protocol: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ssh_auth: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) agent_key_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) jump_host: Option<String>,
    pub(crate) verified: bool,
    pub(crate) auth_persistence: AuthPersistence,
    pub(crate) max_channels: usize,
    pub(crate) remote_env: RemoteEnv,
    pub(crate) has_required_secret: bool,
    pub(crate) has_host_fingerprint: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) host_key_algorithm: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ListResult {
    pub(crate) schema_version: u32,
    pub(crate) status: &'static str,
    pub(crate) hosts: Vec<HostSummary>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentIdentitiesResult {
    pub(crate) schema_version: u32,
    pub(crate) status: &'static str,
    pub(crate) identities: Vec<AgentKeyInfo>,
}

#[derive(Debug, Serialize)]
pub(crate) struct FidoIdentitiesResult {
    pub(crate) schema_version: u32,
    pub(crate) status: &'static str,
    pub(crate) helper_available: bool,
    pub(crate) identities: Vec<FidoKeyInfo>,
}

#[derive(Debug, Serialize)]
struct CapabilitiesResult {
    host_metadata_fields: [&'static str; 2],
    list_hosts_tag_filter: bool,
    exec_stdin: bool,
    exec_stdin_protocols: [&'static str; 1],
    max_stdin_bytes: usize,
    schema_version: u32,
    status: &'static str,
    app_version: &'static str,
    actions: [&'static str; 10],
    ssh_auth: [&'static str; 3],
    max_batch_concurrency: usize,
    max_batch_hosts: usize,
    max_batch_output_bytes: usize,
    max_timeout_ms: u64,
    default_batch_concurrency: usize,
    default_retained_connections: usize,
    max_retained_connections: usize,
    connection_idle_timeout_ms: u64,
    max_exec_many_commands: usize,
    host_key_hash: &'static str,
    agent_forwarding: bool,
}

#[derive(Debug, Serialize)]
struct BatchSummary {
    requested: usize,
    succeeded: usize,
    remote_errors: usize,
    failed: usize,
    cancelled: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct BatchResult {
    schema_version: u32,
    status: &'static str,
    action: &'static str,
    duration_ms: u128,
    results: Vec<BatchItem>,
    summary: BatchSummary,
}

#[derive(Debug, Clone)]
pub(crate) enum BatchAction {
    Probe,
    Exec(String),
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum BatchItem {
    Success(crate::ssh::RemoteResult),
    Failure(RemoteFailure),
}

impl BatchItem {
    fn status(&self) -> &'static str {
        match self {
            Self::Success(result) => result.status,
            Self::Failure(error) => error.status,
        }
    }
}

struct BatchWorkResult {
    item: BatchItem,
    verified_host_keys: Vec<VerifiedHostKey>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ToolResponse {
    TemporarySecrets(crate::temporary_secrets::Response),
    Capabilities(CapabilitiesResult),
    AgentIdentities(AgentIdentitiesResult),
    FidoIdentities(FidoIdentitiesResult),
    List(ListResult),
    Remote(crate::ssh::RemoteResult),
    RemoteMany(crate::ssh::RemoteManyResult),
    Batch(BatchResult),
    Failure(RemoteFailure),
}

impl ToolResponse {
    fn is_failure(&self) -> bool {
        matches!(self, Self::Failure(_))
            || matches!(self, Self::TemporarySecrets(response) if response.status == "error")
    }
}

pub fn run(request_path: &Path, result_path: &Path) -> i32 {
    let response = match execute_request(request_path) {
        Ok(value) => value,
        Err(error) => ToolResponse::Failure(error),
    };
    let failed = response.is_failure();
    let write_result = fs::File::create(result_path)
        .map(BufWriter::new)
        .map_err(|error| error.to_string())
        .and_then(|mut writer| {
            serde_json::to_writer_pretty(&mut writer, &response)
                .map_err(|error| error.to_string())?;
            writer.flush().map_err(|error| error.to_string())
        });
    match write_result {
        Ok(()) => {
            if failed {
                1
            } else {
                0
            }
        }
        Err(_) => 2,
    }
}

fn execute_request(path: &Path) -> Result<ToolResponse, RemoteFailure> {
    let bytes = fs::read(path)
        .map_err(|error| RemoteFailure::new("REQUEST_READ_FAILED", error.to_string()))?;
    let request: ToolRequest = serde_json::from_slice(&bytes)
        .map_err(|error| RemoteFailure::new("REQUEST_INVALID", error.to_string()))?;
    validate_request_input(&request)?;

    if matches!(&request, ToolRequest::Capabilities) {
        return Ok(ToolResponse::Capabilities(CapabilitiesResult {
            host_metadata_fields: ["description", "tags"],
            list_hosts_tag_filter: true,
            exec_stdin: true,
            exec_stdin_protocols: ["ssh"],
            max_stdin_bytes: ssh::MAX_STDIN_BYTES,
            schema_version: SCHEMA_VERSION,
            status: "ok",
            app_version: env!("CARGO_PKG_VERSION"),
            actions: [
                "temporary_secrets_open",
                "temporary_secrets",
                "agent_identities",
                "fido_identities",
                "list_hosts",
                "probe",
                "exec",
                "exec_many",
                "batch_probe",
                "batch_exec",
            ],
            ssh_auth: ["password", "private_key", "ssh_agent"],
            max_batch_concurrency: MAX_BATCH_CONCURRENCY,
            max_batch_hosts: MAX_BATCH_HOSTS,
            max_batch_output_bytes: MAX_BATCH_OUTPUT_BYTES,
            max_timeout_ms: MAX_TOOL_TIMEOUT_MS,
            default_batch_concurrency: DEFAULT_BATCH_CONCURRENCY,
            default_retained_connections: ssh::DEFAULT_RETAINED_CONNECTIONS,
            max_retained_connections: ssh::MAX_RETAINED_CONNECTIONS,
            connection_idle_timeout_ms: ssh::CONNECTION_IDLE_TIMEOUT.as_millis() as u64,
            max_exec_many_commands: MAX_EXEC_MANY_COMMANDS,
            host_key_hash: "sha256",
            agent_forwarding: false,
        }));
    }
    if matches!(&request, ToolRequest::AgentIdentities) {
        return Ok(ToolResponse::AgentIdentities(AgentIdentitiesResult {
            schema_version: SCHEMA_VERSION,
            status: "ok",
            identities: ssh::agent_identities()?,
        }));
    }
    if matches!(&request, ToolRequest::FidoIdentities) {
        return Ok(ToolResponse::FidoIdentities(FidoIdentitiesResult {
            schema_version: SCHEMA_VERSION,
            status: "ok",
            helper_available: fido::helper_available(),
            identities: fido::discover_handles(),
        }));
    }

    match &request {
        ToolRequest::TemporarySecretsOpen { fields } => {
            return crate::temporary_secrets::open(fields.clone())
                .map(ToolResponse::TemporarySecrets)
                .map_err(|code| {
                    RemoteFailure::new(
                        code,
                        "Temporary secret window unavailable or invalid field names.",
                    )
                });
        }
        ToolRequest::TemporarySecrets { session, request } => {
            return crate::temporary_secrets::call(*session, request.clone())
                .map(ToolResponse::TemporarySecrets)
                .map_err(|code| {
                    RemoteFailure::new(
                        code,
                        "Temporary session unavailable; open a new window if it was closed.",
                    )
                });
        }
        _ => {}
    }
    let store = HostStore::load()
        .map_err(|error| RemoteFailure::new("STORE_READ_FAILED", error.to_string()))?;

    match request {
        ToolRequest::Capabilities
        | ToolRequest::TemporarySecretsOpen { .. }
        | ToolRequest::TemporarySecrets { .. } => unreachable!(),
        ToolRequest::AgentIdentities => unreachable!(),
        ToolRequest::FidoIdentities => unreachable!(),
        ToolRequest::ListHosts { tags } => list_hosts(&store, tags).map(ToolResponse::List),
        ToolRequest::Probe {
            alias,
            connect_timeout_ms,
            command_timeout_ms,
        } => {
            let host = find_host(&store, &alias)?.clone();
            validate_profile(&host)?;
            let result = connection::probe(
                &host,
                &store.hosts,
                limits(connect_timeout_ms, command_timeout_ms, None),
            )?;
            merge_verified_host_keys(&store.hosts, &result.verified_host_keys)?;
            Ok(ToolResponse::Remote(result))
        }
        ToolRequest::Exec {
            alias,
            command,
            stdin,
            connect_timeout_ms,
            command_timeout_ms,
        } => {
            let host = find_host(&store, &alias)?;
            validate_profile(host)?;
            Ok(ToolResponse::Remote(connection::execute_with_input(
                host,
                &store.hosts,
                &command,
                stdin.as_deref(),
                limits(connect_timeout_ms, command_timeout_ms, None),
            )?))
        }
        ToolRequest::ExecMany {
            alias,
            commands,
            stdin: _,
            max_concurrency,
            connect_timeout_ms,
            command_timeout_ms,
        } => {
            validate_commands(&commands)?;
            let host = find_host(&store, &alias)?;
            validate_profile(host)?;
            let concurrency = max_concurrency
                .unwrap_or(DEFAULT_BATCH_CONCURRENCY)
                .clamp(1, MAX_BATCH_CONCURRENCY);
            let mut operation_limits = limits(connect_timeout_ms, command_timeout_ms, None);
            operation_limits.output_bytes = Some(MAX_BATCH_OUTPUT_BYTES);
            let mut result = connection::execute_many(
                host,
                &store.hosts,
                &commands,
                concurrency,
                operation_limits,
            )?;
            fit_many_result_budget(&mut result, MAX_BATCH_OUTPUT_BYTES)?;
            Ok(ToolResponse::RemoteMany(result))
        }
        ToolRequest::BatchProbe {
            aliases,
            max_concurrency,
            connect_timeout_ms,
            command_timeout_ms,
            batch_timeout_ms,
            continue_on_error,
        } => ssh::runtime()
            .block_on(execute_batch(
                &store.hosts,
                aliases,
                BatchAction::Probe,
                max_concurrency,
                connect_timeout_ms,
                command_timeout_ms,
                batch_timeout_ms,
                continue_on_error,
                false,
            ))
            .map(ToolResponse::Batch),
        ToolRequest::BatchExec {
            aliases,
            command,
            stdin: _,
            max_concurrency,
            connect_timeout_ms,
            command_timeout_ms,
            batch_timeout_ms,
            continue_on_error,
        } => ssh::runtime()
            .block_on(execute_batch(
                &store.hosts,
                aliases,
                BatchAction::Exec(command),
                max_concurrency,
                connect_timeout_ms,
                command_timeout_ms,
                batch_timeout_ms,
                continue_on_error,
                false,
            ))
            .map(ToolResponse::Batch),
    }
}

fn validate_request_input(request: &ToolRequest) -> Result<(), RemoteFailure> {
    match request {
        ToolRequest::Exec { stdin, .. } => ssh::validate_stdin(stdin.as_deref()),
        ToolRequest::ExecMany { stdin: Some(_), .. }
        | ToolRequest::BatchExec { stdin: Some(_), .. } => Err(RemoteFailure::new(
            "STDIN_UNSUPPORTED",
            "stdin is supported only by single-host SSH exec.",
        )),
        _ => Ok(()),
    }
}

pub(crate) fn list_hosts(
    store: &HostStore,
    tags: Vec<String>,
) -> Result<ListResult, RemoteFailure> {
    let filter = HostFilter {
        search: String::new(),
        tags: normalize_tags(tags),
    };
    let mut hosts = Vec::with_capacity(store.hosts.len());
    for host in store
        .hosts
        .iter()
        .filter(|host| !host.advanced.codex_hidden && filter.matches(host))
    {
        let has_required_secret = match (host.protocol, host.ssh_auth) {
            (Protocol::Ssh, SshAuth::PrivateKey | SshAuth::SshAgent) => true,
            _ => credentials::has(host.id, CredentialKind::Password)
                .map_err(|error| RemoteFailure::new("CREDENTIAL_READ_FAILED", error.to_string()))?,
        };
        hosts.push(HostSummary {
            alias: host.alias.clone(),
            description: host.description.clone(),
            tags: normalize_tags(&host.tags),
            address: host.address.clone(),
            port: host.port,
            username: host.username.clone(),
            protocol: host.protocol.stable_name(),
            ssh_auth: (host.protocol == Protocol::Ssh).then(|| host.ssh_auth.stable_name()),
            agent_key_fingerprint: (host.protocol == Protocol::Ssh
                && host.ssh_auth == SshAuth::SshAgent
                && !host.agent_key_fingerprint.trim().is_empty())
            .then(|| host.agent_key_fingerprint.clone()),
            jump_host: host
                .jump_host
                .and_then(|id| store.hosts.iter().find(|item| item.id == id))
                .map(|item| item.alias.clone()),
            verified: host.verified,
            auth_persistence: host.effective_auth_persistence(),
            max_channels: host.advanced.max_channels(),
            remote_env: host.advanced.remote_env,
            has_required_secret,
            has_host_fingerprint: host.host_fingerprint.is_some(),
            host_key_algorithm: host.host_key_algorithm.clone(),
        });
    }
    Ok(ListResult {
        schema_version: SCHEMA_VERSION,
        status: "ok",
        hosts,
    })
}

/// Runs every host as a task on the caller's runtime. Dropping the returned
/// future (MCP cancellation) aborts the tasks, which closes their channels.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_batch(
    hosts: &[HostProfile],
    aliases: Vec<String>,
    action: BatchAction,
    max_concurrency: Option<usize>,
    connect_timeout_ms: Option<u64>,
    command_timeout_ms: Option<u64>,
    batch_timeout_ms: Option<u64>,
    continue_on_error: bool,
    retain_sessions: bool,
) -> Result<BatchResult, RemoteFailure> {
    let snapshot = Arc::new(hosts.to_vec());
    let hosts = resolve_batch_hosts(&snapshot, &aliases)?;
    let concurrency = max_concurrency
        .unwrap_or(DEFAULT_BATCH_CONCURRENCY)
        .clamp(1, MAX_BATCH_CONCURRENCY)
        .min(hosts.len());
    let per_host_output_bytes =
        (MAX_BATCH_OUTPUT_BYTES / hosts.len()).min(MAX_SINGLE_RESULT_OUTPUT_BYTES);
    let batch_scope = uuid::Uuid::new_v4();
    // Clears the scope's pool bookkeeping even when the batch is cancelled.
    struct ScopeGuard(uuid::Uuid);
    impl Drop for ScopeGuard {
        fn drop(&mut self) {
            ssh::finish_batch_scope(self.0);
        }
    }
    let _scope_guard = ScopeGuard(batch_scope);
    let started_at = Instant::now();
    let deadline = batch_timeout_ms.map(|millis| started_at + timeout_duration(millis));
    let queue = Arc::new(Mutex::new(
        hosts
            .into_iter()
            .enumerate()
            .collect::<VecDeque<(usize, HostProfile)>>(),
    ));
    let results = Arc::new(Mutex::new(
        (0..aliases.len())
            .map(|_| None)
            .collect::<Vec<Option<BatchWorkResult>>>(),
    ));
    let stop = Arc::new(AtomicBool::new(false));
    let action = Arc::new(action);

    let mut workers = JoinSet::new();
    for _ in 0..concurrency {
        let queue = Arc::clone(&queue);
        let results = Arc::clone(&results);
        let stop = Arc::clone(&stop);
        let snapshot = Arc::clone(&snapshot);
        let action = Arc::clone(&action);
        workers.spawn(async move {
            loop {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                let Some((index, host)) = queue.lock().ok().and_then(|mut queue| queue.pop_front())
                else {
                    break;
                };
                let total_timeout =
                    deadline.map(|value| value.saturating_duration_since(Instant::now()));
                let outcome = if total_timeout == Some(Duration::ZERO) {
                    Err(failure_for_alias(
                        &host.alias,
                        "BATCH_TIMEOUT",
                        "The whole-batch deadline expired before this host started.",
                    ))
                } else {
                    let mut operation_limits =
                        limits(connect_timeout_ms, command_timeout_ms, total_timeout);
                    operation_limits.output_bytes = Some(per_host_output_bytes);
                    operation_limits.batch_scope = Some(batch_scope);
                    operation_limits.retain_sessions = retain_sessions;
                    match action.as_ref() {
                        BatchAction::Probe => {
                            connection::probe_async(&host, snapshot.as_slice(), operation_limits)
                                .await
                        }
                        BatchAction::Exec(command) => {
                            connection::execute_with_input_async(
                                &host,
                                snapshot.as_slice(),
                                command,
                                None,
                                operation_limits,
                            )
                            .await
                        }
                    }
                };
                let work_result = match outcome {
                    Ok(mut result) => {
                        limit_result_output(&mut result, per_host_output_bytes);
                        BatchWorkResult {
                            verified_host_keys: result.verified_host_keys.clone(),
                            item: BatchItem::Success(result),
                        }
                    }
                    Err(mut error) => {
                        if error.host_alias.is_none() {
                            error.host_alias = Some(host.alias.clone().into_boxed_str());
                        }
                        if !continue_on_error {
                            stop.store(true, Ordering::Release);
                        }
                        BatchWorkResult {
                            item: BatchItem::Failure(error),
                            verified_host_keys: Vec::new(),
                        }
                    }
                };
                if let Ok(mut slots) = results.lock() {
                    slots[index] = Some(work_result);
                }
            }
        });
    }
    while let Some(joined) = workers.join_next().await {
        if let Err(error) = joined {
            return Err(RemoteFailure::new(
                "BATCH_INTERNAL_FAILED",
                error.to_string(),
            ));
        }
    }

    let mut verified_host_keys = Vec::new();
    let mut values = Vec::with_capacity(aliases.len());
    let mut summary = BatchSummary {
        requested: aliases.len(),
        succeeded: 0,
        remote_errors: 0,
        failed: 0,
        cancelled: 0,
    };
    let mut slots = Arc::try_unwrap(results)
        .map_err(|_| {
            RemoteFailure::new("BATCH_INTERNAL_FAILED", "Batch results are still shared.")
        })?
        .into_inner()
        .map_err(|_| RemoteFailure::new("BATCH_INTERNAL_FAILED", "Batch result lock failed."))?;
    for (index, slot) in slots.iter_mut().enumerate() {
        let Some(work_result) = slot.take() else {
            summary.cancelled += 1;
            values.push(BatchItem::Failure(failure_for_alias(
                &aliases[index],
                "BATCH_CANCELLED",
                "The batch stopped before this host started.",
            )));
            continue;
        };
        match work_result.item.status() {
            "ok" => summary.succeeded += 1,
            "remote_error" => summary.remote_errors += 1,
            _ => summary.failed += 1,
        }
        verified_host_keys.extend(work_result.verified_host_keys);
        values.push(work_result.item);
    }
    if matches!(action.as_ref(), BatchAction::Probe) && !verified_host_keys.is_empty() {
        merge_verified_host_keys(snapshot.as_slice(), &verified_host_keys)?;
    }
    let status = if summary.failed == 0 && summary.cancelled == 0 {
        if summary.remote_errors == 0 {
            "ok"
        } else {
            "completed_with_remote_errors"
        }
    } else {
        "completed_with_errors"
    };
    let mut result = BatchResult {
        schema_version: SCHEMA_VERSION,
        status,
        action: match action.as_ref() {
            BatchAction::Probe => "batch_probe",
            BatchAction::Exec(_) => "batch_exec",
        },
        duration_ms: started_at.elapsed().as_millis(),
        results: values,
        summary,
    };
    fit_batch_result_budget(&mut result, MAX_BATCH_OUTPUT_BYTES)?;
    Ok(result)
}

fn limit_result_output(result: &mut crate::ssh::RemoteResult, limit: usize) {
    let original_bytes = result.stdout.len().saturating_add(result.stderr.len());
    let stdout_limit = limit.min(result.stdout.len());
    truncate_utf8(&mut result.stdout, stdout_limit);
    let stderr_limit = limit.saturating_sub(result.stdout.len());
    truncate_utf8(&mut result.stderr, stderr_limit);
    result.output_truncated |=
        original_bytes > result.stdout.len().saturating_add(result.stderr.len());
}

#[derive(Default)]
struct CountingWriter {
    bytes: usize,
}

impl Write for CountingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialized_pretty_len(value: &impl Serialize) -> Result<usize, RemoteFailure> {
    let mut writer = CountingWriter::default();
    serde_json::to_writer_pretty(&mut writer, value)
        .map_err(|error| RemoteFailure::new("SERIALIZE_FAILED", error.to_string()))?;
    Ok(writer.bytes)
}

fn fit_batch_result_budget(result: &mut BatchResult, limit: usize) -> Result<(), RemoteFailure> {
    loop {
        let serialized = serialized_pretty_len(result)?;
        if serialized <= limit {
            return Ok(());
        }
        if !trim_largest_batch_output(result, serialized - limit) {
            return Err(RemoteFailure::new(
                "BATCH_RESULT_TOO_LARGE",
                "Batch metadata exceeded the final serialized-output budget.",
            ));
        }
    }
}

fn trim_largest_batch_output(result: &mut BatchResult, requested: usize) -> bool {
    let mut largest = None;
    for (index, item) in result.results.iter().enumerate() {
        let BatchItem::Success(item) = item else {
            continue;
        };
        for (stderr, value) in [(false, &item.stdout), (true, &item.stderr)] {
            if largest
                .as_ref()
                .is_none_or(|(_, _, length)| value.len() > *length)
            {
                largest = Some((index, stderr, value.len()));
            }
        }
    }
    let Some((index, stderr, length)) = largest.filter(|(_, _, length)| *length > 0) else {
        return false;
    };
    let BatchItem::Success(item) = &mut result.results[index] else {
        unreachable!();
    };
    let value = if stderr {
        &mut item.stderr
    } else {
        &mut item.stdout
    };
    truncate_utf8(value, length.saturating_sub(requested.min(length)));
    item.output_truncated = true;
    true
}

pub(crate) fn fit_many_result_budget(
    result: &mut crate::ssh::RemoteManyResult,
    limit: usize,
) -> Result<(), RemoteFailure> {
    loop {
        let serialized = serialized_pretty_len(result)?;
        if serialized <= limit {
            return Ok(());
        }
        if !trim_largest_many_output(result, serialized - limit) {
            return Err(RemoteFailure::new(
                "EXEC_MANY_RESULT_TOO_LARGE",
                "Multi-command metadata exceeded the final serialized-output budget.",
            ));
        }
    }
}

fn trim_largest_many_output(result: &mut crate::ssh::RemoteManyResult, requested: usize) -> bool {
    let mut largest = None;
    for (index, item) in result.results.iter().enumerate() {
        for (stderr, value) in [(false, &item.stdout), (true, &item.stderr)] {
            if largest
                .as_ref()
                .is_none_or(|(_, _, length)| value.len() > *length)
            {
                largest = Some((index, stderr, value.len()));
            }
        }
    }
    let Some((index, stderr, length)) = largest.filter(|(_, _, length)| *length > 0) else {
        return false;
    };
    let item = &mut result.results[index];
    let value = if stderr {
        &mut item.stderr
    } else {
        &mut item.stdout
    };
    truncate_utf8(value, length.saturating_sub(requested.min(length)));
    item.output_truncated = true;
    true
}

fn truncate_utf8(value: &mut String, limit: usize) {
    if value.len() <= limit {
        return;
    }
    let mut boundary = limit;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
}

fn resolve_batch_hosts(
    hosts: &[HostProfile],
    aliases: &[String],
) -> Result<Vec<HostProfile>, RemoteFailure> {
    if aliases.is_empty() {
        return Err(RemoteFailure::new(
            "BATCH_ALIASES_REQUIRED",
            "Batch operations require an explicit, non-empty alias list.",
        ));
    }
    if aliases.len() > MAX_BATCH_HOSTS {
        return Err(RemoteFailure::new(
            "BATCH_TOO_LARGE",
            format!("A batch may contain at most {MAX_BATCH_HOSTS} hosts."),
        ));
    }
    let mut seen = HashSet::new();
    let mut resolved = Vec::with_capacity(aliases.len());
    for alias in aliases {
        validate_alias(alias)?;
        let normalized = alias.trim().to_ascii_lowercase();
        if normalized.is_empty() || !seen.insert(normalized) {
            return Err(RemoteFailure::new(
                "BATCH_ALIAS_INVALID",
                format!("The batch contains an empty or duplicate alias: {alias}."),
            ));
        }
        let host = find_host_in(hosts, alias)?.clone();
        validate_profile(&host)?;
        resolved.push(host);
    }
    Ok(resolved)
}

pub(crate) fn merge_verified_host_keys(
    snapshot: &[HostProfile],
    verified_host_keys: &[VerifiedHostKey],
) -> Result<(), RemoteFailure> {
    if verified_host_keys.is_empty() {
        return Ok(());
    }
    // Concurrent calls in the MCP server each load, modify and save the store;
    // without this the store's revision check fails one of them after its
    // remote authentication already succeeded.
    static STORE_MERGE: Mutex<()> = Mutex::new(());
    let _merge = STORE_MERGE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut fresh = HostStore::load()
        .map_err(|error| RemoteFailure::new("STORE_READ_FAILED", error.to_string()))?;
    let mut changed = false;
    for verified in verified_host_keys {
        let Some(old) = snapshot.iter().find(|host| host.id == verified.host_id) else {
            continue;
        };
        let Some(current) = fresh
            .hosts
            .iter_mut()
            .find(|host| host.id == verified.host_id)
        else {
            continue;
        };
        if !current.connection_details_equal(old)
            || current.host_fingerprint.as_deref() != Some(verified.fingerprint.as_str())
        {
            continue;
        }
        current.verified = true;
        current.host_key_algorithm = Some(verified.algorithm.clone());
        current.host_key_first_seen_unix = current
            .host_key_first_seen_unix
            .or(Some(verified.verified_at_unix));
        current.host_key_last_verified_unix = Some(
            current
                .host_key_last_verified_unix
                .unwrap_or_default()
                .max(verified.verified_at_unix),
        );
        changed = true;
    }
    if changed {
        fresh
            .save()
            .map_err(|error| RemoteFailure::new("STORE_WRITE_FAILED", error.to_string()))?;
    }
    Ok(())
}

pub(crate) fn limits(
    connect_timeout_ms: Option<u64>,
    command_timeout_ms: Option<u64>,
    total_timeout: Option<Duration>,
) -> OperationLimits {
    OperationLimits {
        total_timeout,
        connect_timeout: connect_timeout_ms.map(timeout_duration),
        command_timeout: command_timeout_ms.map(timeout_duration),
        output_bytes: None,
        batch_scope: None,
        retain_sessions: false,
    }
}

fn timeout_duration(milliseconds: u64) -> Duration {
    Duration::from_millis(milliseconds.min(MAX_TOOL_TIMEOUT_MS))
}

pub(crate) fn find_host<'a>(
    store: &'a HostStore,
    alias: &str,
) -> Result<&'a HostProfile, RemoteFailure> {
    find_host_in(&store.hosts, alias)
}

pub(crate) fn find_host_in<'a>(
    hosts: &'a [HostProfile],
    alias: &str,
) -> Result<&'a HostProfile, RemoteFailure> {
    validate_alias(alias)?;
    let host = crate::model::find_alias(hosts, alias).ok_or_else(|| {
        RemoteFailure::new(
            "ALIAS_NOT_FOUND",
            format!("No saved host is named {alias}."),
        )
    })?;
    if host.advanced.codex_hidden {
        return Err(RemoteFailure::new(
            "HOST_HIDDEN",
            format!("The host {alias} is hidden from Codex in its profile."),
        ));
    }
    Ok(host)
}

fn validate_alias(alias: &str) -> Result<(), RemoteFailure> {
    if alias.trim().is_empty() || alias.len() > MAX_ALIAS_BYTES {
        return Err(RemoteFailure::new(
            "ALIAS_INVALID",
            format!("A host alias must contain 1 to {MAX_ALIAS_BYTES} UTF-8 bytes."),
        ));
    }
    Ok(())
}

pub(crate) fn validate_commands(commands: &[String]) -> Result<(), RemoteFailure> {
    if commands.is_empty() {
        return Err(RemoteFailure::new(
            "COMMANDS_REQUIRED",
            "exec_many requires a non-empty command list.",
        ));
    }
    if commands.len() > MAX_EXEC_MANY_COMMANDS {
        return Err(RemoteFailure::new(
            "TOO_MANY_COMMANDS",
            format!("exec_many accepts at most {MAX_EXEC_MANY_COMMANDS} commands."),
        ));
    }
    if commands.iter().any(|command| command.is_empty()) {
        return Err(RemoteFailure::new(
            "COMMAND_INVALID",
            "exec_many does not accept empty commands.",
        ));
    }
    Ok(())
}

pub(crate) fn validate_profile(profile: &HostProfile) -> Result<(), RemoteFailure> {
    if let Some(issue) = profile.validation_issue() {
        return Err(RemoteFailure::new(
            "PROFILE_INVALID",
            format!("The saved host is invalid: {issue:?}"),
        ));
    }
    Ok(())
}

fn failure_for_alias(alias: &str, code: &'static str, message: impl Into<String>) -> RemoteFailure {
    let mut failure = RemoteFailure::new(code, message);
    failure.host_alias = Some(alias.to_owned().into_boxed_str());
    failure
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_hosts_are_invisible_and_unusable_for_codex() {
        let mut store = HostStore::default();
        let mut hidden = HostProfile {
            alias: "secret".into(),
            address: "10.0.0.1".into(),
            username: "u".into(),
            ..Default::default()
        };
        hidden.advanced.codex_hidden = true;
        store.hosts.push(hidden);
        store.hosts.push(HostProfile {
            alias: "open".into(),
            address: "10.0.0.2".into(),
            username: "u".into(),
            ..Default::default()
        });
        let listed = list_hosts(&store, vec![]).unwrap();
        assert_eq!(listed.hosts.len(), 1);
        assert_eq!(listed.hosts[0].alias, "open");
        assert_eq!(listed.hosts[0].max_channels, 8);
        assert_eq!(find_host(&store, "secret").unwrap_err().code, "HOST_HIDDEN");
        assert_eq!(
            resolve_batch_hosts(&store.hosts, &["open".into(), "SECRET".into()])
                .unwrap_err()
                .code,
            "HOST_HIDDEN"
        );
    }

    #[test]
    fn discovery_filters_metadata_without_touching_excluded_credentials() {
        let mut store = HostStore::default();
        store.hosts.push(HostProfile {
            alias: "include".into(),
            description: "notes".into(),
            tags: vec!["Prod".into(), "web".into()],
            ssh_auth: SshAuth::SshAgent,
            ..Default::default()
        });
        store.hosts.push(HostProfile {
            alias: "exclude".into(),
            ..Default::default()
        });
        let result = list_hosts(&store, vec![" prod ".into(), "WEB".into()]).unwrap();
        assert_eq!(result.hosts.len(), 1);
        assert_eq!(result.hosts[0].description, "notes");
        assert_eq!(result.hosts[0].tags, ["Prod", "web"]);
        let request: ToolRequest = serde_json::from_str(r#"{"action":"list_hosts"}"#).unwrap();
        assert!(matches!(request, ToolRequest::ListHosts { tags } if tags.is_empty()));
    }

    #[test]
    fn optional_stdin_contract_rejects_wrong_types_and_oversized_utf8() {
        for suffix in ["", ",\"stdin\":null", ",\"stdin\":\"\""] {
            let request: ToolRequest = serde_json::from_str(&format!(
                "{{\"action\":\"exec\",\"alias\":\"a\",\"command\":\"cat\"{suffix}}}"
            ))
            .unwrap();
            assert!(validate_request_input(&request).is_ok());
        }
        assert!(
            serde_json::from_str::<ToolRequest>(
                r#"{"action":"exec","alias":"a","command":"cat","stdin":123}"#
            )
            .is_err()
        );
        let input = "中".repeat(ssh::MAX_STDIN_BYTES / 3 + 1);
        let request: ToolRequest = serde_json::from_value(
            serde_json::json!({"action":"exec","alias":"a","command":"cat","stdin":input}),
        )
        .unwrap();
        assert_eq!(
            validate_request_input(&request).unwrap_err().code,
            "STDIN_TOO_LARGE"
        );
        for action in ["exec_many", "batch_exec"] {
            let request: ToolRequest = serde_json::from_value(serde_json::json!({"action":action,"alias":"a","aliases":["a"],"command":"cat","commands":["cat"],"stdin":"data"})).unwrap();
            assert_eq!(
                validate_request_input(&request).unwrap_err().code,
                "STDIN_UNSUPPORTED"
            );
        }
    }

    #[test]
    fn rejects_implicit_all_host_batch() {
        let store = HostStore::default();
        let error = resolve_batch_hosts(&store.hosts, &[]).unwrap_err();
        assert_eq!(error.code, "BATCH_ALIASES_REQUIRED");
    }

    #[test]
    fn rejects_duplicate_batch_aliases_before_connecting() {
        let mut store = HostStore::default();
        let mut host = HostProfile::new("web-1".to_owned());
        host.address = "127.0.0.1".to_owned();
        host.username = "tester".to_owned();
        store.hosts.push(host);
        let aliases = vec!["web-1".to_owned(), "WEB-1".to_owned()];
        let error = resolve_batch_hosts(&store.hosts, &aliases).unwrap_err();
        assert_eq!(error.code, "BATCH_ALIAS_INVALID");
    }

    #[test]
    fn batch_output_budget_preserves_utf8_and_reports_truncation() {
        let mut result = crate::ssh::RemoteResult {
            status: "ok",
            alias: "host-1".to_owned(),
            exit_code: 0,
            stdout: "测试-output".repeat(4),
            stderr: "error".repeat(4),
            output_truncated: false,
            host_fingerprint: None,
            host_key_algorithm: None,
            auth_key_fingerprint: None,
            verified_host_keys: Vec::new(),
        };
        limit_result_output(&mut result, 17);
        assert!(result.output_truncated);
        assert!(result.stdout.len() + result.stderr.len() <= 17);
        assert!(std::str::from_utf8(result.stdout.as_bytes()).is_ok());
        assert!(std::str::from_utf8(result.stderr.as_bytes()).is_ok());
    }

    #[test]
    fn batch_defaults_allow_eight_workers_and_cap_at_sixteen() {
        assert_eq!(DEFAULT_BATCH_CONCURRENCY, 8);
        assert_eq!(MAX_BATCH_CONCURRENCY, 16);
        assert_eq!(MAX_BATCH_HOSTS, 256);
    }

    #[test]
    fn final_batch_json_including_escaping_stays_inside_budget() {
        let make_result = |alias: &str| crate::ssh::RemoteResult {
            status: "ok",
            alias: alias.to_owned(),
            exit_code: 0,
            stdout: "\\\"\n".repeat(2048),
            stderr: "测试".repeat(1024),
            output_truncated: false,
            host_fingerprint: Some("SHA256:example".to_owned()),
            host_key_algorithm: Some("ssh-ed25519".to_owned()),
            auth_key_fingerprint: None,
            verified_host_keys: Vec::new(),
        };
        let mut result = BatchResult {
            schema_version: SCHEMA_VERSION,
            status: "ok",
            action: "batch_exec",
            duration_ms: 1,
            results: vec![
                BatchItem::Success(make_result("one")),
                BatchItem::Success(make_result("two")),
            ],
            summary: BatchSummary {
                requested: 2,
                succeeded: 2,
                remote_errors: 0,
                failed: 0,
                cancelled: 0,
            },
        };
        fit_batch_result_budget(&mut result, 4096).unwrap();
        assert!(serialized_pretty_len(&result).unwrap() <= 4096);
        assert!(result.results.iter().any(|item| matches!(
            item,
            BatchItem::Success(output) if output.output_truncated
        )));
    }

    #[test]
    fn exec_many_requires_a_bounded_nonempty_command_list() {
        assert_eq!(
            validate_commands(&[]).unwrap_err().code,
            "COMMANDS_REQUIRED"
        );
        assert_eq!(
            validate_commands(&vec!["true".to_owned(); MAX_EXEC_MANY_COMMANDS + 1])
                .unwrap_err()
                .code,
            "TOO_MANY_COMMANDS"
        );
        assert!(validate_commands(&["hostname".to_owned(), "uptime".to_owned()]).is_ok());
    }

    #[test]
    fn tool_timeouts_are_capped_to_prevent_instant_overflow() {
        assert_eq!(
            timeout_duration(u64::MAX),
            Duration::from_millis(MAX_TOOL_TIMEOUT_MS)
        );
    }
}
