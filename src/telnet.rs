use std::future::Future;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::credentials;
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

/// The markers that frame one call's output. The random tag keeps output that
/// merely contains a marker (a log of an earlier call, this program's source)
/// from ending the capture. Each is sent split by an empty shell quote so a
/// terminal that echoes typed input never reproduces the contiguous marker;
/// only the shell's `echo` prints it.
struct Markers {
    begin: String,
    end: String,
    begin_source: String,
    end_source: String,
}

impl Markers {
    fn new() -> Self {
        Self::with_tag(&uuid::Uuid::new_v4().simple().to_string())
    }

    fn with_tag(tag: &str) -> Self {
        Self {
            begin: format!("__CODEX_HOSTS_BEGIN_{tag}__"),
            end: format!("__CODEX_HOSTS_END_{tag}__"),
            begin_source: format!("__CODEX_HOSTS_BEG''IN_{tag}__"),
            end_source: format!("__CODEX_HOSTS_E''ND_{tag}__"),
        }
    }
}

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
    let password = credentials::load(profile.id, credentials::login_password_kind(profile))
        .map_err(|error| RemoteFailure::new("CREDENTIAL_READ_FAILED", error.to_string()))?
        .ok_or_else(|| {
            RemoteFailure::new(
                "CREDENTIAL_MISSING",
                format!(
                    "No password is saved for {}; open_host_editor for this alias lets the user add one.",
                    profile.alias
                ),
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

    let (output, exit_status) = bounded(
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
    // A shell that prints no status after the end marker keeps the old 0.
    let exit_code = exit_status.unwrap_or(0);
    Ok(RemoteResult {
        status: if exit_code == 0 { "ok" } else { "remote_error" },
        alias: profile.alias.clone(),
        exit_code,
        stdout: output,
        stderr: String::new(),
        // Output over the capture limit fails with OUTPUT_LIMIT instead.
        output_truncated: false,
        sudo_user: None,
        su_user: None,
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
) -> Result<(String, Option<i32>), RemoteFailure> {
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

    let markers = Markers::new();
    let request = request_line(command, &markers);
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| RemoteFailure::new("TELNET_WRITE_FAILED", error.to_string()))?;
    transcript.clear();
    let exit_status = read_until_output(
        stream,
        &mut parser,
        &mut transcript,
        request.trim_end(),
        &markers,
    )
    .await?;
    Ok((
        extract_output(&String::from_utf8_lossy(&transcript), &markers),
        exit_status,
    ))
}

/// Reads until the line carrying the end marker is complete and returns the
/// exit status printed after it, if any. Anything the server says before the
/// begin marker is printed, other than the echo of our own request line, came
/// from the login program, so a login-failure message there ends the call.
async fn read_until_output(
    stream: &mut TcpStream,
    parser: &mut TelnetParser,
    transcript: &mut Vec<u8>,
    request: &str,
    markers: &Markers,
) -> Result<Option<i32>, RemoteFailure> {
    let request = request.to_ascii_lowercase();
    let begin = markers.begin.to_ascii_lowercase();
    let mut begin_seen = false;
    // Only the unsearched tail (plus a marker's length of overlap) is scanned
    // per read, so a large output costs linear time.
    let mut end_search_from = 0;
    let mut end_at = None;
    let mut raw = [0_u8; 4096];
    loop {
        let count = stream
            .read(&mut raw)
            .await
            .map_err(|error| RemoteFailure::new("TELNET_READ_FAILED", error.to_string()))?;
        if count == 0 {
            // The status is complete even if the server hung up right after it.
            if let Some(status) = end_at.and_then(|at| end_status(transcript, at, markers, true)) {
                return Ok(status);
            }
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
        if end_at.is_none() {
            end_at = find_bytes(transcript, markers.end.as_bytes(), end_search_from);
            end_search_from = transcript.len().saturating_sub(markers.end.len() - 1);
        }
        if let Some(status) = end_at.and_then(|at| end_status(transcript, at, markers, false)) {
            return Ok(status);
        }
        if !begin_seen {
            let lowercase = String::from_utf8_lossy(transcript).to_ascii_lowercase();
            let server_said = lowercase.replacen(&request, "", 1);
            let before_output = server_said.find(&begin);
            check_login_failure(
                &server_said.as_bytes()[..before_output.unwrap_or(server_said.len())],
            )?;
            begin_seen = before_output.is_some();
        }
        if transcript.len() >= MAX_CAPTURE_BYTES {
            return Err(RemoteFailure::new(
                "OUTPUT_LIMIT",
                "Telnet output exceeded the one-megabyte safety limit.",
            ));
        }
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|position| from + position)
}

/// The status the shell printed right after the end marker at `end_at`, once
/// that line is complete: `Some(Some(code))`, or `Some(None)` when the line
/// carries no digits (a shell without `$?`). `None` while digits may still follow.
fn end_status(
    transcript: &[u8],
    end_at: usize,
    markers: &Markers,
    closed: bool,
) -> Option<Option<i32>> {
    let rest = &transcript[end_at + markers.end.len()..];
    let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
    if digits == rest.len() && !closed {
        return None;
    }
    Some(
        std::str::from_utf8(&rest[..digits])
            .ok()
            .and_then(|digits| digits.parse().ok()),
    )
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
/// in `&` or `;`, which the joining `;` would break) falls back to one line per
/// statement. `$?` after the end marker reports the command's exit status.
fn request_line(command: &str, markers: &Markers) -> String {
    let trimmed = command.trim_end();
    let Markers {
        begin_source,
        end_source,
        ..
    } = markers;
    if trimmed.contains(['\r', '\n', '#']) || trimmed.ends_with(['&', ';']) {
        format!("echo {begin_source}\r\n{command}\r\necho {end_source}$?\r\n")
    } else {
        format!("echo {begin_source}; {trimmed}; echo {end_source}$?\r\n")
    }
}

/// Everything the command printed between the two markers, ignoring echoed
/// input and prompts around them.
fn extract_output(transcript: &str, markers: &Markers) -> String {
    let after_begin = transcript
        .rsplit_once(markers.begin.as_str())
        .map(|(_, value)| value)
        .unwrap_or(transcript);
    let before_end = after_begin
        .split_once(markers.end.as_str())
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
    fn login_failure_is_only_recognised_before_the_request_echo() {
        let request = request_line("echo 'Access denied'", &Markers::new());
        let echoed = format!("$ {request}");
        let server_said =
            echoed
                .to_ascii_lowercase()
                .replacen(&request.trim_end().to_ascii_lowercase(), "", 1);
        assert!(check_login_failure(server_said.as_bytes()).is_ok());
        let late = format!("{request}\r\nLogin incorrect\r\nlogin: ");
        let server_said =
            late.to_ascii_lowercase()
                .replacen(&request.trim_end().to_ascii_lowercase(), "", 1);
        assert!(check_login_failure(server_said.as_bytes()).is_err());
        assert_eq!(
            check_login_failure(b"\r\nLogin incorrect\r\nlogin: ")
                .unwrap_err()
                .code,
            "AUTH_FAILED"
        );
    }

    #[test]
    fn output_is_taken_between_the_printed_markers_even_when_input_is_echoed() {
        let markers = Markers::with_tag("t1");
        let Markers {
            begin,
            end,
            begin_source,
            end_source,
        } = &markers;
        let echoed = format!(
            "$ echo {begin_source}\r\nuname\r\necho {end_source}$?\r\n{begin}\r\nLinux\r\n{end}0\r\n$ "
        );
        assert_eq!(extract_output(&echoed, &markers), "Linux");
        assert!(!echoed.contains(&format!("echo {begin}")));
        let silent = format!("device> {begin}\r\nhi\r\n{end}0\r\ndevice> ");
        assert_eq!(extract_output(&silent, &markers), "hi");
        assert_eq!(
            request_line("uname -s  ", &markers),
            format!("echo {begin_source}; uname -s; echo {end_source}$?\r\n")
        );
        assert!(
            request_line("sleep 1 &", &markers).starts_with(&format!("echo {begin_source}\r\n"))
        );
        assert!(request_line("ls;", &markers).contains("\r\nls;\r\n"));
        assert!(request_line("a\nb", &markers).contains("\r\na\nb\r\n"));
        assert!(request_line("uptime # note", &markers).contains("\r\nuptime # note\r\n"));
    }

    #[test]
    fn markers_differ_per_call_so_output_cannot_end_the_capture() {
        let first = Markers::new();
        let second = Markers::new();
        assert_ne!(first.end, second.end);
        let transcript = format!(
            "{}\r\nold log: {}0\r\nmore\r\n{}0\r\n",
            first.begin, second.end, first.end
        );
        assert_eq!(
            extract_output(&transcript, &first),
            format!("old log: {}0\r\nmore", second.end)
        );
    }

    #[test]
    fn exit_status_is_read_only_once_its_line_is_complete() {
        let markers = Markers::with_tag("t2");
        let at = |text: &str| find_bytes(text.as_bytes(), markers.end.as_bytes(), 0).unwrap();
        let partial = format!("x{}1", markers.end);
        assert_eq!(
            end_status(partial.as_bytes(), at(&partial), &markers, false),
            None
        );
        assert_eq!(
            end_status(partial.as_bytes(), at(&partial), &markers, true),
            Some(Some(1))
        );
        let complete = format!("x{}127\r\n$ ", markers.end);
        assert_eq!(
            end_status(complete.as_bytes(), at(&complete), &markers, false),
            Some(Some(127))
        );
        let no_status = format!("{}$?\r\n", markers.end);
        assert_eq!(
            end_status(no_status.as_bytes(), at(&no_status), &markers, false),
            Some(None)
        );
    }

    #[test]
    fn end_marker_is_found_across_read_boundaries() {
        let marker = b"__END__";
        let first = b"output __EN";
        let mut transcript = first.to_vec();
        assert_eq!(find_bytes(&transcript, marker, 0), None);
        let resume = transcript.len().saturating_sub(marker.len() - 1);
        transcript.extend_from_slice(b"D__0\r\n");
        assert_eq!(find_bytes(&transcript, marker, resume), Some(7));
        assert_eq!(find_bytes(b"ab", marker, 5), None);
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
