//! GUI probes share credentials and jump sessions only within one operation.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::*;
use crate::vault_hosts::{HostFacts, ItemKey, probe::Context};

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) network: Duration,
    pub(crate) credentials: Duration,
    pub(crate) interactive: Duration,
    pub(crate) concurrency: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            network: Duration::from_secs(10),
            credentials: Duration::from_secs(120),
            interactive: Duration::from_secs(60),
            concurrency: 8,
        }
    }
}

pub(crate) struct Outcome {
    pub(crate) result: Result<RemoteResult, RemoteFailure>,
    pub(crate) refreshed: Vec<(ItemKey, HostFacts)>,
}

impl Outcome {
    pub(crate) fn failed(error: RemoteFailure) -> Self {
        Self {
            result: Err(error),
            refreshed: Vec::new(),
        }
    }
}

/// The last GUI ticket cancels the operation; its coordinator joins the
/// cancelled tasks before disconnecting their batch-scoped SSH sessions.
pub(crate) struct Worker {
    cancel: CancellationToken,
    _task: tokio::task::JoinHandle<()>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct Scope(Uuid);
impl Drop for Scope {
    fn drop(&mut self) {
        ssh::finish_batch_scope(self.0);
    }
}

fn connection_capacity() -> &'static Arc<Semaphore> {
    static CAPACITY: OnceLock<Arc<Semaphore>> = OnceLock::new();
    CAPACITY.get_or_init(|| Arc::new(Semaphore::new(ssh::DEFAULT_RETAINED_CONNECTIONS)))
}

pub(crate) fn start(
    hosts: Arc<Vec<HostProfile>>,
    ids: Vec<Uuid>,
    limits: Limits,
    mut completed: impl FnMut(Uuid, Outcome) + Send + 'static,
) -> Arc<Worker> {
    let cancel = CancellationToken::new();
    let cancelled = cancel.clone();
    let task = ssh::runtime().spawn(async move {
        let mut required = HashMap::new();
        let mut plans = Vec::new();
        let mut seen = HashSet::new();
        for id in ids.into_iter().filter(|id| seen.insert(*id)) {
            let Some(profile) = hosts.iter().find(|host| host.id == id) else {
                completed(id, Outcome::failed(RemoteFailure::new("HOST_NOT_FOUND", "The connection-test host no longer exists.")));
                continue;
            };
            let chain = match profile.protocol {
                Protocol::Ssh => crate::model::resolve_ssh_chain(profile, &hosts),
                Protocol::Telnet => Ok(vec![profile]),
            };
            let chain = match chain {
                Ok(chain) => chain,
                Err(_) => {
                    completed(id, Outcome::failed(RemoteFailure::new("INVALID_HOST_CHAIN", "The SSH host chain is invalid.")));
                    continue;
                }
            };
            for host in &chain { required.entry(host.id).or_insert_with(|| (*host).clone()); }
            let chain = chain.iter().map(|host| host.id).collect::<Vec<_>>();
            plans.push((profile.clone(), chain));
        }
        let context = Arc::new(Context::new(required.into_values().collect(), limits.credentials, limits.interactive));
        let _scope = Scope(context.id);
        let network = Arc::new(Semaphore::new(limits.concurrency.max(1)));
        let mut tasks = JoinSet::new();
        let mut running = HashMap::new();
        for (profile, chain) in plans {
            let hosts = Arc::clone(&hosts);
            let context = Arc::clone(&context);
            let network = Arc::clone(&network);
            let id = profile.id;
            let task = tasks.spawn(async move {
                // CLI and human authorization waits never hold network slots
                // or consume a jump hop's TCP/handshake/authentication budget.
                let prepared = context.prepare(&chain).await;
                let refreshed = context.refreshes(&chain);
                let run = async {
                    prepared?;
                    // Reserve a whole route before connecting, so concurrent
                    // deep chains cannot exhaust the SSH pool halfway through.
                    let _capacity = connection_capacity().acquire_many(chain.len() as u32).await
                        .map_err(|_| RemoteFailure::new("TEST_WORKER_STOPPED", "The connection-test capacity was closed."))?;
                    let _slot = network.acquire().await.map_err(|_| RemoteFailure::new(
                        "TEST_WORKER_STOPPED", "The connection-test queue was closed.",
                    ))?;
                    let operation = OperationLimits {
                        connect_timeout: Some(limits.network), command_timeout: Some(limits.network),
                        batch_scope: Some(context.id), ..Default::default()
                    };
                    Arc::clone(&context).scoped(super::probe_async(&profile, &hosts, operation)).await
                };
                (id, Outcome { result: run.await, refreshed })
            });
            running.insert(task.id(), id);
        }
        loop {
            tokio::select! {
                biased;
                _ = cancelled.cancelled() => {
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    break;
                }
                result = tasks.join_next_with_id() => match result {
                    Some(Ok((task, (id, outcome)))) => { running.remove(&task); completed(id, outcome); }
                    Some(Err(error)) => {
                        if let Some(id) = running.remove(&error.id()) {
                            completed(id, Outcome::failed(RemoteFailure::new("TEST_WORKER_STOPPED", "The connection test worker stopped unexpectedly.")));
                        }
                    }
                    None => break,
                }
            }
        }
    });
    Arc::new(Worker {
        cancel,
        _task: task,
    })
}
