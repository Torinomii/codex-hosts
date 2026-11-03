//! Vault-backed hosts: the Login items of the configured Bitwarden folder,
//! mirrored into the host store so the window, the tools, and the session
//! pool see them as ordinary profiles with local state attached.
//!
//! The item is the source of truth. Its name is the alias; its first
//! `ssh://` or `telnet://` URI is the endpoint; its username and password
//! are the login; the `codex-hosts:description` and `codex-hosts:tags`
//! custom fields carry the remark and the tags. Everything else in the
//! profile (host-key trust, privilege settings, approval, retention, jump
//! host, advanced limits) is local and survives every refresh.

use codex_hosts_bw::{ItemKind, ItemSummary};
use uuid::Uuid;

use crate::model::{HostProfile, Protocol, VaultHostRef, normalize_tags};
use crate::storage::HostStore;

/// Custom field holding the host description.
pub(crate) const DESCRIPTION_FIELD: &str = "description";
/// Custom field holding the tags, comma separated.
pub(crate) const TAGS_FIELD: &str = "tags";

/// Why an item of the folder is not served as a host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ItemProblem {
    /// Not a Login item.
    NotLogin(ItemKind),
    /// The name is empty.
    NoName,
    /// No `ssh://` or `telnet://` URI.
    NoHostUri,
    /// Neither the item nor its URI names a user.
    NoUsername,
    /// A local host or another item already uses the name.
    NameTaken,
}

/// What a Login item says about its host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HostFacts {
    pub(crate) alias: String,
    pub(crate) protocol: Protocol,
    pub(crate) address: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) description: String,
    pub(crate) tags: Vec<String>,
    pub(crate) revision: String,
}

/// Reads the host facts out of an item summary.
pub(crate) fn facts(item: &ItemSummary) -> Result<HostFacts, ItemProblem> {
    if item.kind != ItemKind::Login {
        return Err(ItemProblem::NotLogin(item.kind));
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
        description: item
            .codex_field(DESCRIPTION_FIELD)
            .and_then(|text| text.lines().next())
            .unwrap_or_default()
            .trim()
            .to_owned(),
        tags: parse_tags(item.codex_field(TAGS_FIELD).unwrap_or_default()),
        revision: item.revision_date.clone().unwrap_or_default(),
    })
}

/// Splits the `codex-hosts:tags` value (commas or whitespace).
pub(crate) fn parse_tags(text: &str) -> Vec<String> {
    normalize_tags(text.split([',', ';', '\n']).flat_map(str::split_whitespace))
}

/// Joins tags for the `codex-hosts:tags` value.
pub(crate) fn format_tags(tags: &[String]) -> String {
    normalize_tags(tags).join(", ")
}

/// Writes the facts into a mirror profile; local state (including the SSH
/// authentication method: a vault host may log in with a local key or the
/// agent, the item password serves password logins) is untouched. A changed
/// endpoint drops the host-key trust, as the editor does, so the next
/// connection verifies the new server afresh.
pub(crate) fn apply(facts: &HostFacts, item_id: Uuid, host: &mut HostProfile) -> bool {
    let before = host.clone();
    let endpoint_changed = host.address.trim() != facts.address
        || host.port != facts.port
        || host.protocol != facts.protocol;
    host.alias = facts.alias.clone();
    host.description = facts.description.clone();
    host.tags = facts.tags.clone();
    host.address = facts.address.clone();
    host.port = facts.port;
    host.protocol = facts.protocol;
    host.username = facts.username.clone();
    host.login_password_id = None;
    host.vault = Some(VaultHostRef {
        item_id,
        item_name: facts.alias.clone(),
        revision_date: facts.revision.clone(),
    });
    if endpoint_changed {
        host.host_fingerprint = None;
        host.host_key_algorithm = None;
        host.host_key_first_seen_unix = None;
        host.host_key_last_verified_unix = None;
        host.verified = false;
    }
    *host != before
}

/// A fresh mirror for an item seen for the first time. The profile id is the
/// item id, so Credential Manager records (sudo passwords) and pool keys stay
/// stable even if the mirror is rebuilt.
pub(crate) fn mirror(facts: &HostFacts, item_id: Uuid) -> HostProfile {
    let mut host = HostProfile::new(facts.alias.clone());
    host.id = item_id;
    apply(facts, item_id, &mut host);
    host
}

/// Result of bringing the store's mirrors in line with the folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Reconciled {
    /// Whether the store needs saving.
    pub(crate) changed: bool,
    /// Number of vault hosts after the pass.
    pub(crate) hosts: usize,
    /// Items of the folder that are not served, by name.
    pub(crate) problems: Vec<(String, ItemProblem)>,
}

/// Mirrors `items` (the folder's listing) into `store.hosts`: updates or
/// creates one profile per usable Login item, drops mirrors whose item is
/// gone, and leaves local hosts alone. Names are compared like aliases
/// (trimmed, ASCII case-insensitive); a clash with a local host or an
/// already-served item is reported, not resolved.
pub(crate) fn reconcile(store: &mut HostStore, items: &[ItemSummary], folder: Uuid) -> Reconciled {
    let mut result = Reconciled::default();
    let mut taken: Vec<String> = store
        .hosts
        .iter()
        .filter(|host| !host.is_vault_host())
        .map(|host| host.alias.trim().to_ascii_lowercase())
        .collect();
    // Items already mirrored keep their name against newcomers.
    let mut candidates: Vec<&ItemSummary> = items
        .iter()
        .filter(|item| item.folder_id == Some(folder) && item.deleted_date.is_none())
        .collect();
    candidates.sort_by_key(|item| !store.hosts.iter().any(|host| host.id == item.id));
    let mut accepted: Vec<(Uuid, HostFacts)> = Vec::new();
    for item in candidates {
        match facts(item) {
            Ok(facts) => {
                let key = facts.alias.to_ascii_lowercase();
                if taken.contains(&key) {
                    result
                        .problems
                        .push((item.name.clone(), ItemProblem::NameTaken));
                } else {
                    taken.push(key);
                    accepted.push((item.id, facts));
                }
            }
            Err(problem) => result.problems.push((item.name.clone(), problem)),
        }
    }
    let before = store.hosts.len();
    store
        .hosts
        .retain(|host| !host.is_vault_host() || accepted.iter().any(|(id, _)| *id == host.id));
    result.changed |= store.hosts.len() != before;
    for (item_id, facts) in &accepted {
        match store.hosts.iter_mut().find(|host| host.id == *item_id) {
            Some(host) => result.changed |= apply(facts, *item_id, host),
            None => {
                store.hosts.push(mirror(facts, *item_id));
                result.changed = true;
            }
        }
    }
    result.hosts = accepted.len();
    result
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

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
        assert_eq!(
            facts(&note),
            Err(ItemProblem::NotLogin(ItemKind::SecureNote))
        );
        assert_eq!(parse_tags(" a,b;c\nd  e "), ["a", "b", "c", "d", "e"]);
        assert_eq!(
            format_tags(&["b".to_owned(), "a".to_owned(), "b".to_owned()]),
            "b, a"
        );
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
