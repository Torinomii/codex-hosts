//! `open_host_editor`: runs the GUI editor as a child process and reads its
//! result file. The child is placed in a job object that kills it if this
//! server disappears, so an abandoned editor never outlives the Codex session.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ssh::RemoteFailure;
use crate::tool::SCHEMA_VERSION;

/// The editor waits for a person; this only bounds a window nobody ever closes.
const EDITOR_TIMEOUT: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct OpenHostEditorParams {
    /// Alias to create or edit; the editor is prefilled with every field given here.
    pub alias: String,
    /// Address or IP.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    /// Remote user name.
    #[serde(default)]
    pub user: Option<String>,
    /// `ssh` or `telnet`.
    #[serde(default)]
    pub protocol: Option<String>,
    /// `password`, `private-key` (key file or FIDO handle), or `ssh-agent`.
    #[serde(default)]
    pub auth: Option<String>,
    /// Private-key file or FIDO handle path.
    #[serde(default)]
    pub key_path: Option<String>,
    /// SHA-256 fingerprint of the Agent identity to use.
    #[serde(default)]
    pub agent_key_fingerprint: Option<String>,
    /// Alias of a verified SSH host to use as the jump host.
    #[serde(default)]
    pub jump_host: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Fingerprint reported by a HOSTKEY_UNKNOWN or HOSTKEY_MISMATCH failure; the user must confirm it.
    #[serde(default)]
    pub observed_fingerprint: Option<String>,
    #[serde(default)]
    pub observed_algorithm: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct EditorResult {
    schema_version: u32,
    /// `saved`, `trusted`, or `cancelled`.
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    alias: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Callback {
    status: String,
    #[serde(default)]
    alias: Option<String>,
}

pub(super) async fn open_host_editor(
    params: OpenHostEditorParams,
) -> Result<EditorResult, RemoteFailure> {
    if params.alias.trim().is_empty() {
        return Err(RemoteFailure::new(
            "EDITOR_ALIAS_REQUIRED",
            "open_host_editor needs the alias to create or edit.",
        ));
    }
    let exe = std::env::current_exe()
        .map_err(|error| RemoteFailure::new("EDITOR_LAUNCH_FAILED", error.to_string()))?;
    let result_path =
        std::env::temp_dir().join(format!("codex-hosts-editor-{}.json", uuid::Uuid::new_v4()));
    let _cleanup = RemoveOnDrop(result_path.clone());
    let mut command = tokio::process::Command::new(exe);
    command
        .arg("--codex-edit")
        .arg("--result-file")
        .arg(&result_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    push_args(&mut command, &params);
    allow_foreground();
    let mut child = command
        .spawn()
        .map_err(|error| RemoteFailure::new("EDITOR_LAUNCH_FAILED", error.to_string()))?;
    bind_to_job(&child);
    let status = tokio::time::timeout(EDITOR_TIMEOUT, child.wait())
        .await
        .map_err(|_| {
            RemoteFailure::new(
                "EDITOR_TIMEOUT",
                "The host editor stayed open for an hour without a result.",
            )
        })?
        .map_err(|error| RemoteFailure::new("EDITOR_WAIT_FAILED", error.to_string()))?;
    let bytes = std::fs::read(&result_path).map_err(|_| {
        RemoteFailure::new(
            "EDITOR_NO_RESULT",
            format!(
                "The host editor exited with {status} without writing a result; treat it as not saved."
            ),
        )
    })?;
    let callback: Callback = serde_json::from_slice(&bytes)
        .map_err(|error| RemoteFailure::new("EDITOR_RESULT_INVALID", error.to_string()))?;
    Ok(EditorResult {
        schema_version: SCHEMA_VERSION,
        status: callback.status,
        alias: callback.alias,
    })
}

fn push_args(command: &mut tokio::process::Command, params: &OpenHostEditorParams) {
    let mut flag = |name: &str, value: &Option<String>| {
        if let Some(value) = value {
            command.arg(name).arg(value);
        }
    };
    flag("--alias", &Some(params.alias.clone()));
    flag("--host", &params.host);
    flag("--port", &params.port.map(|port| port.to_string()));
    flag("--user", &params.user);
    flag("--protocol", &params.protocol);
    flag("--auth", &params.auth);
    flag("--key-path", &params.key_path);
    flag("--agent-key-fingerprint", &params.agent_key_fingerprint);
    flag("--jump-host", &params.jump_host);
    flag("--description", &params.description);
    flag("--observed-fingerprint", &params.observed_fingerprint);
    flag("--observed-algorithm", &params.observed_algorithm);
    for tag in params.tags.iter().flatten() {
        command.arg("--tag").arg(tag);
    }
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Best effort: a process launched from the background usually lacks the
/// foreground right to hand on, in which case the editor flashes on the taskbar.
#[cfg(windows)]
fn allow_foreground() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
    unsafe {
        AllowSetForegroundWindow(ASFW_ANY);
    }
}

#[cfg(not(windows))]
fn allow_foreground() {}

/// One job object per server process; every editor child joins it so closing
/// the server (or Codex killing it) closes the editors too. The temporary
/// secrets holder is spawned elsewhere and deliberately not part of this job.
#[cfg(windows)]
fn bind_to_job(child: &tokio::process::Child) {
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    struct Job(usize);
    // SAFETY: a job object handle is process-wide and safe to use from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    static JOB: OnceLock<Option<Job>> = OnceLock::new();
    let job = JOB.get_or_init(|| unsafe {
        let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if handle.is_null() {
            return None;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let set = SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&info).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        (set != 0).then_some(Job(handle as usize))
    });
    if let (Some(job), Some(handle)) = (job, child.raw_handle()) {
        unsafe {
            AssignProcessToJobObject(job.0 as _, handle as _);
        }
    }
}

#[cfg(not(windows))]
fn bind_to_job(_child: &tokio::process::Child) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefill_becomes_editor_flags_without_a_shell() {
        let params = OpenHostEditorParams {
            alias: "web 1".into(),
            host: Some("h".into()),
            port: Some(2222),
            user: None,
            protocol: Some("ssh".into()),
            auth: None,
            key_path: None,
            agent_key_fingerprint: None,
            jump_host: None,
            description: Some("a \"quoted\" note".into()),
            tags: Some(vec!["prod".into(), "web".into()]),
            observed_fingerprint: Some("SHA256:abc".into()),
            observed_algorithm: None,
        };
        let mut command = tokio::process::Command::new("x");
        push_args(&mut command, &params);
        let args = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "--alias",
                "web 1",
                "--host",
                "h",
                "--port",
                "2222",
                "--protocol",
                "ssh",
                "--description",
                "a \"quoted\" note",
                "--observed-fingerprint",
                "SHA256:abc",
                "--tag",
                "prod",
                "--tag",
                "web"
            ]
        );
    }
}
