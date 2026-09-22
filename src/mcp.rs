//! MCP stdio server mode (`codex-hosts.exe --mcp`).
//!
//! Serves on the shared SSH runtime so one runtime owns both the transport and
//! the connection pool. Tool handlers must stay on the async path: calling a
//! `block_on` wrapper from here would panic inside the runtime.

mod editor;
mod secrets;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{
    Mutex as AsyncMutex, OwnedMutexGuard, OwnedRwLockReadGuard, OwnedRwLockWriteGuard,
    OwnedSemaphorePermit, RwLock, Semaphore,
};
use tokio_util::sync::CancellationToken;

use crate::connection;
use crate::fido;
use crate::model::{AuthPersistence, HostProfile, resolve_ssh_chain};
use crate::ssh::{self, OperationLimits, RemoteFailure};
use crate::storage::HostStore;
use crate::tool::{
    self, AgentIdentitiesResult, BatchAction, DEFAULT_BATCH_CONCURRENCY, FidoIdentitiesResult,
    MAX_BATCH_CONCURRENCY, MAX_BATCH_OUTPUT_BYTES, SCHEMA_VERSION,
};

/// Authentication may involve a hardware touch or PIN, so the default leaves room for it.
pub(crate) const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(120);
/// A bounded default keeps an abandoned call from holding a host forever; Codex
/// passes an explicit value for anything longer.
pub(crate) const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Upper bound on tool calls in flight; per-host locks decide who may share a session.
const MAX_CONCURRENT_CALLS: usize = 4;

pub fn run() -> i32 {
    match crate::storage::owner_check() {
        crate::storage::OwnerCheck::Ok => {}
        crate::storage::OwnerCheck::Mismatch { owner, current } => {
            eprintln!(
                "codex-hosts --mcp: refusing to start: the host store is owned by {owner}, not by the current user {current}. Fix it with: {}",
                crate::storage::owner_repair_hint()
            );
            return 2;
        }
        crate::storage::OwnerCheck::Unavailable(reason) => {
            eprintln!("codex-hosts --mcp: could not verify the host store owner: {reason}");
        }
    }
    ssh::runtime().block_on(async {
        let service = match Server::new().serve(rmcp::transport::stdio()).await {
            Ok(service) => service,
            Err(error) => {
                eprintln!("codex-hosts --mcp: initialize failed: {error}");
                return 2;
            }
        };
        match service.waiting().await {
            Ok(_) => 0,
            Err(error) => {
                eprintln!("codex-hosts --mcp: server task failed: {error}");
                2
            }
        }
    })
}

#[derive(Clone)]
struct Server {
    tool_router: ToolRouter<Self>,
    gates: Arc<CallGates>,
}

/// Serialises calls that would otherwise share one authenticated session
/// without the host having opted in: two calls on the same per-call host run
/// one after the other, batches run alone, retained hosts run concurrently.
struct CallGates {
    calls: Arc<Semaphore>,
    batch: Arc<RwLock<()>>,
    hosts: Mutex<HashMap<uuid::Uuid, Weak<AsyncMutex<()>>>>,
    /// Channels open on a retained host across concurrent calls, sized by the
    /// profile's `max_channels`.
    channels: Mutex<HashMap<uuid::Uuid, Weak<Semaphore>>>,
}

struct CallGuard {
    _permit: OwnedSemaphorePermit,
    _batch: BatchGuard,
    _hosts: Vec<OwnedMutexGuard<()>>,
    _channels: Option<OwnedSemaphorePermit>,
}

#[allow(dead_code)] // held only for its drop
enum BatchGuard {
    Shared(OwnedRwLockReadGuard<()>),
    Exclusive(OwnedRwLockWriteGuard<()>),
}

impl CallGates {
    fn new() -> Self {
        Self {
            calls: Arc::new(Semaphore::new(MAX_CONCURRENT_CALLS)),
            batch: Arc::new(RwLock::new(())),
            hosts: Mutex::new(HashMap::new()),
            channels: Mutex::new(HashMap::new()),
        }
    }

    fn channel_budget(&self, host: &HostProfile) -> Arc<Semaphore> {
        let mut budgets = self
            .channels
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(budget) = budgets.get(&host.id).and_then(Weak::upgrade) {
            return budget;
        }
        let budget = Arc::new(Semaphore::new(host.advanced.max_channels()));
        budgets.insert(host.id, Arc::downgrade(&budget));
        budget
    }

    fn host_lock(&self, host_id: uuid::Uuid) -> Arc<AsyncMutex<()>> {
        let mut locks = self
            .hosts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(lock) = locks.get(&host_id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(host_id, Arc::downgrade(&lock));
        lock
    }

    async fn single_host(
        &self,
        target: &HostProfile,
        hosts: &[HostProfile],
        channels: u32,
    ) -> CallGuard {
        let permit = self.permit().await;
        let batch = BatchGuard::Shared(Arc::clone(&self.batch).read_owned().await);
        let mut per_call = resolve_ssh_chain(target, hosts)
            .map(|chain| chain.into_iter().cloned().collect::<Vec<_>>())
            .unwrap_or_else(|_| vec![target.clone()])
            .into_iter()
            .filter(|host| host.effective_auth_persistence() == AuthPersistence::PerCall)
            .map(|host| host.id)
            .collect::<Vec<_>>();
        per_call.sort_unstable();
        per_call.dedup();
        let mut guards = Vec::with_capacity(per_call.len());
        for host_id in per_call {
            guards.push(self.host_lock(host_id).lock_owned().await);
        }
        let channels = channels.min(target.advanced.max_channels() as u32).max(1);
        let channel_permit = self
            .channel_budget(target)
            .acquire_many_owned(channels)
            .await
            .expect("channel budget is never closed");
        CallGuard {
            _permit: permit,
            _batch: batch,
            _hosts: guards,
            _channels: Some(channel_permit),
        }
    }

    async fn batch(&self) -> CallGuard {
        let permit = self.permit().await;
        let batch = BatchGuard::Exclusive(Arc::clone(&self.batch).write_owned().await);
        CallGuard {
            _permit: permit,
            _batch: batch,
            _hosts: Vec::new(),
            _channels: None,
        }
    }

    async fn permit(&self) -> OwnedSemaphorePermit {
        Arc::clone(&self.calls)
            .acquire_owned()
            .await
            .expect("call semaphore is never closed")
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListHostsParams {
    /// Return only hosts carrying every one of these tags (case-insensitive).
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ProbeParams {
    /// Saved host alias; matching ignores ASCII case and surrounding whitespace.
    alias: String,
    /// Bounds connection and authentication; leave room for a hardware touch or PIN.
    #[serde(default)]
    connect_timeout_ms: Option<u64>,
    /// Bounds the remote `hostname` command.
    #[serde(default)]
    command_timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DisconnectParams {
    /// Alias whose retained connections to drop, including chains that pass through it; omit to drop every retained connection.
    #[serde(default)]
    alias: Option<String>,
}

#[derive(Debug, Serialize)]
struct DisconnectResult {
    schema_version: u32,
    status: &'static str,
    disconnected: usize,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ExecParams {
    /// Saved host alias.
    alias: String,
    /// The exact command line to run on the remote host; sent as-is, never rewritten.
    command: String,
    /// Bounds connection and authentication; leave room for a hardware touch or PIN.
    #[serde(default)]
    connect_timeout_ms: Option<u64>,
    /// Bounds the remote command; defaults to 10 minutes.
    #[serde(default)]
    command_timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ExecStdinParams {
    /// Saved SSH host alias (Telnet has no separate input channel).
    alias: String,
    /// The exact command line to run on the remote host; sent as-is, never rewritten.
    command: String,
    /// Exact UTF-8 text written to the command's stdin (up to 1 MiB, no newline added), then EOF.
    stdin: String,
    /// Bounds connection and authentication.
    #[serde(default)]
    connect_timeout_ms: Option<u64>,
    /// Bounds the remote command; defaults to 10 minutes.
    #[serde(default)]
    command_timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ExecManyParams {
    /// Saved SSH host alias.
    alias: String,
    /// 2 to 64 independent short commands, run concurrently over one authenticated connection.
    commands: Vec<String>,
    /// Concurrent channels on the connection (1-16, default 8).
    #[serde(default)]
    max_concurrency: Option<usize>,
    /// Bounds connection and authentication.
    #[serde(default)]
    connect_timeout_ms: Option<u64>,
    /// Bounds each command; defaults to 10 minutes.
    #[serde(default)]
    command_timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BatchProbeParams {
    /// Explicit list of 1 to 256 saved host aliases; an empty list is rejected, never "all hosts".
    aliases: Vec<String>,
    /// Hosts worked on at the same time (1-16, default 8).
    #[serde(default)]
    max_concurrency: Option<usize>,
    /// Bounds each host's connection and authentication.
    #[serde(default)]
    connect_timeout_ms: Option<u64>,
    /// Bounds each host's remote `hostname`.
    #[serde(default)]
    command_timeout_ms: Option<u64>,
    /// Deadline for the whole batch; hosts that have not started by then are reported as BATCH_TIMEOUT.
    #[serde(default)]
    batch_timeout_ms: Option<u64>,
    /// Keep going after a host fails (default true); false stops starting new hosts after the first failure.
    #[serde(default = "default_true")]
    continue_on_error: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BatchExecParams {
    /// Explicit list of 1 to 256 saved host aliases; an empty list is rejected, never "all hosts".
    aliases: Vec<String>,
    /// The same command line, sent exactly as given to every listed host.
    command: String,
    /// Hosts worked on at the same time (1-16, default 8).
    #[serde(default)]
    max_concurrency: Option<usize>,
    /// Bounds each host's connection and authentication.
    #[serde(default)]
    connect_timeout_ms: Option<u64>,
    /// Bounds each host's command.
    #[serde(default)]
    command_timeout_ms: Option<u64>,
    /// Deadline for the whole batch; hosts that have not started by then are reported as BATCH_TIMEOUT.
    #[serde(default)]
    batch_timeout_ms: Option<u64>,
    /// Keep going after a host fails (default true); false stops starting new hosts after the first failure.
    #[serde(default = "default_true")]
    continue_on_error: bool,
}

fn default_true() -> bool {
    true
}

#[tool_router]
impl Server {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            gates: Arc::new(CallGates::new()),
        }
    }

    #[tool(
        name = "disconnect",
        description = "Drop authenticated connections this server is keeping for hosts whose profile retains sessions (optionally only those involving one alias). The next call to such a host authenticates again. Safe to repeat.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn disconnect(&self, Parameters(params): Parameters<DisconnectParams>) -> CallToolResult {
        finish(disconnect_hosts(params))
    }

    #[tool(
        name = "list_hosts",
        description = "List saved SSH/Telnet host profiles with their non-secret connection details, trust state, and whether the required credential is stored. Run once per task and plan from the snapshot.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_hosts(&self, Parameters(params): Parameters<ListHostsParams>) -> CallToolResult {
        finish(load_store().and_then(|store| tool::list_hosts(&store, params.tags)))
    }

    #[tool(
        name = "agent_identities",
        description = "List identities currently loaded in Windows OpenSSH Agent or Pageant.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn agent_identities(&self) -> CallToolResult {
        finish(
            ssh::agent_identities_async()
                .await
                .map(|identities| AgentIdentitiesResult {
                    schema_version: SCHEMA_VERSION,
                    status: "ok",
                    identities,
                }),
        )
    }

    #[tool(
        name = "fido_identities",
        description = "List OpenSSH FIDO handles (ECDSA-SK / Ed25519-SK) found on this machine and whether the FIDO helper is available.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn fido_identities(&self) -> CallToolResult {
        finish(Ok::<_, RemoteFailure>(FidoIdentitiesResult {
            schema_version: SCHEMA_VERSION,
            status: "ok",
            helper_available: fido::helper_available(),
            identities: fido::discover_handles(),
        }))
    }

    #[tool(
        name = "probe",
        description = "Connect to one saved host, authenticate, and run `hostname`. Use it to test a host, to surface an unknown or changed SSH host key (the failure carries observed_fingerprint and observed_algorithm for the editor), or to pre-authenticate a host. Success updates the local trust metadata (verified) for that host.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn probe(
        &self,
        Parameters(params): Parameters<ProbeParams>,
        ct: CancellationToken,
    ) -> CallToolResult {
        finish(cancellable(ct, probe_host(&self.gates, params)).await)
    }

    #[tool(
        name = "exec",
        description = "Run one command on a saved SSH or Telnet host and return its exit code and captured output (up to 1 MiB). The command is sent exactly as given. Telnet returns the login transcript with exit_code 0 even when the command failed, so judge from the output. Never retry automatically: the command may not be idempotent.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn exec(
        &self,
        Parameters(params): Parameters<ExecParams>,
        ct: CancellationToken,
    ) -> CallToolResult {
        finish(cancellable(ct, exec_host(&self.gates, params, None)).await)
    }

    #[tool(
        name = "exec_stdin",
        description = "Run one command on a saved SSH host with exact UTF-8 text on its stdin (for example `python3 -` with a script). The program text is opaque to policy review, which is why this is a separate tool. Same result shape and rules as exec.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn exec_stdin(
        &self,
        Parameters(params): Parameters<ExecStdinParams>,
        ct: CancellationToken,
    ) -> CallToolResult {
        let ExecStdinParams {
            alias,
            command,
            stdin,
            connect_timeout_ms,
            command_timeout_ms,
        } = params;
        let params = ExecParams {
            alias,
            command,
            connect_timeout_ms,
            command_timeout_ms,
        };
        finish(cancellable(ct, exec_host(&self.gates, params, Some(stdin))).await)
    }

    #[tool(
        name = "exec_many",
        description = "Run 2 to 64 independent short commands concurrently on one saved SSH host over a single authenticated connection (one hardware touch for the whole set). Results keep input order. Use it to bundle status checks instead of separate calls.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn exec_many(
        &self,
        Parameters(params): Parameters<ExecManyParams>,
        ct: CancellationToken,
    ) -> CallToolResult {
        finish(cancellable(ct, exec_many_host(&self.gates, params)).await)
    }

    #[tool(
        name = "open_host_editor",
        description = "Open the codex-hosts editor window for one alias, prefilled with every non-secret detail given, so the user can add the password, passphrase or PIN, or confirm a reported host key. Returns saved, trusted, or cancelled; a missing result is a failure, never success. Use it as soon as an alias is missing or incomplete instead of asking for connection details in chat.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn open_host_editor(
        &self,
        Parameters(params): Parameters<editor::OpenHostEditorParams>,
        ct: CancellationToken,
    ) -> CallToolResult {
        finish(cancellable(ct, editor::open_host_editor(params)).await)
    }

    #[tool(
        name = "temporary_secrets_open",
        description = "Declare memory-only secret field names (API keys, tokens) and open the app's masked editor for the user to fill them. Returns the session UUID and per-field readiness; values are never returned. Reuse fields that are already ready.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn temporary_secrets_open(
        &self,
        Parameters(params): Parameters<secrets::OpenParams>,
    ) -> CallToolResult {
        finish(secrets::open(params).await)
    }

    #[tool(
        name = "temporary_secrets_status",
        description = "Report readiness of the declared secret fields and the last operations in a temporary-secrets session.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn temporary_secrets_status(
        &self,
        Parameters(params): Parameters<secrets::StatusParams>,
    ) -> CallToolResult {
        finish(secrets::status(params).await)
    }

    #[tool(
        name = "temporary_secrets_run",
        description = "Start a trusted local program with secret fields injected into its environment by name, after the user approves the exact program, arguments, directory and mappings in a popup. Returns an operation_id to poll with temporary_secrets_status; no child output is returned. This executes code and is reviewed like exec.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn temporary_secrets_run(
        &self,
        Parameters(params): Parameters<secrets::RunParams>,
    ) -> CallToolResult {
        finish(secrets::run(params).await)
    }

    #[tool(
        name = "temporary_secrets_clear",
        description = "Clear named secret values from the session (an empty list clears all) and cancel pending approvals.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn temporary_secrets_clear(
        &self,
        Parameters(params): Parameters<secrets::ClearParams>,
    ) -> CallToolResult {
        finish(secrets::clear(params).await)
    }

    #[tool(
        name = "batch_probe",
        description = "Connect to and authenticate against an explicit list of saved hosts, running `hostname` on each, with results in input order. Host keys are never trusted automatically; failures carry the observed fingerprint for the editor. Success updates local trust metadata.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn batch_probe(
        &self,
        Parameters(params): Parameters<BatchProbeParams>,
        ct: CancellationToken,
    ) -> CallToolResult {
        finish(
            cancellable(ct, async move {
                let _guard = self.gates.batch().await;
                let hosts = load_store()?.hosts;
                tool::execute_batch(
                    &hosts,
                    params.aliases,
                    BatchAction::Probe,
                    params.max_concurrency,
                    Some(connect_timeout_or_default(params.connect_timeout_ms)),
                    Some(command_timeout_or_default(params.command_timeout_ms)),
                    params.batch_timeout_ms,
                    params.continue_on_error,
                    true,
                )
                .await
            })
            .await,
        )
    }

    #[tool(
        name = "batch_exec",
        description = "Run the same command on an explicit list of saved hosts (fan-out), with results in input order and a per-batch output budget. Partition heterogeneous hosts by shell before using it. Cancelling stops the batch; commands already started may finish on their hosts.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn batch_exec(
        &self,
        Parameters(params): Parameters<BatchExecParams>,
        ct: CancellationToken,
    ) -> CallToolResult {
        finish(
            cancellable(ct, async move {
                let _guard = self.gates.batch().await;
                let hosts = load_store()?.hosts;
                tool::execute_batch(
                    &hosts,
                    params.aliases,
                    BatchAction::Exec(params.command),
                    params.max_concurrency,
                    Some(connect_timeout_or_default(params.connect_timeout_ms)),
                    Some(command_timeout_or_default(params.command_timeout_ms)),
                    params.batch_timeout_ms,
                    params.continue_on_error,
                    true,
                )
                .await
            })
            .await,
        )
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("codex-hosts", env!("CARGO_PKG_VERSION"))
                    .with_title("Codex Hosts"),
            )
            .with_instructions(instructions())
    }
}

async fn probe_host(
    gates: &CallGates,
    params: ProbeParams,
) -> Result<ssh::RemoteResult, RemoteFailure> {
    let store = load_store()?;
    let host = tool::find_host(&store, &params.alias)?.clone();
    tool::validate_profile(&host)?;
    let _guard = gates.single_host(&host, &store.hosts, 1).await;
    let result = connection::probe_async(
        &host,
        &store.hosts,
        host_limits(&host, params.connect_timeout_ms, params.command_timeout_ms),
    )
    .await?;
    tool::merge_verified_host_keys(&store.hosts, &result.verified_host_keys)?;
    Ok(result)
}

async fn exec_host(
    gates: &CallGates,
    params: ExecParams,
    stdin: Option<String>,
) -> Result<ssh::RemoteResult, RemoteFailure> {
    let store = load_store()?;
    let host = tool::find_host(&store, &params.alias)?.clone();
    tool::validate_profile(&host)?;
    let _guard = gates.single_host(&host, &store.hosts, 1).await;
    connection::execute_with_input_async(
        &host,
        &store.hosts,
        &params.command,
        stdin.as_deref(),
        host_limits(&host, params.connect_timeout_ms, params.command_timeout_ms),
    )
    .await
}

async fn exec_many_host(
    gates: &CallGates,
    params: ExecManyParams,
) -> Result<ssh::RemoteManyResult, RemoteFailure> {
    tool::validate_commands(&params.commands)?;
    let store = load_store()?;
    let host = tool::find_host(&store, &params.alias)?.clone();
    tool::validate_profile(&host)?;
    let concurrency = params
        .max_concurrency
        .unwrap_or(DEFAULT_BATCH_CONCURRENCY)
        .clamp(1, MAX_BATCH_CONCURRENCY)
        .min(host.advanced.max_channels());
    let _guard = gates
        .single_host(&host, &store.hosts, concurrency as u32)
        .await;
    let mut limits = host_limits(&host, params.connect_timeout_ms, params.command_timeout_ms);
    limits.output_bytes = Some(MAX_BATCH_OUTPUT_BYTES);
    let mut result =
        connection::execute_many_async(&host, &store.hosts, &params.commands, concurrency, limits)
            .await?;
    tool::fit_many_result_budget(&mut result, MAX_BATCH_OUTPUT_BYTES)?;
    Ok(result)
}

/// Dropping the operation future on cancellation releases its channel, pooled
/// session, and hardware lock; a remote command that already started may keep
/// running, which the result says explicitly.
async fn cancellable<T, F>(ct: CancellationToken, operation: F) -> Result<T, RemoteFailure>
where
    F: Future<Output = Result<T, RemoteFailure>>,
{
    tokio::select! {
        biased;
        _ = ct.cancelled() => Err(RemoteFailure::new(
            "CANCELLED",
            "The call was cancelled before it finished; a remote command that already started may still be running.",
        )),
        outcome = operation => outcome,
    }
}

fn disconnect_hosts(params: DisconnectParams) -> Result<DisconnectResult, RemoteFailure> {
    let disconnected = match params.alias {
        Some(alias) => {
            let store = load_store()?;
            let host = tool::find_host(&store, &alias)?;
            ssh::disconnect_host(Some(host.id))
        }
        None => ssh::disconnect_all(),
    };
    Ok(DisconnectResult {
        schema_version: SCHEMA_VERSION,
        status: "ok",
        disconnected,
    })
}

fn load_store() -> Result<HostStore, RemoteFailure> {
    HostStore::load().map_err(|error| RemoteFailure::new("STORE_READ_FAILED", error.to_string()))
}

/// Explicit per-call timeouts win; otherwise the host profile's defaults, then
/// the MCP-wide defaults.
fn host_limits(
    host: &HostProfile,
    connect_timeout_ms: Option<u64>,
    command_timeout_ms: Option<u64>,
) -> OperationLimits {
    let seconds = |value: u32| u64::from(value) * 1000;
    let mut limits = tool::limits(
        Some(connect_timeout_or_default(
            connect_timeout_ms.or(host.advanced.connect_timeout_s.map(seconds)),
        )),
        Some(command_timeout_or_default(
            command_timeout_ms.or(host.advanced.command_timeout_s.map(seconds)),
        )),
        None,
    );
    limits.retain_sessions = true;
    limits
}

fn connect_timeout_or_default(connect_timeout_ms: Option<u64>) -> u64 {
    connect_timeout_ms.unwrap_or(DEFAULT_CONNECT_TIMEOUT.as_millis() as u64)
}

fn command_timeout_or_default(command_timeout_ms: Option<u64>) -> u64 {
    command_timeout_ms.unwrap_or(DEFAULT_COMMAND_TIMEOUT.as_millis() as u64)
}

/// Convert a tool outcome into the MCP result and release every connection the
/// call left behind, so the next call authenticates again (invariant: one
/// authentication per call unless the host opts into retention).
fn finish<T: Serialize>(outcome: Result<T, RemoteFailure>) -> CallToolResult {
    let result = match outcome {
        Ok(value) => tool_result(&value, false),
        Err(error) => tool_result(&error, true),
    };
    ssh::release_unused_connections();
    result
}

/// `content` carries the same JSON as `structuredContent` so clients that read
/// only text still see the complete result.
fn tool_result<T: Serialize>(value: &T, is_error: bool) -> CallToolResult {
    let json = serde_json::to_value(value).unwrap_or_else(|error| {
        serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "status": "error",
            "code": "SERIALIZE_FAILED",
            "message": error.to_string(),
        })
    });
    let text = json.to_string();
    let mut result = if is_error {
        CallToolResult::error(vec![ContentBlock::text(text)])
    } else {
        CallToolResult::success(vec![ContentBlock::text(text)])
    };
    result.structured_content = Some(json);
    result
}

fn instructions() -> String {
    format!(
        "codex-hosts gives Codex access to saved SSH/Telnet hosts without exposing credentials. \
Passwords, key passphrases, and FIDO PINs are entered only in the app's own windows and never \
appear in tool parameters or results. Results use tool-protocol schema_version {schema}: status is \
ok, remote_error (non-zero exit_code), or error with a code. \
Defaults when omitted: connect_timeout_ms {connect} ms, command_timeout_ms {command} ms; the \
maximum for any timeout is {max_timeout} ms. Limits: {max_hosts} hosts per batch, {max_many} \
commands per exec_many, batch concurrency up to {max_conc}, batch output up to {max_out} bytes. \
Every call authenticates again unless the host's profile keeps the session; a hardware key may \
need a touch on each call. Never retry a command automatically.",
        schema = SCHEMA_VERSION,
        connect = DEFAULT_CONNECT_TIMEOUT.as_millis(),
        command = DEFAULT_COMMAND_TIMEOUT.as_millis(),
        max_timeout = tool::MAX_TOOL_TIMEOUT_MS,
        max_hosts = tool::MAX_BATCH_HOSTS,
        max_many = tool::MAX_EXEC_MANY_COMMANDS,
        max_conc = tool::MAX_BATCH_CONCURRENCY,
        max_out = tool::MAX_BATCH_OUTPUT_BYTES,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_declare_risk_annotations_and_stable_names() {
        let router = Server::tool_router();
        let tools = router.list_all();
        let mut names = tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "agent_identities",
                "batch_exec",
                "batch_probe",
                "disconnect",
                "exec",
                "exec_many",
                "exec_stdin",
                "fido_identities",
                "list_hosts",
                "open_host_editor",
                "probe",
                "temporary_secrets_clear",
                "temporary_secrets_open",
                "temporary_secrets_run",
                "temporary_secrets_status"
            ]
        );
        for tool in &tools {
            let annotations = tool.annotations.as_ref().expect("annotations");
            let name = tool.name.as_ref();
            let executes =
                name.starts_with("exec") || name == "batch_exec" || name == "temporary_secrets_run";
            let connects = executes || name == "probe" || name == "batch_probe";
            let read_only = matches!(
                name,
                "agent_identities"
                    | "fido_identities"
                    | "list_hosts"
                    | "probe"
                    | "batch_probe"
                    | "temporary_secrets_status"
            );
            assert_eq!(annotations.read_only_hint, Some(read_only), "{name}");
            assert_eq!(annotations.open_world_hint, Some(connects), "{name}");
            if name == "disconnect" {
                assert_eq!(annotations.idempotent_hint, Some(true));
                assert_eq!(annotations.destructive_hint, Some(false));
            }
            if executes {
                assert_eq!(annotations.destructive_hint, Some(true), "{name}");
            }
        }
    }

    #[test]
    fn results_carry_the_same_json_as_text_and_structured_content() {
        let failure = RemoteFailure::new("ALIAS_NOT_FOUND", "missing");
        let result = tool_result(&failure, true);
        assert_eq!(result.is_error, Some(true));
        let structured = result.structured_content.clone().unwrap();
        assert_eq!(structured["code"], "ALIAS_NOT_FOUND");
        let text = result.content[0].as_text().unwrap().text.clone();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            structured
        );
    }

    #[test]
    fn omitted_timeouts_fall_back_to_host_then_global_defaults() {
        let plain = HostProfile::default();
        let defaults = host_limits(&plain, None, None);
        assert_eq!(defaults.connect_timeout, Some(DEFAULT_CONNECT_TIMEOUT));
        assert_eq!(defaults.command_timeout, Some(DEFAULT_COMMAND_TIMEOUT));
        let explicit = host_limits(&plain, Some(5000), Some(1000));
        assert_eq!(explicit.connect_timeout, Some(Duration::from_secs(5)));
        assert_eq!(explicit.command_timeout, Some(Duration::from_secs(1)));
        let mut tuned = HostProfile::default();
        tuned.advanced.connect_timeout_s = Some(30);
        tuned.advanced.command_timeout_s = Some(1800);
        let from_host = host_limits(&tuned, None, None);
        assert_eq!(from_host.connect_timeout, Some(Duration::from_secs(30)));
        assert_eq!(from_host.command_timeout, Some(Duration::from_secs(1800)));
        let overridden = host_limits(&tuned, Some(2000), None);
        assert_eq!(overridden.connect_timeout, Some(Duration::from_secs(2)));
        assert_eq!(overridden.command_timeout, Some(Duration::from_secs(1800)));
    }

    #[test]
    fn retained_host_channel_budget_caps_concurrent_calls() {
        use crate::model::Protocol;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut host = HostProfile {
            alias: "gpu".into(),
            protocol: Protocol::Ssh,
            auth_persistence: AuthPersistence::Session,
            ..Default::default()
        };
        host.advanced.max_channels = Some(2);
        let hosts = vec![host.clone()];
        runtime.block_on(async {
            let gates = CallGates::new();
            let first = gates.single_host(&host, &hosts, 2).await;
            let blocked = tokio::time::timeout(
                Duration::from_millis(50),
                gates.single_host(&host, &hosts, 1),
            )
            .await;
            assert!(blocked.is_err(), "channel budget exhausted");
            drop(first);
            let allowed = tokio::time::timeout(
                Duration::from_millis(50),
                gates.single_host(&host, &hosts, 1),
            )
            .await;
            assert!(allowed.is_ok());
        });
    }

    #[test]
    fn cancellation_returns_a_structured_failure_and_drops_the_operation() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let ct = CancellationToken::new();
        ct.cancel();
        let outcome = runtime.block_on(cancellable(ct, async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok::<(), RemoteFailure>(())
        }));
        assert_eq!(outcome.unwrap_err().code, "CANCELLED");
    }

    #[test]
    fn per_call_hosts_serialise_while_retained_hosts_share() {
        use crate::model::Protocol;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let per_call = HostProfile {
            alias: "strict".into(),
            protocol: Protocol::Ssh,
            ..Default::default()
        };
        let retained = HostProfile {
            alias: "kept".into(),
            protocol: Protocol::Ssh,
            auth_persistence: AuthPersistence::Session,
            ..Default::default()
        };
        let hosts = vec![per_call.clone(), retained.clone()];
        runtime.block_on(async {
            let gates = CallGates::new();
            let first = gates.single_host(&per_call, &hosts, 1).await;
            let second = tokio::time::timeout(
                Duration::from_millis(50),
                gates.single_host(&per_call, &hosts, 1),
            )
            .await;
            assert!(second.is_err(), "same per-call host must wait");
            let shared = tokio::time::timeout(
                Duration::from_millis(50),
                gates.single_host(&retained, &hosts, 1),
            )
            .await;
            assert!(shared.is_ok(), "retained host runs alongside");
            let batch = tokio::time::timeout(Duration::from_millis(50), gates.batch()).await;
            assert!(batch.is_err(), "batch waits for single-host calls");
            drop(first);
            drop(shared);
            let batch = tokio::time::timeout(Duration::from_millis(50), gates.batch()).await;
            assert!(batch.is_ok());
        });
    }

    #[test]
    fn instructions_state_defaults_and_limits() {
        let text = instructions();
        assert!(text.contains(&SCHEMA_VERSION.to_string()));
        assert!(text.contains("120000"));
        assert!(text.contains("600000"));
        assert!(text.contains("256 hosts"));
    }
}
