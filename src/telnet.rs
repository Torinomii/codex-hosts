use std::future::Future;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::credentials::{self, CredentialKind};
use crate::model::HostProfile;
use crate::ssh::{OperationLimits, RemoteFailure, RemoteResult, TOTAL_TIMEOUT_CODE};

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;
const MAX_CAPTURE_BYTES: usize = 1024 * 1024;
const BEGIN_MARKER: &str = "__CODEX_HOSTS_BEGIN__";
const END_MARKER: &str = "__CODEX_HOSTS_END__";
/// The markers are sent split by an empty shell quote so a terminal that echoes
/// typed input never reproduces the contiguous marker; only the command's own
/// output contains it.
const BEGIN_MARKER_SOURCE: &str = "__CODEX_HOSTS_BEG''IN__";
const END_MARKER_SOURCE: &str = "__CODEX_HOSTS_E''ND__";
/// Text a login program prints when it rejects the credentials; `login`
/// delays it by a few seconds, so it can arrive after the command was sent.
const LOGIN_FAILURE_TEXTS: [&str; 3] =
    ["login incorrect", "authentication failed", "access denied"];

/// After the password, `login` discards typed-ahead input, so the command is
/// sent only once the server has gone quiet (or stayed silent) for this long.
const SHELL_SETTLE: Duration = Duration::from_millis(500);
const SHELL_SILENT_LIMIT: Duration = Duration::from_secs(3);

pub fn probe(
    profile: &HostProfile,
    limits: OperationLimits,
) -> Result<RemoteResult, RemoteFailure> {
    execute(profile, "hostname", limits)
}

pub fn execute(
    profile: &HostProfile,
    command: &str,
    limits: OperationLimits,
) -> Result<RemoteResult, RemoteFailure> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| RemoteFailure::new("RUNTIME_CREATE_FAILED", error.to_string()))?;
    let result = runtime.block_on(bounded(
        limits.total_timeout,
        TOTAL_TIMEOUT_CODE,
        "The complete Telnet operation exceeded its time limit.",
        execute_async(profile, command, limits),
    ));
    if limits.total_timeout.is_some() {
        runtime.shutdown_timeout(Duration::from_millis(50));
    } else {
        drop(runtime);
    }
    result
}

pub(crate) async fn execute_bounded(
    profile: &HostProfile,
    command: &str,
    limits: OperationLimits,
) -> Result<RemoteResult, RemoteFailure> {
    bounded(
        limits.total_timeout,
        TOTAL_TIMEOUT_CODE,
        "The complete Telnet operation exceeded its time limit.",
        execute_async(profile, command, limits),
    )
    .await
}

async fn execute_async(
    profile: &HostProfile,
    command: &str,
    limits: OperationLimits,
) -> Result<RemoteResult, RemoteFailure> {
    let password = credentials::load(profile.id, CredentialKind::Password)
        .map_err(|error| RemoteFailure::new("CREDENTIAL_READ_FAILED", error.to_string()))?
        .ok_or_else(|| {
            RemoteFailure::new(
                "CREDENTIAL_MISSING",
                format!("No password is saved for {}.", profile.alias),
            )
        })?;
    let mut stream = bounded(
        limits.connect_timeout,
        "CONNECT_TIMEOUT",
        "The Telnet TCP connection timed out.",
        async {
            TcpStream::connect((profile.address.as_str(), profile.port))
                .await
                .map_err(|error| RemoteFailure::new("CONNECT_FAILED", error.to_string()))
        },
    )
    .await?;
    let _ = stream.set_nodelay(true);

    let output = bounded(
        limits.command_timeout,
        "COMMAND_TIMEOUT",
        "Telnet authentication or remote command execution timed out.",
        run_session(
            &mut stream,
            &profile.username,
            password.as_str(),
            command,
            &login_prompts(profile),
        ),
    )
    .await?;
    let output_truncated = output.len() >= MAX_CAPTURE_BYTES;
    Ok(RemoteResult {
        status: "ok",
        alias: profile.alias.clone(),
        exit_code: 0,
        stdout: output,
        stderr: String::new(),
        output_truncated,
        host_fingerprint: None,
        host_key_algorithm: None,
        auth_key_fingerprint: None,
        verified_host_keys: Vec::new(),
    })
}

const DEFAULT_LOGIN_PROMPTS: &[&str] = &["login:", "username:", "user:"];
const DEFAULT_PASSWORD_PROMPTS: &[&str] = &["password:"];

struct LoginPrompts {
    login: Vec<String>,
    password: Vec<String>,
}

/// Profile overrides replace the default prompt list; an empty override keeps
/// the defaults so a half-filled form never makes a login impossible.
fn login_prompts(profile: &HostProfile) -> LoginPrompts {
    let custom = profile.advanced.telnet_prompts.as_ref();
    let pick = |value: Option<&str>, defaults: &[&str]| -> Vec<String> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            Some(value) => vec![value.to_owned()],
            None => defaults.iter().map(|value| (*value).to_owned()).collect(),
        }
    };
    LoginPrompts {
        login: pick(custom.map(|c| c.login.as_str()), DEFAULT_LOGIN_PROMPTS),
        password: pick(
            custom.map(|c| c.password.as_str()),
            DEFAULT_PASSWORD_PROMPTS,
        ),
    }
}

async fn run_session(
    stream: &mut TcpStream,
    username: &str,
    password: &str,
    command: &str,
    prompts: &LoginPrompts,
) -> Result<String, RemoteFailure> {
    let mut parser = TelnetParser::default();
    let mut transcript = Vec::new();
    let login = prompts.login.iter().map(String::as_str).collect::<Vec<_>>();
    read_until(stream, &mut parser, &mut transcript, &login).await?;
    stream
        .write_all(format!("{username}\r\n").as_bytes())
        .await
        .map_err(|error| RemoteFailure::new("TELNET_WRITE_FAILED", error.to_string()))?;
    transcript.clear();
    let password_prompts = prompts
        .password
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    read_until(stream, &mut parser, &mut transcript, &password_prompts).await?;
    stream
        .write_all(format!("{password}\r\n").as_bytes())
        .await
        .map_err(|error| RemoteFailure::new("TELNET_WRITE_FAILED", error.to_string()))?;
    transcript.clear();
    wait_for_shell(stream, &mut parser, &mut transcript).await?;

    let request = request_line(command);
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| RemoteFailure::new("TELNET_WRITE_FAILED", error.to_string()))?;
    transcript.clear();
    read_until(stream, &mut parser, &mut transcript, &[END_MARKER]).await?;
    Ok(extract_output(&String::from_utf8_lossy(&transcript)))
}

fn check_login_failure(transcript: &[u8]) -> Result<(), RemoteFailure> {
    let lowercase = String::from_utf8_lossy(transcript).to_ascii_lowercase();
    if LOGIN_FAILURE_TEXTS
        .iter()
        .any(|text| lowercase.contains(text))
    {
        return Err(RemoteFailure::new(
            "AUTH_FAILED",
            "The Telnet server rejected the user name or password.",
        ));
    }
    Ok(())
}

/// One line when possible, so an interactive shell prints no prompt between
/// the markers and the command's output; a multi-line command (or one ending
/// in `&`, which `;` would break) falls back to one line per statement.
fn request_line(command: &str) -> String {
    let trimmed = command.trim_end();
    if trimmed.contains(['\r', '\n']) || trimmed.ends_with('&') {
        format!("echo {BEGIN_MARKER_SOURCE}\r\n{command}\r\necho {END_MARKER_SOURCE}\r\n")
    } else {
        format!("echo {BEGIN_MARKER_SOURCE}; {trimmed}; echo {END_MARKER_SOURCE}\r\n")
    }
}

/// Everything the command printed between the two markers, ignoring echoed
/// input and prompts around them.
fn extract_output(transcript: &str) -> String {
    let after_begin = transcript
        .rsplit_once(BEGIN_MARKER)
        .map(|(_, value)| value)
        .unwrap_or(transcript);
    let before_end = after_begin
        .split_once(END_MARKER)
        .map(|(value, _)| value)
        .unwrap_or(after_begin);
    before_end.trim_matches(['\r', '\n', ' ']).to_owned()
}

/// Reads the post-login output (MOTD, prompt) until the server pauses, then
/// returns so the command is not typed ahead into a queue `login` flushes. A
/// rejected password is reported instead of waiting for the command timeout.
async fn wait_for_shell(
    stream: &mut TcpStream,
    parser: &mut TelnetParser,
    transcript: &mut Vec<u8>,
) -> Result<(), RemoteFailure> {
    let mut raw = [0_u8; 4096];
    let mut received = false;
    loop {
        let wait = if received {
            SHELL_SETTLE
        } else {
            SHELL_SILENT_LIMIT
        };
        match tokio::time::timeout(wait, stream.read(&mut raw)).await {
            Err(_) => return Ok(()),
            Ok(Err(error)) => {
                return Err(RemoteFailure::new("TELNET_READ_FAILED", error.to_string()));
            }
            Ok(Ok(0)) => {
                return Err(RemoteFailure::new(
                    "TELNET_CLOSED",
                    "The Telnet server closed the connection after the password.",
                ));
            }
            Ok(Ok(count)) => {
                received = true;
                let parsed = parser.consume(&raw[..count]);
                if !parsed.replies.is_empty() {
                    stream.write_all(&parsed.replies).await.map_err(|error| {
                        RemoteFailure::new("TELNET_WRITE_FAILED", error.to_string())
                    })?;
                }
                let remaining = MAX_CAPTURE_BYTES.saturating_sub(transcript.len());
                transcript.extend_from_slice(&parsed.data[..parsed.data.len().min(remaining)]);
                check_login_failure(transcript)?;
            }
        }
    }
}

async fn read_until(
    stream: &mut TcpStream,
    parser: &mut TelnetParser,
    transcript: &mut Vec<u8>,
    needles: &[&str],
) -> Result<(), RemoteFailure> {
    let mut raw = [0_u8; 4096];
    loop {
        let count = stream
            .read(&mut raw)
            .await
            .map_err(|error| RemoteFailure::new("TELNET_READ_FAILED", error.to_string()))?;
        if count == 0 {
            return Err(RemoteFailure::new(
                "TELNET_CLOSED",
                "The Telnet server closed the connection.",
            ));
        }
        let parsed = parser.consume(&raw[..count]);
        if !parsed.replies.is_empty() {
            stream
                .write_all(&parsed.replies)
                .await
                .map_err(|error| RemoteFailure::new("TELNET_WRITE_FAILED", error.to_string()))?;
        }
        let remaining = MAX_CAPTURE_BYTES.saturating_sub(transcript.len());
        transcript.extend_from_slice(&parsed.data[..parsed.data.len().min(remaining)]);
        let lowercase = String::from_utf8_lossy(transcript).to_ascii_lowercase();
        if needles
            .iter()
            .any(|needle| lowercase.contains(&needle.to_ascii_lowercase()))
        {
            return Ok(());
        }
        if needles == [END_MARKER] {
            check_login_failure(transcript)?;
        }
        if transcript.len() >= MAX_CAPTURE_BYTES {
            return Err(RemoteFailure::new(
                "OUTPUT_LIMIT",
                "Telnet output exceeded the one-megabyte safety limit.",
            ));
        }
    }
}

#[derive(Default)]
struct TelnetParser {
    state: ParserState,
}

#[derive(Default)]
enum ParserState {
    #[default]
    Data,
    Command,
    Option(u8),
    Subnegotiation,
    SubnegotiationCommand,
}

struct ParsedChunk {
    data: Vec<u8>,
    replies: Vec<u8>,
}

impl TelnetParser {
    fn consume(&mut self, bytes: &[u8]) -> ParsedChunk {
        let mut data = Vec::with_capacity(bytes.len());
        let mut replies = Vec::new();
        for byte in bytes.iter().copied() {
            self.state = match self.state {
                ParserState::Data if byte == IAC => ParserState::Command,
                ParserState::Data => {
                    data.push(byte);
                    ParserState::Data
                }
                ParserState::Command if byte == IAC => {
                    data.push(IAC);
                    ParserState::Data
                }
                ParserState::Command if matches!(byte, DO | DONT | WILL | WONT) => {
                    ParserState::Option(byte)
                }
                ParserState::Command if byte == SB => ParserState::Subnegotiation,
                ParserState::Command => ParserState::Data,
                ParserState::Option(command) => {
                    let refusal = if matches!(command, DO | DONT) {
                        WONT
                    } else {
                        DONT
                    };
                    replies.extend_from_slice(&[IAC, refusal, byte]);
                    ParserState::Data
                }
                ParserState::Subnegotiation if byte == IAC => ParserState::SubnegotiationCommand,
                ParserState::Subnegotiation => ParserState::Subnegotiation,
                ParserState::SubnegotiationCommand if byte == SE => ParserState::Data,
                ParserState::SubnegotiationCommand => ParserState::Subnegotiation,
            };
        }
        ParsedChunk { data, replies }
    }
}

async fn bounded<T, F>(
    duration: Option<Duration>,
    code: &'static str,
    message: &'static str,
    future: F,
) -> Result<T, RemoteFailure>
where
    F: Future<Output = Result<T, RemoteFailure>>,
{
    if let Some(duration) = duration {
        tokio::time::timeout(duration, future)
            .await
            .map_err(|_| RemoteFailure::new(code, message))?
    } else {
        future.await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_taken_between_the_printed_markers_even_when_input_is_echoed() {
        let echoed = format!(
            "$ echo {BEGIN_MARKER_SOURCE}\r\nuname\r\necho {END_MARKER_SOURCE}\r\n{BEGIN_MARKER}\r\nLinux\r\n{END_MARKER}\r\n$ "
        );
        assert_eq!(extract_output(&echoed), "Linux");
        assert!(!echoed.contains(&format!("echo {BEGIN_MARKER}")));
        let silent = format!("device> {BEGIN_MARKER}\r\nhi\r\n{END_MARKER}\r\ndevice> ");
        assert_eq!(extract_output(&silent), "hi");
        assert_eq!(
            request_line("uname -s  "),
            format!("echo {BEGIN_MARKER_SOURCE}; uname -s; echo {END_MARKER_SOURCE}\r\n")
        );
        assert!(request_line("sleep 1 &").starts_with(&format!("echo {BEGIN_MARKER_SOURCE}\r\n")));
        assert!(request_line("a\nb").contains("\r\na\nb\r\n"));
    }

    #[test]
    fn profile_prompts_override_defaults_only_when_filled() {
        let mut profile = HostProfile::default();
        let defaults = login_prompts(&profile);
        assert_eq!(defaults.login, ["login:", "username:", "user:"]);
        assert_eq!(defaults.password, ["password:"]);
        profile.advanced.telnet_prompts = Some(crate::model::TelnetPrompts {
            login: " Username: ".into(),
            password: String::new(),
        });
        let custom = login_prompts(&profile);
        assert_eq!(custom.login, ["Username:"]);
        assert_eq!(custom.password, ["password:"]);
    }

    #[test]
    fn strips_telnet_negotiation_and_refuses_options() {
        let mut parser = TelnetParser::default();
        let parsed = parser.consume(&[b'h', b'i', IAC, WILL, 1, b'!']);
        assert_eq!(parsed.data, b"hi!");
        assert_eq!(parsed.replies, [IAC, DONT, 1]);
    }

    #[test]
    fn parser_preserves_state_between_network_reads() {
        let mut parser = TelnetParser::default();
        assert!(parser.consume(&[IAC, DO]).data.is_empty());
        assert_eq!(parser.consume(&[3]).replies, [IAC, WONT, 3]);
    }
}
