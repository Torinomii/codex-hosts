//! What codex-hosts is allowed to ask `bw` to do, and how.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use uuid::Uuid;
use zeroize::Zeroizing;

/// Largest stdout or stderr codex-hosts keeps from one `bw` call.
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// Per-profile CLI data directory variable, so codex-hosts never shares state
/// with the user's day-to-day `bw`.
pub const DATA_DIR_ENV: &str = "BITWARDENCLI_APPDATA_DIR";
/// Session key variable; the only way a session reaches `bw`.
pub const SESSION_ENV: &str = "BW_SESSION";
/// Disables interactive prompts for background calls.
pub const NO_INTERACTION_ENV: &str = "BW_NOINTERACTION";
/// Variable named in `--passwordenv`; holds the master password for exactly
/// one child process.
pub const PASSWORD_ENV: &str = "CODEX_HOSTS_BW_PASSWORD";

/// Parent variables a child may inherit, compared case-insensitively. The
/// list is what a packaged Node program needs to start; `PATH` is deliberately
/// absent because the executable is addressed by absolute path. The M1
/// compatibility prototype confirms or extends this list.
const INHERITED_ENV: &[&str] = &[
    "SYSTEMROOT",
    "WINDIR",
    "SYSTEMDRIVE",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
];

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const TIMEOUT_VERSION: Duration = Duration::from_secs(10);
const TIMEOUT_STATUS: Duration = Duration::from_secs(15);
const TIMEOUT_UNLOCK: Duration = Duration::from_secs(60);
/// Covers the user reading an e-mail and typing the code.
const TIMEOUT_INTERACTIVE_LOGIN: Duration = Duration::from_secs(600);
const TIMEOUT_SYNC: Duration = Duration::from_secs(60);
const TIMEOUT_LIST: Duration = Duration::from_secs(30);
const TIMEOUT_GET: Duration = Duration::from_secs(15);
const TIMEOUT_WRITE: Duration = Duration::from_secs(30);
const TIMEOUT_LOCK: Duration = Duration::from_secs(15);

/// A configuration the policy refuses.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BwPolicyError {
    /// The CLI path is not absolute.
    #[error("bw path must be absolute: {0}")]
    RelativePath(PathBuf),
    /// The CLI path is not a native `.exe` (batch files and shims are refused).
    #[error("bw path must point at a native .exe: {0}")]
    NotAnExe(PathBuf),
    /// The CLI path does not exist or is not a regular file.
    #[error("bw path is not a file: {0}")]
    NotAFile(PathBuf),
    /// The profile data directory is not absolute.
    #[error("profile data directory must be absolute: {0}")]
    RelativeDataDir(PathBuf),
}

/// The user-selected `bw.exe`, validated once when chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BwExecutable {
    path: PathBuf,
}

impl BwExecutable {
    /// Accepts only an absolute path to an existing native `.exe`.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, BwPolicyError> {
        let path = path.into();
        if !path.is_absolute() {
            return Err(BwPolicyError::RelativePath(path));
        }
        let is_exe = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"));
        if !is_exe {
            return Err(BwPolicyError::NotAnExe(path));
        }
        if !path.is_file() {
            return Err(BwPolicyError::NotAFile(path));
        }
        Ok(Self { path })
    }

    /// The validated path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// One vault profile's CLI home: a dedicated data directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BwProfile {
    data_dir: PathBuf,
}

impl BwProfile {
    /// Accepts an absolute data directory; it is created by the caller.
    pub fn new(data_dir: impl Into<PathBuf>) -> Result<Self, BwPolicyError> {
        let data_dir = data_dir.into();
        if !data_dir.is_absolute() {
            return Err(BwPolicyError::RelativeDataDir(data_dir));
        }
        Ok(Self { data_dir })
    }

    /// The data directory passed as `BITWARDENCLI_APPDATA_DIR`.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}

/// A `BW_SESSION` value. No `Debug`, no `Display`; zeroized on drop.
pub struct SessionKey(Zeroizing<String>);

impl SessionKey {
    /// Wraps the raw session key returned by `bw unlock --raw`.
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    /// Borrows the key for storing it; keep the borrow short and never log it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    fn clone_inner(&self) -> Zeroizing<String> {
        self.0.clone()
    }
}

/// A master password typed in the codex-hosts window. No `Debug`, no `Display`;
/// zeroized on drop.
pub struct MasterPassword(Zeroizing<String>);

impl MasterPassword {
    /// Wraps the password.
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    fn clone_inner(&self) -> Zeroizing<String> {
        self.0.clone()
    }
}

/// Two-step login providers `bw login --method` accepts, by the CLI's
/// numeric provider ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TwoFactorMethod {
    /// Authenticator app (TOTP).
    Authenticator,
    /// Code sent by e-mail.
    Email,
    /// YubiKey OTP.
    YubiKey,
}

impl TwoFactorMethod {
    /// The `--method` argument.
    pub fn argument(self) -> &'static str {
        match self {
            Self::Authenticator => "0",
            Self::Email => "1",
            Self::YubiKey => "3",
        }
    }
}

/// Base64-encodes item JSON for `bw create item` / `bw edit item` stdin. The
/// encoding provides no secrecy; the result is treated as a secret buffer.
pub fn encode_item_json(json: &[u8]) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .encode(json)
            .into_bytes(),
    )
}

#[derive(Default)]
struct EnvSecrets {
    session: Option<Zeroizing<String>>,
    master_password: Option<Zeroizing<String>>,
}

/// One planned `bw` call. Holds secrets, so it has no `Debug`.
pub struct BwInvocation {
    exe: BwExecutable,
    profile: BwProfile,
    args: Vec<OsString>,
    secrets: EnvSecrets,
    interactive: bool,
    stdin: Option<Zeroizing<Vec<u8>>>,
    timeout: Duration,
}

impl BwInvocation {
    fn new(exe: &BwExecutable, profile: &BwProfile, args: &[&str], timeout: Duration) -> Self {
        Self {
            exe: exe.clone(),
            profile: profile.clone(),
            args: args.iter().map(OsString::from).collect(),
            secrets: EnvSecrets::default(),
            interactive: false,
            stdin: None,
            timeout,
        }
    }

    /// Arbitrary arguments under the same policy. Intended for diagnostics and
    /// tests; production paths use the named constructors.
    pub fn raw(
        exe: &BwExecutable,
        profile: &BwProfile,
        args: Vec<OsString>,
        timeout: Duration,
        session: Option<&SessionKey>,
    ) -> Self {
        let mut invocation = Self::new(exe, profile, &[], timeout).with_session(session);
        invocation.args = args;
        invocation
    }

    /// `bw --version`.
    pub fn version(exe: &BwExecutable, profile: &BwProfile) -> Self {
        Self::new(exe, profile, &["--version"], TIMEOUT_VERSION)
    }

    /// `bw status`, with the session when one is held.
    pub fn status(exe: &BwExecutable, profile: &BwProfile, session: Option<&SessionKey>) -> Self {
        Self::new(exe, profile, &["status"], TIMEOUT_STATUS).with_session(session)
    }

    /// `bw unlock --raw --passwordenv <var>`: the master password travels only
    /// in the child's environment and the session key comes back on stdout.
    pub fn unlock(exe: &BwExecutable, profile: &BwProfile, password: &MasterPassword) -> Self {
        let mut invocation = Self::new(
            exe,
            profile,
            &["unlock", "--raw", "--passwordenv", PASSWORD_ENV],
            TIMEOUT_UNLOCK,
        );
        invocation.secrets.master_password = Some(password.clone_inner());
        invocation
    }

    /// `bw login <email> --raw --passwordenv <var> [--method <n>]` with bw's
    /// own prompts enabled, for [`crate::interactive::login`] only. The
    /// new-device code is read solely at bw's prompt (`--code` fills the
    /// two-step token, a different field), so this is the one invocation
    /// that runs without `BW_NOINTERACTION`; the adapter answers the code
    /// prompts over stdin and kills the child on anything else.
    pub fn interactive_login(
        exe: &BwExecutable,
        profile: &BwProfile,
        email: &str,
        password: &MasterPassword,
        method: Option<TwoFactorMethod>,
    ) -> Self {
        let mut invocation = Self::new(
            exe,
            profile,
            &["login", email, "--raw", "--passwordenv", PASSWORD_ENV],
            TIMEOUT_INTERACTIVE_LOGIN,
        );
        if let Some(method) = method {
            invocation.args.push(OsString::from("--method"));
            invocation.args.push(OsString::from(method.argument()));
        }
        invocation.secrets.master_password = Some(password.clone_inner());
        invocation.interactive = true;
        invocation
    }

    /// `bw config server <url>`; a local write that `bw login` reads next.
    pub fn config_server(exe: &BwExecutable, profile: &BwProfile, url: &str) -> Self {
        Self::new(exe, profile, &["config", "server", url], TIMEOUT_LOCK)
    }

    /// `bw create folder` with the Base64 folder JSON on stdin.
    pub fn create_folder(
        exe: &BwExecutable,
        profile: &BwProfile,
        session: &SessionKey,
        encoded_json: Zeroizing<Vec<u8>>,
    ) -> Self {
        let mut invocation = Self::new(exe, profile, &["create", "folder"], TIMEOUT_WRITE)
            .with_session(Some(session));
        invocation.stdin = Some(encoded_json);
        invocation
    }

    /// `bw sync`.
    pub fn sync(exe: &BwExecutable, profile: &BwProfile, session: &SessionKey) -> Self {
        Self::new(exe, profile, &["sync"], TIMEOUT_SYNC).with_session(Some(session))
    }

    /// `bw list folders`.
    pub fn list_folders(exe: &BwExecutable, profile: &BwProfile, session: &SessionKey) -> Self {
        Self::new(exe, profile, &["list", "folders"], TIMEOUT_LIST).with_session(Some(session))
    }

    /// `bw list items [--folderid <id>]`.
    pub fn list_items(
        exe: &BwExecutable,
        profile: &BwProfile,
        session: &SessionKey,
        folder: Option<Uuid>,
    ) -> Self {
        let mut invocation =
            Self::new(exe, profile, &["list", "items"], TIMEOUT_LIST).with_session(Some(session));
        if let Some(folder) = folder {
            invocation.args.push(OsString::from("--folderid"));
            invocation.args.push(OsString::from(folder.to_string()));
        }
        invocation
    }

    /// `bw get item <id>` by exact UUID.
    pub fn get_item(
        exe: &BwExecutable,
        profile: &BwProfile,
        session: &SessionKey,
        item: Uuid,
    ) -> Self {
        Self::new(
            exe,
            profile,
            &["get", "item", &item.to_string()],
            TIMEOUT_GET,
        )
        .with_session(Some(session))
    }

    /// `bw create item` with the Base64 item JSON on stdin.
    pub fn create_item(
        exe: &BwExecutable,
        profile: &BwProfile,
        session: &SessionKey,
        encoded_json: Zeroizing<Vec<u8>>,
    ) -> Self {
        let mut invocation =
            Self::new(exe, profile, &["create", "item"], TIMEOUT_WRITE).with_session(Some(session));
        invocation.stdin = Some(encoded_json);
        invocation
    }

    /// `bw edit item <id>` with the complete Base64 item JSON on stdin.
    pub fn edit_item(
        exe: &BwExecutable,
        profile: &BwProfile,
        session: &SessionKey,
        item: Uuid,
        encoded_json: Zeroizing<Vec<u8>>,
    ) -> Self {
        let mut invocation = Self::new(
            exe,
            profile,
            &["edit", "item", &item.to_string()],
            TIMEOUT_WRITE,
        )
        .with_session(Some(session));
        invocation.stdin = Some(encoded_json);
        invocation
    }

    /// `bw delete item <id>` (moves to the trash; no permanent delete).
    pub fn delete_item(
        exe: &BwExecutable,
        profile: &BwProfile,
        session: &SessionKey,
        item: Uuid,
    ) -> Self {
        Self::new(
            exe,
            profile,
            &["delete", "item", &item.to_string()],
            TIMEOUT_WRITE,
        )
        .with_session(Some(session))
    }

    /// `bw restore item <id>`.
    pub fn restore_item(
        exe: &BwExecutable,
        profile: &BwProfile,
        session: &SessionKey,
        item: Uuid,
    ) -> Self {
        Self::new(
            exe,
            profile,
            &["restore", "item", &item.to_string()],
            TIMEOUT_WRITE,
        )
        .with_session(Some(session))
    }

    /// `bw lock`.
    pub fn lock(exe: &BwExecutable, profile: &BwProfile, session: &SessionKey) -> Self {
        Self::new(exe, profile, &["lock"], TIMEOUT_LOCK).with_session(Some(session))
    }

    /// `bw logout`.
    pub fn logout(exe: &BwExecutable, profile: &BwProfile, session: Option<&SessionKey>) -> Self {
        Self::new(exe, profile, &["logout"], TIMEOUT_LOCK).with_session(session)
    }

    fn with_session(mut self, session: Option<&SessionKey>) -> Self {
        self.secrets.session = session.map(SessionKey::clone_inner);
        self
    }

    /// The argv after the executable.
    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    /// Whether the child may prompt (only the interactive login adapter).
    pub fn is_interactive(&self) -> bool {
        self.interactive
    }

    /// Whether a stdin payload is attached.
    pub fn has_stdin_payload(&self) -> bool {
        self.stdin.is_some()
    }

    /// The deadline after which the child is killed.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Names of every variable the child will receive (values omitted).
    pub fn env_names(&self) -> BTreeSet<String> {
        let mut names: BTreeSet<String> = std::env::vars_os()
            .filter_map(|(key, _)| key.into_string().ok())
            .filter(|key| INHERITED_ENV.contains(&key.to_ascii_uppercase().as_str()))
            .collect();
        names.insert(DATA_DIR_ENV.to_owned());
        if !self.interactive {
            names.insert(NO_INTERACTION_ENV.to_owned());
        }
        if self.secrets.session.is_some() {
            names.insert(SESSION_ENV.to_owned());
        }
        if self.secrets.master_password.is_some() {
            names.insert(PASSWORD_ENV.to_owned());
        }
        names
    }

    pub(crate) fn take_stdin(&mut self) -> Option<Zeroizing<Vec<u8>>> {
        self.stdin.take()
    }

    /// Builds the process description with the policy applied.
    pub(crate) fn command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(self.exe.path());
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
        command.env(DATA_DIR_ENV, self.profile.data_dir());
        if !self.interactive {
            command.env(NO_INTERACTION_ENV, "true");
        }
        if let Some(session) = &self.secrets.session {
            command.env(SESSION_ENV, session.as_str());
        }
        if let Some(password) = &self.secrets.master_password {
            command.env(PASSWORD_ENV, password.as_str());
        }
        command.current_dir(self.profile.data_dir());
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::{Debug, Display};
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    static_assertions::assert_not_impl_any!(SessionKey: Debug, Display, Clone);
    static_assertions::assert_not_impl_any!(MasterPassword: Debug, Display, Clone);
    static_assertions::assert_not_impl_any!(BwInvocation: Debug, Display, Clone);

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A stub file standing in for `bw.exe`; removed when dropped so test
    /// runs leave nothing behind in the temp directory.
    struct TempExe(PathBuf);

    impl TempExe {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempExe {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn temp_exe(extension: &str) -> TempExe {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "codex-hosts-bitwarden-policy-{}-{unique}.{extension}",
            std::process::id()
        ));
        std::fs::write(&path, b"stub").unwrap();
        TempExe(path)
    }

    fn fixture() -> (BwExecutable, BwProfile) {
        // The executable is validated once at construction; the stub file can
        // go away afterwards.
        let stub = temp_exe("exe");
        let exe = BwExecutable::new(stub.path()).unwrap();
        let profile =
            BwProfile::new(std::env::temp_dir().join("codex-hosts-bitwarden-profile")).unwrap();
        (exe, profile)
    }

    fn arg_strings(invocation: &BwInvocation) -> Vec<String> {
        invocation
            .args()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn executable_must_be_an_absolute_existing_native_exe() {
        assert_eq!(
            BwExecutable::new("bw.exe"),
            Err(BwPolicyError::RelativePath(PathBuf::from("bw.exe")))
        );
        let cmd = temp_exe("cmd");
        assert_eq!(
            BwExecutable::new(cmd.path()),
            Err(BwPolicyError::NotAnExe(cmd.path().to_path_buf()))
        );
        let missing = std::env::temp_dir().join("codex-hosts-bitwarden-missing.exe");
        assert_eq!(
            BwExecutable::new(&missing),
            Err(BwPolicyError::NotAFile(missing.clone()))
        );
        let upper = temp_exe("EXE");
        assert!(BwExecutable::new(upper.path()).is_ok());
        assert_eq!(
            BwProfile::new("relative"),
            Err(BwPolicyError::RelativeDataDir(PathBuf::from("relative")))
        );
    }

    #[test]
    fn secrets_travel_only_in_the_environment() {
        let (exe, profile) = fixture();
        let session = SessionKey::new("sess-secret".to_owned());
        let password = MasterPassword::new("master-secret".to_owned());

        let unlock = BwInvocation::unlock(&exe, &profile, &password);
        assert_eq!(
            arg_strings(&unlock),
            ["unlock", "--raw", "--passwordenv", PASSWORD_ENV]
        );
        assert!(unlock.env_names().contains(PASSWORD_ENV));
        assert!(!unlock.env_names().contains(SESSION_ENV));

        let get = BwInvocation::get_item(&exe, &profile, &session, Uuid::nil());
        let args = arg_strings(&get);
        assert_eq!(
            args,
            ["get", "item", "00000000-0000-0000-0000-000000000000"]
        );
        assert!(!args.iter().any(|arg| arg.contains("--session")));
        assert!(!args.iter().any(|arg| arg.contains("secret")));
        assert!(get.env_names().contains(SESSION_ENV));
        assert!(!get.env_names().contains(PASSWORD_ENV));
        assert!(get.env_names().contains(DATA_DIR_ENV));
        assert!(get.env_names().contains(NO_INTERACTION_ENV));
        assert!(!get.env_names().contains("PATH"));
        assert!(!get.has_stdin_payload());
    }

    #[test]
    fn writes_carry_base64_json_on_stdin_only() {
        let (exe, profile) = fixture();
        let session = SessionKey::new("s".to_owned());
        let encoded = encode_item_json(br#"{"type":1,"name":"host","login":{"password":"p"}}"#);
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded.as_slice())
            .unwrap();
        assert!(decoded.starts_with(b"{\"type\":1"));

        let create = BwInvocation::create_item(&exe, &profile, &session, encoded);
        assert_eq!(arg_strings(&create), ["create", "item"]);
        assert!(create.has_stdin_payload());

        let edit = BwInvocation::edit_item(
            &exe,
            &profile,
            &session,
            Uuid::nil(),
            encode_item_json(b"{}"),
        );
        assert_eq!(
            arg_strings(&edit),
            ["edit", "item", "00000000-0000-0000-0000-000000000000"]
        );
        assert!(edit.has_stdin_payload());
        assert!(!arg_strings(&edit).iter().any(|arg| arg.contains("eyJ")));
    }

    #[test]
    fn list_items_filters_by_folder_id_only_when_given() {
        let (exe, profile) = fixture();
        let session = SessionKey::new("s".to_owned());
        let all = BwInvocation::list_items(&exe, &profile, &session, None);
        assert_eq!(arg_strings(&all), ["list", "items"]);
        let folder = Uuid::nil();
        let folder_text = folder.to_string();
        let filtered = BwInvocation::list_items(&exe, &profile, &session, Some(folder));
        assert_eq!(
            arg_strings(&filtered),
            ["list", "items", "--folderid", folder_text.as_str()]
        );
    }

    #[test]
    fn timeouts_follow_the_operation_table() {
        let (exe, profile) = fixture();
        let session = SessionKey::new("s".to_owned());
        assert_eq!(
            BwInvocation::version(&exe, &profile).timeout(),
            TIMEOUT_VERSION
        );
        assert_eq!(
            BwInvocation::status(&exe, &profile, None).timeout(),
            TIMEOUT_STATUS
        );
        assert_eq!(
            BwInvocation::sync(&exe, &profile, &session).timeout(),
            TIMEOUT_SYNC
        );
        assert_eq!(
            BwInvocation::lock(&exe, &profile, &session).timeout(),
            TIMEOUT_LOCK
        );
        assert!(!BwInvocation::logout(&exe, &profile, None).is_interactive());
    }

    #[test]
    fn only_the_login_may_prompt() {
        let (exe, profile) = fixture();
        let password = MasterPassword::new("master-secret".to_owned());
        let plain =
            BwInvocation::interactive_login(&exe, &profile, "me@example.com", &password, None);
        assert_eq!(
            arg_strings(&plain),
            [
                "login",
                "me@example.com",
                "--raw",
                "--passwordenv",
                PASSWORD_ENV
            ]
        );
        assert!(plain.is_interactive());
        assert!(!plain.env_names().contains(NO_INTERACTION_ENV));
        assert!(plain.env_names().contains(PASSWORD_ENV));
        assert!(!arg_strings(&plain).iter().any(|arg| arg.contains("secret")));
        assert_eq!(plain.timeout(), TIMEOUT_INTERACTIVE_LOGIN);
        let email = BwInvocation::interactive_login(
            &exe,
            &profile,
            "me@example.com",
            &password,
            Some(TwoFactorMethod::Email),
        );
        assert_eq!(&arg_strings(&email)[5..], ["--method", "1"]);
        assert!(!BwInvocation::unlock(&exe, &profile, &password).is_interactive());
    }
}
