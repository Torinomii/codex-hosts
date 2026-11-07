//! Bitwarden vault data as codex-hosts sees it: non-secret projections of
//! status, folders, and item lists, plus bounded handling of one full item
//! in [`VaultItem`]. Whole `bw` responses never leave this module as plain
//! strings.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::item::ItemKind;
use uuid::Uuid;

/// Why a `bw` response could not be interpreted. Messages never include
/// response content.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum VaultParseError {
    /// The response is not valid JSON.
    #[error("bw output is not valid JSON")]
    NotJson,
    /// The JSON does not have the expected shape; `found` names the types
    /// and keys actually present (see [`describe`]), never any value.
    #[error("bw output is not {expected}: found {found}")]
    UnexpectedJson {
        /// What the operation needed, for example "a folder array".
        expected: &'static str,
        /// Value-free description of what `bw` printed.
        found: String,
    },
    /// A parsed value lacks something the operation needs.
    #[error("bw output is not {0}")]
    UnexpectedShape(&'static str),
    /// `bw status` reported a status codex-hosts does not know.
    #[error("unknown vault status {0:?}")]
    UnknownStatus(String),
    /// The item is not of the kind the operation needs.
    #[error("expected a {expected:?} item, found {found:?}")]
    WrongKind {
        /// Kind the operation needs.
        expected: ItemKind,
        /// Kind the item actually has.
        found: ItemKind,
    },
    /// The item carries no `revisionDate`, so it cannot be edited safely.
    #[error("item has no revisionDate")]
    MissingRevision,
}

pub(crate) fn classify(
    error: &serde_json::Error,
    expected: &'static str,
    stdout: &[u8],
) -> VaultParseError {
    if error.is_syntax() || error.is_eof() {
        VaultParseError::NotJson
    } else {
        VaultParseError::UnexpectedJson {
            expected,
            found: describe(stdout),
        }
    }
}

/// Distinct element shapes listed for an array before "…".
const MAX_ARRAY_SHAPES: usize = 4;

/// Describes a JSON document by types and object keys only, so a parse
/// failure can be shown and recorded without exposing vault content. Keys
/// are `bw`'s own field names; nested values appear only as their type.
pub(crate) fn describe(stdout: &[u8]) -> String {
    let Ok(value) = serde_json::from_slice::<Value>(stdout) else {
        return "invalid JSON".to_owned();
    };
    let Value::Array(items) = &value else {
        return describe_shallow(&value);
    };
    if items.is_empty() {
        return "an empty array".to_owned();
    }
    let mut shapes: Vec<(String, usize)> = Vec::new();
    for item in items {
        let shape = describe_shallow(item);
        match shapes.iter_mut().find(|(seen, _)| *seen == shape) {
            Some((_, count)) => *count += 1,
            None => shapes.push((shape, 1)),
        }
    }
    let listed = shapes
        .iter()
        .take(MAX_ARRAY_SHAPES)
        .map(|(shape, count)| format!("{count} × {shape}"))
        .collect::<Vec<_>>()
        .join(", ");
    let more = if shapes.len() > MAX_ARRAY_SHAPES {
        ", …"
    } else {
        ""
    };
    format!("an array of {} elements: {listed}{more}", items.len())
}

fn describe_shallow(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut fields: Vec<String> = map
                .iter()
                .map(|(key, value)| format!("{key}: {}", type_name(value)))
                .collect();
            fields.sort_unstable();
            format!("{{{}}}", fields.join(", "))
        }
        other => type_name(other).to_owned(),
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(text) if text.is_empty() => "empty string",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Deserializes an optional id the way current `bw` builds print one: a UUID
/// string, `null`, or `""` (the view classes default `id` to an empty string,
/// so the "No Folder" pseudo entry carries `"id": ""`).
pub(crate) fn optional_uuid<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Uuid>, D::Error> {
    let raw: Option<String> = Option::deserialize(deserializer)?;
    match raw.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(text) => Uuid::parse_str(text)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

/// Authentication state reported by `bw status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VaultStatus {
    /// Not logged in.
    Unauthenticated,
    /// Logged in, vault locked.
    Locked,
    /// Logged in and unlocked for the session that asked.
    Unlocked,
}

/// Non-secret `bw status` report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusReport {
    /// Authentication state.
    pub status: VaultStatus,
    /// Configured server, if any.
    pub server_url: Option<String>,
    /// Signed-in account, if any.
    pub user_email: Option<String>,
    /// Last sync timestamp as the CLI reports it.
    pub last_sync: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatusJson {
    status: String,
    #[serde(default)]
    server_url: Option<String>,
    #[serde(default)]
    user_email: Option<String>,
    #[serde(default)]
    last_sync: Option<String>,
}

/// Parses `bw status` stdout.
pub fn parse_status(stdout: &[u8]) -> Result<StatusReport, VaultParseError> {
    let json: StatusJson = serde_json::from_slice(stdout)
        .map_err(|error| classify(&error, "a status object", stdout))?;
    let status = match json.status.as_str() {
        "unauthenticated" => VaultStatus::Unauthenticated,
        "locked" => VaultStatus::Locked,
        "unlocked" => VaultStatus::Unlocked,
        other => return Err(VaultParseError::UnknownStatus(other.to_owned())),
    };
    Ok(StatusReport {
        status,
        server_url: json.server_url,
        user_email: json.user_email,
        last_sync: json.last_sync,
    })
}

/// A real folder; the CLI's "No Folder" pseudo entry (`null` or empty id)
/// is dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultFolder {
    /// Folder id; settings store this, never the name.
    pub id: Uuid,
    /// Decrypted name. Names may repeat and nest only by `/` convention.
    pub name: String,
}

#[derive(Deserialize)]
struct FolderJson {
    #[serde(default, deserialize_with = "optional_uuid")]
    id: Option<Uuid>,
    #[serde(default)]
    name: String,
}

/// Parses `bw list folders` stdout.
pub fn parse_folders(stdout: &[u8]) -> Result<Vec<VaultFolder>, VaultParseError> {
    let folders: Vec<FolderJson> = serde_json::from_slice(stdout)
        .map_err(|error| classify(&error, "a folder array", stdout))?;
    Ok(folders
        .into_iter()
        .filter_map(|folder| {
            folder.id.map(|id| VaultFolder {
                id,
                name: folder.name,
            })
        })
        .collect())
}

/// Parses the single folder object `bw create folder` prints.
pub fn parse_folder(stdout: &[u8]) -> Result<VaultFolder, VaultParseError> {
    let folder: FolderJson = serde_json::from_slice(stdout)
        .map_err(|error| classify(&error, "a folder object", stdout))?;
    let id = folder.id.ok_or(VaultParseError::UnexpectedShape(
        "a folder object with an id",
    ))?;
    Ok(VaultFolder {
        id,
        name: folder.name,
    })
}

/// The JSON `bw create folder` takes on stdin (before Base64 encoding).
pub fn folder_json(name: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "name": name })).expect("a folder name serializes")
}

/// Folders whose name equals `name` exactly (`bw get folder` matches by
/// case-insensitive substring, which is why codex-hosts compares itself).
/// More than one match means the user must choose.
pub fn exact_folder_matches<'a>(folders: &'a [VaultFolder], name: &str) -> Vec<&'a VaultFolder> {
    folders
        .iter()
        .filter(|folder| folder.name == name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_maps_the_three_states_and_keeps_non_secret_fields() {
        let report = parse_status(
            br#"{"serverUrl":"https://vault.example","lastSync":"2026-11-02T01:00:00.000Z","userEmail":"me@example.com","userId":"u","status":"locked"}"#,
        )
        .unwrap();
        assert_eq!(report.status, VaultStatus::Locked);
        assert_eq!(report.server_url.as_deref(), Some("https://vault.example"));
        assert_eq!(report.user_email.as_deref(), Some("me@example.com"));
        assert_eq!(
            parse_status(br#"{"status":"unauthenticated"}"#)
                .unwrap()
                .status,
            VaultStatus::Unauthenticated
        );
        assert_eq!(
            parse_status(br#"{"status":"unlocked","serverUrl":null}"#)
                .unwrap()
                .status,
            VaultStatus::Unlocked
        );
    }

    #[test]
    fn status_errors_are_classified() {
        assert_eq!(
            parse_status(br#"{"status":"half-open"}"#),
            Err(VaultParseError::UnknownStatus("half-open".to_owned()))
        );
        assert_eq!(parse_status(b"not json"), Err(VaultParseError::NotJson));
        assert_eq!(
            parse_status(br#"{"serverUrl":"x"}"#),
            Err(VaultParseError::UnexpectedJson {
                expected: "a status object",
                found: "{serverUrl: string}".to_owned(),
            })
        );
        assert_eq!(parse_status(b""), Err(VaultParseError::NotJson));
    }

    #[test]
    fn folders_drop_the_pseudo_folder_and_match_exactly() {
        // bw 2026.10.1 prints the "No Folder" entry with `"id": ""` (older
        // builds printed `null`); both are dropped.
        let folders = parse_folders(
            br#"[{"object":"folder","id":"11111111-1111-1111-1111-111111111111","name":"codex-hosts"},{"object":"folder","id":"22222222-2222-2222-2222-222222222222","name":"codex-hosts/ssh-keys"},{"object":"folder","id":"33333333-3333-3333-3333-333333333333","name":"codex-hosts"},{"object":"folder","id":null,"name":"No Folder"},{"object":"folder","id":"","name":"No Folder"}]"#,
        )
        .unwrap();
        assert_eq!(folders.len(), 3);
        let matches = exact_folder_matches(&folders, "codex-hosts");
        assert_eq!(matches.len(), 2);
        assert!(exact_folder_matches(&folders, "codex-hosts/ssh").is_empty());
        assert_eq!(
            exact_folder_matches(&folders, "codex-hosts/ssh-keys")[0].id,
            Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()
        );
        assert_eq!(
            parse_folders(br#"{"not":"an array"}"#),
            Err(VaultParseError::UnexpectedJson {
                expected: "a folder array",
                found: "{not: string}".to_owned(),
            })
        );
        assert_eq!(
            parse_folder(br#"{"object":"folder","id":"","name":"x"}"#),
            Err(VaultParseError::UnexpectedShape(
                "a folder object with an id"
            ))
        );
    }

    #[test]
    fn shape_descriptions_carry_types_and_keys_but_no_values() {
        let found = describe(
            br#"[{"object":"folder","id":"11111111-1111-1111-1111-111111111111","name":"Secret Name"},{"object":"folder","id":"22222222-2222-2222-2222-222222222222","name":"Other"},{"object":"folder","id":"","name":"No Folder"}]"#,
        );
        assert_eq!(
            found,
            "an array of 3 elements: 2 × {id: string, name: string, object: string}, 1 × {id: empty string, name: string, object: string}"
        );
        assert!(!found.contains("Secret"));
        assert!(!found.contains("1111"));
        assert_eq!(describe(b"[]"), "an empty array");
        assert_eq!(describe(b"null"), "null");
        assert_eq!(
            describe(br#"{"data":[1],"object":"list","nested":{"password":"x"},"on":true,"n":3}"#),
            "{data: array, n: number, nested: object, object: string, on: boolean}"
        );
        assert_eq!(describe(b"{"), "invalid JSON");
        let many: Vec<String> = (0..6).map(|i| format!("{{\"k{i}\":{i}}}")).collect();
        let found = describe(format!("[{}]", many.join(",")).as_bytes());
        assert!(found.starts_with("an array of 6 elements: 1 × {k0: number}"));
        assert!(found.ends_with(", …"));
        assert!(!found.contains("k5"));
    }
}
