//! The "enter the master password" prompt a tool call raises in the window.
//!
//! A tool call that finds no usable session asks the running codex-hosts
//! window (starting it if needed, like SSH call approvals do) to unlock the
//! vault. The window shows a password prompt, runs `bw unlock`, stores the key
//! per the policy, and answers "unlocked"; the caller then reads the key from
//! Credential Manager. The pipe between the two carries ids and statuses,
//! never the password or the key.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::ssh::RemoteFailure;
use crate::temporary_secrets::{self, Request};

/// How long a prompt waits for the user.
pub(crate) const LIFETIME: Duration = Duration::from_secs(90);
const MAX_PROMPTS: usize = 8;

struct Entry {
    id: Uuid,
    status: &'static str,
    expires: Instant,
}

/// Prompts raised by tool calls, as the window sees them.
#[derive(Default)]
pub(crate) struct UnlockQueue {
    entries: VecDeque<Entry>,
}

impl UnlockQueue {
    pub(crate) fn expire(&mut self) {
        let now = Instant::now();
        for entry in &mut self.entries {
            if entry.status == "pending" && now >= entry.expires {
                entry.status = "expired";
            }
        }
        self.entries
            .retain(|entry| now < entry.expires + Duration::from_secs(30));
    }

    /// The prompt the window should show now, if any.
    pub(crate) fn pending(&self) -> Option<Uuid> {
        self.entries
            .iter()
            .find(|entry| entry.status == "pending")
            .map(|entry| entry.id)
    }

    pub(crate) fn submit(&mut self, id: Uuid) -> &'static str {
        self.expire();
        if self.entries.iter().any(|entry| entry.id == id) {
            return "unavailable";
        }
        if self.entries.len() >= MAX_PROMPTS {
            match self
                .entries
                .iter()
                .position(|entry| entry.status != "pending")
            {
                Some(index) => {
                    self.entries.remove(index);
                }
                None => return "unavailable",
            }
        }
        self.entries.push_back(Entry {
            id,
            status: "pending",
            expires: Instant::now() + LIFETIME,
        });
        "pending"
    }

    /// The caller's poll: a finished prompt is handed over once.
    pub(crate) fn take(&mut self, id: Uuid) -> &'static str {
        self.expire();
        let Some(index) = self.entries.iter().position(|entry| entry.id == id) else {
            return "unavailable";
        };
        let status = self.entries[index].status;
        if status != "pending" {
            self.entries.remove(index);
        }
        status
    }

    pub(crate) fn cancel(&mut self, id: Uuid) {
        self.finish(id, "cancelled");
    }

    /// The window's answer for a prompt.
    pub(crate) fn finish(&mut self, id: Uuid, status: &'static str) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
            if entry.status == "pending" {
                entry.status = status;
            }
        } else if status == "cancelled" {
            // A cancellation that overtook its own request must not let a
            // late request open an orphaned prompt.
            self.entries.push_back(Entry {
                id,
                status,
                expires: Instant::now() + LIFETIME,
            });
        }
    }
}

fn failure(detail: &str) -> RemoteFailure {
    RemoteFailure::new(
        "BITWARDEN_LOCKED",
        format!(
            "The Bitwarden vault is locked and was not unlocked in the codex-hosts window ({detail}); no password was read."
        ),
    )
}

async fn call(
    session: Uuid,
    request: Request,
) -> Result<temporary_secrets::Response, RemoteFailure> {
    tokio::task::spawn_blocking(move || temporary_secrets::call(session, request))
        .await
        .map_err(|_| failure("window unavailable"))?
        .map_err(|_| failure("window unavailable"))
}

struct CancelOnDrop {
    session: Uuid,
    id: Uuid,
    active: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.active
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let (session, id) = (self.session, self.id);
            runtime.spawn_blocking(move || {
                let _ = temporary_secrets::call(session, Request::CancelBitwardenUnlock { id });
            });
        }
    }
}

/// Asks the window to unlock the vault and waits for the user's answer.
pub(crate) async fn request_unlock() -> Result<(), RemoteFailure> {
    let response = tokio::task::spawn_blocking(|| {
        temporary_secrets::request_gui(Request::Show { temporary: false })
    })
    .await
    .map_err(|_| failure("window unavailable"))?
    .map_err(|_| failure("window unavailable"))?;
    let session = response.session;
    let id = Uuid::new_v4();
    let mut guard = CancelOnDrop {
        session,
        id,
        active: true,
    };
    let started = Instant::now();
    let response = call(session, Request::UnlockBitwarden { id }).await?;
    if response.status != "pending" || response.operation_id != Some(id) {
        return Err(failure("prompt refused"));
    }
    loop {
        if started.elapsed() >= LIFETIME {
            return Err(failure("prompt expired"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let response = call(session, Request::TakeBitwardenUnlock { id }).await?;
        if response.operation_id != Some(id) {
            return Err(failure("prompt lost"));
        }
        match response.status.as_str() {
            "pending" => {}
            "unlocked" => {
                guard.active = false;
                return Ok(());
            }
            "expired" => return Err(failure("prompt expired")),
            "cancelled" => return Err(failure("cancelled by the user")),
            _ => return Err(failure("prompt unavailable")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_are_handed_over_once_and_cancellations_tombstone() {
        let mut queue = UnlockQueue::default();
        let id = Uuid::new_v4();
        assert_eq!(queue.submit(id), "pending");
        assert_eq!(queue.submit(id), "unavailable");
        assert_eq!(queue.pending(), Some(id));
        assert_eq!(queue.take(id), "pending");
        queue.finish(id, "unlocked");
        assert_eq!(queue.pending(), None);
        assert_eq!(queue.take(id), "unlocked");
        assert_eq!(
            queue.take(id),
            "unavailable",
            "a finished prompt is handed over once"
        );

        let late = Uuid::new_v4();
        queue.cancel(late);
        assert_eq!(queue.submit(late), "unavailable");
        assert_eq!(queue.take(late), "cancelled");
    }
}
