//! Runs one [`BwInvocation`] (or any prepared CLI command, see
//! [`run_command`]) with bounded output and a hard timeout.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use zeroize::{Zeroize, Zeroizing};

use super::operation::{self, Control, Stage, Stop};
use super::policy::{BwInvocation, MAX_OUTPUT_BYTES};

/// What a finished child left behind. Both streams may contain whole vault
/// items or raw CLI diagnostics, so they are zeroized on drop and never
/// logged or forwarded as-is; callers parse stdout and classify by exit code.
pub struct BwOutput {
    /// Exit code, or `None` when the child was terminated by a signal.
    pub exit_code: Option<i32>,
    /// Captured stdout.
    pub stdout: Zeroizing<Vec<u8>>,
    /// Captured stderr.
    pub stderr: Zeroizing<Vec<u8>>,
}

/// Why a call did not produce a [`BwOutput`].
#[derive(Debug, thiserror::Error)]
pub enum BwRunError {
    /// The process could not be started.
    #[error("could not start the CLI: {0}")]
    Spawn(std::io::Error),
    /// A stream exceeded [`MAX_OUTPUT_BYTES`]; the child was killed.
    #[error("the CLI produced more than {MAX_OUTPUT_BYTES} bytes")]
    OutputTooLarge,
    /// The child did not finish within the invocation's timeout; it was killed.
    #[error("the CLI did not finish within {0:?}")]
    Timeout(Duration),
    /// The GUI cancelled this operation; the child was stopped and reaped.
    #[error("the credential operation was cancelled")]
    Cancelled,
    /// Reading from or writing to the child failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Spawns the child described by `invocation` and waits for it.
pub async fn run(mut invocation: BwInvocation) -> Result<BwOutput, BwRunError> {
    let timeout = invocation.timeout();
    let payload = invocation.take_stdin();
    run_command(invocation.command(), payload, timeout).await
}

/// Spawns `command` (environment, arguments and window flags already set by
/// the caller's policy), writes `payload` to its stdin, and waits at most
/// `timeout`; both streams are capped at [`MAX_OUTPUT_BYTES`]. Used by the
/// other password-manager CLI adapters so every child runs under the same
/// limits.
pub async fn run_command(
    mut command: tokio::process::Command,
    payload: Option<Zeroizing<Vec<u8>>>,
    timeout: Duration,
) -> Result<BwOutput, BwRunError> {
    let control = Control::current();
    let stage = control
        .as_ref()
        .map(|control| control.begin(command.as_std().get_args()));
    let timeout = control
        .as_ref()
        .map_or(timeout, |control| control.remaining(timeout));
    if let Some(control) = &control {
        if timeout.is_zero() {
            control.stop(Stop::TimedOut);
        }
        if let Some(reason) = control.snapshot().stopped {
            return Err(match reason {
                Stop::Cancelled => BwRunError::Cancelled,
                Stop::TimedOut => BwRunError::Timeout(timeout),
            });
        }
        if std::path::Path::new(command.as_std().get_program())
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("op.exe"))
            && matches!(stage, Some(Stage::Reading | Stage::Writing))
        {
            control.stage(Stage::Authorizing);
        }
    }
    command
        .stdin(if payload.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(BwRunError::Spawn)?;
    let stdin = child.stdin.take();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| BwRunError::Io(std::io::Error::other("child stdout was not captured")))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| BwRunError::Io(std::io::Error::other("child stderr was not captured")))?;

    let work = async {
        if let (Some(mut stdin), Some(payload)) = (stdin, payload) {
            stdin.write_all(&payload).await?;
            stdin.shutdown().await?;
            drop(stdin);
        }
        let progress = control.clone().zip(stage);
        let (stdout, stderr) =
            tokio::try_join!(read_capped(stdout, progress), read_capped(stderr, None))?;
        let status = child.wait().await?;
        Ok::<BwOutput, BwRunError>(BwOutput {
            exit_code: status.code(),
            stdout,
            stderr,
        })
    };

    let outcome = tokio::select! {
        result = tokio::time::timeout(timeout, work) => Some(result),
        () = operation::cancelled(control.as_ref()) => None,
    };
    match outcome {
        Some(Ok(Ok(output))) => Ok(output),
        Some(Ok(Err(error))) => {
            let _ = child.kill().await;
            Err(error)
        }
        Some(Err(_)) => {
            if let Some(control) = &control {
                control.stop(Stop::TimedOut);
            }
            let _ = child.kill().await;
            Err(BwRunError::Timeout(timeout))
        }
        None => {
            let _ = child.kill().await;
            Err(BwRunError::Cancelled)
        }
    }
}

impl std::fmt::Debug for BwOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BwOutput")
            .field("exit_code", &self.exit_code)
            .field("stdout_len", &self.stdout.len())
            .field("stderr_len", &self.stderr.len())
            .finish()
    }
}

async fn read_capped<R: AsyncRead + Unpin>(
    mut reader: R,
    mut progress: Option<(Control, Stage)>,
) -> Result<Zeroizing<Vec<u8>>, BwRunError> {
    let mut buffer = Zeroizing::new(Vec::new());
    let mut chunk = [0u8; 8192];
    let result = loop {
        let read = match reader.read(&mut chunk).await {
            Ok(0) => break Ok(()),
            Ok(read) => read,
            Err(error) => break Err(BwRunError::Io(error)),
        };
        if let Some((control, stage)) = progress.take() {
            control.stage(stage);
        }
        if buffer.len() + read > MAX_OUTPUT_BYTES {
            break Err(BwRunError::OutputTooLarge);
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    chunk.zeroize();
    result.map(|()| buffer)
}
