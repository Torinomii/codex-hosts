//! Scripted `bw` answers for the vault tests.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::future::Future;
use std::sync::{Arc, Mutex};

use zeroize::Zeroizing;

use super::vault::CliRunner;
use codex_hosts_bw::{BwExecutable, BwInvocation, BwOutput, BwProfile, BwRunError};

/// One scripted answer: expected argv prefix, exit code, stdout, stderr.
pub(crate) type Answer = (Vec<&'static str>, Option<i32>, &'static str, &'static str);

/// A [`CliRunner`] that replays scripted answers in order, checking each
/// argv against the expected prefix.
#[derive(Clone)]
pub(crate) struct Scripted {
    answers: Arc<Mutex<VecDeque<Answer>>>,
}

impl Scripted {
    pub(crate) fn new(answers: Vec<Answer>) -> Self {
        Self {
            answers: Arc::new(Mutex::new(answers.into())),
        }
    }
}

impl CliRunner for Scripted {
    fn run(
        &self,
        invocation: BwInvocation,
    ) -> impl Future<Output = Result<BwOutput, BwRunError>> + Send {
        let args: Vec<String> = invocation
            .args()
            .iter()
            .map(|arg: &OsString| arg.to_string_lossy().into_owned())
            .collect();
        let next = self.answers.lock().unwrap().pop_front();
        async move {
            let (expected, exit_code, stdout, stderr) =
                next.unwrap_or_else(|| panic!("unexpected bw call {args:?}"));
            assert!(
                args.iter()
                    .zip(expected.iter())
                    .all(|(got, want)| got == want)
                    && args.len() >= expected.len(),
                "expected {expected:?}, got {args:?}"
            );
            Ok(BwOutput {
                exit_code,
                stdout: Zeroizing::new(stdout.as_bytes().to_vec()),
                stderr: Zeroizing::new(stderr.as_bytes().to_vec()),
            })
        }
    }
}

/// A stub `bw.exe` and profile directory under the temp directory.
pub(crate) fn stub_bw(dir_name: &str) -> (BwExecutable, BwProfile) {
    let dir =
        std::env::temp_dir().join(format!("codex-hosts-bw-{dir_name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe_path = dir.join("stub-bw.exe");
    std::fs::write(&exe_path, b"stub").unwrap();
    (
        BwExecutable::new(exe_path).unwrap(),
        BwProfile::new(dir).unwrap(),
    )
}
