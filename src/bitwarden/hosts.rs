//! Bitwarden's side of vault-backed hosts: reading the Login items of the
//! configured folder into [`HostFacts`]. The item's custom fields are named
//! `codex-hosts:description` and `codex-hosts:tags`; the mapping into
//! profiles is shared with other providers in [`crate::vault_hosts`].

use codex_hosts_bw::{ItemKind, ItemSummary};
use uuid::Uuid;

use crate::model::{Protocol, VaultProvider};
use crate::storage::HostStore;
use crate::vault_hosts::{
    self, DESCRIPTION_FIELD, HostFacts, ItemKey, ItemProblem, Listed, Reconciled, TAGS_FIELD,
};

/// Reads the host facts out of an item summary.
pub(crate) fn facts(item: &ItemSummary) -> Result<HostFacts, ItemProblem> {
    if item.kind != ItemKind::Login {
        return Err(ItemProblem::NotLogin);
    }
    let alias = item.name.trim();
    if alias.is_empty() {
        return Err(ItemProblem::NoName);
    }
    let uri = item.host_uri().ok_or(ItemProblem::NoHostUri)?;
    let username = item
        .username
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .or(uri.username.clone())
        .ok_or(ItemProblem::NoUsername)?;
    let protocol = match uri.scheme {
        codex_hosts_bw::UriScheme::Ssh => Protocol::Ssh,
        codex_hosts_bw::UriScheme::Telnet => Protocol::Telnet,
    };
    Ok(HostFacts {
        alias: alias.to_owned(),
        protocol,
        address: uri.host.clone(),
        port: uri.port_or_default(),
        username,
        description: vault_hosts::description_line(
            item.codex_field(DESCRIPTION_FIELD).unwrap_or_default(),
        ),
        tags: vault_hosts::parse_tags(item.codex_field(TAGS_FIELD).unwrap_or_default()),
        revision: item.revision_date.clone().unwrap_or_default(),
    })
}

/// Mirrors `items` (the folder's listing) into `store.hosts`; items outside
/// `folder` or in the trash are ignored.
pub(crate) fn reconcile(store: &mut HostStore, items: &[ItemSummary], folder: Uuid) -> Reconciled {
    let listed = items
        .iter()
        .filter(|item| item.folder_id == Some(folder) && item.deleted_date.is_none())
        .map(|item| Listed {
            key: ItemKey::bitwarden(item.id),
            name: item.name.clone(),
            facts: facts(item),
        })
        .collect();
    vault_hosts::reconcile(store, VaultProvider::Bitwarden, listed)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::model::HostProfile;

    fn item(id: u8, name: &str, uri: &str, username: Option<&str>) -> ItemSummary {
        ItemSummary {
            id: Uuid::from_u128(u128::from(id)),
            kind: ItemKind::Login,
            name: name.to_owned(),
            folder_id: Some(folder()),
            organization_id: None,
            username: username.map(str::to_owned),
            uris: vec!["https://wiki.example".to_owned(), uri.to_owned()],
            codex_fields: BTreeMap::from([
                (TAGS_FIELD.to_owned(), "prod, web web".to_owned()),
                (
                    DESCRIPTION_FIELD.to_owned(),
                    "front door\nsecond line".to_owned(),
                ),
            ]),
            public_key: None,
            key_fingerprint: None,
            revision_date: Some("2026-11-03T00:00:00.000Z".to_owned()),
            deleted_date: None,
        }
    }

    fn folder() -> Uuid {
        Uuid::from_u128(0xF01D)
    }

    #[test]
    fn facts_come_from_the_uri_the_username_and_our_fields() {
        let read = facts(&item(
            1,
            " build-box ",
            "ssh://10.0.0.5:2222",
            Some("deploy"),
        ))
        .unwrap();
        assert_eq!(read.alias, "build-box");
        assert_eq!(read.protocol, Protocol::Ssh);
        assert_eq!(read.address, "10.0.0.5");
        assert_eq!(read.port, 2222);
        assert_eq!(read.username, "deploy");
        assert_eq!(read.description, "front door");
        assert_eq!(read.tags, ["prod", "web"]);
        assert_eq!(read.revision, "2026-11-03T00:00:00.000Z");

        let from_uri = facts(&item(2, "switch", "telnet://ops@switch.local", None)).unwrap();
        assert_eq!(from_uri.protocol, Protocol::Telnet);
        assert_eq!(from_uri.port, 23);
        assert_eq!(from_uri.username, "ops");
    }

    #[test]
    fn items_without_the_essentials_are_reported() {
        assert_eq!(
            facts(&item(1, "x", "https://only.web", Some("u"))),
            Err(ItemProblem::NoHostUri)
        );
        assert_eq!(
            facts(&item(1, "x", "ssh://host", None)),
            Err(ItemProblem::NoUsername)
        );
        assert_eq!(
            facts(&item(1, "  ", "ssh://u@host", None)),
            Err(ItemProblem::NoName)
        );
        let mut note = item(1, "n", "ssh://u@host", None);
        note.kind = ItemKind::SecureNote;
        assert_eq!(facts(&note), Err(ItemProblem::NotLogin));
    }

    #[test]
    fn reconcile_mirrors_updates_removes_and_reports_clashes() {
        let mut store = HostStore::default();
        let mut local = HostProfile::new("edge".to_owned());
        local.address = "1.2.3.4".to_owned();
        store.hosts.push(local);

        let items = vec![
            item(1, "build-box", "ssh://10.0.0.5:2222", Some("deploy")),
            item(2, "Edge", "ssh://u@h", None),
            item(3, "dup", "ssh://u@h1", None),
            item(4, "DUP", "ssh://u@h2", None),
            item(5, "no-uri", "https://x", Some("u")),
        ];
        let first = reconcile(&mut store, &items, folder());
        assert!(first.changed);
        assert_eq!(first.hosts, 2);
        assert_eq!(
            first.problems,
            vec![
                ("Edge".to_owned(), ItemProblem::NameTaken),
                ("DUP".to_owned(), ItemProblem::NameTaken),
                ("no-uri".to_owned(), ItemProblem::NoHostUri),
            ]
        );
        let mirrored = store.find_alias("build-box").unwrap().clone();
        assert_eq!(mirrored.id, Uuid::from_u128(1));
        assert_eq!(mirrored.vault.as_ref().unwrap().item_id, Uuid::from_u128(1));
        assert_eq!(mirrored.username, "deploy");
        assert_eq!(mirrored.port, 2222);
        assert_eq!(mirrored.tags, ["prod", "web"]);
        assert!(store.find_alias("edge").unwrap().vault.is_none());

        // Local state survives a refresh; a moved endpoint drops the trust.
        let host = store
            .hosts
            .iter_mut()
            .find(|host| host.id == Uuid::from_u128(1))
            .unwrap();
        host.advanced.require_call_approval = true;
        host.host_fingerprint = Some("SHA256:old".to_owned());
        host.verified = true;
        let unchanged = reconcile(&mut store, &items, folder());
        assert!(
            !unchanged.changed,
            "a second pass over the same items is a no-op"
        );
        assert!(store.find_alias("build-box").unwrap().verified);

        let mut moved = items.clone();
        moved[0] = item(1, "build-box", "ssh://10.0.0.6:2222", Some("deploy"));
        moved[0].revision_date = Some("2026-11-04T00:00:00.000Z".to_owned());
        moved.remove(2); // "dup" disappears: "DUP" takes over the name
        let second = reconcile(&mut store, &moved, folder());
        assert!(second.changed);
        let refreshed = store.find_alias("build-box").unwrap();
        assert_eq!(refreshed.address, "10.0.0.6");
        assert!(refreshed.advanced.require_call_approval, "local state kept");
        assert!(!refreshed.verified, "trust dropped with the endpoint");
        assert_eq!(
            refreshed.vault.as_ref().unwrap().revision_date,
            "2026-11-04T00:00:00.000Z"
        );
        assert_eq!(store.find_alias("dup").unwrap().id, Uuid::from_u128(4));

        let gone = reconcile(&mut store, &[], folder());
        assert!(gone.changed);
        assert_eq!(gone.hosts, 0);
        assert_eq!(store.hosts.len(), 1, "only the local host remains");
    }
}
