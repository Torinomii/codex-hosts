//! Admissions live for one GUI probe operation and are never used by MCP calls.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::OnceCell;

use super::*;

pub(crate) type Entries = HashMap<Uuid, Result<Prepared, RemoteFailure>>;

pub(crate) struct Prepared {
    admitted: AdmittedLogin,
    key: ItemKey,
    facts: HostFacts,
}

impl Prepared {
    pub(crate) fn new(
        host: &HostProfile,
        key: ItemKey,
        facts: HostFacts,
        admission: Admission,
        login: VaultLogin,
    ) -> Self {
        let mut host = host.clone();
        apply(&facts, &key, &mut host);
        Self {
            admitted: AdmittedLogin {
                host,
                admission,
                login,
            },
            key,
            facts,
        }
    }

    fn admission(&self) -> AdmittedLogin {
        let login = match &self.admitted.login {
            VaultLogin::Bitwarden(login) => VaultLogin::Bitwarden(bitwarden::VaultLogin {
                session: codex_hosts_bw::SessionKey::new(login.session.expose().to_owned()),
                password: login.password.clone(),
            }),
            VaultLogin::OnePassword(password) => VaultLogin::OnePassword(password.clone()),
        };
        AdmittedLogin {
            host: self.admitted.host.clone(),
            admission: self.admitted.admission.clone(),
            login,
        }
    }
}

tokio::task_local! { static ACTIVE: Arc<Context>; }

pub(crate) struct Context {
    pub(crate) id: Uuid,
    hosts: HashMap<Uuid, HostProfile>,
    bitwarden: OnceCell<Result<Entries, RemoteFailure>>,
    onepassword: OnceCell<Result<Entries, RemoteFailure>>,
    credential_timeout: Duration,
    auth_timeout: Duration,
}

impl Context {
    pub(crate) fn new(
        hosts: Vec<HostProfile>,
        credential_timeout: Duration,
        auth_timeout: Duration,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            hosts: hosts.into_iter().map(|host| (host.id, host)).collect(),
            bitwarden: OnceCell::new(),
            onepassword: OnceCell::new(),
            credential_timeout,
            auth_timeout,
        }
    }

    async fn entries(&self, provider: VaultProvider) -> Result<&Entries, RemoteFailure> {
        let cell = match provider {
            VaultProvider::Bitwarden => &self.bitwarden,
            VaultProvider::OnePassword => &self.onepassword,
        };
        cell.get_or_init(|| async {
            let hosts: Vec<_> = self
                .hosts
                .values()
                .filter(|host| host.vault_provider() == Some(provider))
                .cloned()
                .collect();
            let load = async {
                match provider {
                    VaultProvider::Bitwarden => bitwarden::probe::load(&hosts).await,
                    VaultProvider::OnePassword => onepassword::probe::load(&hosts).await,
                }
            };
            tokio::time::timeout(self.credential_timeout, load)
                .await
                .map_err(|_| {
                    RemoteFailure::new(
                        crate::ssh::TOTAL_TIMEOUT_CODE,
                        "Preparing credentials for the connection tests timed out.",
                    )
                })?
        })
        .await
        .as_ref()
        .map_err(Clone::clone)
    }

    pub(crate) async fn prepare(&self, chain: &[Uuid]) -> Result<(), RemoteFailure> {
        let needs = |provider| {
            chain.iter().any(|id| {
                self.hosts
                    .get(id)
                    .is_some_and(|host| host.vault_provider() == Some(provider))
            })
        };
        let load = async |provider| {
            if needs(provider) {
                self.entries(provider).await.map(|_| ())
            } else {
                Ok(())
            }
        };
        let (bw, op) = tokio::join!(
            load(VaultProvider::Bitwarden),
            load(VaultProvider::OnePassword)
        );
        bw?;
        op?;
        for id in chain {
            if let Some(host) = self.hosts.get(id).filter(|host| host.is_vault_host()) {
                self.lookup(host)?;
            }
        }
        Ok(())
    }

    fn lookup(&self, host: &HostProfile) -> Result<&Prepared, RemoteFailure> {
        let loaded = match host.vault_provider() {
            Some(VaultProvider::Bitwarden) => self.bitwarden.get(),
            Some(VaultProvider::OnePassword) => self.onepassword.get(),
            None => None,
        };
        let missing = || {
            RemoteFailure::new(
                "PROFILE_INVALID",
                "The host was not prepared for this connection-test operation.",
            )
        };
        let entries = loaded.ok_or_else(missing)?.as_ref().map_err(Clone::clone)?;
        let prepared = entries
            .get(&host.id)
            .ok_or_else(missing)?
            .as_ref()
            .map_err(Clone::clone)?;
        if ItemKey::of(host).as_ref() != Some(&prepared.key) {
            return Err(missing());
        }
        Ok(prepared)
    }

    pub(crate) fn refreshes(&self, chain: &[Uuid]) -> Vec<(ItemKey, HostFacts)> {
        chain
            .iter()
            .filter_map(|id| self.hosts.get(id))
            .filter_map(|host| self.lookup(host).ok())
            .map(|prepared| (prepared.key.clone(), prepared.facts.clone()))
            .collect()
    }

    pub(crate) async fn scoped<T>(self: Arc<Self>, future: impl Future<Output = T>) -> T {
        ACTIVE.scope(self, future).await
    }
}

pub(crate) fn scope() -> Option<Uuid> {
    ACTIVE.try_with(|context| context.id).ok()
}

pub(crate) fn auth_timeout() -> Option<Duration> {
    ACTIVE.try_with(|context| context.auth_timeout).ok()
}

pub(super) fn admitted(host: &HostProfile) -> Option<Result<AdmittedLogin, RemoteFailure>> {
    ACTIVE
        .try_with(|context| context.lookup(host).map(Prepared::admission))
        .ok()
}

pub(super) fn password(host: &HostProfile) -> Option<Result<Zeroizing<String>, RemoteFailure>> {
    ACTIVE
        .try_with(|context| {
            let prepared = context.lookup(host)?;
            prepared.admitted.login.password().cloned().ok_or_else(|| {
                RemoteFailure::new(
                    match host.vault_provider() {
                        Some(VaultProvider::Bitwarden) => "BITWARDEN_ITEM_MISSING",
                        _ => "ONEPASSWORD_ITEM_MISSING",
                    },
                    format!(
                        "The password manager item behind {} has no password.",
                        host.alias
                    ),
                )
            })
        })
        .ok()
}
