//! Vault items (ciphers): non-secret summaries for lists and a bounded
//! wrapper around one full item for reads and read-patch-write edits.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use serde_json::{Map, Value};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::host_uri::parse_host_uri;
use crate::parse::VaultParseError;

/// Name prefix of the text custom fields codex-hosts reads and writes
/// (`codex-hosts:tags`, `codex-hosts:description`, ...). Every other custom
/// field belongs to the user and is never read.
pub const CODEX_FIELD_PREFIX: &str = "codex-hosts:";
/// Bitwarden custom-field type code for a plain text field.
const FIELD_TYPE_TEXT: u8 = 0;

/// Bitwarden cipher types, by the numeric `type` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ItemKind {
    /// Type 1: account with username and password.
    Login,
    /// Type 2.
    SecureNote,
    /// Type 3.
    Card,
    /// Type 4.
    Identity,
    /// Type 5: SSH private key, public key, and fingerprint.
    SshKey,
    /// Any other type code.
    Unknown(u8),
}

impl ItemKind {
    fn from_code(code: u8) -> Self {
        match code {
            1 => ItemKind::Login,
            2 => ItemKind::SecureNote,
            3 => ItemKind::Card,
            4 => ItemKind::Identity,
            5 => ItemKind::SshKey,
            other => ItemKind::Unknown(other),
        }
    }

    /// The numeric `type` code.
    pub fn code(self) -> u8 {
        match self {
            ItemKind::Login => 1,
            ItemKind::SecureNote => 2,
            ItemKind::Card => 3,
            ItemKind::Identity => 4,
            ItemKind::SshKey => 5,
            ItemKind::Unknown(code) => code,
        }
    }
}

/// Everything a list or a status view may show about an item. Contains no
/// password, private key, TOTP seed, note, or custom field value other than
/// the text fields codex-hosts owns ([`CODEX_FIELD_PREFIX`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemSummary {
    /// Item id; a vault host stores this as its item reference.
    pub id: Uuid,
    /// Cipher type.
    pub kind: ItemKind,
    /// Display name.
    pub name: String,
    /// Folder, if any.
    pub folder_id: Option<Uuid>,
    /// Organisation, if the item is shared.
    pub organization_id: Option<Uuid>,
    /// Login username (Login items).
    pub username: Option<String>,
    /// Login URIs as stored; the first `ssh://` or `telnet://` one describes
    /// the host (see [`ItemSummary::host_uri`]).
    pub uris: Vec<String>,
    /// Text custom fields named `codex-hosts:<key>`, keyed by `<key>`.
    pub codex_fields: BTreeMap<String, String>,
    /// Public key in OpenSSH form (SSH key items).
    pub public_key: Option<String>,
    /// Public key fingerprint (SSH key items).
    pub key_fingerprint: Option<String>,
    /// Server revision timestamp; required for edits.
    pub revision_date: Option<String>,
    /// Set when the item is in the trash.
    pub deleted_date: Option<String>,
}

impl ItemSummary {
    /// The first URI that names an SSH or Telnet endpoint, if any.
    pub fn host_uri(&self) -> Option<crate::host_uri::HostUri> {
        self.uris.iter().find_map(|uri| parse_host_uri(uri))
    }

    /// A codex-hosts custom field by key (`tags`, `description`, ...).
    pub fn codex_field(&self, key: &str) -> Option<&str> {
        self.codex_fields.get(key).map(String::as_str)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItemHeader {
    id: Uuid,
    #[serde(rename = "type")]
    kind: u8,
    #[serde(default)]
    name: String,
    #[serde(default, deserialize_with = "crate::parse::optional_uuid")]
    folder_id: Option<Uuid>,
    #[serde(default, deserialize_with = "crate::parse::optional_uuid")]
    organization_id: Option<Uuid>,
    #[serde(default)]
    revision_date: Option<String>,
    #[serde(default)]
    deleted_date: Option<String>,
    #[serde(default)]
    login: Option<LoginHeader>,
    #[serde(default)]
    fields: Vec<FieldHeader>,
    #[serde(default)]
    ssh_key: Option<SshKeyHeader>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginHeader {
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    uris: Vec<UriHeader>,
}

#[derive(Deserialize)]
struct UriHeader {
    #[serde(default)]
    uri: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FieldHeader {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<u8>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SshKeyHeader {
    #[serde(default)]
    public_key: Option<String>,
    #[serde(default)]
    key_fingerprint: Option<String>,
}

impl From<ItemHeader> for ItemSummary {
    fn from(header: ItemHeader) -> Self {
        let (public_key, key_fingerprint) = header
            .ssh_key
            .map(|key| (key.public_key, key.key_fingerprint))
            .unwrap_or_default();
        let (username, uris) = header
            .login
            .map(|login| {
                (
                    login.username,
                    login.uris.into_iter().filter_map(|uri| uri.uri).collect(),
                )
            })
            .unwrap_or_default();
        let codex_fields = header
            .fields
            .into_iter()
            .filter(|field| field.kind.unwrap_or(FIELD_TYPE_TEXT) == FIELD_TYPE_TEXT)
            .filter_map(|field| {
                let key = field.name?.strip_prefix(CODEX_FIELD_PREFIX)?.to_owned();
                Some((key, field.value.unwrap_or_default()))
            })
            .collect();
        Self {
            id: header.id,
            kind: ItemKind::from_code(header.kind),
            name: header.name,
            folder_id: header.folder_id,
            organization_id: header.organization_id,
            username,
            uris,
            codex_fields,
            public_key,
            key_fingerprint,
            revision_date: header.revision_date,
            deleted_date: header.deleted_date,
        }
    }
}

/// Parses `bw list items` stdout into summaries. Secret fields are never
/// deserialized; the caller still owns (and zeroizes) the raw buffer.
pub fn parse_item_list(stdout: &[u8]) -> Result<Vec<ItemSummary>, VaultParseError> {
    let headers: Vec<ItemHeader> = serde_json::from_slice(stdout)
        .map_err(|error| crate::parse::classify(&error, "an item array", stdout))?;
    Ok(headers.into_iter().map(ItemSummary::from).collect())
}

/// How one field changes in a patch.
pub enum FieldEdit<T> {
    /// Leave the stored value as it is.
    Keep,
    /// Replace the stored value.
    Set(T),
    /// Remove the stored value (JSON `null`).
    Clear,
}

/// Edits to a Login item. Everything not named here is preserved verbatim
/// (other URIs, the user's own custom fields, TOTP, notes, reprompt,
/// `revisionDate`, ...). Deliberately no `Debug`: the password edit is a
/// secret.
pub struct LoginPatch {
    /// New display name, if changing.
    pub name: Option<String>,
    /// Username edit.
    pub username: FieldEdit<String>,
    /// Password edit; the UI must distinguish keep, set, and clear instead of
    /// treating an empty field as "overwrite with empty".
    pub password: FieldEdit<Zeroizing<String>>,
    /// Replaces the first `ssh://`/`telnet://` URI (or adds one) and leaves
    /// every other URI alone.
    pub host_uri: Option<String>,
    /// Replaces the complete set of `codex-hosts:` text fields; the user's
    /// other custom fields are kept.
    pub codex_fields: Option<BTreeMap<String, String>>,
}

impl Default for LoginPatch {
    fn default() -> Self {
        Self {
            name: None,
            username: FieldEdit::Keep,
            password: FieldEdit::Keep,
            host_uri: None,
            codex_fields: None,
        }
    }
}

/// One full item as returned by `bw get item`. The raw JSON stays in a
/// zeroizing buffer; secrets are extracted one field at a time and only on
/// request. Parsed `serde_json::Value` copies made during extraction or
/// patching are short-lived and are not zeroized (a known limit of
/// `serde_json`).
pub struct VaultItem {
    raw: Zeroizing<Vec<u8>>,
    summary: ItemSummary,
}

impl fmt::Debug for VaultItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VaultItem")
            .field("summary", &self.summary)
            .field("raw_len", &self.raw.len())
            .finish()
    }
}

impl VaultItem {
    /// Parses `bw get item` stdout, taking ownership of the buffer.
    pub fn parse(stdout: Zeroizing<Vec<u8>>) -> Result<Self, VaultParseError> {
        let header: ItemHeader = serde_json::from_slice(&stdout)
            .map_err(|error| crate::parse::classify(&error, "an item object", &stdout))?;
        Ok(Self {
            raw: stdout,
            summary: header.into(),
        })
    }

    /// The non-secret view.
    pub fn summary(&self) -> &ItemSummary {
        &self.summary
    }

    /// Whether the item is in the trash (`bw get item` does not exclude it).
    pub fn is_deleted(&self) -> bool {
        self.summary.deleted_date.is_some()
    }

    /// Fails unless the item has `kind`.
    pub fn expect_kind(&self, kind: ItemKind) -> Result<(), VaultParseError> {
        if self.summary.kind == kind {
            Ok(())
        } else {
            Err(VaultParseError::WrongKind {
                expected: kind,
                found: self.summary.kind,
            })
        }
    }

    /// The server revision the item was read at.
    pub fn revision_date(&self) -> Result<&str, VaultParseError> {
        self.summary
            .revision_date
            .as_deref()
            .ok_or(VaultParseError::MissingRevision)
    }

    /// Whether another client changed the item since `baseline_revision`
    /// was read. This is a pre-write check, not an atomic compare-and-swap.
    pub fn changed_since(&self, baseline_revision: &str) -> bool {
        self.summary.revision_date.as_deref() != Some(baseline_revision)
    }

    /// The Login password, if the item is a Login and has one.
    pub fn login_password(&self) -> Result<Option<Zeroizing<String>>, VaultParseError> {
        self.expect_kind(ItemKind::Login)?;
        self.take_string(&["login", "password"])
    }

    /// The SSH private key material, if the item is an SSH key.
    pub fn ssh_private_key(&self) -> Result<Option<Zeroizing<String>>, VaultParseError> {
        self.expect_kind(ItemKind::SshKey)?;
        self.take_string(&["sshKey", "privateKey"])
    }

    fn take_string(&self, path: &[&str]) -> Result<Option<Zeroizing<String>>, VaultParseError> {
        let mut value: Value = serde_json::from_slice(&self.raw)
            .map_err(|error| crate::parse::classify(&error, "an item object", &self.raw))?;
        let mut cursor = &mut value;
        for key in path {
            match cursor.get_mut(key) {
                Some(next) => cursor = next,
                None => return Ok(None),
            }
        }
        match cursor.take() {
            Value::String(text) => Ok(Some(Zeroizing::new(text))),
            Value::Null => Ok(None),
            _ => Err(VaultParseError::UnexpectedShape("a string field")),
        }
    }

    /// Applies `patch` to a copy of the full item and returns the complete
    /// JSON to send through `bw edit item` on stdin. Unedited fields and the
    /// original `revisionDate` are preserved.
    pub fn apply_login_patch(
        &self,
        patch: &LoginPatch,
    ) -> Result<Zeroizing<Vec<u8>>, VaultParseError> {
        self.expect_kind(ItemKind::Login)?;
        let mut value: Value = serde_json::from_slice(&self.raw)
            .map_err(|error| crate::parse::classify(&error, "an item object", &self.raw))?;
        let object = value
            .as_object_mut()
            .ok_or(VaultParseError::UnexpectedShape("an item object"))?;
        if !object.contains_key("revisionDate") {
            return Err(VaultParseError::MissingRevision);
        }
        if let Some(name) = &patch.name {
            object.insert("name".to_owned(), Value::String(name.clone()));
        }
        let login = object
            .entry("login")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or(VaultParseError::UnexpectedShape("a login object"))?;
        match &patch.username {
            FieldEdit::Keep => {}
            FieldEdit::Set(username) => {
                login.insert("username".to_owned(), Value::String(username.clone()));
            }
            FieldEdit::Clear => {
                login.insert("username".to_owned(), Value::Null);
            }
        }
        match &patch.password {
            FieldEdit::Keep => {}
            FieldEdit::Set(password) => {
                login.insert(
                    "password".to_owned(),
                    Value::String(password.as_str().to_owned()),
                );
            }
            FieldEdit::Clear => {
                login.insert("password".to_owned(), Value::Null);
            }
        }
        if let Some(host_uri) = &patch.host_uri {
            let uris = login
                .entry("uris")
                .or_insert_with(|| Value::Array(Vec::new()));
            let Value::Array(uris) = uris else {
                return Err(VaultParseError::UnexpectedShape("a uri array"));
            };
            let existing = uris.iter_mut().find(|entry| {
                entry
                    .get("uri")
                    .and_then(Value::as_str)
                    .is_some_and(|uri| parse_host_uri(uri).is_some())
            });
            match existing {
                Some(entry) => {
                    if let Some(object) = entry.as_object_mut() {
                        object.insert("uri".to_owned(), Value::String(host_uri.clone()));
                    }
                }
                None => uris.push(serde_json::json!({ "match": null, "uri": host_uri })),
            }
        }
        if let Some(codex_fields) = &patch.codex_fields {
            let fields = object
                .entry("fields")
                .or_insert_with(|| Value::Array(Vec::new()));
            let Value::Array(fields) = fields else {
                return Err(VaultParseError::UnexpectedShape("a field array"));
            };
            fields.retain(|field| {
                let ours = field
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.starts_with(CODEX_FIELD_PREFIX));
                let text = field
                    .get("type")
                    .and_then(Value::as_u64)
                    .is_none_or(|kind| kind == u64::from(FIELD_TYPE_TEXT));
                !(ours && text)
            });
            for (key, value) in codex_fields {
                fields.push(serde_json::json!({
                    "name": format!("{CODEX_FIELD_PREFIX}{key}"),
                    "value": value,
                    "type": FIELD_TYPE_TEXT,
                    "linkedId": null,
                }));
            }
        }
        serde_json::to_vec(&value)
            .map(Zeroizing::new)
            .map_err(|_| VaultParseError::UnexpectedShape("a serializable item"))
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::{Debug, Display};

    use super::*;

    static_assertions::assert_not_impl_any!(LoginPatch: Debug, Display, Clone);
    static_assertions::assert_not_impl_any!(VaultItem: Display, Clone);

    const LOGIN: &str = r#"{"object":"item","id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","organizationId":null,"folderId":"11111111-1111-1111-1111-111111111111","type":1,"reprompt":0,"name":"build-box ssh","notes":"keep","favorite":false,"fields":[{"name":"codex-hosts-host-id","value":"host-uuid","type":0,"linkedId":null}],"login":{"uris":[{"match":5,"uri":"ssh://10.0.0.5:22"}],"username":"deploy","password":"s3cr3t-pw","totp":"otpauth://x","passwordRevisionDate":null},"collectionIds":[],"revisionDate":"2026-11-02T01:00:00.000Z","creationDate":"2026-11-01T00:00:00.000Z","deletedDate":null,"unknownFutureField":{"keep":true}}"#;
    const SSH_KEY: &str = r#"{"object":"item","id":"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb","type":5,"name":"build-box key","folderId":null,"sshKey":{"privateKey":"-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----","publicKey":"ssh-ed25519 AAAAC3 comment","keyFingerprint":"SHA256:fp"},"revisionDate":"2026-11-02T02:00:00.000Z","deletedDate":"2026-11-02T03:00:00.000Z"}"#;

    fn login_item() -> VaultItem {
        VaultItem::parse(Zeroizing::new(LOGIN.as_bytes().to_vec())).unwrap()
    }

    #[test]
    fn list_projection_carries_no_secret_fields() {
        let list = format!("[{LOGIN},{SSH_KEY}]");
        let items = parse_item_list(list.as_bytes()).unwrap();
        assert_eq!(items.len(), 2);
        let login = &items[0];
        assert_eq!(login.kind, ItemKind::Login);
        assert_eq!(login.username.as_deref(), Some("deploy"));
        assert_eq!(
            login.folder_id,
            Some(Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap())
        );
        assert!(login.deleted_date.is_none());
        let key = &items[1];
        assert_eq!(key.kind, ItemKind::SshKey);
        assert_eq!(key.key_fingerprint.as_deref(), Some("SHA256:fp"));
        assert_eq!(
            key.public_key.as_deref(),
            Some("ssh-ed25519 AAAAC3 comment")
        );
        assert!(key.deleted_date.is_some());
        let shown = format!("{items:?}");
        assert!(!shown.contains("s3cr3t-pw"));
        assert!(!shown.contains("PRIVATE KEY"));
        assert!(!shown.contains("otpauth"));
        assert!(!shown.contains("keep"));
    }

    #[test]
    fn unknown_type_codes_survive() {
        let items = parse_item_list(
            br#"[{"id":"cccccccc-cccc-4ccc-8ccc-cccccccccccc","type":9,"name":"x","folderId":"","organizationId":null}]"#,
        )
        .unwrap();
        assert_eq!(items[0].kind, ItemKind::Unknown(9));
        assert_eq!(items[0].kind.code(), 9);
        assert_eq!(items[0].folder_id, None);
        assert_eq!(items[0].organization_id, None);
        assert_eq!(ItemKind::SshKey.code(), 5);
        assert_eq!(
            parse_item_list(b"[{\"name\":\"no id\"}]"),
            Err(VaultParseError::UnexpectedJson {
                expected: "an item array",
                found: "an array of 1 elements: 1 × {name: string}".to_owned(),
            })
        );
        assert_eq!(parse_item_list(b"["), Err(VaultParseError::NotJson));
    }

    #[test]
    fn full_item_exposes_secrets_only_on_request() {
        let item = login_item();
        assert_eq!(item.summary().name, "build-box ssh");
        assert!(!item.is_deleted());
        assert_eq!(item.revision_date().unwrap(), "2026-11-02T01:00:00.000Z");
        assert!(!item.changed_since("2026-11-02T01:00:00.000Z"));
        assert!(item.changed_since("2026-10-31T00:00:00.000Z"));
        assert_eq!(
            item.login_password().unwrap().unwrap().as_str(),
            "s3cr3t-pw"
        );
        assert!(matches!(
            item.ssh_private_key(),
            Err(VaultParseError::WrongKind {
                expected: ItemKind::SshKey,
                found: ItemKind::Login
            })
        ));
        let shown = format!("{item:?}");
        assert!(shown.contains("build-box ssh"));
        assert!(!shown.contains("s3cr3t-pw"));

        let key = VaultItem::parse(Zeroizing::new(SSH_KEY.as_bytes().to_vec())).unwrap();
        assert!(key.is_deleted());
        assert!(
            key.ssh_private_key()
                .unwrap()
                .unwrap()
                .starts_with("-----BEGIN OPENSSH PRIVATE KEY-----")
        );
        assert_eq!(
            key.expect_kind(ItemKind::Login),
            Err(VaultParseError::WrongKind {
                expected: ItemKind::Login,
                found: ItemKind::SshKey
            })
        );
    }

    #[test]
    fn patch_changes_only_the_named_fields_and_keeps_the_rest() {
        let item = login_item();
        let patch = LoginPatch {
            name: Some("build-box ssh (renamed)".to_owned()),
            username: FieldEdit::Set("ops".to_owned()),
            password: FieldEdit::Set(Zeroizing::new("n3w-pw".to_owned())),
            ..LoginPatch::default()
        };
        let bytes = item.apply_login_patch(&patch).unwrap();
        let patched: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(patched["name"], "build-box ssh (renamed)");
        assert_eq!(patched["login"]["username"], "ops");
        assert_eq!(patched["login"]["password"], "n3w-pw");
        assert_eq!(patched["revisionDate"], "2026-11-02T01:00:00.000Z");
        assert_eq!(patched["login"]["totp"], "otpauth://x");
        assert_eq!(patched["login"]["uris"][0]["uri"], "ssh://10.0.0.5:22");
        assert_eq!(patched["fields"][0]["name"], "codex-hosts-host-id");
        assert_eq!(patched["notes"], "keep");
        assert_eq!(patched["unknownFutureField"]["keep"], true);
        assert_eq!(patched["id"], "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
        assert_eq!(patched["type"], 1);
    }

    const HOST_ITEM: &str = r#"{"object":"item","id":"dddddddd-dddd-4ddd-8ddd-dddddddddddd","type":1,"name":"build-box","folderId":"11111111-1111-1111-1111-111111111111","notes":"private note","fields":[{"name":"codex-hosts:tags","value":"prod, web","type":0,"linkedId":null},{"name":"codex-hosts:description","value":"front door","type":0,"linkedId":null},{"name":"codex-hosts:secret","value":"hidden","type":1,"linkedId":null},{"name":"my-field","value":"mine","type":0,"linkedId":null}],"login":{"uris":[{"match":null,"uri":"https://wiki.example/build-box"},{"match":null,"uri":"ssh://deploy@10.0.0.5:2222"}],"username":"deploy","password":"pw"},"revisionDate":"2026-11-03T00:00:00.000Z"}"#;

    #[test]
    fn summaries_expose_host_uris_and_only_our_text_fields() {
        let items = parse_item_list(format!("[{HOST_ITEM}]").as_bytes()).unwrap();
        let item = &items[0];
        assert_eq!(
            item.uris,
            [
                "https://wiki.example/build-box",
                "ssh://deploy@10.0.0.5:2222"
            ]
        );
        let uri = item.host_uri().unwrap();
        assert_eq!(uri.host, "10.0.0.5");
        assert_eq!(uri.port, Some(2222));
        assert_eq!(uri.username.as_deref(), Some("deploy"));
        assert_eq!(item.codex_field("tags"), Some("prod, web"));
        assert_eq!(item.codex_field("description"), Some("front door"));
        assert_eq!(
            item.codex_field("secret"),
            None,
            "hidden fields stay private"
        );
        assert_eq!(item.codex_fields.len(), 2);
        let shown = format!("{item:?}");
        assert!(!shown.contains("private note"));
        assert!(!shown.contains("mine"));
        assert!(!shown.contains("hidden"));
    }

    #[test]
    fn patch_replaces_the_host_uri_and_our_fields_but_keeps_the_users_own() {
        let item = VaultItem::parse(Zeroizing::new(HOST_ITEM.as_bytes().to_vec())).unwrap();
        let patch = LoginPatch {
            host_uri: Some("telnet://switch.local".to_owned()),
            codex_fields: Some(BTreeMap::from([("tags".to_owned(), "lab".to_owned())])),
            ..LoginPatch::default()
        };
        let patched: Value =
            serde_json::from_slice(&item.apply_login_patch(&patch).unwrap()).unwrap();
        let uris = patched["login"]["uris"].as_array().unwrap();
        assert_eq!(uris.len(), 2);
        assert_eq!(uris[0]["uri"], "https://wiki.example/build-box");
        assert_eq!(uris[1]["uri"], "telnet://switch.local");
        let fields = patched["fields"].as_array().unwrap();
        let names: Vec<&str> = fields
            .iter()
            .map(|field| field["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["codex-hosts:secret", "my-field", "codex-hosts:tags"],
            "hidden and foreign fields survive, our text fields are rewritten"
        );
        assert_eq!(fields[2]["value"], "lab");
        assert_eq!(fields[2]["type"], 0);
        assert_eq!(patched["notes"], "private note");

        let bare = login_item();
        let added: Value = serde_json::from_slice(
            &bare
                .apply_login_patch(&LoginPatch {
                    host_uri: Some("ssh://ops@10.0.0.9".to_owned()),
                    ..LoginPatch::default()
                })
                .unwrap(),
        )
        .unwrap();
        assert_eq!(added["login"]["uris"][0]["uri"], "ssh://ops@10.0.0.9");
        assert_eq!(added["login"]["uris"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn keep_and_clear_are_distinct_from_setting_an_empty_value() {
        let item = login_item();
        let kept: Value =
            serde_json::from_slice(&item.apply_login_patch(&LoginPatch::default()).unwrap())
                .unwrap();
        assert_eq!(kept["login"]["password"], "s3cr3t-pw");
        assert_eq!(kept["login"]["username"], "deploy");

        let cleared: Value = serde_json::from_slice(
            &item
                .apply_login_patch(&LoginPatch {
                    username: FieldEdit::Clear,
                    password: FieldEdit::Clear,
                    ..LoginPatch::default()
                })
                .unwrap(),
        )
        .unwrap();
        assert_eq!(cleared["login"]["password"], Value::Null);
        assert_eq!(cleared["login"]["username"], Value::Null);
        assert_eq!(cleared["name"], "build-box ssh");

        let emptied: Value = serde_json::from_slice(
            &item
                .apply_login_patch(&LoginPatch {
                    password: FieldEdit::Set(Zeroizing::new(String::new())),
                    ..LoginPatch::default()
                })
                .unwrap(),
        )
        .unwrap();
        assert_eq!(emptied["login"]["password"], "");
    }

    #[test]
    fn patch_refuses_the_wrong_kind_and_items_without_a_revision() {
        let key = VaultItem::parse(Zeroizing::new(SSH_KEY.as_bytes().to_vec())).unwrap();
        assert!(matches!(
            key.apply_login_patch(&LoginPatch::default()),
            Err(VaultParseError::WrongKind { .. })
        ));
        let no_revision = VaultItem::parse(Zeroizing::new(
            br#"{"id":"dddddddd-dddd-4ddd-8ddd-dddddddddddd","type":1,"name":"x","login":{"password":"p"}}"#.to_vec(),
        ))
        .unwrap();
        assert_eq!(
            no_revision.revision_date(),
            Err(VaultParseError::MissingRevision)
        );
        assert!(matches!(
            no_revision.apply_login_patch(&LoginPatch::default()),
            Err(VaultParseError::MissingRevision)
        ));
        assert!(no_revision.changed_since("anything"));
    }
}
