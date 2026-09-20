//! MCP stdio server mode (`codex-hosts.exe --mcp`).
//!
//! Serves on the shared SSH runtime so one runtime owns both the transport and
//! the connection pool. Tool handlers must stay on the async path: calling a
//! `block_on` wrapper from here would panic inside the runtime.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::connection;
use crate::fido;
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

pub fn run() -> i32 {
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
        }
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
        finish(cancellable(ct, probe_host(params)).await)
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
        finish(cancellable(ct, exec_host(params, None)).await)
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
        finish(cancellable(ct, exec_host(params, Some(stdin))).await)
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
        finish(cancellable(ct, exec_many_host(params)).await)
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

async fn probe_host(params: ProbeParams) -> Result<ssh::RemoteResult, RemoteFailure> {
    let store = load_store()?;
    let host = tool::find_host(&store, &params.alias)?.clone();
    tool::validate_profile(&host)?;
    let result = connection::probe_async(
        &host,
        &store.hosts,
        mcp_limits(params.connect_timeout_ms, params.command_timeout_ms),
    )
    .await?;
    tool::merge_verified_host_keys(&store.hosts, &result.verified_host_keys)?;
    Ok(result)
}

async fn exec_host(
    params: ExecParams,
    stdin: Option<String>,
) -> Result<ssh::RemoteResult, RemoteFailure> {
    let store = load_store()?;
    let host = tool::find_host(&store, &params.alias)?.clone();
    tool::validate_profile(&host)?;
    connection::execute_with_input_async(
        &host,
        &store.hosts,
        &params.command,
        stdin.as_deref(),
        mcp_limits(params.connect_timeout_ms, params.command_timeout_ms),
    )
    .await
}

async fn exec_many_host(params: ExecManyParams) -> Result<ssh::RemoteManyResult, RemoteFailure> {
    tool::validate_commands(&params.commands)?;
    let store = load_store()?;
    let host = tool::find_host(&store, &params.alias)?.clone();
    tool::validate_profile(&host)?;
    let concurrency = params
        .max_concurrency
        .unwrap_or(DEFAULT_BATCH_CONCURRENCY)
        .clamp(1, MAX_BATCH_CONCURRENCY);
    let mut limits = mcp_limits(params.connect_timeout_ms, params.command_timeout_ms);
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

fn load_store() -> Result<HostStore, RemoteFailure> {
    HostStore::load().map_err(|error| RemoteFailure::new("STORE_READ_FAILED", error.to_string()))
}

fn mcp_limits(connect_timeout_ms: Option<u64>, command_timeout_ms: Option<u64>) -> OperationLimits {
    tool::limits(
        Some(connect_timeout_or_default(connect_timeout_ms)),
        Some(command_timeout_or_default(command_timeout_ms)),
        None,
    )
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
                "exec",
                "exec_many",
                "exec_stdin",
                "fido_identities",
                "list_hosts",
                "probe"
            ]
        );
        for tool in &tools {
            let annotations = tool.annotations.as_ref().expect("annotations");
            let name = tool.name.as_ref();
            let executes = name.starts_with("exec") || name == "batch_exec";
            let connects = executes || name == "probe" || name == "batch_probe";
            assert_eq!(annotations.read_only_hint, Some(!executes), "{name}");
            assert_eq!(annotations.open_world_hint, Some(connects), "{name}");
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
    fn omitted_timeouts_receive_bounded_defaults_and_explicit_values_win() {
        let defaults = mcp_limits(None, None);
        assert_eq!(defaults.connect_timeout, Some(DEFAULT_CONNECT_TIMEOUT));
        assert_eq!(defaults.command_timeout, Some(DEFAULT_COMMAND_TIMEOUT));
        let explicit = mcp_limits(Some(5000), Some(1000));
        assert_eq!(explicit.connect_timeout, Some(Duration::from_secs(5)));
        assert_eq!(explicit.command_timeout, Some(Duration::from_secs(1)));
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
    fn instructions_state_defaults_and_limits() {
        let text = instructions();
        assert!(text.contains(&SCHEMA_VERSION.to_string()));
        assert!(text.contains("120000"));
        assert!(text.contains("600000"));
        assert!(text.contains("256 hosts"));
    }
}
