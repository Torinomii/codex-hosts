use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::{self, KeyboardInteractiveAuthResponse};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey, load_secret_key};
use russh::{ChannelMsg, Disconnect};
use serde::Serialize;
use tokio::net::TcpStream;

use crate::credentials::{self, CredentialKind};
use crate::model::{HostProfile, SshAuth, resolve_ssh_chain};

const MAX_CAPTURE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, Default)]
pub struct OperationLimits {
    pub connect_timeout: Option<Duration>,
    pub command_timeout: Option<Duration>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteResult {
    pub status: &'static str,
    pub alias: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteFailure {
    pub status: &'static str,
    pub code: &'static str,
    pub message: Box<str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_alias: Option<Box<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_fingerprint: Option<Box<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_fingerprint: Option<Box<str>>,
}

impl RemoteFailure {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: "error",
            code,
            message: message.into().into_boxed_str(),
            host_alias: None,
            expected_fingerprint: None,
            observed_fingerprint: None,
        }
    }

    fn host_key(host: &HostProfile, observed: String) -> Self {
        let expected = host
            .host_fingerprint
            .clone()
            .filter(|value| !value.is_empty())
            .map(String::into_boxed_str);
        Self {
            status: "error",
            code: if expected.is_some() {
                "HOSTKEY_MISMATCH"
            } else {
                "HOSTKEY_UNKNOWN"
            },
            message: if expected.is_some() {
                "The detected SSH host key differs from the trusted fingerprint."
            } else {
                "The SSH host key has not been trusted yet."
            }
            .into(),
            host_alias: Some(host.alias.clone().into_boxed_str()),
            expected_fingerprint: expected,
            observed_fingerprint: Some(observed.into_boxed_str()),
        }
    }
}

#[derive(Clone)]
struct ServerKeyObserver {
    observed: Arc<Mutex<Option<String>>>,
}

impl client::Handler for ServerKeyObserver {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        if let Ok(mut observed) = self.observed.lock() {
            *observed = Some(server_public_key.fingerprint(HashAlg::Sha256).to_string());
        }
        Ok(true)
    }
}

pub fn probe(
    profile: &HostProfile,
    hosts: &[HostProfile],
    limits: OperationLimits,
) -> Result<RemoteResult, RemoteFailure> {
    execute(profile, hosts, "hostname", limits)
}

pub fn execute(
    profile: &HostProfile,
    hosts: &[HostProfile],
    command: &str,
    limits: OperationLimits,
) -> Result<RemoteResult, RemoteFailure> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| RemoteFailure::new("RUNTIME_CREATE_FAILED", error.to_string()))?;
    runtime.block_on(execute_async(profile, hosts, command, limits))
}

async fn execute_async(
    profile: &HostProfile,
    hosts: &[HostProfile],
    command: &str,
    limits: OperationLimits,
) -> Result<RemoteResult, RemoteFailure> {
    let chain = resolve_ssh_chain(profile, hosts)
        .map_err(|_| RemoteFailure::new("INVALID_HOST_CHAIN", "The SSH host chain is invalid."))?;
    let mut sessions = connect_chain(&chain, limits).await?;
    let result = run_with_optional_timeout(
        limits.command_timeout,
        "COMMAND_TIMEOUT",
        "SSH authentication or remote command execution timed out.",
        async {
            let target = sessions.last_mut().ok_or_else(|| {
                RemoteFailure::new("INVALID_HOST_CHAIN", "The SSH chain is empty.")
            })?;
            run_command(target, command).await
        },
    )
    .await;

    for session in sessions.iter_mut().rev() {
        let _ = session.disconnect(Disconnect::ByApplication, "", "").await;
    }
    let command_result = result?;
    Ok(RemoteResult {
        status: if command_result.exit_code == 0 {
            "ok"
        } else {
            "remote_error"
        },
        alias: profile.alias.clone(),
        exit_code: command_result.exit_code,
        stdout: command_result.stdout,
        stderr: command_result.stderr,
        host_fingerprint: profile.host_fingerprint.clone(),
    })
}

async fn connect_chain(
    chain: &[&HostProfile],
    limits: OperationLimits,
) -> Result<Vec<client::Handle<ServerKeyObserver>>, RemoteFailure> {
    let config = Arc::new(client::Config {
        inactivity_timeout: limits.command_timeout,
        nodelay: true,
        ..Default::default()
    });
    let mut sessions: Vec<client::Handle<ServerKeyObserver>> = Vec::with_capacity(chain.len());

    for (index, host) in chain.iter().enumerate() {
        let observed = Arc::new(Mutex::new(None));
        let observer = ServerKeyObserver {
            observed: Arc::clone(&observed),
        };
        let mut session = if index == 0 {
            let stream = run_with_optional_timeout(
                limits.connect_timeout,
                "CONNECT_TIMEOUT",
                "The TCP connection timed out.",
                async {
                    TcpStream::connect((host.address.as_str(), host.port))
                        .await
                        .map_err(|error| RemoteFailure::new("CONNECT_FAILED", error.to_string()))
                },
            )
            .await?;
            let _ = stream.set_nodelay(true);
            run_with_optional_timeout(
                limits.connect_timeout,
                "CONNECT_TIMEOUT",
                "The SSH handshake timed out.",
                async {
                    client::connect_stream(Arc::clone(&config), stream, observer)
                        .await
                        .map_err(|error| RemoteFailure::new("HANDSHAKE_FAILED", error.to_string()))
                },
            )
            .await?
        } else {
            let previous: &client::Handle<ServerKeyObserver> =
                sessions.last().ok_or_else(|| {
                    RemoteFailure::new("INVALID_HOST_CHAIN", "A jump-host session is missing.")
                })?;
            let channel = run_with_optional_timeout(
                limits.connect_timeout,
                "CONNECT_TIMEOUT",
                "Opening the SSH jump-host channel timed out.",
                async {
                    previous
                        .channel_open_direct_tcpip(
                            host.address.clone(),
                            u32::from(host.port),
                            "127.0.0.1",
                            0,
                        )
                        .await
                        .map_err(|error| {
                            RemoteFailure::new("JUMP_CHANNEL_FAILED", error.to_string())
                        })
                },
            )
            .await?;
            run_with_optional_timeout(
                limits.connect_timeout,
                "CONNECT_TIMEOUT",
                "The SSH handshake through the jump host timed out.",
                async {
                    client::connect_stream(Arc::clone(&config), channel.into_stream(), observer)
                        .await
                        .map_err(|error| RemoteFailure::new("HANDSHAKE_FAILED", error.to_string()))
                },
            )
            .await?
        };

        let detected = observed
            .lock()
            .map_err(|_| RemoteFailure::new("HOSTKEY_MISSING", "Cannot read the SSH host key."))?
            .clone()
            .ok_or_else(|| {
                RemoteFailure::new("HOSTKEY_MISSING", "The server did not provide a host key.")
            })?;
        if host.host_fingerprint.as_deref() != Some(detected.as_str()) {
            let _ = session.disconnect(Disconnect::ByApplication, "", "").await;
            return Err(RemoteFailure::host_key(host, detected));
        }

        run_with_optional_timeout(
            limits.connect_timeout,
            "AUTH_TIMEOUT",
            "SSH authentication timed out.",
            authenticate(&mut session, host),
        )
        .await?;
        sessions.push(session);
    }
    Ok(sessions)
}

async fn authenticate(
    session: &mut client::Handle<ServerKeyObserver>,
    host: &HostProfile,
) -> Result<(), RemoteFailure> {
    match host.ssh_auth {
        SshAuth::Password => {
            let password = credentials::load(host.id, CredentialKind::Password)
                .map_err(|error| RemoteFailure::new("CREDENTIAL_READ_FAILED", error.to_string()))?
                .ok_or_else(|| {
                    RemoteFailure::new(
                        "CREDENTIAL_MISSING",
                        format!("No password is saved for {}.", host.alias),
                    )
                })?;
            authenticate_password(session, &host.username, password.as_str()).await
        }
        SshAuth::PrivateKey => {
            let passphrase = credentials::load(host.id, CredentialKind::KeyPassphrase)
                .map_err(|error| RemoteFailure::new("CREDENTIAL_READ_FAILED", error.to_string()))?;
            let key = load_secret_key(
                host.private_key_path.trim(),
                passphrase.as_ref().map(|value| value.as_str()),
            )
            .map_err(|error| RemoteFailure::new("PRIVATE_KEY_LOAD_FAILED", error.to_string()))?;
            let hash = session
                .best_supported_rsa_hash()
                .await
                .map_err(|error| RemoteFailure::new("AUTH_FAILED", error.to_string()))?
                .flatten();
            let result = session
                .authenticate_publickey(
                    host.username.clone(),
                    PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await
                .map_err(|error| RemoteFailure::new("AUTH_FAILED", error.to_string()))?;
            if result.success() {
                Ok(())
            } else {
                Err(RemoteFailure::new(
                    "AUTH_FAILED",
                    format!("The private key was not accepted for {}.", host.alias),
                ))
            }
        }
    }
}

async fn authenticate_password(
    session: &mut client::Handle<ServerKeyObserver>,
    username: &str,
    password: &str,
) -> Result<(), RemoteFailure> {
    if session
        .authenticate_password(username.to_owned(), password.to_owned())
        .await
        .map(|result| result.success())
        .unwrap_or(false)
    {
        return Ok(());
    }

    let mut response = session
        .authenticate_keyboard_interactive_start(username.to_owned(), None)
        .await
        .map_err(|error| RemoteFailure::new("AUTH_FAILED", error.to_string()))?;
    for _ in 0..8 {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(()),
            KeyboardInteractiveAuthResponse::Failure { .. } => {
                return Err(RemoteFailure::new(
                    "AUTH_FAILED",
                    "The server rejected password authentication.",
                ));
            }
            KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                response = session
                    .authenticate_keyboard_interactive_respond(
                        prompts.iter().map(|_| password.to_owned()).collect(),
                    )
                    .await
                    .map_err(|error| RemoteFailure::new("AUTH_FAILED", error.to_string()))?;
            }
        }
    }
    Err(RemoteFailure::new(
        "AUTH_FAILED",
        "The server sent too many keyboard-interactive prompts.",
    ))
}

struct CommandResult {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

async fn run_command(
    session: &mut client::Handle<ServerKeyObserver>,
    command: &str,
) -> Result<CommandResult, RemoteFailure> {
    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|error| RemoteFailure::new("CHANNEL_OPEN_FAILED", error.to_string()))?;
    channel
        .exec(true, command.as_bytes())
        .await
        .map_err(|error| RemoteFailure::new("REMOTE_EXEC_FAILED", error.to_string()))?;

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code = -1;
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Data { data } => append_limited(&mut stdout, &data),
            ChannelMsg::ExtendedData { data, .. } => append_limited(&mut stderr, &data),
            ChannelMsg::ExitStatus { exit_status } => {
                exit_code = i32::try_from(exit_status).unwrap_or(-1)
            }
            _ => {}
        }
    }
    Ok(CommandResult {
        exit_code,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

async fn run_with_optional_timeout<T, F>(
    duration: Option<Duration>,
    code: &'static str,
    message: &'static str,
    future: F,
) -> Result<T, RemoteFailure>
where
    F: Future<Output = Result<T, RemoteFailure>>,
{
    if let Some(duration) = duration {
        tokio::time::timeout(duration, future)
            .await
            .map_err(|_| RemoteFailure::new(code, message))?
    } else {
        future.await
    }
}

fn append_limited(output: &mut Vec<u8>, data: &[u8]) {
    let remaining = MAX_CAPTURE_BYTES.saturating_sub(output.len());
    output.extend_from_slice(&data[..data.len().min(remaining)]);
}
