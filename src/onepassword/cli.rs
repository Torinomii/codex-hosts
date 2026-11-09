//! What codex-hosts asks `op.exe` to do, and how its answers are read.
//!
//! Authentication is the 1Password app's: with "Integrate with 1Password
//! CLI" on, the app asks the user (Windows Hello) the first time a process
//! starts `op`, and the authorization lasts for that process until it has
//! been idle ten minutes. codex-hosts therefore starts `op.exe` directly,
//! never through a shell, and holds no 1Password credential or session.
//! Every call names the account and asks for JSON; an item to create or
//! write back goes to `op` on stdin, never as an argument. The child gets a
//! minimal environment (no `PATH`, no `OP_*` token or session variables).
//! The app verifies the signature of the `op` that connects to it, so a
//! stand-in executable cannot read the vault.

use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use codex_hosts_bw::{BwOutput, BwRunError, HostUri, parse_host_uri};
use serde::Deserialize;
use serde_json::Value;
use zeroize::Zeroizing;

/// Parent variables `op` needs to find its configuration and the app; the
/// P0 run showed it fails with only `SystemRoot` and needs nothing else.
const INHERITED_ENV: &[&str] = &[
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "LOCALAPPDATA",
    "APPDATA",
    "PROGRAMDATA",
    "TEMP",
    "TMP",
];

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const TIMEOUT_QUICK: Duration = Duration::from_secs(15);
/// Covers the user answering the Windows Hello prompt.
const TIMEOUT_VAULT: Duration = Duration::from_secs(90);

/// The user-selected or detected `op.exe`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpExecutable {
    path: PathBuf,
}

impl OpExecutable {
    /// Accepts an absolute path to an existing file named `op.exe`, which
    /// rules out the 1Password app itself and shims. The version is checked
    /// separately ([`parse_version`]).
    pub(crate) fn new(path: impl Into<PathBuf>) -> Result<Self, OpError> {
        let path = path.into();
        let named_op = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("op.exe"));
        if !path.is_absolute() || !named_op {
            return Err(OpError::NotOp);
        }
        if !path.is_file() {
            return Err(OpError::NotOp);
        }
        Ok(Self { path })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// Finds the official CLI where its installers put it: winget (package
/// directory first, then its `Links` entry), scoop, Chocolatey, the manual
/// install folder, then `PATH`. Links are resolved, and launcher shims
/// (scoop `shims`, Chocolatey `bin`) are skipped: the real `op.exe` is the
/// process the 1Password app authorizes.
pub(crate) fn detect() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let mut candidates = Vec::new();
    if let Some(local) = var("LOCALAPPDATA") {
        let winget = local.join("Microsoft").join("WinGet");
        if let Ok(entries) = std::fs::read_dir(winget.join("Packages")) {
            let mut packages: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with("AgileBits.1Password.CLI_"))
                })
                .map(|entry| entry.path().join("op.exe"))
                .collect();
            packages.sort();
            candidates.extend(packages);
        }
        candidates.push(winget.join("Links").join("op.exe"));
    }
    if let Some(profile) = var("USERPROFILE") {
        candidates.push(
            profile
                .join("scoop")
                .join("apps")
                .join("1password-cli")
                .join("current")
                .join("op.exe"),
        );
    }
    if let Some(data) = var("PROGRAMDATA") {
        candidates.push(
            data.join("chocolatey")
                .join("lib")
                .join("op")
                .join("tools")
                .join("op.exe"),
        );
    }
    for program_files in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(root) = var(program_files) {
            candidates.push(root.join("1Password CLI").join("op.exe"));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(
            std::env::split_paths(&path)
                .filter(|dir| !is_shim_dir(dir))
                .map(|dir| dir.join("op.exe")),
        );
    }
    candidates
        .into_iter()
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .map(strip_verbatim)
        .find(|path| path.is_file() && OpExecutable::new(path.clone()).is_ok())
}

/// Directories of launchers that start the real executable elsewhere.
fn is_shim_dir(dir: &Path) -> bool {
    let lower = dir
        .to_string_lossy()
        .to_ascii_lowercase()
        .replace('/', "\\");
    lower.ends_with("\\scoop\\shims") || lower.ends_with("\\chocolatey\\bin")
}

/// `canonicalize` answers with a `\\?\` path; the plain form is what the
/// user sees and what is stored.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
        Some(plain) if !plain.starts_with("UNC\\") => PathBuf::from(plain),
        _ => path,
    }
}

/// A 1Password account, vault or item id: 26 lowercase letters and digits
/// (account ids are uppercase). Checked before it becomes an argument so a
/// value from the store can never be read as a flag.
pub(crate) fn is_op_id(text: &str) -> bool {
    text.len() == 26 && text.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// The longest vault name codex-hosts creates.
const MAX_VAULT_NAME: usize = 100;

/// One planned `op` call. Arguments carry no secrets; an item JSON (which
/// may hold the password) is the zeroizing stdin payload. No `Debug`.
pub(crate) struct OpCall {
    args: Vec<OsString>,
    timeout: Duration,
    payload: Option<Zeroizing<Vec<u8>>>,
}

impl OpCall {
    fn new(args: &[&str], account: Option<&str>, timeout: Duration) -> Result<Self, OpError> {
        let mut args: Vec<OsString> = args.iter().map(OsString::from).collect();
        if let Some(account) = account {
            if !is_op_id(account) {
                return Err(OpError::BadId);
            }
            args.extend(["--account".into(), account.into()]);
        }
        Ok(Self {
            args,
            timeout,
            payload: None,
        })
    }

    fn with_ids(
        command: &[&str],
        ids: &[&str],
        account: &str,
        vault: &str,
        timeout: Duration,
    ) -> Result<Self, OpError> {
        if !ids.iter().chain([&vault]).all(|id| is_op_id(id)) {
            return Err(OpError::BadId);
        }
        let mut args: Vec<&str> = command.to_vec();
        args.extend_from_slice(ids);
        args.extend(["--vault", vault, "--format", "json"]);
        Self::new(&args, Some(account), timeout)
    }

    /// Reads the item JSON from stdin (`-`, as the P0 run used).
    fn with_payload(mut self, json: Zeroizing<Vec<u8>>) -> Self {
        self.args.push("-".into());
        self.payload = Some(json);
        self
    }

    pub(crate) fn version() -> Self {
        Self::new(&["--version"], None, TIMEOUT_QUICK).expect("no ids")
    }

    /// Needs no authorization; fails while the CLI integration is off.
    pub(crate) fn account_list() -> Self {
        Self::new(
            &["account", "list", "--format", "json"],
            None,
            TIMEOUT_QUICK,
        )
        .expect("no ids")
    }

    pub(crate) fn vault_list(account: &str) -> Result<Self, OpError> {
        Self::new(
            &["vault", "list", "--format", "json"],
            Some(account),
            TIMEOUT_VAULT,
        )
    }

    pub(crate) fn item_list(account: &str, vault: &str) -> Result<Self, OpError> {
        Self::with_ids(&["item", "list"], &[], account, vault, TIMEOUT_VAULT)
    }

    /// The whole item, password included (JSON output carries concealed
    /// values even without `--reveal`).
    pub(crate) fn item_get(account: &str, vault: &str, item: &str) -> Result<Self, OpError> {
        Self::with_ids(&["item", "get"], &[item], account, vault, TIMEOUT_VAULT)
    }

    /// The CLI accepts a JSON array of item IDs on stdin for a single read.
    pub(crate) fn items_get(account: &str, vault: &str, items: &[&str]) -> Result<Self, OpError> {
        if items.is_empty() || items.iter().any(|id| !is_op_id(id)) {
            return Err(OpError::BadId);
        }
        let ids: Vec<_> = items
            .iter()
            .map(|id| serde_json::json!({"id": id}))
            .collect();
        let input = Zeroizing::new(serde_json::to_vec(&ids).map_err(|_| OpError::Parse)?);
        Ok(
            Self::with_ids(&["item", "get"], &[], account, vault, TIMEOUT_VAULT)?
                .with_payload(input),
        )
    }

    /// Creates an item from its JSON template.
    pub(crate) fn item_create(
        account: &str,
        vault: &str,
        json: Zeroizing<Vec<u8>>,
    ) -> Result<Self, OpError> {
        Ok(
            Self::with_ids(&["item", "create"], &[], account, vault, TIMEOUT_VAULT)?
                .with_payload(json),
        )
    }

    /// Replaces the item with `json`, the whole item as `item get` gave it
    /// with the host's fields changed.
    pub(crate) fn item_edit(
        account: &str,
        vault: &str,
        item: &str,
        json: Zeroizing<Vec<u8>>,
    ) -> Result<Self, OpError> {
        Ok(
            Self::with_ids(&["item", "edit"], &[item], account, vault, TIMEOUT_VAULT)?
                .with_payload(json),
        )
    }

    /// Moves the item to the archive, where 1Password can restore it.
    pub(crate) fn item_archive(account: &str, vault: &str, item: &str) -> Result<Self, OpError> {
        let mut call = Self::with_ids(&["item", "delete"], &[item], account, vault, TIMEOUT_VAULT)?;
        call.args.push("--archive".into());
        Ok(call)
    }

    /// Creates a vault. The name is the user's, so it must not read as a flag.
    pub(crate) fn vault_create(account: &str, name: &str) -> Result<Self, OpError> {
        let name = name.trim();
        if name.is_empty()
            || name.starts_with('-')
            || name.chars().count() > MAX_VAULT_NAME
            || name.chars().any(char::is_control)
        {
            return Err(OpError::BadName);
        }
        Self::new(
            &["vault", "create", name, "--format", "json"],
            Some(account),
            TIMEOUT_VAULT,
        )
    }

    #[cfg(test)]
    pub(crate) fn args(&self) -> &[OsString] {
        &self.args
    }

    #[cfg(test)]
    pub(crate) fn payload(&self) -> Option<&[u8]> {
        self.payload.as_deref().map(Vec::as_slice)
    }

    /// The process description with the policy applied.
    fn command(&self, exe: &OpExecutable) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(exe.path());
        command.args(&self.args);
        command.env_clear();
        for (key, value) in std::env::vars_os() {
            let inherit = key
                .to_str()
                .is_some_and(|name| INHERITED_ENV.contains(&name.to_ascii_uppercase().as_str()));
            if inherit {
                command.env(key, value);
            }
        }
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }
}

/// Runs one `op` call; tests script answers instead of spawning.
pub(crate) trait OpRunner: Send + Sync {
    fn run(
        &self,
        exe: &OpExecutable,
        call: OpCall,
    ) -> impl Future<Output = Result<BwOutput, BwRunError>> + Send;

    fn start_desktop_app(&self) -> impl Future<Output = Result<bool, BwRunError>> + Send {
        async { Ok(false) }
    }
}

/// The real CLI, under the same output cap and timeout as `bw`.
#[derive(Clone, Copy, Default)]
pub(crate) struct RealRunner;

impl OpRunner for RealRunner {
    fn run(
        &self,
        exe: &OpExecutable,
        call: OpCall,
    ) -> impl Future<Output = Result<BwOutput, BwRunError>> + Send {
        let command = call.command(exe);
        codex_hosts_bw::run_command(command, call.payload, call.timeout)
    }

    async fn start_desktop_app(&self) -> Result<bool, BwRunError> {
        super::desktop::start().map_err(BwRunError::Spawn)
    }
}

/// Why an `op` call failed. Never carries CLI output or secrets.
#[derive(Debug, thiserror::Error)]
pub(crate) enum OpError {
    /// The chosen file is not `op.exe`, or not a 2.x CLI.
    #[error("not the 1Password CLI")]
    NotOp,
    /// "Integrate with 1Password CLI" is off, or no account is added.
    #[error("the 1Password CLI integration is off")]
    IntegrationOff,
    /// The app is not running or did not answer.
    #[error("the 1Password app is not reachable")]
    AppUnavailable,
    /// The app is locked, or the user declined or ignored the prompt.
    #[error("1Password did not authorize the call")]
    NotAuthorized,
    /// The item does not exist in the vault.
    #[error("1Password item missing")]
    ItemMissing,
    /// A stored id is not a 1Password id.
    #[error("invalid 1Password id")]
    BadId,
    /// A vault name codex-hosts will not pass to `op`.
    #[error("invalid vault name")]
    BadName,
    /// JSON output was not in the expected shape.
    #[error("unexpected 1Password CLI output")]
    Parse,
    /// `op` exited non-zero for another reason.
    #[error("op failed (exit {exit_code:?})")]
    Cli {
        /// Exit code.
        exit_code: Option<i32>,
    },
    /// The process could not be run.
    #[error(transparent)]
    Run(BwRunError),
}

impl OpError {
    /// The stable code a tool result carries.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::NotOp => "ONEPASSWORD_CLI_UNSUPPORTED",
            Self::IntegrationOff => "ONEPASSWORD_NOT_CONFIGURED",
            Self::AppUnavailable => "ONEPASSWORD_UNAVAILABLE",
            Self::NotAuthorized => "ONEPASSWORD_LOCKED",
            Self::ItemMissing => "ONEPASSWORD_ITEM_MISSING",
            Self::BadId | Self::BadName | Self::Parse | Self::Cli { .. } | Self::Run(_) => {
                "ONEPASSWORD_UNAVAILABLE"
            }
        }
    }

    /// Whether the call is worth one more try: a process's first call can
    /// fail to reach the app although it is running.
    pub(crate) fn retry_once(&self) -> bool {
        matches!(self, Self::AppUnavailable)
    }
}

/// This diagnostic is emitted before the CLI connects to the app. A lost
/// connection can occur after dispatch and must not replay a vault write.
pub(crate) fn failed_before_app_connection(output: &BwOutput) -> bool {
    let message = b"cannot connect to 1password app";
    output.exit_code != Some(0)
        && output
            .stderr
            .windows(message.len())
            .any(|part| part.eq_ignore_ascii_case(message))
}

/// Classifies a finished call: stdout on success, otherwise the reason read
/// from stderr. Only the documented or observed messages are matched.
pub(crate) fn outcome(output: BwOutput) -> Result<Zeroizing<Vec<u8>>, OpError> {
    if output.exit_code == Some(0) {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    let reason = if stderr.contains("no accounts configured") {
        OpError::IntegrationOff
    } else if stderr.contains("cannot connect to 1password app")
        || stderr.contains("lostconnectiontoapp")
        || stderr.contains("connectionreset")
    {
        OpError::AppUnavailable
    } else if stderr.contains("isn't an item") {
        OpError::ItemMissing
    } else if stderr.contains("not signed in")
        || stderr.contains("authoriz")
        || stderr.contains("dismissed")
        || stderr.contains("locked")
    {
        OpError::NotAuthorized
    } else {
        OpError::Cli {
            exit_code: output.exit_code,
        }
    };
    Err(reason)
}

/// A run error as a vault error: an unanswered prompt shows up as the
/// timeout.
pub(crate) fn run_failure(error: BwRunError) -> OpError {
    match error {
        BwRunError::Timeout(_) => OpError::NotAuthorized,
        other => OpError::Run(other),
    }
}

/// Reads `op --version`: only the 2.x CLI is accepted (the app answers
/// `--version` with its own 8.x version).
pub(crate) fn parse_version(stdout: &[u8]) -> Result<(u32, u32, u32), OpError> {
    let text = std::str::from_utf8(stdout)
        .map_err(|_| OpError::NotOp)?
        .trim();
    let mut parts = text.split('.').map(|part| part.parse::<u32>().ok());
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(Some(2)), Some(Some(minor)), Some(Some(patch)), None) => Ok((2, minor, patch)),
        _ => Err(OpError::NotOp),
    }
}

/// One account from `op account list`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct OpAccount {
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) email: String,
    pub(crate) account_uuid: String,
}

/// One vault from `op vault list`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct OpVault {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct OpUrl {
    #[serde(default)]
    pub(crate) primary: bool,
    pub(crate) href: String,
}

/// What `op item list` reports about an item; nothing secret.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct OpItemSummary {
    pub(crate) id: String,
    pub(crate) title: String,
    /// Raised by every edit; the host's revision.
    pub(crate) version: u64,
    pub(crate) vault: OpVault,
    pub(crate) category: String,
    /// The username, for Login items.
    #[serde(default)]
    pub(crate) additional_information: Option<String>,
    #[serde(default)]
    pub(crate) urls: Vec<OpUrl>,
}

impl OpItemSummary {
    pub(crate) fn is_login(&self) -> bool {
        self.category == "LOGIN"
    }

    /// The first `ssh://` or `telnet://` URL, the primary one first.
    pub(crate) fn host_uri(&self) -> Option<HostUri> {
        let primary = self.urls.iter().filter(|url| url.primary);
        let others = self.urls.iter().filter(|url| !url.primary);
        primary
            .chain(others)
            .find_map(|url| parse_host_uri(&url.href))
    }
}

/// The section our custom fields live in.
pub(crate) const SECTION_LABEL: &str = "codex-hosts";
/// Its id in items codex-hosts creates.
const SECTION_ID: &str = "codex_hosts";

/// What codex-hosts writes into a Login item. No `Debug`: the password edit
/// is a secret.
pub(crate) struct ItemFields<'a> {
    pub(crate) title: &'a str,
    pub(crate) host_uri: &'a str,
    pub(crate) username: &'a str,
    pub(crate) password: &'a codex_hosts_bw::FieldEdit<Zeroizing<String>>,
    /// The codex-hosts section's text fields by label; an empty value
    /// removes the field.
    pub(crate) section: &'a [(&'a str, String)],
}

/// The template `op item create` reads for a new host item.
pub(crate) fn new_item_json(fields: &ItemFields<'_>) -> Zeroizing<Vec<u8>> {
    let mut item = serde_json::json!({
        "title": fields.title.trim(),
        "category": "LOGIN",
        "urls": [{"label": "host", "primary": true, "href": fields.host_uri}],
        "sections": [{"id": SECTION_ID, "label": SECTION_LABEL}],
        "fields": [{
            "id": "username", "type": "STRING", "purpose": "USERNAME",
            "label": "username", "value": fields.username,
        }],
    });
    let list = item["fields"].as_array_mut().expect("built above");
    if let codex_hosts_bw::FieldEdit::Set(password) = fields.password {
        list.push(password_field(password.as_str()));
    }
    for (label, value) in fields.section {
        if !value.trim().is_empty() {
            list.push(section_field(SECTION_ID, label, value));
        }
    }
    Zeroizing::new(item.to_string().into_bytes())
}

fn password_field(value: &str) -> Value {
    serde_json::json!({
        "id": "password", "type": "CONCEALED", "purpose": "PASSWORD",
        "label": "password", "value": value,
    })
}

fn section_field(section_id: &str, label: &str, value: &str) -> Value {
    serde_json::json!({
        "id": format!("{SECTION_ID}_{label}"), "type": "STRING", "label": label,
        "value": value.trim(), "section": {"id": section_id, "label": SECTION_LABEL},
    })
}

fn is_codex_field(field: &Value, label: &str) -> bool {
    field
        .get("section")
        .and_then(|section| section.get("label"))
        .and_then(Value::as_str)
        == Some(SECTION_LABEL)
        && field.get("type").and_then(Value::as_str) != Some("CONCEALED")
        && field.get("label").and_then(Value::as_str) == Some(label)
}

/// A whole item from `op item get`. The raw output (with the password) is
/// zeroized on drop; only the non-secret parts are parsed eagerly.
pub(crate) struct OpItem {
    raw: Zeroizing<Vec<u8>>,
    summary: OpItemSummary,
}

impl std::fmt::Debug for OpItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpItem")
            .field("summary", &self.summary)
            .field("raw_len", &self.raw.len())
            .finish()
    }
}

impl OpItem {
    pub(crate) fn summary(&self) -> &OpItemSummary {
        &self.summary
    }

    fn fields(&self) -> Result<Vec<Value>, OpError> {
        let mut value: Value = serde_json::from_slice(&self.raw).map_err(|_| OpError::Parse)?;
        match value.get_mut("fields").map(Value::take) {
            Some(Value::Array(fields)) => Ok(fields),
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(_) => Err(OpError::Parse),
        }
    }

    /// The username field, falling back to the list's username.
    pub(crate) fn username(&self) -> Result<Option<String>, OpError> {
        let from_field = self
            .fields()?
            .iter()
            .find(|field| field.get("purpose").and_then(Value::as_str) == Some("USERNAME"))
            .and_then(|field| field.get("value").and_then(Value::as_str))
            .map(str::to_owned);
        Ok(from_field.or_else(|| self.summary.additional_information.clone()))
    }

    /// The Login password.
    pub(crate) fn password(&self) -> Result<Option<Zeroizing<String>>, OpError> {
        for mut field in self.fields()? {
            if field.get("purpose").and_then(Value::as_str) == Some("PASSWORD") {
                return match field.get_mut("value").map(Value::take) {
                    Some(Value::String(text)) => Ok(Some(Zeroizing::new(text))),
                    None | Some(Value::Null) => Ok(None),
                    Some(_) => Err(OpError::Parse),
                };
            }
        }
        Ok(None)
    }

    /// A text field of the codex-hosts section, by label.
    pub(crate) fn codex_field(&self, label: &str) -> Result<Option<String>, OpError> {
        Ok(self.fields()?.iter().find_map(|field| {
            is_codex_field(field, label)
                .then(|| {
                    field
                        .get("value")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .flatten()
        }))
    }

    /// The whole item with the host's parts replaced by `fields`, for
    /// `op item edit`: every other field, URL, section and setting stays as
    /// 1Password has it. The parsed copy holds the password only until the
    /// JSON is written into the zeroizing buffer.
    pub(crate) fn patched(&self, fields: &ItemFields<'_>) -> Result<Zeroizing<Vec<u8>>, OpError> {
        let mut item: Value = serde_json::from_slice(&self.raw).map_err(|_| OpError::Parse)?;
        let object = item.as_object_mut().ok_or(OpError::Parse)?;
        object.insert("title".into(), Value::from(fields.title.trim()));

        let urls = array_entry(object, "urls")?;
        let host_url = urls
            .iter()
            .position(|url| {
                url.get("primary").and_then(Value::as_bool) == Some(true) && is_host_href(url)
            })
            .or_else(|| urls.iter().position(is_host_href));
        match host_url {
            Some(index) => urls[index]["href"] = Value::from(fields.host_uri),
            None => {
                let primary = !urls
                    .iter()
                    .any(|url| url.get("primary").and_then(Value::as_bool) == Some(true));
                urls.push(serde_json::json!({
                    "label": "host", "primary": primary, "href": fields.host_uri,
                }));
            }
        }

        let sections = array_entry(object, "sections")?;
        let section_id = match sections
            .iter()
            .find(|section| section.get("label").and_then(Value::as_str) == Some(SECTION_LABEL))
            .and_then(|section| section.get("id").and_then(Value::as_str))
        {
            Some(id) => id.to_owned(),
            None => {
                sections.push(serde_json::json!({"id": SECTION_ID, "label": SECTION_LABEL}));
                SECTION_ID.to_owned()
            }
        };

        let list = array_entry(object, "fields")?;
        let by_purpose = |list: &[Value], purpose: &str| {
            list.iter()
                .position(|field| field.get("purpose").and_then(Value::as_str) == Some(purpose))
        };
        match by_purpose(list, "USERNAME") {
            Some(index) => list[index]["value"] = Value::from(fields.username),
            None => list.push(serde_json::json!({
                "id": "username", "type": "STRING", "purpose": "USERNAME",
                "label": "username", "value": fields.username,
            })),
        }
        let password = by_purpose(list, "PASSWORD");
        match (fields.password, password) {
            (codex_hosts_bw::FieldEdit::Set(value), Some(index)) => {
                list[index]["value"] = Value::from(value.as_str());
            }
            (codex_hosts_bw::FieldEdit::Set(value), None) => {
                list.push(password_field(value.as_str()));
            }
            (codex_hosts_bw::FieldEdit::Clear, Some(index)) => {
                list[index]["value"] = Value::from("");
            }
            (codex_hosts_bw::FieldEdit::Clear | codex_hosts_bw::FieldEdit::Keep, _) => {}
        }
        for (label, value) in fields.section {
            let value = value.trim();
            let existing = list.iter().position(|field| is_codex_field(field, label));
            match (existing, value.is_empty()) {
                (Some(index), true) => {
                    list.remove(index);
                }
                (Some(index), false) => list[index]["value"] = Value::from(value),
                (None, true) => {}
                (None, false) => list.push(section_field(&section_id, label, value)),
            }
        }
        let json = Zeroizing::new(serde_json::to_vec(&item).map_err(|_| OpError::Parse)?);
        drop(item);
        Ok(json)
    }
}

fn is_host_href(url: &Value) -> bool {
    url.get("href")
        .and_then(Value::as_str)
        .is_some_and(|href| parse_host_uri(href).is_some())
}

/// The array under `key`, created when the item has none.
fn array_entry<'a>(
    object: &'a mut serde_json::Map<String, Value>,
    key: &str,
) -> Result<&'a mut Vec<Value>, OpError> {
    let entry = object
        .entry(key)
        .or_insert_with(|| Value::Array(Vec::new()));
    if entry.is_null() {
        *entry = Value::Array(Vec::new());
    }
    entry.as_array_mut().ok_or(OpError::Parse)
}

pub(crate) fn parse_accounts(stdout: &[u8]) -> Result<Vec<OpAccount>, OpError> {
    parse_list(stdout)
}

pub(crate) fn parse_vaults(stdout: &[u8]) -> Result<Vec<OpVault>, OpError> {
    parse_list(stdout)
}

pub(crate) fn parse_item_list(stdout: &[u8]) -> Result<Vec<OpItemSummary>, OpError> {
    parse_list(stdout)
}

/// `op` prints nothing at all for an empty list.
fn parse_list<T: serde::de::DeserializeOwned>(stdout: &[u8]) -> Result<Vec<T>, OpError> {
    if stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    serde_json::from_slice(stdout).map_err(|_| OpError::Parse)
}

/// The vault `op vault create` reports.
pub(crate) fn parse_vault(stdout: &[u8]) -> Result<OpVault, OpError> {
    serde_json::from_slice(stdout).map_err(|_| OpError::Parse)
}

pub(crate) fn parse_item(stdout: Zeroizing<Vec<u8>>) -> Result<OpItem, OpError> {
    let summary = serde_json::from_slice(&stdout).map_err(|_| OpError::Parse)?;
    Ok(OpItem {
        raw: stdout,
        summary,
    })
}

/// Bulk output can be a JSON array or a stream of whole-item objects.
/// Borrowed raw slices avoid materializing passwords in ordinary JSON strings.
pub(crate) fn parse_items(stdout: Zeroizing<Vec<u8>>) -> Result<Vec<OpItem>, OpError> {
    use serde_json::value::RawValue;
    let items: Vec<&RawValue> =
        if stdout.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'[') {
            serde_json::from_slice(&stdout).map_err(|_| OpError::Parse)?
        } else {
            serde_json::Deserializer::from_slice(&stdout)
                .into_iter::<&RawValue>()
                .collect::<Result<_, _>>()
                .map_err(|_| OpError::Parse)?
        };
    items
        .into_iter()
        .map(|item| parse_item(Zeroizing::new(item.get().as_bytes().to_vec())))
        .collect()
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// One scripted answer: expected argv prefix, exit code, stdout, stderr.
    pub(crate) type Answer = (Vec<&'static str>, Option<i32>, String, &'static str);

    /// Replays scripted answers in order, checking each argv prefix.
    #[derive(Clone, Default)]
    pub(crate) struct Scripted {
        answers: Arc<Mutex<VecDeque<Answer>>>,
        payloads: Arc<Mutex<Vec<Value>>>,
    }

    impl Scripted {
        pub(crate) fn new(answers: Vec<Answer>) -> Self {
            Self {
                answers: Arc::new(Mutex::new(answers.into())),
                payloads: Arc::default(),
            }
        }

        pub(crate) fn finished(&self) -> bool {
            self.answers.lock().unwrap().is_empty()
        }

        /// The stdin JSON of every call that had one, in order.
        pub(crate) fn payloads(&self) -> Vec<Value> {
            self.payloads.lock().unwrap().clone()
        }
    }

    impl OpRunner for Scripted {
        fn run(
            &self,
            _exe: &OpExecutable,
            call: OpCall,
        ) -> impl Future<Output = Result<BwOutput, BwRunError>> + Send {
            let args: Vec<String> = call
                .args()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            if let Some(payload) = call.payload() {
                self.payloads
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(payload).expect("JSON payload"));
            }
            let next = self.answers.lock().unwrap().pop_front();
            async move {
                let (expected, exit_code, stdout, stderr) =
                    next.unwrap_or_else(|| panic!("unexpected op call {args:?}"));
                assert!(
                    args.len() >= expected.len()
                        && args.iter().zip(&expected).all(|(got, want)| got == want),
                    "expected {expected:?}, got {args:?}"
                );
                Ok(BwOutput {
                    exit_code,
                    stdout: Zeroizing::new(stdout.into_bytes()),
                    stderr: Zeroizing::new(stderr.as_bytes().to_vec()),
                })
            }
        }
    }

    /// A stub `op.exe` in its own temp directory.
    pub(crate) fn stub_op(name: &str) -> OpExecutable {
        let dir =
            std::env::temp_dir().join(format!("codex-hosts-op-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("op.exe");
        std::fs::write(&path, b"stub").unwrap();
        OpExecutable::new(path).unwrap()
    }

    pub(crate) const ACCOUNT: &str = "TESTACCOUNTAAAAAAAAAAAAAAA";
    pub(crate) const VAULT: &str = "testvault00000000000000000";
    pub(crate) const ITEM: &str = "testitem000000000000000000";

    /// `op item list` output in the shape the P0 run recorded.
    pub(crate) fn item_list_json(items: &[(&str, &str, u64, &str)]) -> String {
        let items: Vec<Value> = items
            .iter()
            .map(|(id, title, version, href)| {
                serde_json::json!({
                    "id": id, "title": title, "version": version,
                    "vault": {"id": VAULT, "name": "codex-hosts-test"},
                    "category": "LOGIN",
                    "created_at": "2026-11-03T17:23:43Z",
                    "updated_at": "2026-11-03T17:24:02Z",
                    "additional_information": "p0",
                    "urls": [{"label": "host", "primary": true, "href": href}]
                })
            })
            .collect();
        Value::Array(items).to_string()
    }

    /// `op item get` output in the shape the P0 run recorded.
    pub(crate) fn item_json(id: &str, title: &str, version: u64, password: &str) -> String {
        serde_json::json!({
            "id": id, "title": title, "version": version,
            "vault": {"id": VAULT, "name": "codex-hosts-test"},
            "category": "LOGIN",
            "additional_information": "p0",
            "urls": [{"label": "host", "primary": true, "href": "ssh://p0@192.0.2.10:2222"}],
            "sections": [{"id": "codex_hosts", "label": SECTION_LABEL}],
            "fields": [
                {"id": "username", "type": "STRING", "purpose": "USERNAME", "label": "username", "value": "p0"},
                {"id": "password", "type": "CONCEALED", "purpose": "PASSWORD", "label": "password", "value": password},
                {"id": "notesPlain", "type": "STRING", "purpose": "NOTES", "label": "notesPlain"},
                {"id": "description", "section": {"id": "codex_hosts", "label": SECTION_LABEL},
                 "type": "STRING", "label": "description", "value": "P0 fake host\nmore"},
                {"id": "tags", "section": {"id": "codex_hosts", "label": SECTION_LABEL},
                 "type": "STRING", "label": "tags", "value": "p0, test"}
            ]
        })
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[test]
    fn bulk_reads_validate_ids_and_parse_arrays_or_object_streams() {
        let call = OpCall::items_get(ACCOUNT, VAULT, &[ITEM]).unwrap();
        assert_eq!(call.args().last().unwrap(), "-");
        let input: Value = serde_json::from_slice(call.payload().unwrap()).unwrap();
        assert_eq!(input, serde_json::json!([{"id": ITEM}]));
        assert!(matches!(
            OpCall::items_get(ACCOUNT, VAULT, &["--help"]),
            Err(OpError::BadId)
        ));
        assert!(matches!(
            OpCall::items_get(ACCOUNT, VAULT, &[]),
            Err(OpError::BadId)
        ));
        let item = item_json(ITEM, "batch", 1, "synthetic-batch-password");
        for output in [format!("[{item},{item}]"), format!("{item}\n{item}\n")] {
            let parsed = parse_items(Zeroizing::new(output.into_bytes())).unwrap();
            assert_eq!(parsed.len(), 2);
            assert!(
                parsed[0]
                    .password()
                    .unwrap()
                    .is_some_and(|p| p.as_str() == "synthetic-batch-password")
            );
            assert!(!format!("{parsed:?}").contains("synthetic-batch-password"));
        }
        for output in ["null", "[{}]", "{", "{} {}"] {
            assert!(parse_items(Zeroizing::new(output.as_bytes().to_vec())).is_err());
        }
    }

    fn output(code: i32, stderr: &str) -> BwOutput {
        BwOutput {
            exit_code: Some(code),
            stdout: Zeroizing::new(b"[]".to_vec()),
            stderr: Zeroizing::new(stderr.as_bytes().to_vec()),
        }
    }

    #[test]
    fn only_op_exe_with_a_2x_version_is_accepted() {
        let stub = stub_op("accept");
        assert!(OpExecutable::new(stub.path().to_path_buf()).is_ok());
        let app = stub.path().with_file_name("1Password.exe");
        std::fs::write(&app, b"stub").unwrap();
        assert!(matches!(OpExecutable::new(app), Err(OpError::NotOp)));
        assert!(matches!(OpExecutable::new("op.exe"), Err(OpError::NotOp)));
        assert_eq!(parse_version(b"2.41.1\r\n").unwrap(), (2, 41, 1));
        // The app answers `--version` with its own version.
        assert!(matches!(parse_version(b"8.12.42"), Err(OpError::NotOp)));
        assert!(matches!(parse_version(b""), Err(OpError::NotOp)));
    }

    #[test]
    fn ids_are_checked_before_they_become_arguments() {
        assert!(is_op_id(ACCOUNT) && is_op_id(VAULT) && is_op_id(ITEM));
        assert!(!is_op_id("--reveal-everything-x-yz012"));
        assert!(matches!(
            OpCall::item_get(ACCOUNT, VAULT, "--format"),
            Err(OpError::BadId)
        ));
        assert!(matches!(OpCall::vault_list("me@x"), Err(OpError::BadId)));
        let call = OpCall::item_get(ACCOUNT, VAULT, ITEM).unwrap();
        let args: Vec<_> = call.args().iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "item",
                "get",
                ITEM,
                "--vault",
                VAULT,
                "--format",
                "json",
                "--account",
                ACCOUNT
            ]
        );
    }

    #[test]
    fn writes_send_the_item_on_stdin_and_check_their_names() {
        let json = Zeroizing::new(b"{}".to_vec());
        let create = OpCall::item_create(ACCOUNT, VAULT, json.clone()).unwrap();
        let args: Vec<_> = create.args().iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "item",
                "create",
                "--vault",
                VAULT,
                "--format",
                "json",
                "--account",
                ACCOUNT,
                "-"
            ]
        );
        assert_eq!(create.payload(), Some(&b"{}"[..]));
        let edit = OpCall::item_edit(ACCOUNT, VAULT, ITEM, json).unwrap();
        assert_eq!(edit.args()[2], ITEM);
        assert_eq!(edit.args().last().unwrap(), "-");
        let archive = OpCall::item_archive(ACCOUNT, VAULT, ITEM).unwrap();
        assert_eq!(archive.args().last().unwrap(), "--archive");
        assert!(archive.payload().is_none());

        let vault = OpCall::vault_create(ACCOUNT, " codex-hosts ").unwrap();
        assert_eq!(vault.args()[2], "codex-hosts");
        for bad in ["", "--help", "a\nb", &"x".repeat(101)] {
            assert!(
                matches!(OpCall::vault_create(ACCOUNT, bad), Err(OpError::BadName)),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_new_item_carries_the_host_and_its_section() {
        let password = codex_hosts_bw::FieldEdit::Set(Zeroizing::new("pw".to_owned()));
        let json = new_item_json(&ItemFields {
            title: " box ",
            host_uri: "ssh://root@10.0.0.5:22",
            username: "root",
            password: &password,
            section: &[("description", "web".into()), ("tags", String::new())],
        });
        let item: Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(item["title"], "box");
        assert_eq!(item["category"], "LOGIN");
        assert_eq!(item["urls"][0]["href"], "ssh://root@10.0.0.5:22");
        let fields = item["fields"].as_array().unwrap();
        assert_eq!(fields.len(), 3, "an empty tags field is left out");
        assert_eq!(fields[1]["purpose"], "PASSWORD");
        assert_eq!(fields[2]["section"]["label"], SECTION_LABEL);
        assert_eq!(fields[2]["value"], "web");
    }

    #[test]
    fn an_edit_changes_only_the_host_parts_of_the_item() {
        let mut raw: Value = serde_json::from_str(&item_json(ITEM, "old", 3, "old-pw")).unwrap();
        raw["urls"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"label": "web", "href": "https://example.com"}));
        raw["fields"].as_array_mut().unwrap().push(
            serde_json::json!({"id": "otp", "type": "OTP", "label": "one-time", "value": "x"}),
        );
        let item = parse_item(Zeroizing::new(raw.to_string().into_bytes())).unwrap();
        let keep = codex_hosts_bw::FieldEdit::Keep;
        let patched = item
            .patched(&ItemFields {
                title: "new",
                host_uri: "ssh://admin@10.0.0.9:2200",
                username: "admin",
                password: &keep,
                section: &[("description", String::new()), ("tags", "a, b".into())],
            })
            .unwrap();
        let patched: Value = serde_json::from_slice(&patched).unwrap();
        assert_eq!(patched["title"], "new");
        assert_eq!(patched["urls"][0]["href"], "ssh://admin@10.0.0.9:2200");
        assert_eq!(patched["urls"][1]["href"], "https://example.com");
        let fields = patched["fields"].as_array().unwrap();
        let find = |id: &str| fields.iter().find(|field| field["id"] == id);
        assert_eq!(find("username").unwrap()["value"], "admin");
        assert_eq!(find("password").unwrap()["value"], "old-pw", "kept");
        assert!(find("description").is_none(), "an emptied field goes");
        assert_eq!(find("tags").unwrap()["value"], "a, b");
        assert_eq!(find("otp").unwrap()["value"], "x", "other fields stay");

        // A Login item without our section gets it, with the new fields in it.
        let mut bare: Value = serde_json::from_str(&item_json(ITEM, "t", 1, "pw")).unwrap();
        bare.as_object_mut().unwrap().remove("sections");
        bare["fields"]
            .as_array_mut()
            .unwrap()
            .retain(|field| field.get("section").is_none());
        let item = parse_item(Zeroizing::new(bare.to_string().into_bytes())).unwrap();
        let set = codex_hosts_bw::FieldEdit::Set(Zeroizing::new("new-pw".to_owned()));
        let patched: Value = serde_json::from_slice(
            &item
                .patched(&ItemFields {
                    title: "t",
                    host_uri: "telnet://h",
                    username: "u",
                    password: &set,
                    section: &[("description", "d".into())],
                })
                .unwrap(),
        )
        .unwrap();
        assert_eq!(patched["sections"][0]["label"], SECTION_LABEL);
        let fields = patched["fields"].as_array().unwrap();
        assert!(
            fields
                .iter()
                .any(|f| f["purpose"] == "PASSWORD" && f["value"] == "new-pw")
        );
        assert!(
            fields
                .iter()
                .any(|f| f["label"] == "description" && f["section"]["id"] == SECTION_ID)
        );
    }

    #[test]
    fn shim_folders_are_not_taken_for_the_cli() {
        assert!(is_shim_dir(Path::new(r"C:\Users\u\scoop\shims")));
        assert!(is_shim_dir(Path::new(r"C:\ProgramData\chocolatey\bin")));
        assert!(!is_shim_dir(Path::new(r"C:\Program Files\1Password CLI")));
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\C:\op\op.exe")),
            PathBuf::from(r"C:\op\op.exe")
        );
    }

    #[test]
    fn the_child_gets_a_minimal_environment() {
        let call = OpCall::account_list();
        let command = call.command(&stub_op("env"));
        let names: Vec<String> = command
            .as_std()
            .get_envs()
            .filter(|(_, value)| value.is_some())
            .map(|(key, _)| key.to_string_lossy().to_ascii_uppercase())
            .collect();
        assert!(
            names
                .iter()
                .all(|name| INHERITED_ENV.contains(&name.as_str()))
        );
        assert!(
            !names
                .iter()
                .any(|name| name == "PATH" || name.starts_with("OP_"))
        );
    }

    #[test]
    fn failures_are_classified_from_the_observed_messages() {
        let cases = [
            (
                "[ERROR] connecting to desktop app: cannot connect to 1Password app, make sure it is running",
                "ONEPASSWORD_UNAVAILABLE",
            ),
            (
                "[ERROR] No accounts configured for use with 1Password CLI.",
                "ONEPASSWORD_NOT_CONFIGURED",
            ),
            (
                "[ERROR] \"zzz\" isn't an item in the \"v\" vault.",
                "ONEPASSWORD_ITEM_MISSING",
            ),
            ("[ERROR] account is not signed in", "ONEPASSWORD_LOCKED"),
            ("[ERROR] something new", "ONEPASSWORD_UNAVAILABLE"),
        ];
        for (stderr, code) in cases {
            assert_eq!(
                outcome(output(1, stderr)).unwrap_err().code(),
                code,
                "{stderr}"
            );
        }
        assert!(outcome(output(1, cases[0].0)).unwrap_err().retry_once());
        assert!(outcome(output(0, "INFO noise")).is_ok());
        assert_eq!(
            run_failure(BwRunError::Timeout(Duration::from_secs(90))).code(),
            "ONEPASSWORD_LOCKED"
        );
    }

    #[test]
    fn recorded_output_parses_without_exposing_the_password() {
        let list = parse_item_list(
            item_list_json(&[(ITEM, "p0-fake-host", 2, "ssh://p0@192.0.2.10:2222")]).as_bytes(),
        )
        .unwrap();
        assert_eq!(list[0].version, 2);
        assert!(list[0].is_login());
        let uri = list[0].host_uri().unwrap();
        assert_eq!(
            (uri.host.as_str(), uri.port_or_default()),
            ("192.0.2.10", 2222)
        );
        assert!(parse_item_list(b"").unwrap().is_empty());

        let item = parse_item(Zeroizing::new(
            item_json(ITEM, "p0-fake-host", 3, "s3cret-value").into_bytes(),
        ))
        .unwrap();
        assert_eq!(item.summary().title, "p0-fake-host");
        assert_eq!(item.username().unwrap().as_deref(), Some("p0"));
        assert_eq!(item.password().unwrap().unwrap().as_str(), "s3cret-value");
        assert_eq!(
            item.codex_field("tags").unwrap().as_deref(),
            Some("p0, test")
        );
        assert_eq!(item.codex_field("password").unwrap(), None);
        assert!(!format!("{item:?}").contains("s3cret"));

        let accounts = parse_accounts(
            br#"[{"url":"my.1password.com","email":"e","user_uuid":"U","account_uuid":"TESTACCOUNTAAAAAAAAAAAAAAA"}]"#,
        )
        .unwrap();
        assert_eq!(accounts[0].account_uuid, ACCOUNT);
        assert!(matches!(parse_vaults(b"null"), Err(OpError::Parse)));
    }
}
