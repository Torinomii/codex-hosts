//! One unlocked, synced read for a GUI connection-test operation.

use std::collections::HashMap;

use super::*;
use crate::vault_hosts::probe::{Entries, Prepared};

pub(crate) async fn load(hosts: &[HostProfile]) -> Result<Entries, RemoteFailure> {
    let _turn = CLI_GATE.lock().await;
    let settings = settings()?;
    let account = Account::from_settings(&settings)?;
    let mut vault = unlocked_or_prompt(&account).await?;
    ensure_fresh(&account, &mut vault, settings.sync_max_age_seconds).await?;
    let items = vault
        .read_items(folder_of(&account)?)
        .await
        .map_err(|error| item_failure(error, &account, &account.profile.folder_name))?;
    let items: HashMap<_, _> = items
        .into_iter()
        .map(|item| (item.summary().id, item))
        .collect();
    session::touch(account.id()).map_err(unavailable)?;
    let session = vault
        .take_session()
        .ok_or_else(|| VaultError::Locked.into_remote(&account.profile.email))?;
    let fingerprint = session::fingerprint(&session);
    Ok(hosts
        .iter()
        .map(|host| {
            let result = (|| {
                let key = ItemKey::of(host).ok_or_else(|| unavailable("not a vault host"))?;
                let item = items
                    .get(&key.id)
                    .ok_or_else(|| VaultError::ItemMissing.into_remote(&host.alias))?;
                if item.is_deleted() {
                    return Err(VaultError::ItemDeleted.into_remote(&host.alias));
                }
                let facts = hosts::facts(item.summary())
                    .map_err(|problem| problem_failure(&host.alias, &problem))?;
                let password = item
                    .login_password()
                    .map_err(|error| unavailable(error.to_string()))?;
                let admission = vault_hosts::Admission::Bitwarden(Admission {
                    item_id: key.id,
                    revision: facts.revision.clone(),
                    session_fingerprint: fingerprint,
                });
                let login = vault_hosts::VaultLogin::Bitwarden(VaultLogin {
                    session: SessionKey::new(session.expose().to_owned()),
                    password,
                });
                Ok(Prepared::new(host, key, facts, admission, login))
            })();
            (host.id, result)
        })
        .collect())
}
