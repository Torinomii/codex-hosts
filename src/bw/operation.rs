//! Non-secret progress and cancellation for one GUI worker operation.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::future::pending;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Preparing,
    Authorizing,
    Syncing,
    Reading,
    Writing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Cancelled,
    TimedOut,
}

pub struct Snapshot {
    pub stage: Stage,
    pub elapsed: Duration,
    pub stage_elapsed: Duration,
    pub stopped: Option<Stop>,
    pub may_have_written: bool,
}

struct Progress {
    stage: Stage,
    since: Instant,
    stopped: Option<Stop>,
    may_have_written: bool,
    finished: Option<Instant>,
}

struct Inner {
    started: Instant,
    timeout: Duration,
    cancel: CancellationToken,
    progress: Mutex<Progress>,
}

#[derive(Clone)]
pub struct Control(Arc<Inner>);

thread_local! { static CURRENT: RefCell<Option<Control>> = const { RefCell::new(None) }; }

pub struct Scope {
    previous: Option<Control>,
    _thread: PhantomData<Rc<()>>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|slot| *slot.borrow_mut() = self.previous.take());
    }
}

impl Control {
    pub fn new(timeout: Duration) -> Self {
        let started = Instant::now();
        Self(Arc::new(Inner {
            started,
            timeout,
            cancel: CancellationToken::new(),
            progress: Mutex::new(Progress {
                stage: Stage::Preparing,
                since: started,
                stopped: None,
                may_have_written: false,
                finished: None,
            }),
        }))
    }

    pub fn current() -> Option<Self> {
        CURRENT.with(|slot| slot.borrow().clone())
    }

    pub fn enter(&self) -> Scope {
        Scope {
            previous: CURRENT.with(|slot| slot.borrow_mut().replace(self.clone())),
            _thread: PhantomData,
        }
    }

    pub fn stage(&self, stage: Stage) {
        let mut progress = self
            .0
            .progress
            .lock()
            .expect("operation progress lock is not poisoned");
        if progress.finished.is_some() {
            return;
        }
        if progress.stage != stage {
            progress.stage = stage;
            progress.since = Instant::now();
        }
        progress.may_have_written |= stage == Stage::Writing;
    }

    pub fn stop(&self, reason: Stop) {
        let mut progress = self
            .0
            .progress
            .lock()
            .expect("operation progress lock is not poisoned");
        if progress.finished.is_some() {
            return;
        }
        progress.stopped.get_or_insert(reason);
        self.0.cancel.cancel();
    }

    pub fn finish(&self) {
        self.0
            .progress
            .lock()
            .expect("operation progress lock is not poisoned")
            .finished
            .get_or_insert_with(Instant::now);
    }

    pub fn check_timeout(&self) {
        let finished = self
            .0
            .progress
            .lock()
            .expect("operation progress lock is not poisoned")
            .finished
            .is_some();
        if !finished && self.0.started.elapsed() >= self.0.timeout {
            self.stop(Stop::TimedOut);
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.check_timeout();
        let progress = self
            .0
            .progress
            .lock()
            .expect("operation progress lock is not poisoned");
        let now = progress.finished.unwrap_or_else(Instant::now);
        Snapshot {
            stage: progress.stage,
            elapsed: now.duration_since(self.0.started),
            stage_elapsed: now.duration_since(progress.since),
            stopped: progress.stopped,
            may_have_written: progress.may_have_written,
        }
    }

    pub fn remaining(&self, limit: Duration) -> Duration {
        limit.min(self.0.timeout.saturating_sub(self.0.started.elapsed()))
    }

    /// Only fixed command verbs are examined; arguments and output are never kept.
    pub fn begin(&self, args: impl Iterator<Item = impl AsRef<OsStr>>) -> Stage {
        let mut words = args
            .take(2)
            .map(|word| word.as_ref().to_string_lossy().into_owned());
        let first = words.next().unwrap_or_default();
        let second = words.next().unwrap_or_default();
        let stage = match (first.as_str(), second.as_str()) {
            ("login" | "unlock", _) => Stage::Authorizing,
            ("sync", _) => Stage::Syncing,
            ("create" | "edit" | "delete" | "restore" | "lock" | "logout", _)
            | ("item" | "vault", "create" | "edit" | "delete") => Stage::Writing,
            ("get" | "list", _) | ("item" | "vault", "get" | "list") => Stage::Reading,
            _ => Stage::Preparing,
        };
        self.stage(stage);
        stage
    }
}

pub async fn cancelled(control: Option<&Control>) {
    match control {
        Some(control) => control.0.cancel.cancelled().await,
        None => pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_restore_context_and_keep_write_uncertainty() {
        let control = Control::new(Duration::from_secs(10));
        assert!(Control::current().is_none());
        {
            let _scope = control.enter();
            let current = Control::current().unwrap();
            current.begin(["item", "edit"].into_iter());
            current.stage(Stage::Reading);
            current.stop(Stop::Cancelled);
            let snapshot = current.snapshot();
            assert!(snapshot.may_have_written);
            assert_eq!(snapshot.stage, Stage::Reading);
            assert_eq!(snapshot.stopped, Some(Stop::Cancelled));
        }
        assert!(Control::current().is_none());
    }

    #[test]
    fn a_completed_job_does_not_time_out_while_the_ui_is_idle() {
        let control = Control::new(Duration::from_millis(20));
        control.finish();
        control.stage(Stage::Syncing);
        assert_eq!(control.snapshot().stage, Stage::Preparing);
        let elapsed = control.snapshot().elapsed;
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(control.snapshot().stopped, None);
        assert_eq!(control.snapshot().elapsed, elapsed);
        control.stop(Stop::Cancelled);
        assert_eq!(control.snapshot().stopped, None);
    }

    fn sleeper() -> tokio::process::Command {
        let exe = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples/fake_bw.exe");
        let mut command = tokio::process::Command::new(exe);
        command.env_clear().args(["sleep", "5000"]);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        command
    }

    #[tokio::test]
    async fn cancelling_a_running_cli_waits_for_its_cleanup() {
        let control = Control::new(Duration::from_secs(10));
        let _scope = control.enter();
        let cancel = control.clone();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            cancel.stop(Stop::Cancelled);
        });
        let started = Instant::now();
        let result = crate::run_command(sleeper(), None, Duration::from_secs(10)).await;
        stopper.join().unwrap();
        assert!(matches!(result, Err(crate::BwRunError::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[tokio::test]
    async fn a_job_deadline_bounds_the_actual_cli_process() {
        let control = Control::new(Duration::from_millis(150));
        let _scope = control.enter();
        let started = Instant::now();
        let result = crate::run_command(sleeper(), None, Duration::from_secs(10)).await;
        assert!(matches!(result, Err(crate::BwRunError::Timeout(_))));
        assert_eq!(control.snapshot().stopped, Some(Stop::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
