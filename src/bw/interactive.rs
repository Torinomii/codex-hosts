//! `bw login` driven through bw's own prompts.
//!
//! The two codes a login can need reach `bw` differently: a two-step login
//! code is a token the CLI could take as `--code`, but the e-mailed
//! new-device code is read only at the CLI's interactive prompt, and every
//! password grant without it makes the server send a fresh one. So the login
//! runs once with prompts enabled over pipes ([`BwInvocation::interactive_login`]):
//! this adapter recognises the few prompts it knows by their fixed wording,
//! tells the caller which code is wanted, and writes the caller's answer to
//! the child's stdin. A prompt it does not know kills the child. The CLI
//! writes its prompts, and inquirer's echo of the typed code, to stderr, so
//! stderr stays in a zeroizing buffer and is only classified.

use std::process::Stdio;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::UnboundedReceiver;
use zeroize::{Zeroize, Zeroizing};

use super::operation::{self, Control, Stage, Stop};
use super::output::{CliFailure, OutputError, classify_failure, parse_raw_session};
use super::policy::{BwInvocation, MAX_OUTPUT_BYTES, SessionKey};
use super::runner::BwRunError;

/// Which code `bw` is waiting for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodePrompt {
    /// `Two-step login code:` — from the authenticator app, the two-step
    /// e-mail (sent by `bw` just before), or a YubiKey.
    TwoStep,
    /// `New device verification required. Enter OTP sent to login email:` —
    /// the account has no two-step login and this device is new to it.
    NewDevice,
}

/// How an interactive login ended.
pub enum LoginOutcome {
    /// Logged in; the key `--raw` printed.
    LoggedIn(SessionKey),
    /// The account has several two-step methods and `bw` asked which to
    /// use; the child was stopped, and a rerun with `--method` continues.
    MethodRequired,
    /// `bw` asked for something this adapter does not answer; the child was
    /// stopped.
    UnexpectedPrompt,
    /// The caller withdrew by dropping its answer sender; the child was
    /// stopped.
    Cancelled,
    /// `bw` exited non-zero, classified from what it printed after the last
    /// answer.
    Failed(CliFailure),
}

/// Why an interactive login produced no [`LoginOutcome`].
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    /// Starting, reading, writing, the output cap, or the deadline.
    #[error(transparent)]
    Run(#[from] BwRunError),
    /// A successful exit printed no usable session key.
    #[error(transparent)]
    Output(#[from] OutputError),
}

/// What the stderr scan found at the next prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seen {
    Code(CodePrompt),
    Method,
    Unexpected,
}

const PROMPTS: &[(&[u8], Seen)] = &[
    (
        b"New device verification required.",
        Seen::Code(CodePrompt::NewDevice),
    ),
    (b"Two-step login code:", Seen::Code(CodePrompt::TwoStep)),
    (b"Two-step login method:", Seen::Method),
];

/// Runs `invocation` (built by [`BwInvocation::interactive_login`]). Each
/// code prompt is reported through `on_prompt`; the next string received on
/// `answers` is typed at it. Closing `answers` cancels the login.
pub async fn login(
    invocation: BwInvocation,
    mut on_prompt: impl FnMut(CodePrompt),
    answers: &mut UnboundedReceiver<Zeroizing<String>>,
) -> Result<LoginOutcome, LoginError> {
    debug_assert!(invocation.is_interactive());
    let control = Control::current();
    if let Some(control) = &control {
        control.stage(Stage::Authorizing);
    }
    let timeout = control.as_ref().map_or(invocation.timeout(), |control| {
        control.remaining(invocation.timeout())
    });
    if let Some(control) = &control {
        if timeout.is_zero() {
            control.stop(Stop::TimedOut);
        }
        if let Some(reason) = control.snapshot().stopped {
            return Err(match reason {
                Stop::Cancelled => BwRunError::Cancelled,
                Stop::TimedOut => BwRunError::Timeout(timeout),
            }
            .into());
        }
    }
    let deadline = tokio::time::Instant::now() + timeout;
    let mut command = invocation.command();
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(BwRunError::Spawn)?;
    let missing = || BwRunError::Io(std::io::Error::other("child stream was not captured"));
    let mut stdin = child.stdin.take().ok_or_else(missing)?;
    let mut stdout = child.stdout.take().ok_or_else(missing)?;
    let mut stderr = child.stderr.take().ok_or_else(missing)?;

    let mut scan = PromptScan::default();
    let mut out = Zeroizing::new(Vec::new());
    let mut err_chunk = Zeroizing::new([0u8; 4096]);
    let mut out_chunk = Zeroizing::new([0u8; 4096]);
    let (mut out_done, mut err_done) = (false, false);
    let mut awaiting = false;

    let result = loop {
        tokio::select! {
            read = stderr.read(&mut err_chunk[..]), if !err_done => match read {
                Ok(0) => err_done = true,
                Ok(read) => {
                    if !scan.push(&err_chunk[..read]) {
                        break Err(BwRunError::OutputTooLarge.into());
                    }
                    match scan.next_prompt() {
                        None => {}
                        Some(Seen::Code(prompt)) => {
                            awaiting = true;
                            on_prompt(prompt);
                        }
                        Some(Seen::Method) => break Ok(LoginOutcome::MethodRequired),
                        Some(Seen::Unexpected) => break Ok(LoginOutcome::UnexpectedPrompt),
                    }
                }
                Err(error) => break Err(BwRunError::Io(error).into()),
            },
            read = stdout.read(&mut out_chunk[..]), if !out_done => match read {
                Ok(0) => out_done = true,
                Ok(read) => {
                    if out.len() + read > MAX_OUTPUT_BYTES {
                        break Err(BwRunError::OutputTooLarge.into());
                    }
                    out.extend_from_slice(&out_chunk[..read]);
                }
                Err(error) => break Err(BwRunError::Io(error).into()),
            },
            answer = answers.recv() => match answer {
                None => break Ok(LoginOutcome::Cancelled),
                // An answer nobody asked for is dropped (and zeroized).
                Some(_) if !awaiting => {}
                Some(answer) => {
                    awaiting = false;
                    let mut line = Zeroizing::new(typed_line(&answer));
                    let written = async {
                        stdin.write_all(&line).await?;
                        stdin.flush().await
                    }
                    .await;
                    line.zeroize();
                    if let Err(error) = written {
                        break Err(BwRunError::Io(error).into());
                    }
                    scan.answered();
                }
            },
            status = child.wait(), if out_done && err_done => match status {
                Ok(status) if status.code() == Some(0) => {
                    break parse_raw_session(std::mem::take(&mut out))
                        .map(LoginOutcome::LoggedIn)
                        .map_err(LoginError::from);
                }
                Ok(_) => break Ok(LoginOutcome::Failed(classify_failure(scan.tail()))),
                Err(error) => break Err(BwRunError::Io(error).into()),
            },
            () = tokio::time::sleep_until(deadline) => {
                if let Some(control) = &control { control.stop(Stop::TimedOut); }
                break Err(BwRunError::Timeout(timeout).into());
            }
            () = operation::cancelled(control.as_ref()) => break Err(BwRunError::Cancelled.into()),
        }
    };
    // Every early exit leaves the child running; none of them may keep it.
    if !matches!(
        result,
        Ok(LoginOutcome::LoggedIn(_) | LoginOutcome::Failed(_))
    ) {
        let _ = child.kill().await;
    }
    result
}

/// The answer as one typed line: surrounding blanks dropped, no control
/// characters (a stray newline would answer a later prompt too).
fn typed_line(answer: &str) -> Vec<u8> {
    let mut line: Vec<u8> = answer
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>()
        .into_bytes();
    line.push(b'\n');
    line
}

/// Finds bw's prompts in its stderr. Inquirer re-renders the prompt line on
/// every typed key and ends it with a newline on submit, so after an answer
/// everything up to the next newline is the echo, not a new prompt.
#[derive(Default)]
struct PromptScan {
    buffer: Zeroizing<Vec<u8>>,
    /// Where the next prompt may start.
    from: usize,
    /// The echo of the last answer has not ended yet.
    echoing: bool,
}

impl PromptScan {
    /// Appends a chunk; false once the buffer would pass the output cap.
    fn push(&mut self, chunk: &[u8]) -> bool {
        if self.buffer.len() + chunk.len() > MAX_OUTPUT_BYTES {
            return false;
        }
        self.buffer.extend_from_slice(chunk);
        true
    }

    fn answered(&mut self) {
        self.from = self.buffer.len();
        self.echoing = true;
    }

    /// What bw printed since the last answer, for the failure classifier.
    fn tail(&self) -> &[u8] {
        &self.buffer[self.from.min(self.buffer.len())..]
    }

    /// The next complete prompt line, consumed so it is reported once.
    fn next_prompt(&mut self) -> Option<Seen> {
        if self.echoing {
            let newline = self.buffer[self.from..].iter().position(|&b| b == b'\n')?;
            self.from += newline + 1;
            self.echoing = false;
        }
        let region = &self.buffer[self.from..];
        let mut start = 0;
        while let Some(found) = find(&region[start..], b"? ") {
            let at = start + found;
            start = at + 2;
            if !at_line_start(region, at) {
                continue;
            }
            // Every bw prompt is one line ending in a colon; wait until the
            // colon has arrived so a split chunk is not misread.
            let line = &region[start..];
            let line = &line[..line.iter().position(|&b| b == b'\n').unwrap_or(line.len())];
            let colon = line.iter().position(|&b| b == b':')?;
            let seen = PROMPTS
                .iter()
                .find(|(text, _)| line.starts_with(text))
                .map_or(Seen::Unexpected, |(_, seen)| *seen);
            self.from += start + colon + 1;
            return Some(seen);
        }
        None
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Whether `at` begins a line once preceding ANSI control sequences
/// (`ESC [ params final`) are skipped.
fn at_line_start(region: &[u8], mut at: usize) -> bool {
    loop {
        if at == 0 || matches!(region[at - 1], b'\n' | b'\r') {
            return true;
        }
        let final_byte = region[at - 1];
        if !(0x40..=0x7e).contains(&final_byte) {
            return false;
        }
        let mut cursor = at - 1;
        while cursor > 0 && (0x30..=0x3f).contains(&region[cursor - 1]) {
            cursor -= 1;
        }
        if cursor >= 2 && region[cursor - 1] == b'[' && region[cursor - 2] == 0x1b {
            at = cursor - 2;
        } else {
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_chunks(chunks: &[&[u8]]) -> (PromptScan, Vec<Option<Seen>>) {
        let mut scan = PromptScan::default();
        let seen = chunks
            .iter()
            .map(|chunk| {
                assert!(scan.push(chunk));
                scan.next_prompt()
            })
            .collect();
        (scan, seen)
    }

    #[test]
    fn known_prompts_are_recognised_once_even_when_split() {
        let (mut scan, seen) = scan_chunks(&[
            b"Could not find data file; creating it instead.\n? New device verif",
            b"ication required. Enter OTP sent to login email: \x1b[17D\x1b[17C",
        ]);
        assert_eq!(seen, [None, Some(Seen::Code(CodePrompt::NewDevice))]);
        assert_eq!(scan.next_prompt(), None, "reported once");

        let (_, seen) = scan_chunks(&[b"? Two-step login code: "]);
        assert_eq!(seen, [Some(Seen::Code(CodePrompt::TwoStep))]);
        let (_, seen) = scan_chunks(&[b"\x1b[?25l? Two-step login method: (Use arrow keys)\n"]);
        assert_eq!(seen, [Some(Seen::Method)]);
        let (_, seen) = scan_chunks(&[b"? Master password: [input is hidden] "]);
        assert_eq!(seen, [Some(Seen::Unexpected)]);
        let (_, seen) = scan_chunks(&[b"Is this a question? No: it is text.\n"]);
        assert_eq!(seen, [None], "a question mark inside a line is no prompt");
    }

    #[test]
    fn the_echo_of_an_answer_is_not_a_new_prompt() {
        let (mut scan, _) = scan_chunks(&[b"? Two-step login code: \x1b[22D\x1b[22C"]);
        scan.answered();
        assert!(scan.push(b"\x1b[2K\x1b[G? Two-step login code: 1\x1b[23D\x1b[23C"));
        assert_eq!(scan.next_prompt(), None);
        assert!(scan.push(b"\x1b[2K\x1b[G? Two-step login code: 123456\x1b[28D\x1b[28C\n"));
        assert_eq!(scan.next_prompt(), None);
        assert!(scan.push(b"? New device verification required. Enter OTP sent to login email: "));
        assert_eq!(scan.next_prompt(), Some(Seen::Code(CodePrompt::NewDevice)));
        scan.answered();
        assert!(scan.push(b"\x1b[2K\x1b[G? New device verification required. Enter OTP sent to login email: 654321\n"));
        assert!(scan.push(b"invalid new device otp\n"));
        assert_eq!(scan.next_prompt(), None);
        assert_eq!(classify_failure(scan.tail()), CliFailure::InvalidCode);
    }

    #[test]
    fn answers_become_one_line_without_control_characters() {
        assert_eq!(typed_line(" 123 456\r\n"), b"123 456\n");
        assert_eq!(typed_line("12\n34"), b"1234\n");
    }
}
