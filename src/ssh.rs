use std::collections::{HashMap, HashSet};
use std::fs;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use russh::client::{self, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::{
    Algorithm, HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyBase64, load_secret_key,
};
use russh::{ChannelMsg, Disconnect};
use serde::Serialize;
use tokio::net::TcpStream;
use tokio::runtime::Runtime;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinSet;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CreateMutexW, INFINITE, ReleaseMutex, WaitForSingleObject,
};

use crate::credentials::{self, CredentialKind};
use crate::fido;
use crate::model::{HostProfile, SshAuth, resolve_ssh_chain};

const MAX_CAPTURE_BYTES: usize = 1024 * 1024;
const MAX_AGENT_IDENTITIES: usize = 32;
const AGENT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_RETAINED_CONNECTIONS: usize = 16;
pub const MAX_RETAINED_CONNECTIONS: usize = 32;
pub const CONNECTION_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CONNECTION_REAPER_INTERVAL: Duration = Duration::from_secs(30);
#[cfg(windows)]
const WINDOWS_OPENSSH_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

#[derive(Debug, Clone, Copy, Default)]
pub struct OperationLimits {
    pub total_timeout: Option<Duration>,
    pub connect_timeout: Option<Duration>,
    pub command_timeout: Option<Duration>,
    pub output_bytes: Option<usize>,
    pub batch_scope: Option<uuid::Uuid>,
}

pub const TOTAL_TIMEOUT_CODE: &str = "OPERATION_TIMEOUT";

#[derive(Debug, Clone, Serialize)]
pub struct RemoteResult {
    pub status: &'static str,
    pub alias: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub output_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_key_algorithm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_key_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verified_host_keys: Vec<VerifiedHostKey>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteCommandResult {
    pub index: usize,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub output_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<Box<str>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteManyResult {
    pub status: &'static str,
    pub alias: String,
    pub results: Vec<RemoteCommandResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_key_algorithm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_key_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verified_host_keys: Vec<VerifiedHostKey>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifiedHostKey {
    #[serde(skip_serializing)]
    pub host_id: uuid::Uuid,
    pub alias: String,
    pub fingerprint: String,
    pub algorithm: String,
    pub verified_at_unix: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentKeyInfo {
    pub fingerprint: String,
    pub algorithm: String,
    pub comment: String,
    pub certificate: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteFailure {
    pub status: &'static str,
    pub code: &'static str,
    pub message: Box<str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_alias: Option<Box<str>>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub host_key: Option<Box<HostKeyFailure>>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct HostKeyFailure {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_fingerprint: Option<Box<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_fingerprint: Option<Box<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_algorithm: Option<Box<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_algorithm: Option<Box<str>>,
}

impl RemoteFailure {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: "error",
            code,
            message: message.into().into_boxed_str(),
            host_alias: None,
            host_key: None,
        }
    }

    fn host_key(host: &HostProfile, observed: ObservedHostKey) -> Self {
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
            host_key: Some(Box::new(HostKeyFailure {
                expected_fingerprint: expected,
                observed_fingerprint: Some(observed.fingerprint.into_boxed_str()),
                expected_algorithm: host.host_key_algorithm.clone().map(String::into_boxed_str),
                observed_algorithm: Some(observed.algorithm.into_boxed_str()),
            })),
        }
    }
}

#[derive(Debug, Clone)]
struct ObservedHostKey {
    fingerprint: String,
    algorithm: String,
}

#[derive(Clone)]
struct ServerKeyObserver {
    observed: Arc<Mutex<Option<ObservedHostKey>>>,
}

impl client::Handler for ServerKeyObserver {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        if let Ok(mut observed) = self.observed.lock() {
            *observed = Some(ObservedHostKey {
                fingerprint: server_public_key.fingerprint(HashAlg::Sha256).to_string(),
                algorithm: server_public_key.algorithm().to_string(),
            });
        }
        Ok(true)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SessionKey {
    parent: Option<Box<SessionKey>>,
    host_id: uuid::Uuid,
    address: String,
    port: u16,
    username: String,
    auth: &'static str,
    private_key_path: String,
    private_key_version: Option<(u64, u64)>,
    agent_key_fingerprint: String,
    host_fingerprint: Option<String>,
    host_key_algorithm: Option<String>,
}

impl SessionKey {
    fn new(host: &HostProfile, parent: Option<&SessionKey>) -> Self {
        let private_key_version = (host.ssh_auth == SshAuth::PrivateKey)
            .then(|| fs::metadata(host.private_key_path.trim()).ok())
            .flatten()
            .map(|metadata| {
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map(|duration| duration.as_secs())
                    .unwrap_or_default();
                (metadata.len(), modified)
            });
        Self {
            parent: parent.cloned().map(Box::new),
            host_id: host.id,
            address: host.address.trim().to_owned(),
            port: host.port,
            username: host.username.trim().to_owned(),
            auth: host.ssh_auth.stable_name(),
            private_key_path: host.private_key_path.trim().to_owned(),
            private_key_version,
            agent_key_fingerprint: host.agent_key_fingerprint.trim().to_owned(),
            host_fingerprint: host.host_fingerprint.clone(),
            host_key_algorithm: host.host_key_algorithm.clone(),
        }
    }
}

struct PooledSession {
    key: SessionKey,
    handle: Arc<client::Handle<ServerKeyObserver>>,
    _parent: Option<Arc<PooledSession>>,
    verified_host_key: VerifiedHostKey,
    auth_key_fingerprint: Option<String>,
    chain_ids: Vec<uuid::Uuid>,
}

struct PoolEntry {
    session: Arc<PooledSession>,
    last_used: Instant,
}

#[derive(Default)]
struct PoolState {
    entries: HashMap<SessionKey, PoolEntry>,
    pending_connections: usize,
    blocked_reconnects: HashSet<(uuid::Uuid, SessionKey)>,
    batch_connections: HashSet<(uuid::Uuid, SessionKey)>,
}

#[derive(Default)]
struct SessionPool {
    state: Mutex<PoolState>,
    connection_locks: Mutex<HashMap<SessionKey, Weak<AsyncMutex<()>>>>,
}

struct ConnectionReservation {
    pool: Arc<SessionPool>,
}

impl Drop for ConnectionReservation {
    fn drop(&mut self) {
        if let Ok(mut state) = self.pool.state.lock() {
            state.pending_connections = state.pending_connections.saturating_sub(1);
        }
    }
}

impl SessionPool {
    fn connection_lock(&self, key: &SessionKey) -> Arc<AsyncMutex<()>> {
        let mut locks = self
            .connection_locks
            .lock()
            .expect("SSH connection lock registry poisoned");
        if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(key.clone(), Arc::downgrade(&lock));
        lock
    }

    fn ready(
        &self,
        key: &SessionKey,
        batch_scope: Option<uuid::Uuid>,
    ) -> Result<Option<Arc<PooledSession>>, RemoteFailure> {
        let mut state = self.state.lock().map_err(|_| {
            RemoteFailure::new("SSH_POOL_FAILED", "The SSH connection pool lock failed.")
        })?;
        if batch_scope.is_some_and(|scope| state.blocked_reconnects.contains(&(scope, key.clone())))
        {
            return Err(RemoteFailure::new(
                "SSH_BATCH_RECONNECT_BLOCKED",
                "The shared SSH connection was lost; this batch will not reconnect automatically.",
            ));
        }
        let closed = state
            .entries
            .get(key)
            .is_some_and(|entry| entry.session.handle.is_closed());
        if closed {
            state.entries.remove(key);
            if let Some(scope) = batch_scope
                && state.batch_connections.contains(&(scope, key.clone()))
            {
                state.blocked_reconnects.insert((scope, key.clone()));
                return Err(RemoteFailure::new(
                    "SSH_BATCH_RECONNECT_BLOCKED",
                    "The shared SSH connection was lost; this batch will not reconnect automatically.",
                ));
            }
            return Ok(None);
        }
        let Some(entry) = state.entries.get_mut(key) else {
            return Ok(None);
        };
        entry.last_used = Instant::now();
        let session = Arc::clone(&entry.session);
        if let Some(scope) = batch_scope {
            state.batch_connections.insert((scope, key.clone()));
        }
        Ok(Some(session))
    }

    fn begin_connection(
        self: &Arc<Self>,
    ) -> Result<(Vec<Arc<PooledSession>>, ConnectionReservation), RemoteFailure> {
        let now = Instant::now();
        let mut state = self.state.lock().map_err(|_| {
            RemoteFailure::new("SSH_POOL_FAILED", "The SSH connection pool lock failed.")
        })?;
        let mut evicted = Vec::new();

        let idle_keys = state
            .entries
            .iter()
            .filter(|(_, entry)| {
                Arc::strong_count(&entry.session) == 1
                    && now.duration_since(entry.last_used) >= CONNECTION_IDLE_TIMEOUT
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in idle_keys {
            if let Some(entry) = state.entries.remove(&key) {
                evicted.push(entry.session);
            }
        }

        while state.entries.len() + state.pending_connections >= DEFAULT_RETAINED_CONNECTIONS {
            let oldest = state
                .entries
                .iter()
                .filter(|(_, entry)| Arc::strong_count(&entry.session) == 1)
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else {
                break;
            };
            if let Some(entry) = state.entries.remove(&oldest) {
                evicted.push(entry.session);
            }
        }

        if state.entries.len() + state.pending_connections >= MAX_RETAINED_CONNECTIONS {
            return Err(RemoteFailure::new(
                "SSH_CONNECTION_POOL_FULL",
                format!(
                    "All {MAX_RETAINED_CONNECTIONS} SSH connection slots are currently in use."
                ),
            ));
        }
        state.pending_connections += 1;
        drop(state);
        Ok((
            evicted,
            ConnectionReservation {
                pool: Arc::clone(self),
            },
        ))
    }

    fn finish_connection(
        &self,
        session: Result<Arc<PooledSession>, RemoteFailure>,
    ) -> Result<Arc<PooledSession>, RemoteFailure> {
        let mut state = self.state.lock().map_err(|_| {
            RemoteFailure::new("SSH_POOL_FAILED", "The SSH connection pool lock failed.")
        })?;
        if let Ok(session) = &session {
            state.entries.insert(
                session.key.clone(),
                PoolEntry {
                    session: Arc::clone(session),
                    last_used: Instant::now(),
                },
            );
        }
        session
    }

    fn remove(&self, session: &Arc<PooledSession>) {
        if let Ok(mut state) = self.state.lock()
            && state
                .entries
                .get(&session.key)
                .is_some_and(|entry| Arc::ptr_eq(&entry.session, session))
        {
            state.entries.remove(&session.key);
        }
    }

    fn block_reconnect(&self, scope: Option<uuid::Uuid>, key: &SessionKey) {
        if let Some(scope) = scope
            && let Ok(mut state) = self.state.lock()
        {
            state.blocked_reconnects.insert((scope, key.clone()));
        }
    }

    fn record_batch_connection(&self, scope: Option<uuid::Uuid>, key: &SessionKey) {
        if let Some(scope) = scope
            && let Ok(mut state) = self.state.lock()
        {
            state.batch_connections.insert((scope, key.clone()));
        }
    }

    fn finish_batch_scope(&self, scope: uuid::Uuid) {
        if let Ok(mut state) = self.state.lock() {
            state
                .blocked_reconnects
                .retain(|(blocked_scope, _)| *blocked_scope != scope);
            state
                .batch_connections
                .retain(|(batch_scope, _)| *batch_scope != scope);
        }
    }

    fn invalidate_host(&self, host_id: uuid::Uuid) -> Vec<Arc<PooledSession>> {
        let Ok(mut state) = self.state.lock() else {
            return Vec::new();
        };
        let keys = state
            .entries
            .iter()
            .filter(|(_, entry)| entry.session.chain_ids.contains(&host_id))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        keys.into_iter()
            .filter_map(|key| state.entries.remove(&key).map(|entry| entry.session))
            .collect()
    }

    fn idle_sessions(&self) -> Vec<Arc<PooledSession>> {
        let Ok(mut state) = self.state.lock() else {
            return Vec::new();
        };
        let now = Instant::now();
        let keys = state
            .entries
            .iter()
            .filter(|(_, entry)| {
                Arc::strong_count(&entry.session) == 1
                    && now.duration_since(entry.last_used) >= CONNECTION_IDLE_TIMEOUT
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        keys.into_iter()
            .filter_map(|key| state.entries.remove(&key).map(|entry| entry.session))
            .collect()
    }
}

fn ssh_runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("codex-hosts-ssh")
            .enable_all()
            .build()
            .expect("creating the shared SSH runtime should succeed")
    })
}

fn session_pool() -> &'static Arc<SessionPool> {
    static POOL: OnceLock<Arc<SessionPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        let pool = Arc::new(SessionPool::default());
        let weak = Arc::downgrade(&pool);
        ssh_runtime().spawn(async move {
            let mut interval = tokio::time::interval(CONNECTION_REAPER_INTERVAL);
            interval.tick().await;
            loop {
                interval.tick().await;
                let Some(pool) = weak.upgrade() else {
                    break;
                };
                for session in pool.idle_sessions() {
                    let _ = session
                        .handle
                        .disconnect(Disconnect::ByApplication, "idle timeout", "")
                        .await;
                }
            }
        });
        pool
    })
}

fn hardware_auth_lock() -> &'static AsyncMutex<()> {
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

#[cfg(windows)]
struct CrossProcessHardwareGuard(usize);

#[cfg(windows)]
impl Drop for CrossProcessHardwareGuard {
    fn drop(&mut self) {
        let handle = self.0 as windows_sys::Win32::Foundation::HANDLE;
        unsafe {
            ReleaseMutex(handle);
            CloseHandle(handle);
        }
    }
}

#[cfg(windows)]
async fn acquire_cross_process_hardware_lock() -> Result<CrossProcessHardwareGuard, RemoteFailure> {
    tokio::task::spawn_blocking(|| {
        let name = "Local\\codex-hosts-hardware-auth-v1\0"
            .encode_utf16()
            .collect::<Vec<_>>();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(RemoteFailure::new(
                "HARDWARE_AUTH_LOCK_FAILED",
                std::io::Error::last_os_error().to_string(),
            ));
        }
        let wait = unsafe { WaitForSingleObject(handle, INFINITE) };
        if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
            unsafe {
                CloseHandle(handle);
            }
            return Err(RemoteFailure::new(
                "HARDWARE_AUTH_LOCK_FAILED",
                format!("Windows hardware-authentication lock wait failed with code {wait}."),
            ));
        }
        Ok(CrossProcessHardwareGuard(handle as usize))
    })
    .await
    .map_err(|error| RemoteFailure::new("HARDWARE_AUTH_LOCK_FAILED", error.to_string()))?
}

#[cfg(not(windows))]
async fn acquire_cross_process_hardware_lock() -> Result<(), RemoteFailure> {
    Ok(())
}

pub fn invalidate_profile(host_id: uuid::Uuid) {
    let sessions = session_pool().invalidate_host(host_id);
    for session in sessions {
        ssh_runtime().spawn(async move {
            let _ = session
                .handle
                .disconnect(Disconnect::ByApplication, "profile changed", "")
                .await;
        });
    }
}

pub fn finish_batch_scope(scope: uuid::Uuid) {
    session_pool().finish_batch_scope(scope);
}

pub fn profile_may_require_interaction(profile: &HostProfile, hosts: &[HostProfile]) -> bool {
    resolve_ssh_chain(profile, hosts).is_ok_and(|chain| {
        chain.iter().any(|host| match host.ssh_auth {
            SshAuth::SshAgent => true,
            SshAuth::PrivateKey => {
                let path = std::path::Path::new(host.private_key_path.trim());
                fido::looks_like_fido_handle(path)
                    || load_secret_key(path, None).is_ok_and(|key| fido::is_security_key(&key))
            }
            SshAuth::Password => false,
        })
    })
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
    ssh_runtime().block_on(run_with_optional_timeout(
        limits.total_timeout,
        TOTAL_TIMEOUT_CODE,
        "The complete SSH operation exceeded its time limit.",
        execute_async(profile, hosts, command, limits),
    ))
}

pub fn execute_many(
    profile: &HostProfile,
    hosts: &[HostProfile],
    commands: &[String],
    max_concurrency: usize,
    limits: OperationLimits,
) -> Result<RemoteManyResult, RemoteFailure> {
    ssh_runtime().block_on(run_with_optional_timeout(
        limits.total_timeout,
        TOTAL_TIMEOUT_CODE,
        "The complete SSH multi-command operation exceeded its time limit.",
        execute_many_async(profile, hosts, commands, max_concurrency, limits),
    ))
}

pub fn agent_identities() -> Result<Vec<AgentKeyInfo>, RemoteFailure> {
    ssh_runtime().block_on(async {
        let agents = tokio::time::timeout(AGENT_DISCOVERY_TIMEOUT, connect_local_agents())
            .await
            .map_err(|_| {
                RemoteFailure::new(
                    "SSH_AGENT_TIMEOUT",
                    "SSH Agent/Pageant identity discovery exceeded five seconds.",
                )
            })??;
        let mut seen = std::collections::HashSet::new();
        Ok(agents
            .into_iter()
            .flat_map(|agent| agent.identities)
            .filter(|identity| {
                seen.insert(
                    identity
                        .public_key()
                        .fingerprint(HashAlg::Sha256)
                        .to_string(),
                )
            })
            .take(MAX_AGENT_IDENTITIES)
            .map(|identity| {
                let key = identity.public_key();
                let comment = identity.comment().to_owned();
                let public_key = matches!(&identity, AgentIdentity::PublicKey { .. })
                    .then(|| format!("{} {}", key.algorithm(), key.public_key_base64()));
                AgentKeyInfo {
                    fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
                    algorithm: key.algorithm().to_string(),
                    comment,
                    certificate: matches!(&identity, AgentIdentity::Certificate { .. }),
                    public_key,
                }
            })
            .collect())
    })
}

async fn execute_async(
    profile: &HostProfile,
    hosts: &[HostProfile],
    command: &str,
    limits: OperationLimits,
) -> Result<RemoteResult, RemoteFailure> {
    let chain = resolve_ssh_chain(profile, hosts)
        .map_err(|_| RemoteFailure::new("INVALID_HOST_CHAIN", "The SSH host chain is invalid."))?;
    let (sessions, verified_host_keys, auth_key_fingerprints) =
        connect_chain(&chain, limits).await?;
    let capture_budget = CaptureBudget::new(limits.output_bytes.unwrap_or(MAX_CAPTURE_BYTES));
    let result = run_with_optional_timeout(
        limits.command_timeout,
        "COMMAND_TIMEOUT",
        "SSH authentication or remote command execution timed out.",
        async {
            let target = sessions.last().ok_or_else(|| {
                RemoteFailure::new("INVALID_HOST_CHAIN", "The SSH chain is empty.")
            })?;
            run_command(target.handle.as_ref(), command, &capture_budget).await
        },
    )
    .await;
    let command_result = match result {
        Ok(result) => result,
        Err(error) => {
            if let Some(target) = sessions.last() {
                session_pool().remove(target);
                session_pool().block_reconnect(limits.batch_scope, &target.key);
            }
            return Err(error);
        }
    };
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
        output_truncated: command_result.output_truncated,
        host_fingerprint: profile.host_fingerprint.clone(),
        host_key_algorithm: verified_host_keys.last().map(|item| item.algorithm.clone()),
        auth_key_fingerprint: auth_key_fingerprints.last().cloned().flatten(),
        verified_host_keys,
    })
}

async fn execute_many_async(
    profile: &HostProfile,
    hosts: &[HostProfile],
    commands: &[String],
    max_concurrency: usize,
    limits: OperationLimits,
) -> Result<RemoteManyResult, RemoteFailure> {
    let chain = resolve_ssh_chain(profile, hosts)
        .map_err(|_| RemoteFailure::new("INVALID_HOST_CHAIN", "The SSH host chain is invalid."))?;
    let (sessions, verified_host_keys, auth_key_fingerprints) =
        connect_chain(&chain, limits).await?;
    let target = sessions
        .last()
        .ok_or_else(|| RemoteFailure::new("INVALID_HOST_CHAIN", "The SSH chain is empty."))?;
    let target_session = Arc::clone(target);
    let capture_budget = CaptureBudget::new(limits.output_bytes.unwrap_or(MAX_CAPTURE_BYTES));
    let command_concurrency = max_concurrency.max(1).min(commands.len().max(1));
    let mut next = 0usize;
    let mut tasks = JoinSet::new();
    let mut results = (0..commands.len())
        .map(|index| RemoteCommandResult {
            index,
            status: "cancelled",
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            output_truncated: false,
            error_code: Some("COMMAND_CANCELLED"),
            error_message: Some("The command did not start.".into()),
        })
        .collect::<Vec<_>>();

    while next < commands.len() || !tasks.is_empty() {
        while next < commands.len() && tasks.len() < command_concurrency {
            let index = next;
            next += 1;
            let command = commands[index].clone();
            let session = Arc::clone(&target_session);
            let budget = capture_budget.clone();
            tasks.spawn(async move {
                let result = run_with_optional_timeout(
                    limits.command_timeout,
                    "COMMAND_TIMEOUT",
                    "The remote command timed out.",
                    run_command(session.handle.as_ref(), &command, &budget),
                )
                .await;
                (index, result)
            });
        }
        let Some(joined) = tasks.join_next().await else {
            break;
        };
        let (index, result) = joined
            .map_err(|error| RemoteFailure::new("COMMAND_WORKER_FAILED", error.to_string()))?;
        results[index] = match result {
            Ok(result) => RemoteCommandResult {
                index,
                status: if result.exit_code == 0 {
                    "ok"
                } else {
                    "remote_error"
                },
                exit_code: Some(result.exit_code),
                stdout: result.stdout,
                stderr: result.stderr,
                output_truncated: result.output_truncated,
                error_code: None,
                error_message: None,
            },
            Err(error) => {
                session_pool().remove(&target_session);
                session_pool().block_reconnect(limits.batch_scope, &target_session.key);
                RemoteCommandResult {
                    index,
                    status: "error",
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    output_truncated: false,
                    error_code: Some(error.code),
                    error_message: Some(error.message),
                }
            }
        };
    }
    let status = if results.iter().all(|result| result.status == "ok") {
        "ok"
    } else if results.iter().any(|result| result.status == "error") {
        "completed_with_errors"
    } else {
        "completed_with_remote_errors"
    };
    Ok(RemoteManyResult {
        status,
        alias: profile.alias.clone(),
        results,
        host_fingerprint: profile.host_fingerprint.clone(),
        host_key_algorithm: verified_host_keys.last().map(|item| item.algorithm.clone()),
        auth_key_fingerprint: auth_key_fingerprints.last().cloned().flatten(),
        verified_host_keys,
    })
}

async fn connect_chain(
    chain: &[&HostProfile],
    limits: OperationLimits,
) -> Result<
    (
        Vec<Arc<PooledSession>>,
        Vec<VerifiedHostKey>,
        Vec<Option<String>>,
    ),
    RemoteFailure,
> {
    let mut sessions = Vec::with_capacity(chain.len());
    let mut verified_host_keys = Vec::with_capacity(chain.len());
    let mut auth_key_fingerprints = Vec::with_capacity(chain.len());

    for host in chain {
        let parent = sessions.last().cloned();
        let session = pooled_connection(host, parent.as_ref(), limits).await?;
        verified_host_keys.push(session.verified_host_key.clone());
        auth_key_fingerprints.push(session.auth_key_fingerprint.clone());
        sessions.push(session);
    }
    Ok((sessions, verified_host_keys, auth_key_fingerprints))
}

async fn pooled_connection(
    host: &HostProfile,
    parent: Option<&Arc<PooledSession>>,
    limits: OperationLimits,
) -> Result<Arc<PooledSession>, RemoteFailure> {
    let key = SessionKey::new(host, parent.map(|session| &session.key));
    if let Some(session) = session_pool().ready(&key, limits.batch_scope)? {
        return Ok(session);
    }
    let connection_lock = session_pool().connection_lock(&key);
    let _guard = connection_lock.lock().await;
    if let Some(session) = session_pool().ready(&key, limits.batch_scope)? {
        return Ok(session);
    }

    let pool = Arc::clone(session_pool());
    let (evicted, reservation) = pool.begin_connection()?;
    for session in evicted {
        let _ = session
            .handle
            .disconnect(Disconnect::ByApplication, "LRU eviction", "")
            .await;
    }
    let connected = connect_one(host, parent, key.clone(), limits).await;
    if let Err(error) = &connected {
        session_pool().block_reconnect(limits.batch_scope, &key);
        if error.code == "JUMP_CHANNEL_FAILED"
            && let Some(parent) = parent
        {
            session_pool().remove(parent);
            session_pool().block_reconnect(limits.batch_scope, &parent.key);
        }
    }
    let result = pool.finish_connection(connected);
    if let Ok(session) = &result {
        pool.record_batch_connection(limits.batch_scope, &session.key);
    }
    drop(reservation);
    result
}

async fn connect_one(
    host: &HostProfile,
    parent: Option<&Arc<PooledSession>>,
    key: SessionKey,
    limits: OperationLimits,
) -> Result<Arc<PooledSession>, RemoteFailure> {
    let config = Arc::new(client::Config {
        inactivity_timeout: limits.command_timeout,
        nodelay: true,
        ..Default::default()
    });
    let observed = Arc::new(Mutex::new(None));
    let observer = ServerKeyObserver {
        observed: Arc::clone(&observed),
    };
    let mut session = if let Some(parent) = parent {
        let channel = run_with_optional_timeout(
            limits.connect_timeout,
            "CONNECT_TIMEOUT",
            "Opening the SSH jump-host channel timed out.",
            async {
                parent
                    .handle
                    .channel_open_direct_tcpip(
                        host.address.clone(),
                        u32::from(host.port),
                        "127.0.0.1",
                        0,
                    )
                    .await
                    .map_err(|error| RemoteFailure::new("JUMP_CHANNEL_FAILED", error.to_string()))
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
    } else {
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
    };

    let detected = observed
        .lock()
        .map_err(|_| RemoteFailure::new("HOSTKEY_MISSING", "Cannot read the SSH host key."))?
        .clone()
        .ok_or_else(|| {
            RemoteFailure::new("HOSTKEY_MISSING", "The server did not provide a host key.")
        })?;
    if host.host_fingerprint.as_deref() != Some(detected.fingerprint.as_str()) {
        let _ = session
            .disconnect(Disconnect::ByApplication, "host key rejected", "")
            .await;
        return Err(RemoteFailure::host_key(host, detected));
    }

    let auth_key_fingerprint = run_with_optional_timeout(
        limits.connect_timeout,
        "AUTH_TIMEOUT",
        "SSH authentication timed out.",
        authenticate(&mut session, host),
    )
    .await?;
    let mut chain_ids = parent
        .map(|session| session.chain_ids.clone())
        .unwrap_or_default();
    chain_ids.push(host.id);
    Ok(Arc::new(PooledSession {
        key,
        handle: Arc::new(session),
        _parent: parent.cloned(),
        verified_host_key: VerifiedHostKey {
            host_id: host.id,
            alias: host.alias.clone(),
            fingerprint: detected.fingerprint,
            algorithm: detected.algorithm,
            verified_at_unix: unix_timestamp(),
        },
        auth_key_fingerprint,
        chain_ids,
    }))
}

async fn authenticate(
    session: &mut client::Handle<ServerKeyObserver>,
    host: &HostProfile,
) -> Result<Option<String>, RemoteFailure> {
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
            authenticate_password(session, &host.username, password.as_str())
                .await
                .map(|()| None)
        }
        SshAuth::PrivateKey => {
            let passphrase = credentials::load(host.id, CredentialKind::KeyPassphrase)
                .map_err(|error| RemoteFailure::new("CREDENTIAL_READ_FAILED", error.to_string()))?;
            let key = load_secret_key(
                host.private_key_path.trim(),
                passphrase.as_ref().map(|value| value.as_str()),
            )
            .map_err(|error| {
                RemoteFailure::new(
                    "PRIVATE_KEY_LOAD_FAILED",
                    format!(
                        "Could not read the selected OpenSSH key or FIDO handle: {error}. Use the file-passphrase field only for an encrypted file; enter a hardware PIN only in the FIDO create/recover window."
                    ),
                )
            })?;
            let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
            if fido::is_security_key(&key) {
                // Only the signing/authentication prompt is serialized. Once a
                // session is authenticated, its channels can run concurrently.
                let _hardware_guard = hardware_auth_lock().lock().await;
                let _cross_process_guard = acquire_cross_process_hardware_lock().await?;
                let key = Arc::new(key);
                let public_key = key.public_key().clone();
                let mut signer = fido::FidoSigner::new(key)
                    .map_err(|error| RemoteFailure::new("FIDO_KEY_INVALID", error.to_string()))?;
                let result = session
                    .authenticate_publickey_with(
                        host.username.clone(),
                        public_key,
                        None,
                        &mut signer,
                    )
                    .await
                    .map_err(|error| {
                        RemoteFailure::new("FIDO_SIGNING_FAILED", error.to_string())
                    })?;
                return if result.success() {
                    Ok(Some(fingerprint))
                } else {
                    Err(RemoteFailure::new(
                        "FIDO_AUTH_REJECTED",
                        format!(
                            "The server did not accept the FIDO SSH public key for {}. Authorize the matching public key on that server, then retry.",
                            host.alias
                        ),
                    ))
                };
            }
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
                Ok(Some(fingerprint))
            } else {
                Err(RemoteFailure::new(
                    "AUTH_FAILED",
                    format!(
                        "The server did not accept the selected OpenSSH public key for {}. Authorize the matching public key on that server, then retry.",
                        host.alias
                    ),
                ))
            }
        }
        SshAuth::SshAgent => authenticate_agent(session, host).await.map(Some),
    }
}

type DynamicAgentClient = AgentClient<Box<dyn AgentStream + Send + Unpin>>;

struct LocalAgent {
    client: DynamicAgentClient,
    identities: Vec<AgentIdentity>,
}

#[cfg(windows)]
async fn connect_local_agents() -> Result<Vec<LocalAgent>, RemoteFailure> {
    let mut agents = Vec::new();
    let openssh_error = match AgentClient::connect_named_pipe(WINDOWS_OPENSSH_AGENT_PIPE).await {
        Ok(client) => {
            let mut client = client.dynamic();
            match client.request_identities().await {
                Ok(identities) => {
                    let message = if identities.is_empty() {
                        "no identities".to_owned()
                    } else {
                        "available".to_owned()
                    };
                    agents.push(LocalAgent { client, identities });
                    message
                }
                Err(error) => error.to_string(),
            }
        }
        Err(error) => error.to_string(),
    };

    let pageant_error = match AgentClient::connect_pageant().await {
        Ok(client) => {
            let mut client = client.dynamic();
            match client.request_identities().await {
                Ok(identities) => {
                    let message = if identities.is_empty() {
                        "no identities".to_owned()
                    } else {
                        "available".to_owned()
                    };
                    agents.push(LocalAgent { client, identities });
                    message
                }
                Err(error) => error.to_string(),
            }
        }
        Err(error) => error.to_string(),
    };
    if !agents.is_empty() {
        return Ok(agents);
    }
    Err(RemoteFailure::new(
        "SSH_AGENT_UNAVAILABLE",
        format!(
            "Windows OpenSSH Agent is unavailable ({openssh_error}); Pageant is unavailable ({pageant_error})."
        ),
    ))
}

#[cfg(not(windows))]
async fn connect_local_agents() -> Result<Vec<LocalAgent>, RemoteFailure> {
    Err(RemoteFailure::new(
        "SSH_AGENT_UNAVAILABLE",
        "SSH Agent/Pageant authentication is currently supported only on Windows.",
    ))
}

async fn authenticate_agent(
    session: &mut client::Handle<ServerKeyObserver>,
    host: &HostProfile,
) -> Result<String, RemoteFailure> {
    // Agent identities may be backed by Pageant or a hardware key. Keep the
    // authentication prompt lane single-file without limiting later channels.
    let _hardware_guard = hardware_auth_lock().lock().await;
    let _cross_process_guard = acquire_cross_process_hardware_lock().await?;
    let agents = connect_local_agents().await?;
    if agents.iter().all(|agent| agent.identities.is_empty()) {
        return Err(RemoteFailure::new(
            "SSH_AGENT_NO_KEYS",
            "Neither the running Windows OpenSSH Agent nor Pageant provided an identity.",
        ));
    }

    let requested = host.agent_key_fingerprint.trim();
    let mut matched_requested_key = requested.is_empty();
    let mut seen_fingerprints = HashSet::new();
    for mut agent in agents {
        for identity in agent.identities.into_iter().take(MAX_AGENT_IDENTITIES) {
            let public_key = identity.public_key();
            let fingerprint = public_key.fingerprint(HashAlg::Sha256).to_string();
            if !seen_fingerprints.insert(fingerprint.clone()) {
                continue;
            }
            if !requested.is_empty() && requested != fingerprint {
                continue;
            }
            matched_requested_key = true;
            let hash = if matches!(public_key.algorithm(), Algorithm::Rsa { .. }) {
                session
                    .best_supported_rsa_hash()
                    .await
                    .map_err(|error| {
                        RemoteFailure::new("SSH_AGENT_AUTH_FAILED", error.to_string())
                    })?
                    .flatten()
            } else {
                None
            };
            let result = match identity {
                AgentIdentity::PublicKey { key, .. } => {
                    session
                        .authenticate_publickey_with(
                            host.username.clone(),
                            key,
                            hash,
                            &mut agent.client,
                        )
                        .await
                }
                AgentIdentity::Certificate { certificate, .. } => {
                    session
                        .authenticate_certificate_with(
                            host.username.clone(),
                            certificate,
                            hash,
                            &mut agent.client,
                        )
                        .await
                }
            }
            .map_err(|error| RemoteFailure::new("SSH_AGENT_SIGNING_REJECTED", error.to_string()))?;
            if result.success() {
                return Ok(fingerprint);
            }
        }
    }

    if !matched_requested_key {
        Err(RemoteFailure::new(
            "SSH_AGENT_KEY_NOT_FOUND",
            format!(
                "The selected SSH Agent/Pageant identity is not currently exposed for {}.",
                host.alias
            ),
        ))
    } else {
        Err(RemoteFailure::new(
            "SSH_AGENT_AUTH_REJECTED",
            format!(
                "The server did not accept any identity exposed by SSH Agent/Pageant for {}.",
                host.alias
            ),
        ))
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
    output_truncated: bool,
}

#[derive(Clone)]
struct CaptureBudget {
    remaining: Arc<AtomicUsize>,
}

impl CaptureBudget {
    fn new(bytes: usize) -> Self {
        Self {
            remaining: Arc::new(AtomicUsize::new(bytes)),
        }
    }

    fn take(&self, requested: usize) -> usize {
        let mut available = self.remaining.load(Ordering::Acquire);
        loop {
            let taken = available.min(requested);
            match self.remaining.compare_exchange_weak(
                available,
                available - taken,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return taken,
                Err(current) => available = current,
            }
        }
    }
}

async fn run_command(
    session: &client::Handle<ServerKeyObserver>,
    command: &str,
    budget: &CaptureBudget,
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
    let mut output_truncated = false;
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Data { data } => {
                output_truncated |= append_limited(&mut stdout, &data, budget)
            }
            ChannelMsg::ExtendedData { data, .. } => {
                output_truncated |= append_limited(&mut stderr, &data, budget)
            }
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
        output_truncated,
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

fn append_limited(output: &mut Vec<u8>, data: &[u8], budget: &CaptureBudget) -> bool {
    let taken = budget.take(data.len());
    output.extend_from_slice(&data[..taken]);
    taken < data.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_timeout_cancels_the_wrapped_operation() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(run_with_optional_timeout(
            Some(Duration::from_millis(1)),
            TOTAL_TIMEOUT_CODE,
            "timed out",
            async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok(())
            },
        ));
        assert_eq!(result.unwrap_err().code, TOTAL_TIMEOUT_CODE);
    }

    #[test]
    fn host_key_error_keeps_the_flat_tool_protocol_fields() {
        let host = HostProfile {
            alias: "server".to_owned(),
            host_fingerprint: Some("SHA256:expected".to_owned()),
            host_key_algorithm: Some("ssh-ed25519".to_owned()),
            ..HostProfile::default()
        };
        let value = serde_json::to_value(RemoteFailure::host_key(
            &host,
            ObservedHostKey {
                fingerprint: "SHA256:observed".to_owned(),
                algorithm: "ssh-rsa".to_owned(),
            },
        ))
        .unwrap();
        assert_eq!(value["expected_fingerprint"], "SHA256:expected");
        assert_eq!(value["observed_fingerprint"], "SHA256:observed");
        assert_eq!(value["expected_algorithm"], "ssh-ed25519");
        assert_eq!(value["observed_algorithm"], "ssh-rsa");
        assert!(value.get("host_key").is_none());
    }

    #[test]
    fn capture_limit_reports_discarded_bytes() {
        let budget = CaptureBudget::new(1);
        let mut output = vec![b'x'; MAX_CAPTURE_BYTES - 1];
        assert!(append_limited(&mut output, b"yz", &budget));
        assert_eq!(output.len(), MAX_CAPTURE_BYTES);
        assert_eq!(output.last(), Some(&b'y'));
    }

    #[test]
    fn shared_capture_budget_is_consumed_across_streams() {
        let budget = CaptureBudget::new(3);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert!(!append_limited(&mut stdout, b"ab", &budget));
        assert!(append_limited(&mut stderr, b"cd", &budget));
        assert_eq!(stdout, b"ab");
        assert_eq!(stderr, b"c");
    }

    #[test]
    fn connection_pool_limits_match_the_retention_contract() {
        assert_eq!(DEFAULT_RETAINED_CONNECTIONS, 16);
        assert_eq!(MAX_RETAINED_CONNECTIONS, 32);
        assert_eq!(CONNECTION_IDLE_TIMEOUT, Duration::from_secs(300));
    }

    #[test]
    fn hardware_and_agent_profiles_receive_interactive_time() {
        let mut hardware = HostProfile {
            ssh_auth: SshAuth::PrivateKey,
            private_key_path: "id_ecdsa_sk".to_owned(),
            ..HostProfile::default()
        };
        assert!(profile_may_require_interaction(
            &hardware,
            &[hardware.clone()]
        ));
        hardware.private_key_path = "id_ed25519".to_owned();
        assert!(!profile_may_require_interaction(
            &hardware,
            &[hardware.clone()]
        ));
        hardware.ssh_auth = SshAuth::SshAgent;
        assert!(profile_may_require_interaction(
            &hardware,
            &[hardware.clone()]
        ));
    }

    #[test]
    fn batch_scope_blocks_reconnect_until_the_batch_finishes() {
        let pool = SessionPool::default();
        let host = HostProfile::default();
        let key = SessionKey::new(&host, None);
        let scope = uuid::Uuid::new_v4();
        pool.block_reconnect(Some(scope), &key);
        let Err(error) = pool.ready(&key, Some(scope)) else {
            panic!("the blocked batch connection unexpectedly became available");
        };
        assert_eq!(error.code, "SSH_BATCH_RECONNECT_BLOCKED");
        pool.finish_batch_scope(scope);
        assert!(pool.ready(&key, Some(scope)).unwrap().is_none());
    }

    #[test]
    fn concurrent_requests_for_one_jump_share_the_same_creation_lock() {
        let pool = SessionPool::default();
        let key = SessionKey::new(&HostProfile::default(), None);
        let first = pool.connection_lock(&key);
        let second = pool.connection_lock(&key);
        assert!(Arc::ptr_eq(&first, &second));
    }
}
