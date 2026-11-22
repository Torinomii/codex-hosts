//! Temporary-secret tools. The vault lives in the tray GUI process; these are
//! thin IPC callers, run on the blocking pool because the IPC client owns its
//! own short-lived runtime (bounded by its five-second pipe deadline and the
//! twenty-second launch wait).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::ssh::RemoteFailure;
use crate::temporary_secrets::{self, Request, Response};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct OpenParams {
    /// Non-secret field names such as `myproject-local-qwen-apikey` (1-64 names, up to 256 bytes each).
    pub fields: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct StatusParams {
    /// Session UUID returned by temporary_secrets_open.
    pub session: uuid::Uuid,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RunParams {
    /// Session UUID returned by temporary_secrets_open.
    pub session: uuid::Uuid,
    /// Absolute path of the trusted program to start.
    pub program: PathBuf,
    /// Non-secret arguments; secrets are never substituted into them.
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory for the program.
    pub cwd: PathBuf,
    /// Environment variable name to secret field name; the value is injected, never returned.
    pub env: BTreeMap<String, String>,
    /// 1-600000 ms; the direct child is terminated on expiry.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

pub(super) async fn open(params: OpenParams) -> Result<Response, RemoteFailure> {
    blocking(move || {
        temporary_secrets::open(params.fields).map_err(|code| {
            window_failure(
                code,
                "Temporary secret window unavailable or invalid field names.",
            )
        })
    })
    .await
}

pub(super) async fn status(params: StatusParams) -> Result<Response, RemoteFailure> {
    call(params.session, Request::Status).await
}

pub(super) async fn run(params: RunParams) -> Result<Response, RemoteFailure> {
    // Execution's fields are private on purpose; build it through serde so its
    // own validation runs.
    let mut execution = serde_json::json!({
        "action": "execute",
        "program": params.program,
        "args": params.args,
        "cwd": params.cwd,
        "env": params.env,
    });
    if let Some(timeout_ms) = params.timeout_ms {
        execution["timeout_ms"] = serde_json::json!(timeout_ms);
    }
    let request: Request = serde_json::from_value(execution)
        .map_err(|error| RemoteFailure::new("REQUEST_INVALID", error.to_string()))?;
    call(params.session, request).await
}

/// The vault answers its own failures (`SECRET_MISSING`, `OPERATION_LIMIT`, ...)
/// as a response with `status: "error"`; MCP clients read `isError`, so those
/// become tool errors here.
pub(super) fn flag_errors(
    outcome: Result<Response, RemoteFailure>,
) -> Result<Response, serde_json::Value> {
    match outcome {
        Ok(response) if response.status == "error" => {
            Err(serde_json::to_value(response).unwrap_or_default())
        }
        Ok(response) => Ok(response),
        Err(error) => Err(serde_json::to_value(error).unwrap_or_default()),
    }
}

async fn call(session: uuid::Uuid, request: Request) -> Result<Response, RemoteFailure> {
    blocking(move || {
        temporary_secrets::call(session, request).map_err(|code| {
            window_failure(
                code,
                "Temporary session unavailable; open a new window if it was closed.",
            )
        })
    })
    .await
}

fn window_failure(code: &'static str, message: &'static str) -> RemoteFailure {
    let message = if code == temporary_secrets::INCOMPATIBLE_HOLDER {
        "When ready to discard this session's values, exit the older codex-hosts from the system tray and start the updated app."
    } else {
        message
    };
    RemoteFailure::new(code, message)
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, RemoteFailure> + Send + 'static,
) -> Result<T, RemoteFailure> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| RemoteFailure::new("TEMPORARY_SESSION_UNAVAILABLE", error.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_params_form_an_execute_request_and_unknown_fields_are_rejected() {
        let params = RunParams {
            session: uuid::Uuid::nil(),
            program: PathBuf::from(r"C:\py\python.exe"),
            args: vec!["script.py".into()],
            cwd: PathBuf::from(r"C:\project"),
            env: BTreeMap::from([("API_KEY".to_owned(), "proj-key".to_owned())]),
            timeout_ms: None,
        };
        let value = serde_json::json!({
            "action": "execute",
            "program": params.program,
            "args": params.args,
            "cwd": params.cwd,
            "env": params.env,
        });
        let request: Request = serde_json::from_value(value).unwrap();
        assert!(matches!(request, Request::Execute(_)));
        let rejected = serde_json::from_value::<Request>(serde_json::json!({
            "action": "execute",
            "program": "x",
            "cwd": "y",
            "env": {},
            "secret": "not allowed"
        }));
        assert!(rejected.is_err());
    }
}
