//! Bounded, process-local login snapshots with sliding idle expiry.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use super::{HostFacts, HostProfile, ItemKey, Uuid, Zeroizing};

pub(crate) const LISTING_TTL: Duration = Duration::from_secs(60);
const MAX_LOGINS: usize = 128;

/// A non-secret reset marker shared by the GUI and MCP processes.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Epoch {
    path: PathBuf,
    value: Uuid,
}

fn epoch_value(path: &Path) -> Result<Uuid, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Uuid::parse_str(text.trim()).map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Uuid::nil()),
        Err(error) => Err(error.to_string()),
    }
}

fn epoch_path(provider: crate::model::VaultProvider) -> Result<PathBuf, String> {
    let name = match provider {
        crate::model::VaultProvider::Bitwarden => "bitwarden",
        crate::model::VaultProvider::OnePassword => "onepassword",
    };
    Ok(crate::storage::data_directory()
        .map_err(|error| error.to_string())?
        .join(format!("vault-cache-{name}.epoch")))
}

impl Epoch {
    pub(crate) fn current(provider: crate::model::VaultProvider) -> Option<Self> {
        let path = epoch_path(provider).ok()?;
        Some(Self {
            value: epoch_value(&path).ok()?,
            path,
        })
    }

    pub(crate) fn is_current(&self) -> bool {
        epoch_value(&self.path).is_ok_and(|value| value == self.value)
    }
}

/// Clears this process immediately and notifies other running instances.
pub(crate) fn reset(provider: crate::model::VaultProvider) -> Result<(), String> {
    match provider {
        crate::model::VaultProvider::Bitwarden => crate::bitwarden::clear_login_cache(),
        crate::model::VaultProvider::OnePassword => crate::onepassword::clear_login_cache(),
    }
    let path = epoch_path(provider)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(path, Uuid::new_v4().to_string()).map_err(|error| error.to_string())
}

pub(crate) fn lifetime(minutes: u32) -> Duration {
    Duration::from_secs(u64::from(minutes) * 60)
}

/// Only the host facts and login password are retained, never a whole item.
pub(crate) struct Login {
    pub(crate) key: ItemKey,
    pub(crate) facts: HostFacts,
    pub(crate) password: Option<Zeroizing<String>>,
    pub(crate) epoch: Option<Epoch>,
}

impl Login {
    fn copy_for_use(&self) -> Self {
        Self {
            key: self.key.clone(),
            facts: self.facts.clone(),
            epoch: self.epoch.clone(),
            password: self
                .password
                .as_ref()
                .map(|password| Zeroizing::new(password.as_str().to_owned())),
        }
    }
}

#[cfg(test)]
static_assertions::assert_not_impl_any!(Login: std::fmt::Debug, std::fmt::Display, Clone);

struct Entry {
    login: Login,
    expires: Instant,
}

struct State<S> {
    scope: Option<S>,
    entries: HashMap<Uuid, Entry>,
    expiring: bool,
    epoch: Option<Epoch>,
}

impl<S> State<S> {
    fn prune(&mut self) {
        if self.epoch.as_ref().is_some_and(|epoch| !epoch.is_current()) {
            self.entries.clear();
        }
        let now = Instant::now();
        self.entries.retain(|_, entry| entry.expires > now);
    }
}

/// The caller's CLI gate covers lookup, fetch and insertion, so concurrent
/// requests for one item share the first successful read.
pub(crate) struct Logins<S> {
    state: Arc<Mutex<State<S>>>,
}

impl<S: PartialEq + Send + 'static> Logins<S> {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                scope: None,
                entries: HashMap::new(),
                expiring: false,
                epoch: None,
            })),
        }
    }

    pub(crate) fn get(&self, scope: &S, host: &HostProfile, ttl: Duration) -> Option<Login> {
        let mut state = self.state.lock().expect("login cache lock is not poisoned");
        state.prune();
        if ttl.is_zero() || state.scope.as_ref() != Some(scope) {
            state.entries.clear();
            state.scope = None;
            return None;
        }
        let entry = state.entries.get_mut(&host.id)?;
        let reference = host.vault()?;
        if ItemKey::of(host).as_ref() != Some(&entry.login.key)
            || !super::same_revision(&reference.revision_date, &entry.login.facts.revision)
        {
            state.entries.remove(&host.id);
            return None;
        }
        entry.expires = Instant::now() + ttl;
        Some(entry.login.copy_for_use())
    }

    pub(crate) fn remember(&self, scope: S, login: &Login, ttl: Duration) -> Result<(), String> {
        if ttl.is_zero()
            || login
                .epoch
                .as_ref()
                .is_some_and(|epoch| !epoch.is_current())
        {
            self.clear();
            return Ok(());
        }
        let mut state = self.state.lock().expect("login cache lock is not poisoned");
        state.prune();
        state.epoch = login.epoch.clone();
        if state.scope.as_ref() != Some(&scope) {
            state.entries.clear();
            state.scope = Some(scope);
        }
        if !state.entries.contains_key(&login.key.id)
            && state.entries.len() >= MAX_LOGINS
            && let Some(oldest) = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.expires)
                .map(|(id, _)| *id)
        {
            state.entries.remove(&oldest);
        }
        state.entries.insert(
            login.key.id,
            Entry {
                login: login.copy_for_use(),
                expires: Instant::now() + ttl,
            },
        );
        if !state.expiring {
            state.expiring = true;
            let weak = Arc::downgrade(&self.state);
            // A native worker also expires secrets while a GUI job's short-lived
            // Tokio runtime is stopped. It exits when the cache becomes empty.
            if let Err(error) = std::thread::Builder::new()
                .name("vault-login-expiry".to_owned())
                .spawn(move || {
                    loop {
                        let Some(shared) = weak.upgrade() else {
                            return;
                        };
                        let delay = {
                            let mut state =
                                shared.lock().expect("login cache lock is not poisoned");
                            state.prune();
                            match state.entries.values().map(|entry| entry.expires).min() {
                                Some(next) => next.saturating_duration_since(Instant::now()),
                                None => {
                                    state.expiring = false;
                                    return;
                                }
                            }
                        };
                        drop(shared);
                        std::thread::sleep(delay.min(Duration::from_secs(1)));
                    }
                })
            {
                state.entries.clear();
                state.expiring = false;
                return Err(format!(
                    "Could not start the vault login expiry worker: {error}."
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn clear(&self) {
        let mut state = self.state.lock().expect("login cache lock is not poisoned");
        state.entries.clear();
        state.scope = None;
        state.epoch = None;
    }
}

/// A local change made by another GUI/MCP process invalidates its snapshots.
pub(crate) fn file_stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Protocol;
    const TTL: Duration = Duration::from_secs(60);

    fn login(id: u128) -> Login {
        Login {
            key: ItemKey::bitwarden(Uuid::from_u128(id)),
            facts: HostFacts {
                alias: format!("cache-{id}"),
                protocol: Protocol::Ssh,
                address: "192.0.2.1".into(),
                port: 22,
                username: "test".into(),
                description: String::new(),
                tags: Vec::new(),
                revision: "1".into(),
            },
            password: Some(Zeroizing::new("synthetic-cache-password".into())),
            epoch: None,
        }
    }

    #[test]
    fn scopes_and_item_revisions_never_share_passwords() {
        let cache = Logins::new();
        let first = login(1);
        let mut host = super::super::mirror(&first.facts, &first.key);
        cache.remember("account-a", &first, TTL).unwrap();
        assert!(cache.get(&"account-a", &host, TTL).is_some());
        host.vault.as_mut().unwrap().revision_date = "2".into();
        assert!(cache.get(&"account-a", &host, TTL).is_none());
        cache.remember("account-a", &first, TTL).unwrap();
        assert!(cache.get(&"account-b", &host, TTL).is_none());
        assert!(cache.get(&"account-a", &host, TTL).is_none());
    }

    #[test]
    fn cache_hits_renew_the_idle_expiry_and_idle_secrets_are_erased() {
        let cache = Logins::new();
        let login = login(1);
        let host = super::super::mirror(&login.facts, &login.key);
        cache
            .remember((), &login, Duration::from_millis(40))
            .unwrap();
        let expires = cache.state.lock().unwrap().entries[&host.id].expires;
        assert!(cache.get(&(), &host, Duration::from_millis(80)).is_some());
        assert!(cache.state.lock().unwrap().entries[&host.id].expires > expires);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !cache.state.lock().unwrap().entries.is_empty() {
            assert!(Instant::now() < deadline, "idle secrets must be erased");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(cache.get(&(), &host, TTL).is_none());
    }

    #[test]
    fn capacity_and_explicit_clear_erase_retained_entries() {
        let cache = Logins::new();
        for id in 1..=(MAX_LOGINS as u128 + 1) {
            cache.remember((), &login(id), TTL).unwrap();
        }
        assert_eq!(cache.state.lock().unwrap().entries.len(), MAX_LOGINS);
        let first = login(1);
        assert!(
            cache
                .get(&(), &super::super::mirror(&first.facts, &first.key), TTL)
                .is_none()
        );
        cache.clear();
        assert!(cache.state.lock().unwrap().entries.is_empty());
    }

    #[test]
    fn disabled_caches_do_not_keep_passwords() {
        let cache = Logins::new();
        let login = login(1);
        cache.remember((), &login, Duration::ZERO).unwrap();
        assert!(cache.state.lock().unwrap().entries.is_empty());
        assert!(
            cache
                .get(
                    &(),
                    &super::super::mirror(&login.facts, &login.key),
                    Duration::ZERO
                )
                .is_none()
        );
    }

    #[test]
    fn reset_markers_erase_idle_snapshots_and_reject_inflight_old_reads() {
        let dir = std::env::temp_dir().join(format!("codex-hosts-cache-epoch-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("epoch");
        let cache = Logins::new();
        let mut login = login(1);
        login.epoch = Some(Epoch {
            path: path.clone(),
            value: Uuid::nil(),
        });
        cache.remember((), &login, TTL).unwrap();
        let epoch = Uuid::new_v4();
        std::fs::write(&path, epoch.to_string()).unwrap();
        assert!(
            cache
                .get(&(), &super::super::mirror(&login.facts, &login.key), TTL)
                .is_none()
        );
        cache.remember((), &login, TTL).unwrap();
        assert!(
            cache.state.lock().unwrap().entries.is_empty(),
            "an old in-flight read cannot refill a cleared cache"
        );
        login.epoch = Some(Epoch {
            path: path.clone(),
            value: epoch,
        });
        cache.remember((), &login, TTL).unwrap();
        std::fs::write(&path, Uuid::new_v4().to_string()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !cache.state.lock().unwrap().entries.is_empty() {
            assert!(
                Instant::now() < deadline,
                "other instances must erase idle snapshots"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
