//! Fixed sudo authentication protocol. Caller arguments remain data until READY.
use crate::credentials::{self, CredentialKind};
use crate::model::{HostProfile, Protocol, SshAuth, valid_sudo_username};
use crate::ssh::RemoteFailure;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Saved SSH host alias. Advanced settings control password supply, not passwordless execution.
    pub alias: String,
    /// Absolute executable path. For a shell script, explicitly select /bin/sh and args ["-c", "visible script"].
    pub program: String,
    /// Exact argv entries; no shell expansion. No business stdin or password parameters are accepted.
    #[serde(default)]
    pub args: Vec<String>,
    /// Required execution username for this call; choose root explicitly for root execution.
    pub user: String,
    #[serde(default)]
    pub connect_timeout_ms: Option<u64>,
    #[serde(default)]
    pub command_timeout_ms: Option<u64>,
}

pub struct Launch<P = AuthProtocol> {
    pub user: String,
    pub command: String,
    pub protocol: P,
    pub ack: Vec<u8>,
}

pub fn validate(
    profile: &HostProfile,
    program: &str,
    args: &[String],
    user: Option<&str>,
) -> Result<String, RemoteFailure> {
    if profile.protocol != Protocol::Ssh {
        return Err(RemoteFailure::new(
            "SSH_REQUIRED",
            "exec_sudo requires an SSH host.",
        ));
    }
    let user = user
        .ok_or_else(|| {
            RemoteFailure::new(
                "SUDO_USER_REQUIRED",
                "Specify user for this call; execution targets are not saved in host settings.",
            )
        })?
        .trim();
    if !valid_sudo_username(user) || user.starts_with('#') {
        return Err(RemoteFailure::new(
            "SUDO_INVALID_INPUT",
            "The sudo execution username must be nonempty and contain no CR, LF, NUL or initial #.",
        ));
    }
    if !program.starts_with('/')
        || program.contains('\0')
        || args.iter().any(|arg| arg.contains('\0'))
    {
        return Err(RemoteFailure::new(
            "SUDO_INVALID_INPUT",
            "program must be an absolute POSIX path; program and args cannot contain NUL.",
        ));
    }
    Ok(user.to_owned())
}

pub fn launch(
    profile: &HostProfile,
    program: &str,
    args: &[String],
    user: Option<&str>,
) -> Result<Launch, RemoteFailure> {
    let user = validate(profile, program, args, user)?;
    let nonce = Uuid::new_v4();
    let protocol = AuthProtocol::new(nonce);
    // The remote login shell only parses quoted arguments. This constant shim
    // signals that authentication is finished before it executes any caller code.
    // Never put the literal READY frame in argv: sudo may echo that argv when
    // rejecting a command, which must not look like the shim actually started.
    const SHIM: &str = "printf '\\036CH_SUDO_READY_%s\\037' \"$1\" >&2 || exit 125; IFS= read -r _codex_hosts_ack && [ \"$_codex_hosts_ack\" = \"CH_SUDO_ACK_$1\" ] || exit 125; shift; exec \"$@\"";
    let mut argv = vec![
        "/usr/bin/sudo".to_owned(),
        "-S".into(),
        "-p".into(),
        protocol.prompt(),
        "-u".into(),
        user.clone(),
        "--".into(),
        "/bin/sh".into(),
        "-c".into(),
        SHIM.into(),
        "codex-hosts-sudo".into(),
        nonce.to_string(),
        program.into(),
    ];
    argv.extend_from_slice(args);
    let command = argv
        .iter()
        .map(|arg| quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(Launch {
        user,
        command,
        protocol,
        ack: format!("CH_SUDO_ACK_{nonce}\n").into_bytes(),
    })
}

pub(crate) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Credential Manager only; the runtime path is [`password_for_async`].
#[cfg(test)]
pub fn password_for(
    profile: &HostProfile,
    username: &str,
) -> Result<Zeroizing<String>, RemoteFailure> {
    password_for_with(profile, username, |kind| {
        credentials::load(profile.id, kind)
    })
}

#[cfg(test)]
pub fn su_password_for(
    profile: &HostProfile,
    username: &str,
) -> Result<Zeroizing<String>, RemoteFailure> {
    password_for_method_with(profile, username, PasswordMethod::Su, |kind| {
        credentials::load(profile.id, kind)
    })
}

/// A vault host's login password comes from its item: the one this call's
/// admission already read (`vault_password`), else read again.
pub async fn password_for_async(
    profile: &HostProfile,
    username: &str,
    vault_password: Option<&Zeroizing<String>>,
) -> Result<Zeroizing<String>, RemoteFailure> {
    password_for_method_async(profile, username, PasswordMethod::Sudo, vault_password).await
}

/// Runtime variant of [`password_for_async`] for su.
pub async fn su_password_for_async(
    profile: &HostProfile,
    username: &str,
    vault_password: Option<&Zeroizing<String>>,
) -> Result<Zeroizing<String>, RemoteFailure> {
    password_for_method_async(profile, username, PasswordMethod::Su, vault_password).await
}

async fn password_for_method_async(
    profile: &HostProfile,
    username: &str,
    method: PasswordMethod,
    vault_password: Option<&Zeroizing<String>>,
) -> Result<Zeroizing<String>, RemoteFailure> {
    for candidate in candidates(profile, username, method)? {
        let password = match candidate {
            Candidate::Login if profile.is_vault_host() => match vault_password {
                Some(password) => Some(password.clone()),
                // Vault refusals keep their own codes: the user acts in the
                // credential manager, not in this host's Advanced settings.
                None => Some(crate::bitwarden::resolve_login_password(profile, None).await?),
            },
            candidate => credentials::load(profile.id, candidate.kind(profile))
                .map_err(|_| method.failure("read", username))?,
        };
        if let Some(password) = usable(password, method, username)? {
            return Ok(password);
        }
    }
    Err(method.failure("missing", username))
}

#[derive(Clone, Copy)]
enum PasswordMethod {
    Sudo,
    Su,
}

impl PasswordMethod {
    fn allowed(self, profile: &HostProfile) -> bool {
        profile.protocol == Protocol::Ssh
            && match self {
                Self::Sudo => profile.advanced.sudo.as_ref().is_some_and(|s| s.enabled),
                Self::Su => profile.advanced.su.as_ref().is_some_and(|s| s.enabled),
            }
    }

    fn failure(self, suffix: &str, username: &str) -> RemoteFailure {
        let (code, role) = match self {
            Self::Sudo => (
                match suffix {
                    "disabled" => "SUDO_AUTO_AUTH_DISABLED",
                    "read" => "SUDO_CREDENTIAL_READ_FAILED",
                    "invalid" => "SUDO_INVALID_PASSWORD",
                    _ => "SUDO_CREDENTIAL_MISSING",
                },
                "sudo authentication user",
            ),
            Self::Su => (
                match suffix {
                    "disabled" => "SU_AUTO_AUTH_DISABLED",
                    "read" => "SU_CREDENTIAL_READ_FAILED",
                    "invalid" => "SU_INVALID_PASSWORD",
                    _ => "SU_CREDENTIAL_MISSING",
                },
                "su target user",
            ),
        };
        let message = match suffix {
            "disabled" => format!(
                "Automatic password supply for {role} {username:?} is disabled in this host's Advanced settings. Remote authentication requested a password; no password was read or sent for this authentication request."
            ),
            "read" => format!("Cannot read the saved password for {role} {username:?}."),
            "invalid" => {
                format!("The password for {role} {username:?} cannot contain CR, LF or NUL.")
            }
            _ => format!(
                "No password is available for {role} {username:?}. Password-authenticated SSH logins reuse only that same user's login password; otherwise save a user password in Advanced settings."
            ),
        };
        RemoteFailure::new(code, message)
    }
}

pub fn login_password_available(profile: &HostProfile) -> bool {
    profile.protocol == Protocol::Ssh
        && profile.ssh_auth == SshAuth::Password
        && PasswordMethod::Sudo.allowed(profile)
        && (
            // Presence only: listing hosts never reads the vault.
            profile.is_vault_host()
                || credentials::load(profile.id, credentials::login_password_kind(profile))
                    .ok()
                    .flatten()
                    .is_some_and(|password| {
                        !password.is_empty() && validate_password(&password).is_ok()
                    })
        )
}

/// Where a password for `username` may come from, in the order it is tried.
#[derive(Clone, Copy)]
enum Candidate {
    /// A user password the user saved in Advanced settings.
    Record(Uuid),
    /// The caller's own SSH login password.
    Login,
}

impl Candidate {
    fn kind(self, profile: &HostProfile) -> CredentialKind {
        match self {
            Self::Record(id) => CredentialKind::SudoPassword(id),
            Self::Login => credentials::login_password_kind(profile),
        }
    }
}

/// Checks the requested purpose before any password is read. A saved user
/// password comes first. Ordinary sudo authenticates the caller, even when
/// executing as root or another user, so only that caller's
/// password-authenticated SSH secret may be reused: never a key passphrase
/// or another account's login password. Passwordless paths never get here.
fn candidates(
    profile: &HostProfile,
    username: &str,
    method: PasswordMethod,
) -> Result<Vec<Candidate>, RemoteFailure> {
    if !method.allowed(profile) {
        return Err(method.failure("disabled", username));
    }
    let mut candidates = Vec::with_capacity(2);
    if let Some(credential) = profile.advanced.sudo.as_ref().and_then(|settings| {
        settings
            .credentials
            .iter()
            .find(|credential| credential.username.trim() == username)
    }) {
        candidates.push(Candidate::Record(credential.id));
    }
    if profile.protocol == Protocol::Ssh
        && profile.ssh_auth == SshAuth::Password
        && username == profile.username.trim()
    {
        candidates.push(Candidate::Login);
    }
    Ok(candidates)
}

/// An empty slot counts as absent; a present password must be typeable.
fn usable(
    password: Option<Zeroizing<String>>,
    method: PasswordMethod,
    username: &str,
) -> Result<Option<Zeroizing<String>>, RemoteFailure> {
    match password.filter(|password| !password.is_empty()) {
        Some(password) => {
            validate_password(&password).map_err(|_| method.failure("invalid", username))?;
            Ok(Some(password))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
fn password_for_with<E>(
    profile: &HostProfile,
    username: &str,
    load: impl FnMut(CredentialKind) -> Result<Option<Zeroizing<String>>, E>,
) -> Result<Zeroizing<String>, RemoteFailure> {
    password_for_method_with(profile, username, PasswordMethod::Sudo, load)
}

#[cfg(test)]
fn password_for_method_with<E>(
    profile: &HostProfile,
    username: &str,
    method: PasswordMethod,
    mut load: impl FnMut(CredentialKind) -> Result<Option<Zeroizing<String>>, E>,
) -> Result<Zeroizing<String>, RemoteFailure> {
    for candidate in candidates(profile, username, method)? {
        let password =
            load(candidate.kind(profile)).map_err(|_| method.failure("read", username))?;
        if let Some(password) = usable(password, method, username)? {
            return Ok(password);
        }
    }
    Err(method.failure("missing", username))
}

pub fn validate_password(password: &str) -> Result<(), RemoteFailure> {
    if password.contains(['\r', '\n', '\0']) {
        return Err(RemoteFailure::new(
            "SUDO_INVALID_PASSWORD",
            "A sudo password cannot contain CR, LF or NUL.",
        ));
    }
    Ok(())
}

pub enum Frame {
    Output(Vec<u8>),
    Authenticate(String),
    Ready,
}

pub struct AuthProtocol {
    auth: Vec<u8>,
    ready: Vec<u8>,
    pending: Vec<u8>,
    pub ready_seen: bool,
    prompted: bool,
}

/// Shared SSH transport; authentication parsing remains specific to sudo/su.
pub trait PasswordProtocol {
    const NAME: &'static str;
    const INPUT_FAILED: &'static str;
    const START_FAILED: &'static str;
    const CHANNEL_CLOSED: &'static str;
    const INVALID_PASSWORD: &'static str;
    fn feed(&mut self, data: &[u8]) -> Result<Vec<Frame>, RemoteFailure>;
    fn finish(&mut self) -> Vec<u8>;
    fn ready_seen(&self) -> bool;
}

impl PasswordProtocol for AuthProtocol {
    const NAME: &'static str = "sudo";
    const INPUT_FAILED: &'static str = "SUDO_INPUT_FAILED";
    const START_FAILED: &'static str = "SUDO_START_FAILED";
    const CHANNEL_CLOSED: &'static str = "SUDO_CHANNEL_CLOSED";
    const INVALID_PASSWORD: &'static str = "SUDO_INVALID_PASSWORD";
    fn feed(&mut self, data: &[u8]) -> Result<Vec<Frame>, RemoteFailure> {
        self.feed(data)
    }
    fn finish(&mut self) -> Vec<u8> {
        self.finish()
    }
    fn ready_seen(&self) -> bool {
        self.ready_seen
    }
}

impl AuthProtocol {
    pub fn new(nonce: Uuid) -> Self {
        Self {
            auth: format!("\x1eCH_SUDO_AUTH_{nonce}:").into_bytes(),
            ready: format!("\x1eCH_SUDO_READY_{nonce}\x1f").into_bytes(),
            pending: Vec::new(),
            ready_seen: false,
            prompted: false,
        }
    }

    fn prompt(&self) -> String {
        format!("{}%p\x1f", String::from_utf8_lossy(&self.auth))
    }

    pub fn feed(&mut self, data: &[u8]) -> Result<Vec<Frame>, RemoteFailure> {
        if self.ready_seen {
            return Ok(vec![Frame::Output(data.to_vec())]);
        }
        self.pending.extend_from_slice(data);
        let mut frames = Vec::new();
        loop {
            if self.pending.starts_with(&self.ready) {
                self.pending.drain(..self.ready.len());
                self.ready_seen = true;
                frames.push(Frame::Ready);
                if !self.pending.is_empty() {
                    frames.push(Frame::Output(std::mem::take(&mut self.pending)));
                }
                break;
            }
            if self.pending.starts_with(&self.auth) {
                let rest = &self.pending[self.auth.len()..];
                if let Some(end) = rest.iter().position(|byte| *byte == 0x1f) {
                    let username = std::str::from_utf8(&rest[..end])
                        .ok()
                        .filter(|name| valid_sudo_username(name) && name.len() <= 1024)
                        .ok_or_else(|| {
                            RemoteFailure::new(
                                "SUDO_PROTOCOL_FAILED",
                                "Invalid sudo authentication username frame.",
                            )
                        })?
                        .to_owned();
                    if self.prompted {
                        return Err(RemoteFailure::new(
                            "SUDO_AUTH_REPEATED",
                            "sudo requested authentication again; the password was sent at most once. Check the password or authentication policy.",
                        ));
                    }
                    self.prompted = true;
                    self.pending.drain(..self.auth.len() + end + 1);
                    frames.push(Frame::Authenticate(username));
                    continue;
                }
                if rest.len() > 1024 {
                    return Err(RemoteFailure::new(
                        "SUDO_PROTOCOL_FAILED",
                        "The sudo authentication frame is too long.",
                    ));
                }
                break;
            }
            if self.auth.starts_with(&self.pending) || self.ready.starts_with(&self.pending) {
                break;
            }
            // Emit ordinary stderr in one block, retaining a possible split marker.
            let next = self
                .pending
                .iter()
                .enumerate()
                .skip(1)
                .find_map(|(i, byte)| (*byte == 0x1e).then_some(i))
                .unwrap_or(self.pending.len());
            frames.push(Frame::Output(self.pending.drain(..next).collect()));
        }
        Ok(frames)
    }

    pub fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}

/// Keep a short tail so a secret echoed across SSH packets is still redacted.
/// Both the matching bytes and the pending tail are zeroized on drop.
pub struct Redactor {
    secret: Zeroizing<Vec<u8>>,
    pending: Zeroizing<Vec<u8>>,
}

impl Redactor {
    pub fn new(secret: &str) -> Self {
        Self {
            secret: Zeroizing::new(secret.as_bytes().to_vec()),
            pending: Zeroizing::new(Vec::new()),
        }
    }

    pub fn feed(&mut self, data: &[u8], finish: bool) -> Vec<u8> {
        if self.secret.is_empty() {
            return data.to_vec();
        }
        self.pending.extend_from_slice(data);
        let mut output = Vec::new();
        let mut cursor = 0;
        while self.pending.len() - cursor >= self.secret.len() {
            if self.pending[cursor..].starts_with(&self.secret) {
                output.extend_from_slice(b"[REDACTED]");
                cursor += self.secret.len();
            } else {
                output.push(self.pending[cursor]);
                cursor += 1;
            }
        }
        if finish {
            let tail = &self.pending[cursor..];
            if !tail.is_empty() && self.secret.starts_with(tail) {
                output.extend_from_slice(b"[REDACTED]");
            } else {
                output.extend_from_slice(tail);
            }
            cursor = self.pending.len();
        }
        let remaining = self.pending.len() - cursor;
        self.pending.copy_within(cursor.., 0);
        self.pending[remaining..].zeroize();
        self.pending.truncate(remaining);
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_password_reuse_and_explicit_overrides_follow_the_actual_prompt() {
        let id = Uuid::new_v4();
        let mut host = HostProfile {
            username: "alice".into(),
            ..Default::default()
        };
        host.advanced.sudo = Some(crate::model::SudoSettings {
            enabled: true,
            ..Default::default()
        });
        let load = |kind| -> Result<_, ()> {
            Ok(Some(Zeroizing::new(match kind {
                CredentialKind::Password => "login-password".into(),
                CredentialKind::SudoPassword(record) if record == id => "override-password".into(),
                _ => panic!("sudo must not load unrelated credentials"),
            })))
        };
        assert_eq!(
            password_for_with(&host, "alice", load).unwrap().as_str(),
            "login-password"
        );
        for name in ["root", "bob", "Alice"] {
            assert_eq!(
                password_for_with(&host, name, |_| -> Result<_, ()> {
                    panic!("a different authentication user must not read the SSH login password")
                })
                .unwrap_err()
                .code,
                "SUDO_CREDENTIAL_MISSING"
            );
        }
        host.advanced.sudo = Some(crate::model::SudoSettings {
            enabled: true,
            credentials: vec![crate::model::SudoCredential {
                id,
                username: "alice".into(),
            }],
        });
        assert_eq!(
            password_for_with(&host, "alice", load).unwrap().as_str(),
            "override-password"
        );
        assert_eq!(
            password_for_with(&host, "alice", |kind| -> Result<_, ()> {
                Ok((kind == CredentialKind::Password)
                    .then(|| Zeroizing::new("login-password".into())))
            })
            .unwrap()
            .as_str(),
            "login-password"
        );
        assert_eq!(
            password_for_with(&host, "alice", |_| -> Result<_, ()> { Err(()) })
                .unwrap_err()
                .code,
            "SUDO_CREDENTIAL_READ_FAILED"
        );
    }

    #[test]
    fn a_vault_backed_login_counts_as_available_without_reading_the_vault() {
        let mut host = HostProfile {
            username: "alice".into(),
            ..Default::default()
        };
        host.advanced.sudo = Some(crate::model::SudoSettings {
            enabled: true,
            ..Default::default()
        });
        assert!(
            !login_password_available(&host),
            "nothing is saved for a fresh host"
        );
        host.vault = Some(crate::model::VaultHostRef {
            item_id: Uuid::new_v4(),
            item_name: "alice@host".into(),
            revision_date: String::new(),
        });
        assert!(login_password_available(&host));
        host.ssh_auth = SshAuth::PrivateKey;
        assert!(
            !login_password_available(&host),
            "a key login has no password to reuse, bound or not"
        );
    }

    #[test]
    fn a_vault_host_answers_sudo_and_su_with_the_password_its_admission_read() {
        let mut host = HostProfile {
            username: "alice".into(),
            ..Default::default()
        };
        host.advanced.sudo = Some(crate::model::SudoSettings {
            enabled: true,
            ..Default::default()
        });
        host.advanced.su = Some(crate::model::SuSettings { enabled: true });
        host.vault = Some(crate::model::VaultHostRef {
            item_id: Uuid::new_v4(),
            item_name: "alice@host".into(),
            revision_date: String::new(),
        });
        let admitted = Zeroizing::new("from-the-item".to_owned());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Reading the vault again would need a configured account (and,
        // under "every call", a second master password); none is set up here.
        let sudo = runtime
            .block_on(password_for_async(&host, "alice", Some(&admitted)))
            .unwrap();
        let su = runtime
            .block_on(su_password_for_async(&host, "alice", Some(&admitted)))
            .unwrap();
        assert_eq!(sudo.as_str(), "from-the-item");
        assert_eq!(su.as_str(), "from-the-item");
        let other = runtime
            .block_on(password_for_async(&host, "bob", Some(&admitted)))
            .unwrap_err();
        assert_eq!(
            other.code, "SUDO_CREDENTIAL_MISSING",
            "another account never gets the login password"
        );
    }

    #[test]
    fn keys_agents_empty_and_invalid_login_passwords_are_not_submitted_as_sudo_passwords() {
        let mut host = HostProfile {
            username: "alice".into(),
            ..Default::default()
        };
        host.advanced.sudo = Some(crate::model::SudoSettings {
            enabled: true,
            ..Default::default()
        });
        for auth in [SshAuth::PrivateKey, SshAuth::SshAgent] {
            host.ssh_auth = auth;
            assert_eq!(
                password_for_with(&host, "alice", |_| -> Result<_, ()> {
                    panic!("key and Agent logins have no reusable user password")
                })
                .unwrap_err()
                .code,
                "SUDO_CREDENTIAL_MISSING"
            );
        }
        host.ssh_auth = SshAuth::Password;
        for password in [None, Some(Zeroizing::new(String::new()))] {
            assert_eq!(
                password_for_with(&host, "alice", |_| -> Result<_, ()> {
                    Ok(password.clone())
                })
                .unwrap_err()
                .code,
                "SUDO_CREDENTIAL_MISSING"
            );
        }
        for password in ["line\nbreak", "bad\0password"] {
            assert_eq!(
                password_for_with(&host, "alice", |_| -> Result<_, ()> {
                    Ok(Some(Zeroizing::new(password.into())))
                })
                .unwrap_err()
                .code,
                "SUDO_INVALID_PASSWORD"
            );
        }
    }

    #[test]
    fn frames_survive_every_packet_boundary_and_ready_disables_prompts() {
        let nonce = Uuid::new_v4();
        let template = AuthProtocol::new(nonce);
        let prompt = template.prompt().replace("%p", "alice");
        let bytes = [
            b"banner".as_slice(),
            prompt.as_bytes(),
            &template.ready,
            prompt.as_bytes(),
        ]
        .concat();
        for split in 0..=bytes.len() {
            let mut protocol = AuthProtocol::new(nonce);
            let mut frames = protocol.feed(&bytes[..split]).unwrap();
            frames.extend(protocol.feed(&bytes[split..]).unwrap());
            assert_eq!(
                frames
                    .iter()
                    .filter(|frame| matches!(frame, Frame::Authenticate(name) if name == "alice"))
                    .count(),
                1
            );
            assert_eq!(
                frames
                    .iter()
                    .filter(|frame| matches!(frame, Frame::Ready))
                    .count(),
                1
            );
            let output: Vec<u8> = frames
                .into_iter()
                .filter_map(|frame| match frame {
                    Frame::Output(data) => Some(data),
                    _ => None,
                })
                .flatten()
                .collect();
            assert_eq!(output, [b"banner".as_slice(), prompt.as_bytes()].concat());
        }
    }

    #[test]
    fn repeated_authentication_stops_without_a_second_secret() {
        let mut protocol = AuthProtocol::new(Uuid::new_v4());
        let prompt = protocol.prompt().replace("%p", "root");
        assert!(
            matches!(protocol.feed(prompt.as_bytes()).unwrap().as_slice(), [Frame::Authenticate(name)] if name == "root")
        );
        assert_eq!(
            protocol.feed(prompt.as_bytes()).err().unwrap().code,
            "SUDO_AUTH_REPEATED"
        );
    }

    #[test]
    fn echo_redaction_survives_all_packet_boundaries() {
        let bytes = b"prefix complex' secret suffix";
        for split in 0..=bytes.len() {
            let mut redactor = Redactor::new("complex' secret");
            let mut output = redactor.feed(&bytes[..split], false);
            output.extend(redactor.feed(&bytes[split..], true));
            assert_eq!(output, b"prefix [REDACTED] suffix");
        }
        assert!(validate_password("  ").is_ok());
        assert!(validate_password("a\nb").is_err());
    }

    #[test]
    fn required_targets_and_su_diagnostics_preserve_the_requested_username() {
        for body in [
            serde_json::json!({"alias":"a","program":"/bin/true"}),
            serde_json::json!({"alias":"a","program":"/bin/true","user":null}),
        ] {
            assert!(serde_json::from_value::<Request>(body.clone()).is_err());
            assert!(serde_json::from_value::<crate::su::Request>(body).is_err());
        }
        let mut host = HostProfile {
            ssh_auth: SshAuth::PrivateKey,
            ..Default::default()
        };
        host.advanced.su = Some(crate::model::SuSettings { enabled: true });
        let error =
            password_for_method_with(&host, "sudoer", PasswordMethod::Su, |_| -> Result<_, ()> {
                panic!("No matching credential must be read")
            })
            .unwrap_err();
        assert_eq!(error.code, "SU_CREDENTIAL_MISSING");
        assert!(error.message.contains("\"sudoer\""));
        assert!(!error.message.contains("\"suer\""));
        for reason in ["read", "invalid", "missing"] {
            let error = PasswordMethod::Su.failure(reason, "sudo authentication user");
            assert!(error.message.contains("\"sudo authentication user\""));
            assert!(error.code.starts_with("SU_"));
        }
    }

    #[test]
    fn request_rejects_password_stdin_and_raw_command_before_execution() {
        for field in ["password", "stdin", "command"] {
            let mut request =
                serde_json::json!({"alias":"test", "program":"/usr/bin/id", "user":"root"});
            request[field] = serde_json::json!("must not be accepted");
            assert!(serde_json::from_value::<Request>(request).is_err());
        }
        let request: Request = serde_json::from_value(
            serde_json::json!({"alias":"test", "program":"/usr/bin/id", "user":"root"}),
        )
        .unwrap();
        assert!(request.args.is_empty());
        let mut host = HostProfile::default();
        assert_eq!(
            validate(&host, "/usr/bin/id", &[], None).unwrap_err().code,
            "SUDO_USER_REQUIRED"
        );
        assert_eq!(
            validate(&host, "/usr/bin/id", &[], Some("root")).unwrap(),
            "root"
        );
        host.advanced.sudo = Some(crate::model::SudoSettings {
            enabled: false,
            ..Default::default()
        });
        assert_eq!(
            validate(&host, "/usr/bin/id", &[], None).unwrap_err().code,
            "SUDO_USER_REQUIRED"
        );
        assert_eq!(
            validate(&host, "/usr/bin/id", &[], Some("root")).unwrap(),
            "root"
        );
        assert!(validate(&host, "id", &[], Some("root")).is_err());
        assert!(validate(&host, "/bin/echo", &["bad\0arg".into()], Some("root")).is_err());
        host.protocol = Protocol::Telnet;
        assert_eq!(
            validate(&host, "/usr/bin/id", &[], None).unwrap_err().code,
            "SSH_REQUIRED"
        );
    }

    #[test]
    fn denied_command_diagnostics_cannot_contain_a_literal_ready_frame() {
        let mut host = HostProfile::default();
        host.advanced.sudo = Some(crate::model::SudoSettings {
            enabled: true,
            ..Default::default()
        });
        let launch = launch(&host, "/usr/bin/id", &[], Some("root")).unwrap();
        assert!(
            !launch
                .command
                .as_bytes()
                .windows(launch.protocol.ready.len())
                .any(|window| window == launch.protocol.ready)
        );
        assert!(launch.command.contains("\\036CH_SUDO_READY_%s\\037"));
    }

    #[test]
    fn password_supply_permissions_are_independent_and_checked_before_secret_reads() {
        let mut host = HostProfile {
            username: "alice".into(),
            ..Default::default()
        };
        for method in [PasswordMethod::Sudo, PasswordMethod::Su] {
            let error = password_for_method_with(&host, "alice", method, |_| -> Result<_, ()> {
                panic!("missing settings must never authorize reading the login password")
            })
            .unwrap_err();
            assert_eq!(error.code, method.failure("disabled", "alice").code);
            assert!(error.message.contains("\"alice\""));
        }
        let id = Uuid::new_v4();
        for sudo_allowed in [false, true] {
            for su_allowed in [false, true] {
                host.advanced.sudo = Some(crate::model::SudoSettings {
                    enabled: sudo_allowed,
                    credentials: vec![crate::model::SudoCredential {
                        id,
                        username: "bob".into(),
                    }],
                });
                host.advanced.su = Some(crate::model::SuSettings {
                    enabled: su_allowed,
                });
                for (method, allowed) in [
                    (PasswordMethod::Sudo, sudo_allowed),
                    (PasswordMethod::Su, su_allowed),
                ] {
                    for username in ["alice", "bob"] {
                        let result = password_for_method_with(&host, username, method, |kind| {
                            assert!(allowed, "disabled purpose must not read any secret");
                            let expected = if username == "alice" {
                                CredentialKind::Password
                            } else {
                                CredentialKind::SudoPassword(id)
                            };
                            assert_eq!(kind, expected);
                            Ok::<_, ()>(Some(Zeroizing::new("allowed-password".into())))
                        });
                        if allowed {
                            assert_eq!(result.unwrap().as_str(), "allowed-password");
                        } else {
                            assert_eq!(
                                result.unwrap_err().code,
                                method.failure("disabled", username).code
                            );
                        }
                    }
                }
            }
        }
        host.protocol = Protocol::Telnet;
        for method in [PasswordMethod::Sudo, PasswordMethod::Su] {
            assert!(
                password_for_method_with(&host, "bob", method, |_| -> Result<_, ()> {
                    panic!("SSH-only credentials must not be supplied to a Telnet profile")
                })
                .is_err()
            );
        }
    }
}
