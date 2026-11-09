//! Login and quiet-list snapshots never outlive the account's unlock policy.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use super::*;
use crate::model::ReauthPolicy;
use crate::vault_hosts::cache::{self, Login, Logins};

#[derive(PartialEq)]
struct Scope {
    profile: BitwardenProfile,
    exe: PathBuf,
    max_age: u64,
    fingerprint: u64,
    data: Option<(SystemTime, u64)>,
    synced: Option<(SystemTime, u64)>,
    epoch: Option<cache::Epoch>,
    minutes: u32,
}

static LISTING: Mutex<Option<(Scope, Instant)>> = Mutex::new(None);

fn logins() -> &'static Logins<Scope> {
    static LOGINS: OnceLock<Logins<Scope>> = OnceLock::new();
    LOGINS.get_or_init(Logins::new)
}

fn scope(account: &Account, vault: &Vault, max_age: u64) -> Option<Scope> {
    let directory = session::data_dir(account.id()).ok()?;
    Some(Scope {
        profile: account.profile.clone(),
        exe: account.exe.path().to_owned(),
        max_age,
        fingerprint: session::fingerprint(vault.session()?),
        data: cache::file_stamp(&directory.join("data.json")),
        synced: cache::file_stamp(&directory.join("last-sync")),
        epoch: cache::Epoch::current(crate::model::VaultProvider::Bitwarden),
        minutes: account.cache_minutes,
    })
}

pub(super) fn clear() {
    logins().clear();
    *LISTING
        .lock()
        .expect("vault listing cache lock is not poisoned") = None;
}

pub(super) fn listing_fresh(account: &Account, vault: &Vault, max_age: u64) -> bool {
    let current = scope(account, vault, max_age);
    let mut listing = LISTING
        .lock()
        .expect("vault listing cache lock is not poisoned");
    let fresh = listing.as_ref().is_some_and(|(held, at)| {
        account.cache_minutes > 0
            && current.as_ref() == Some(held)
            && at.elapsed() < cache::LISTING_TTL
    });
    if !fresh {
        *listing = None;
    }
    fresh
}

pub(super) fn remember_listing(account: &Account, vault: &Vault, max_age: u64) {
    *LISTING
        .lock()
        .expect("vault listing cache lock is not poisoned") =
        scope(account, vault, max_age).map(|scope| (scope, Instant::now()));
}

pub(super) async fn read(
    account: &Account,
    vault: &mut Vault,
    max_age: u64,
    host: &HostProfile,
) -> Result<(Login, bool), RemoteFailure> {
    let key = ItemKey::of(host).ok_or_else(|| unavailable("not a vault host"))?;
    let epoch = cache::Epoch::current(crate::model::VaultProvider::Bitwarden);
    if account.profile.policy == ReauthPolicy::PerCall {
        clear();
    } else if epoch.is_some()
        && let Some(scope) = scope(account, vault, max_age)
        && let Some(login) = logins().get(&scope, host, cache::lifetime(account.cache_minutes))
    {
        return Ok((login, false));
    }
    ensure_fresh(account, vault, max_age).await?;
    let item = match vault.get_item(key.id).await {
        Ok(item) => item,
        Err(VaultError::ItemMissing) => {
            vault_hosts::drop_mirror(key.id);
            return Err(VaultError::ItemMissing.into_remote(&host.alias));
        }
        Err(error) => return Err(item_failure(error, account, &host.alias)),
    };
    if item.is_deleted() {
        vault_hosts::drop_mirror(key.id);
        return Err(VaultError::ItemDeleted.into_remote(&host.alias));
    }
    if item.summary().id != key.id {
        return Err(unavailable("The Bitwarden response names another item."));
    }
    let facts =
        hosts::facts(item.summary()).map_err(|problem| problem_failure(&host.alias, &problem))?;
    let password = item.login_password().map_err(unavailable)?;
    Ok((
        Login {
            key,
            facts,
            password,
            epoch,
        },
        true,
    ))
}

pub(super) fn remember(
    account: &Account,
    vault: &Vault,
    max_age: u64,
    login: &Login,
) -> Result<(), RemoteFailure> {
    if account.profile.policy != ReauthPolicy::PerCall
        && login.epoch.is_some()
        && let Some(scope) = scope(account, vault, max_age)
    {
        logins()
            .remember(scope, login, cache::lifetime(account.cache_minutes))
            .map_err(unavailable)?;
    }
    Ok(())
}
