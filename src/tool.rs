use std::fs;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::connection;
use crate::credentials::{self, CredentialKind};
use crate::model::{HostProfile, Protocol, SshAuth};
use crate::ssh::{OperationLimits, RemoteFailure};
use crate::storage::HostStore;

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum ToolRequest {
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
    jump_host: Option<String>,
    verified: bool,
    has_required_secret: bool,
    has_host_fingerprint: bool,
}

#[derive(Debug, Serialize)]
struct ListResult {
    status: &'static str,
    hosts: Vec<HostSummary>,
}

pub fn run(request_path: &Path, result_path: &Path) -> i32 {
    let response = match execute_request(request_path) {
        Ok(value) => value,
        Err(error) => serde_json::to_value(error).expect("serializing RemoteFailure cannot fail"),
    };
    match serde_json::to_vec_pretty(&response)
        .map_err(|error| error.to_string())
        .and_then(|bytes| fs::write(result_path, bytes).map_err(|error| error.to_string()))
    {
        Ok(()) => {
            if response.get("status").and_then(|value| value.as_str()) == Some("error") {
                1
            } else {
                0
            }
        }
        Err(_) => 2,
    }
}

fn execute_request(path: &Path) -> Result<serde_json::Value, RemoteFailure> {
    let bytes = fs::read(path)
        .map_err(|error| RemoteFailure::new("REQUEST_READ_FAILED", error.to_string()))?;
    let request: ToolRequest = serde_json::from_slice(&bytes)
        .map_err(|error| RemoteFailure::new("REQUEST_INVALID", error.to_string()))?;
    let mut store = HostStore::load()
        .map_err(|error| RemoteFailure::new("STORE_READ_FAILED", error.to_string()))?;

    match request {
        ToolRequest::ListHosts => {
            let mut hosts = Vec::with_capacity(store.hosts.len());
            for host in &store.hosts {
                let secret_kind =
                    if host.protocol == Protocol::Ssh && host.ssh_auth == SshAuth::PrivateKey {
                        CredentialKind::KeyPassphrase
                    } else {
                        CredentialKind::Password
                    };
                let secret_is_optional =
                    host.protocol == Protocol::Ssh && host.ssh_auth == SshAuth::PrivateKey;
                let has_secret = credentials::has(host.id, secret_kind).map_err(|error| {
                    RemoteFailure::new("CREDENTIAL_READ_FAILED", error.to_string())
                })?;
                hosts.push(HostSummary {
                    alias: host.alias.clone(),
                    address: host.address.clone(),
                    port: host.port,
                    username: host.username.clone(),
                    protocol: host.protocol.stable_name(),
                    ssh_auth: (host.protocol == Protocol::Ssh).then(|| host.ssh_auth.stable_name()),
                    jump_host: host
                        .jump_host
                        .and_then(|id| store.hosts.iter().find(|item| item.id == id))
                        .map(|item| item.alias.clone()),
                    verified: host.verified,
                    has_required_secret: secret_is_optional || has_secret,
                    has_host_fingerprint: host.host_fingerprint.is_some(),
                });
            }
            serde_json::to_value(ListResult {
                status: "ok",
                hosts,
            })
            .map_err(|error| RemoteFailure::new("SERIALIZE_FAILED", error.to_string()))
        }
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
                limits(connect_timeout_ms, command_timeout_ms),
            )?;
            if let Some(stored) = store.hosts.iter_mut().find(|item| item.id == host.id) {
                stored.verified = true;
            }
            store
                .save()
                .map_err(|error| RemoteFailure::new("STORE_WRITE_FAILED", error.to_string()))?;
            serde_json::to_value(result)
                .map_err(|error| RemoteFailure::new("SERIALIZE_FAILED", error.to_string()))
        }
        ToolRequest::Exec {
            alias,
            command,
            connect_timeout_ms,
            command_timeout_ms,
        } => {
            let host = find_host(&store, &alias)?;
            validate_profile(host)?;
            serde_json::to_value(connection::execute(
                host,
                &store.hosts,
                &command,
                limits(connect_timeout_ms, command_timeout_ms),
            )?)
            .map_err(|error| RemoteFailure::new("SERIALIZE_FAILED", error.to_string()))
        }
    }
}

fn limits(connect_timeout_ms: Option<u64>, command_timeout_ms: Option<u64>) -> OperationLimits {
    OperationLimits {
        connect_timeout: connect_timeout_ms.map(Duration::from_millis),
        command_timeout: command_timeout_ms.map(Duration::from_millis),
    }
}

fn find_host<'a>(store: &'a HostStore, alias: &str) -> Result<&'a HostProfile, RemoteFailure> {
    store.find_alias(alias).ok_or_else(|| {
        RemoteFailure::new(
            "ALIAS_NOT_FOUND",
            format!("No saved host is named {alias}."),
        )
    })
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
