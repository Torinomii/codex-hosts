//! Noninteractive util-linux su with target-user authentication and exact argv.
use crate::model::{HostProfile, Protocol, valid_sudo_username};
use crate::ssh::RemoteFailure;
use crate::sudo::{Frame, Launch, PasswordProtocol, quote};
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Saved SSH host alias. Advanced settings control password supply, not passwordless execution.
    pub alias: String,
    /// Absolute POSIX executable path; select /bin/sh explicitly for a script.
    pub program: String,
    /// Exact argv entries without expansion; no business stdin or password.
    #[serde(default)]
    pub args: Vec<String>,
    /// Required execution username for this call; choose root explicitly for root execution.
    pub user: String,
    #[serde(default)]
    pub connect_timeout_ms: Option<u64>,
    #[serde(default)]
    pub command_timeout_ms: Option<u64>,
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
            "exec_su requires an SSH host.",
        ));
    }
    let user = user
        .ok_or_else(|| {
            RemoteFailure::new(
                "SU_USER_REQUIRED",
                "Specify user for this call; execution targets are not saved in host settings.",
            )
        })?
        .trim();
    if !valid_sudo_username(user)
        || user.starts_with(['-', '#'])
        || user.chars().any(char::is_control)
    {
        return Err(RemoteFailure::new(
            "SU_INVALID_INPUT",
            "The su target username must be nonempty and contain no control characters or initial - or #.",
        ));
    }
    if !program.starts_with('/')
        || program.contains('\0')
        || args.iter().any(|arg| arg.contains('\0'))
    {
        return Err(RemoteFailure::new(
            "SU_INVALID_INPUT",
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
) -> Result<Launch<AuthProtocol>, RemoteFailure> {
    let user = validate(profile, program, args, user)?;
    let nonce = Uuid::new_v4();
    let protocol = AuthProtocol::new(nonce, user.clone());
    // su forwards the arguments following the username to /bin/sh -c.
    // Only the fixed script is executable syntax; caller data remains positional.
    // Neither BEGIN nor READY is present literally in argv/denial diagnostics.
    const SHIM: &str = "printf '\\036CH_SU_READY_%s\\037' \"$1\" >&2 || exit 125; IFS= read -r _codex_hosts_ack && [ \"$_codex_hosts_ack\" = \"CH_SU_ACK_$1\" ] || exit 125; shift; exec \"$@\"";
    const START: &str = "printf '\\036CH_SU_BEGIN_%s\\037' \"$1\" >&2 || exit 125; shift; exec /usr/bin/env LC_ALL=C /usr/bin/su \"$@\"";
    let mut argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        START.into(),
        "codex-hosts-su-start".into(),
        nonce.to_string(),
        "-s".into(),
        "/bin/sh".into(),
        "-c".into(),
        SHIM.into(),
        "--".into(),
        user.clone(),
        "codex-hosts-su".into(),
        nonce.to_string(),
        program.into(),
    ];
    argv.extend_from_slice(args);
    Ok(Launch {
        user,
        command: argv.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" "),
        protocol,
        ack: format!("CH_SU_ACK_{nonce}\n").into_bytes(),
    })
}

/// Credential Manager only; the runtime path is [`password_for_async`].
#[cfg(test)]
pub fn password_for(
    profile: &HostProfile,
    target: &str,
) -> Result<Zeroizing<String>, RemoteFailure> {
    // The shared password store is indexed by username. The SSH password can
    // only be reused when it belongs to this very target, never for another user.
    crate::sudo::su_password_for(profile, target)
}

/// The shared password store is indexed by username. The SSH password can
/// only be reused when it belongs to this very target, never for another user.
pub async fn password_for_async(
    profile: &HostProfile,
    target: &str,
    vault_password: Option<&Zeroizing<String>>,
) -> Result<Zeroizing<String>, RemoteFailure> {
    crate::sudo::su_password_for_async(profile, target, vault_password).await
}

pub struct AuthProtocol {
    begin: Vec<u8>,
    ready: Vec<u8>,
    target: String,
    pending: Vec<u8>,
    begun: bool,
    prompted: bool,
    line_start: bool,
    ready_seen: bool,
}

impl AuthProtocol {
    pub fn new(nonce: Uuid, target: String) -> Self {
        Self {
            begin: format!("\x1eCH_SU_BEGIN_{nonce}\x1f").into_bytes(),
            ready: format!("\x1eCH_SU_READY_{nonce}\x1f").into_bytes(),
            target,
            pending: Vec::new(),
            begun: false,
            prompted: false,
            line_start: true,
            ready_seen: false,
        }
    }
}

impl PasswordProtocol for AuthProtocol {
    const NAME: &'static str = "su";
    const INPUT_FAILED: &'static str = "SU_INPUT_FAILED";
    const START_FAILED: &'static str = "SU_START_FAILED";
    const CHANNEL_CLOSED: &'static str = "SU_CHANNEL_CLOSED";
    const INVALID_PASSWORD: &'static str = "SU_INVALID_PASSWORD";
    fn ready_seen(&self) -> bool {
        self.ready_seen
    }
    fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }

    fn feed(&mut self, data: &[u8]) -> Result<Vec<Frame>, RemoteFailure> {
        if self.ready_seen {
            return Ok(vec![Frame::Output(data.to_vec())]);
        }
        self.pending.extend_from_slice(data);
        let bytes = std::mem::take(&mut self.pending);
        let mut cursor = 0;
        let mut frames = Vec::new();
        while cursor < bytes.len() {
            let rest = &bytes[cursor..];
            if !self.begun && rest.starts_with(&self.begin) {
                cursor += self.begin.len();
                self.begun = true;
                self.line_start = true;
                continue;
            }
            if self.begun && rest.starts_with(&self.ready) {
                cursor += self.ready.len();
                self.ready_seen = true;
                frames.push(Frame::Ready);
                if cursor < bytes.len() {
                    frames.push(Frame::Output(bytes[cursor..].to_vec()));
                }
                cursor = bytes.len();
                break;
            }
            // Linux-PAM's C-locale password prompt on stderr. Only accept it
            // after our BEGIN, at a line boundary, and before the fixed shim.
            const PROMPT: &[u8] = b"Password: ";
            if self.begun && self.line_start && rest.starts_with(PROMPT) {
                if self.prompted {
                    return Err(RemoteFailure::new(
                        "SU_AUTH_REPEATED",
                        "su requested authentication again; no password is retried.",
                    ));
                }
                cursor += PROMPT.len();
                self.prompted = true;
                self.line_start = true;
                frames.push(Frame::Authenticate(self.target.clone()));
                continue;
            }
            if (!self.begun && self.begin.starts_with(rest))
                || (self.begun && self.ready.starts_with(rest))
                || (self.begun && self.line_start && PROMPT.starts_with(rest))
            {
                break;
            }
            let byte = bytes[cursor];
            cursor += 1;
            self.line_start = byte == b'\n' || byte == b'\r';
            if let Some(Frame::Output(bytes)) = frames.last_mut() {
                bytes.push(byte);
            } else {
                frames.push(Frame::Output(vec![byte]));
            }
        }
        self.pending.extend_from_slice(&bytes[cursor..]);
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_handles_every_split_and_stops_authentication_at_ready() {
        let nonce = Uuid::new_v4();
        let wire = format!(
            "warning\n\x1eCH_SU_BEGIN_{nonce}\x1fPassword: \x1eCH_SU_READY_{nonce}\x1fPassword: caller\n"
        );
        for split in 0..=wire.len() {
            let mut p = AuthProtocol::new(nonce, "bob".into());
            let mut frames = p.feed(&wire.as_bytes()[..split]).unwrap();
            frames.extend(p.feed(&wire.as_bytes()[split..]).unwrap());
            let users: Vec<_> = frames
                .iter()
                .filter_map(|f| {
                    if let Frame::Authenticate(u) = f {
                        Some(u.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(users, ["bob"]);
            assert_eq!(
                frames.iter().filter(|f| matches!(f, Frame::Ready)).count(),
                1
            );
            let output: Vec<_> = frames
                .into_iter()
                .filter_map(|f| {
                    if let Frame::Output(v) = f {
                        Some(v)
                    } else {
                        None
                    }
                })
                .flatten()
                .collect();
            assert_eq!(output, b"warning\nPassword: caller\n");
        }
    }
    #[test]
    fn passwordless_ready_does_not_authenticate_and_diagnostics_do_not_prompt() {
        let nonce = Uuid::new_v4();
        let mut p = AuthProtocol::new(nonce, "bob".into());
        let wire = format!(
            "Password: before\n\x1eCH_SU_BEGIN_{nonce}\x1fsu: user Password: does not exist\n\x1eCH_SU_READY_{nonce}\x1f"
        );
        let frames = p.feed(wire.as_bytes()).unwrap();
        assert!(!frames.iter().any(|f| matches!(f, Frame::Authenticate(_))));
        assert!(p.ready_seen());
        let mut p = AuthProtocol::new(nonce, "bob".into());
        let wire = format!("\x1eCH_SU_BEGIN_{nonce}\x1fPassword: \nPassword: ");
        assert_eq!(
            p.feed(wire.as_bytes()).err().unwrap().code,
            "SU_AUTH_REPEATED"
        );
    }
    #[test]
    fn validation_and_launcher_keep_caller_data_out_of_script_syntax() {
        let mut host = HostProfile::default();
        assert_eq!(
            validate(&host, "/bin/true", &[], None).unwrap_err().code,
            "SU_USER_REQUIRED"
        );
        assert_eq!(
            validate(&host, "/bin/true", &[], Some("bob")).unwrap(),
            "bob"
        );
        host.advanced.su = Some(crate::model::SuSettings { enabled: false });
        assert_eq!(
            validate(&host, "/bin/true", &[], None).unwrap_err().code,
            "SU_USER_REQUIRED"
        );
        assert_eq!(
            validate(&host, "/bin/true", &[], Some("root")).unwrap(),
            "root"
        );
        for user in ["-", "#0", "\u{1e}", "a\nb"] {
            assert!(validate(&host, "/bin/true", &[], Some(user)).is_err());
        }
        assert!(validate(&host, "id", &[], Some("root")).is_err());
        assert!(validate(&host, "/bin/true", &["\0".into()], Some("root")).is_err());
        let launch = launch(&host, "/bin/printf", &["'$()\n\\".into()], Some("bob")).unwrap();
        assert!(!launch.command.contains('\u{1e}'));
        assert!(launch.command.contains("exec \"$@\""));
        let request =
            serde_json::json!({"alias":"a","program":"/bin/true","user":"root","password":"x"});
        assert!(serde_json::from_value::<Request>(request).is_err());
    }
}
