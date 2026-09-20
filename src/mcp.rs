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
use std::time::Duration;

use crate::connection;
use crate::fido;
use crate::ssh::{self, OperationLimits, RemoteFailure};
use crate::storage::HostStore;
use crate::tool::{self, AgentIdentitiesResult, FidoIdentitiesResult, SCHEMA_VERSION};

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
    async fn probe(&self, Parameters(params): Parameters<ProbeParams>) -> CallToolResult {
        finish(probe_host(params).await)
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

fn load_store() -> Result<HostStore, RemoteFailure> {
    HostStore::load().map_err(|error| RemoteFailure::new("STORE_READ_FAILED", error.to_string()))
}

fn mcp_limits(connect_timeout_ms: Option<u64>, command_timeout_ms: Option<u64>) -> OperationLimits {
    let mut limits = tool::limits(connect_timeout_ms, command_timeout_ms, None);
    limits.connect_timeout = limits.connect_timeout.or(Some(DEFAULT_CONNECT_TIMEOUT));
    limits.command_timeout = limits.command_timeout.or(Some(DEFAULT_COMMAND_TIMEOUT));
    limits
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
            ["agent_identities", "fido_identities", "list_hosts", "probe"]
        );
        for tool in &tools {
            let annotations = tool.annotations.as_ref().expect("annotations");
            assert_eq!(annotations.read_only_hint, Some(true), "{}", tool.name);
            assert_eq!(
                annotations.open_world_hint,
                Some(tool.name.as_ref() == "probe"),
                "{}",
                tool.name
            );
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
    fn instructions_state_defaults_and_limits() {
        let text = instructions();
        assert!(text.contains(&SCHEMA_VERSION.to_string()));
        assert!(text.contains("120000"));
        assert!(text.contains("600000"));
        assert!(text.contains("256 hosts"));
    }
}
