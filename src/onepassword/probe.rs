//! Bulk reads only the items used by one GUI connection-test operation.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::vault_hosts::probe::{Entries, Prepared};

pub(crate) async fn load(hosts: &[HostProfile]) -> Result<Entries, RemoteFailure> {
    let _turn = CLI_GATE.lock().await;
    let op = Op::from_settings(&settings()?)?;
    load_with(&op, hosts).await
}

async fn load_with<R: OpRunner>(
    op: &Op<R>,
    hosts: &[HostProfile],
) -> Result<Entries, RemoteFailure> {
    // Filter vanished IDs before the bulk read, so one removed item does not
    // fail every otherwise valid host in the operation.
    let listed = op.list_items().await.map_err(|error| failure(error, ""))?;
    let present: HashSet<_> = listed
        .iter()
        .filter(|item| item.vault.id == op.account.vault_id)
        .map(|item| item.id.as_str())
        .collect();
    let ids: HashSet<_> = hosts
        .iter()
        .filter_map(|host| host.vault()?.external_id.as_deref())
        .filter(|id| present.contains(id))
        .collect();
    let mut ids: Vec<_> = ids.into_iter().collect();
    ids.sort_unstable();
    let mut items = HashMap::new();
    if !ids.is_empty() {
        let output = op
            .run(|| OpCall::items_get(&op.account.account_id, &op.account.vault_id, &ids))
            .await
            .map_err(|error| failure(error, ""))?;
        for item in cli::parse_items(output).map_err(|error| failure(error, ""))? {
            if item.summary().vault.id != op.account.vault_id
                || !ids.contains(&item.summary().id.as_str())
                || items.contains_key(&item.summary().id)
            {
                return Err(failure(OpError::Parse, ""));
            }
            items.insert(item.summary().id.clone(), item);
        }
    }
    Ok(hosts
        .iter()
        .map(|host| {
            let result = (|| {
                let id = external_id(host)?;
                let item = items
                    .get(&id)
                    .ok_or_else(|| failure(OpError::ItemMissing, &host.alias))?;
                let facts =
                    item_facts(item).map_err(|problem| problem_failure(&host.alias, &problem))?;
                let key = op.item_key(&id);
                if key.id != host.id {
                    return Err(failure(OpError::BadId, &host.alias));
                }
                let password = item
                    .password()
                    .map_err(|error| failure(error, &host.alias))?;
                let admission = Admission::OnePassword {
                    item_id: key.id,
                    revision: facts.revision.clone(),
                };
                Ok(Prepared::new(
                    host,
                    key,
                    facts,
                    admission,
                    VaultLogin::OnePassword(password),
                ))
            })();
            (host.id, result)
        })
        .collect())
}
