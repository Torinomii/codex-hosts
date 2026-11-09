//! Short-lived login reads, scoped to the configured app, account and vault.

use std::path::PathBuf;
use std::sync::OnceLock;

use super::*;
use crate::vault_hosts::cache::{self, Login, Logins};

#[derive(PartialEq)]
struct Scope {
    exe: PathBuf,
    account: OnePasswordAccount,
    epoch: Option<cache::Epoch>,
    minutes: u32,
}

fn logins() -> &'static Logins<Scope> {
    static LOGINS: OnceLock<Logins<Scope>> = OnceLock::new();
    LOGINS.get_or_init(Logins::new)
}

fn scope<R: OpRunner>(op: &Op<R>) -> Scope {
    Scope {
        exe: op.exe.path().to_owned(),
        account: op.account.clone(),
        epoch: cache::Epoch::current(VaultProvider::OnePassword),
        minutes: op.cache_minutes,
    }
}

pub(super) fn clear() {
    logins().clear();
}

/// The boolean says a CLI read occurred; cache hits renew their idle expiry.
pub(super) async fn read<R: OpRunner>(
    op: &Op<R>,
    host: &HostProfile,
) -> Result<(Login, bool), RemoteFailure> {
    let item_id = external_id(host)?;
    let key = op.item_key(&item_id);
    if key.id != host.id {
        clear();
        return Err(failure(OpError::BadId, &host.alias));
    }
    let epoch = cache::Epoch::current(VaultProvider::OnePassword);
    if epoch.is_some()
        && let Some(login) = logins().get(&scope(op), host, cache::lifetime(op.cache_minutes))
    {
        return Ok((login, false));
    }
    let item = read_item(op, host).await?;
    if item.summary().id != item_id || item.summary().vault.id != op.account.vault_id {
        return Err(failure(OpError::Parse, &host.alias));
    }
    let facts = item_facts(&item).map_err(|problem| problem_failure(&host.alias, &problem))?;
    let password = item
        .password()
        .map_err(|error| failure(error, &host.alias))?;
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

pub(super) fn remember<R: OpRunner>(op: &Op<R>, login: &Login) -> Result<(), RemoteFailure> {
    if login.epoch.is_none() {
        return Ok(());
    }
    logins()
        .remember(scope(op), login, cache::lifetime(op.cache_minutes))
        .map_err(|error| RemoteFailure::new("ONEPASSWORD_UNAVAILABLE", error))
}
