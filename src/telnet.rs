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
        run_session(&mut stream, &profile.username, password.as_str(), command),
    )
    .await?;
    Ok(RemoteResult {
        status: "ok",
        alias: profile.alias.clone(),
        exit_code: 0,
        stdout: output,
        stderr: String::new(),
        host_fingerprint: None,
    })
}

async fn run_session(
    stream: &mut TcpStream,
    username: &str,
    password: &str,
    command: &str,
) -> Result<String, RemoteFailure> {
    let mut parser = TelnetParser::default();
    let mut transcript = Vec::new();
    read_until(
        stream,
        &mut parser,
        &mut transcript,
        &["login:", "username:", "user:"],
    )
    .await?;
    stream
        .write_all(format!("{username}\r\n").as_bytes())
        .await
        .map_err(|error| RemoteFailure::new("TELNET_WRITE_FAILED", error.to_string()))?;
    transcript.clear();
    read_until(stream, &mut parser, &mut transcript, &["password:"]).await?;
    stream
        .write_all(format!("{password}\r\n").as_bytes())
        .await
        .map_err(|error| RemoteFailure::new("TELNET_WRITE_FAILED", error.to_string()))?;

    let request = format!("echo {BEGIN_MARKER}\r\n{command}\r\necho {END_MARKER}\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| RemoteFailure::new("TELNET_WRITE_FAILED", error.to_string()))?;
    transcript.clear();
    read_until(stream, &mut parser, &mut transcript, &[END_MARKER]).await?;
    let text = String::from_utf8_lossy(&transcript);
    let after_begin = text
        .split_once(BEGIN_MARKER)
        .map(|(_, value)| value)
        .unwrap_or(text.as_ref());
    let before_end = after_begin
        .split_once(END_MARKER)
        .map(|(value, _)| value)
        .unwrap_or(after_begin);
    Ok(before_end.trim_matches(['\r', '\n', ' ']).to_owned())
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
