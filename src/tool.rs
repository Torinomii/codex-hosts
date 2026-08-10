use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::connection;
use crate::credentials::{self, CredentialKind};
use crate::fido::{self, FidoKeyInfo};
use crate::model::{HostProfile, Protocol, SshAuth};
use crate::ssh::{self, AgentKeyInfo, OperationLimits, RemoteFailure, VerifiedHostKey};
use crate::storage::HostStore;

const SCHEMA_VERSION: u32 = 1;
const DEFAULT_BATCH_CONCURRENCY: usize = 8;
const MAX_BATCH_CONCURRENCY: usize = 16;
const MAX_BATCH_HOSTS: usize = 256;
const MAX_BATCH_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_SINGLE_RESULT_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_EXEC_MANY_COMMANDS: usize = 64;
const MAX_ALIAS_BYTES: usize = 256;
const MAX_TOOL_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum ToolRequest {
    Capabilities,
    AgentIdentities,
    FidoIdentities,
    ListHosts,
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
        connect_timeout_ms: Option<u64>,
        #[serde(default)]
        command_timeout_ms: Option<u64>,
    },
    ExecMany {
        alias: String,
        commands: Vec<String>,
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
struct HostSummary {
    alias: String,
    address: String,
    port: u16,
    username: String,
    protocol: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    ssh_auth: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_key_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    jump_host: Option<String>,
    verified: bool,
    has_required_secret: bool,
    has_host_fingerprint: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    host_key_algorithm: Option<String>,
}

#[derive(Debug, Serialize)]
struct ListResult {
    schema_version: u32,
    status: &'static str,
    hosts: Vec<HostSummary>,
}

#[derive(Debug, Serialize)]
struct AgentIdentitiesResult {
    schema_version: u32,
    status: &'static str,
    identities: Vec<AgentKeyInfo>,
}

#[derive(Debug, Serialize)]
struct FidoIdentitiesResult {
    schema_version: u32,
    status: &'static str,
    helper_available: bool,
    identities: Vec<FidoKeyInfo>,
}

#[derive(Debug, Serialize)]
struct CapabilitiesResult {
    schema_version: u32,
    status: &'static str,
    app_version: &'static str,
    actions: [&'static str; 8],
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
struct BatchResult {
    schema_version: u32,
    status: &'static str,
    action: &'static str,
    duration_ms: u128,
    results: Vec<BatchItem>,
    summary: BatchSummary,
}

#[derive(Clone, Copy)]
enum BatchAction<'a> {
    Probe,
    Exec(&'a str),
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

    if matches!(&request, ToolRequest::Capabilities) {
        return Ok(ToolResponse::Capabilities(CapabilitiesResult {
            schema_version: SCHEMA_VERSION,
            status: "ok",
            app_version: env!("CARGO_PKG_VERSION"),
            actions: [
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

    let store = HostStore::load()
        .map_err(|error| RemoteFailure::new("STORE_READ_FAILED", error.to_string()))?;

    match request {
        ToolRequest::Capabilities => unreachable!(),
        ToolRequest::AgentIdentities => unreachable!(),
        ToolRequest::FidoIdentities => unreachable!(),
        ToolRequest::ListHosts => list_hosts(&store),
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
            connect_timeout_ms,
            command_timeout_ms,
        } => {
            let host = find_host(&store, &alias)?;
            validate_profile(host)?;
            Ok(ToolResponse::Remote(connection::execute(
                host,
                &store.hosts,
                &command,
                limits(connect_timeout_ms, command_timeout_ms, None),
            )?))
        }
        ToolRequest::ExecMany {
            alias,
            commands,
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
        } => execute_batch(
            &store,
            aliases,
            BatchAction::Probe,
            max_concurrency,
            connect_timeout_ms,
            command_timeout_ms,
            batch_timeout_ms,
            continue_on_error,
        )
        .map(ToolResponse::Batch),
        ToolRequest::BatchExec {
            aliases,
            command,
            max_concurrency,
            connect_timeout_ms,
            command_timeout_ms,
            batch_timeout_ms,
            continue_on_error,
        } => execute_batch(
            &store,
            aliases,
            BatchAction::Exec(&command),
            max_concurrency,
            connect_timeout_ms,
            command_timeout_ms,
            batch_timeout_ms,
            continue_on_error,
        )
        .map(ToolResponse::Batch),
    }
}

fn list_hosts(store: &HostStore) -> Result<ToolResponse, RemoteFailure> {
    let mut hosts = Vec::with_capacity(store.hosts.len());
    for host in &store.hosts {
        let has_required_secret = match (host.protocol, host.ssh_auth) {
            (Protocol::Ssh, SshAuth::PrivateKey | SshAuth::SshAgent) => true,
            _ => credentials::has(host.id, CredentialKind::Password)
                .map_err(|error| RemoteFailure::new("CREDENTIAL_READ_FAILED", error.to_string()))?,
        };
        hosts.push(HostSummary {
            alias: host.alias.clone(),
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
            has_required_secret,
            has_host_fingerprint: host.host_fingerprint.is_some(),
            host_key_algorithm: host.host_key_algorithm.clone(),
        });
    }
    Ok(ToolResponse::List(ListResult {
        schema_version: SCHEMA_VERSION,
        status: "ok",
        hosts,
    }))
}

#[allow(clippy::too_many_arguments)]
fn execute_batch(
    store: &HostStore,
    aliases: Vec<String>,
    action: BatchAction<'_>,
    max_concurrency: Option<usize>,
    connect_timeout_ms: Option<u64>,
    command_timeout_ms: Option<u64>,
    batch_timeout_ms: Option<u64>,
    continue_on_error: bool,
) -> Result<BatchResult, RemoteFailure> {
    let hosts = resolve_batch_hosts(store, &aliases)?;
    let concurrency = max_concurrency
        .unwrap_or(DEFAULT_BATCH_CONCURRENCY)
        .clamp(1, MAX_BATCH_CONCURRENCY)
        .min(hosts.len());
    let per_host_output_bytes =
        (MAX_BATCH_OUTPUT_BYTES / hosts.len()).min(MAX_SINGLE_RESULT_OUTPUT_BYTES);
    let batch_scope = uuid::Uuid::new_v4();
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
    let snapshot = Arc::new(store.hosts.clone());

    thread::scope(|scope| {
        for _ in 0..concurrency {
            let queue = Arc::clone(&queue);
            let results = Arc::clone(&results);
            let stop = Arc::clone(&stop);
            let snapshot = Arc::clone(&snapshot);
            scope.spawn(move || {
                loop {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    let Some((index, host)) =
                        queue.lock().ok().and_then(|mut queue| queue.pop_front())
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
                        match action {
                            BatchAction::Probe => {
                                connection::probe(&host, snapshot.as_slice(), operation_limits)
                            }
                            BatchAction::Exec(command) => connection::execute(
                                &host,
                                snapshot.as_slice(),
                                command,
                                operation_limits,
                            ),
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
    });
    ssh::finish_batch_scope(batch_scope);

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
    if matches!(action, BatchAction::Probe) && !verified_host_keys.is_empty() {
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
        action: match action {
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

fn fit_many_result_budget(
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
    store: &HostStore,
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
    let mut hosts = Vec::with_capacity(aliases.len());
    for alias in aliases {
        validate_alias(alias)?;
        let normalized = alias.trim().to_ascii_lowercase();
        if normalized.is_empty() || !seen.insert(normalized) {
            return Err(RemoteFailure::new(
                "BATCH_ALIAS_INVALID",
                format!("The batch contains an empty or duplicate alias: {alias}."),
            ));
        }
        let host = find_host(store, alias)?.clone();
        validate_profile(&host)?;
        hosts.push(host);
    }
    Ok(hosts)
}

fn merge_verified_host_keys(
    snapshot: &[HostProfile],
    verified_host_keys: &[VerifiedHostKey],
) -> Result<(), RemoteFailure> {
    if verified_host_keys.is_empty() {
        return Ok(());
    }
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

fn limits(
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
    }
}

fn timeout_duration(milliseconds: u64) -> Duration {
    Duration::from_millis(milliseconds.min(MAX_TOOL_TIMEOUT_MS))
}

fn find_host<'a>(store: &'a HostStore, alias: &str) -> Result<&'a HostProfile, RemoteFailure> {
    validate_alias(alias)?;
    store.find_alias(alias).ok_or_else(|| {
        RemoteFailure::new(
            "ALIAS_NOT_FOUND",
            format!("No saved host is named {alias}."),
        )
    })
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

fn validate_commands(commands: &[String]) -> Result<(), RemoteFailure> {
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

fn validate_profile(profile: &HostProfile) -> Result<(), RemoteFailure> {
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
    fn rejects_implicit_all_host_batch() {
        let store = HostStore::default();
        let error = resolve_batch_hosts(&store, &[]).unwrap_err();
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
        let error = resolve_batch_hosts(&store, &aliases).unwrap_err();
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
