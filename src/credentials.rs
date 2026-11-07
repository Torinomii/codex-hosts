use keyring::{Entry, Error as KeyringError};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

const SERVICE: &str = "Codex Hosts";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    Password,
    LoginPassword(Uuid),
    KeyPassphrase,
    SudoPassword(Uuid),
    /// The unlocked Bitwarden session key of one account profile, kept
    /// between calls for as long as the re-authentication policy allows.
    BitwardenSession(Uuid),
}

pub struct CredentialSnapshot {
    password_kind: CredentialKind,
    password: Option<Zeroizing<String>>,
    key_passphrase: Option<Zeroizing<String>>,
    sudo: Vec<(Uuid, Option<Zeroizing<String>>)>,
}

impl CredentialKind {
    fn account_suffix(self) -> String {
        match self {
            Self::Password => "password".into(),
            Self::LoginPassword(id) => format!("login-password:{id}"),
            Self::KeyPassphrase => "key-passphrase".into(),
            Self::SudoPassword(id) => format!("sudo-password:{id}"),
            Self::BitwardenSession(id) => format!("bitwarden-session:{id}"),
        }
    }
}

pub fn login_password_kind(host: &crate::model::HostProfile) -> CredentialKind {
    host.login_password_id
        .map(CredentialKind::LoginPassword)
        .unwrap_or(CredentialKind::Password)
}

pub fn store(id: Uuid, kind: CredentialKind, secret: &str) -> Result<(), CredentialError> {
    entry(id, kind)?.set_password(secret)?;
    Ok(())
}

pub fn load(id: Uuid, kind: CredentialKind) -> Result<Option<Zeroizing<String>>, CredentialError> {
    match entry(id, kind)?.get_password() {
        Ok(secret) => Ok(Some(Zeroizing::new(secret))),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn has(id: Uuid, kind: CredentialKind) -> Result<bool, CredentialError> {
    Ok(load(id, kind)?.is_some())
}

pub fn snapshot_host(
    host: &crate::model::HostProfile,
) -> Result<CredentialSnapshot, CredentialError> {
    let password_kind = login_password_kind(host);
    let mut snapshot = CredentialSnapshot {
        password_kind,
        password: load(host.id, password_kind)?,
        key_passphrase: load(host.id, CredentialKind::KeyPassphrase)?,
        sudo: Vec::new(),
    };
    if let Some(sudo) = &host.advanced.sudo {
        for credential in &sudo.credentials {
            snapshot.sudo.push((
                credential.id,
                load(host.id, CredentialKind::SudoPassword(credential.id))?,
            ));
        }
    }
    Ok(snapshot)
}

pub fn snapshot_kind(
    id: Uuid,
    kind: CredentialKind,
) -> Result<Option<Zeroizing<String>>, CredentialError> {
    load(id, kind)
}

pub fn restore_kind(
    id: Uuid,
    kind: CredentialKind,
    secret: Option<&Zeroizing<String>>,
) -> Result<(), CredentialError> {
    match secret {
        Some(secret) => store(id, kind, secret.as_str()),
        None => delete(id, kind),
    }
}

pub fn restore(id: Uuid, snapshot: &CredentialSnapshot) -> Result<(), CredentialError> {
    let mut first_error = None;
    for (kind, secret) in [
        (snapshot.password_kind, snapshot.password.as_ref()),
        (
            CredentialKind::KeyPassphrase,
            snapshot.key_passphrase.as_ref(),
        ),
    ]
    .into_iter()
    .chain(
        snapshot
            .sudo
            .iter()
            .map(|(id, secret)| (CredentialKind::SudoPassword(*id), secret.as_ref())),
    ) {
        let result = match secret {
            Some(secret) => store(id, kind, secret.as_str()),
            None => delete(id, kind),
        };
        if first_error.is_none()
            && let Err(error) = result
        {
            first_error = Some(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub fn delete_all(id: Uuid) -> Result<(), CredentialError> {
    for kind in [CredentialKind::Password, CredentialKind::KeyPassphrase] {
        delete(id, kind)?;
    }
    Ok(())
}

pub fn delete_host(host: &crate::model::HostProfile) -> Result<(), CredentialError> {
    delete_all(host.id)?;
    if host.login_password_id.is_some() {
        delete(host.id, login_password_kind(host))?;
    }
    if let Some(sudo) = &host.advanced.sudo {
        for credential in &sudo.credentials {
            delete(host.id, CredentialKind::SudoPassword(credential.id))?;
        }
    }
    Ok(())
}

pub type CredentialUpdate = (CredentialKind, Option<Zeroizing<String>>);

/// Snapshot every modified slot first. Metadata is committed only after all
/// writes succeed; even a failed write is included in the rollback attempt.
pub fn save_transaction(
    id: Uuid,
    updates: &[CredentialUpdate],
    save_metadata: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    transact(
        updates,
        |kind| load(id, kind).map_err(|error| error.to_string()),
        |kind, secret| {
            match secret {
                Some(secret) => store(id, kind, secret),
                None => delete(id, kind),
            }
            .map_err(|error| error.to_string())
        },
        save_metadata,
    )
}

fn transact(
    updates: &[CredentialUpdate],
    mut read: impl FnMut(CredentialKind) -> Result<Option<Zeroizing<String>>, String>,
    mut write: impl FnMut(CredentialKind, Option<&str>) -> Result<(), String>,
    save_metadata: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let snapshots = updates
        .iter()
        .map(|(kind, _)| read(*kind))
        .collect::<Result<Vec<_>, _>>()?;
    let mut changed = 0;
    let mut error = None;
    for (kind, secret) in updates {
        changed += 1;
        if let Err(failure) = write(*kind, secret.as_ref().map(|secret| secret.as_str())) {
            error = Some(failure);
            break;
        }
    }
    if error.is_none() {
        error = save_metadata().err();
    }
    if let Some(mut error) = error {
        let mut rollback = Vec::new();
        for ((kind, _), secret) in updates[..changed].iter().zip(&snapshots) {
            if let Err(failure) = write(*kind, secret.as_ref().map(|secret| secret.as_str())) {
                rollback.push(failure);
            }
        }
        if !rollback.is_empty() {
            error.push_str(&format!(
                "; credential rollback failed: {}",
                rollback.join("; ")
            ));
        }
        return Err(error);
    }
    Ok(())
}

pub fn delete(id: Uuid, kind: CredentialKind) -> Result<(), CredentialError> {
    delete_entry(entry(id, kind)?)
}

fn delete_entry(entry: Entry) -> Result<(), CredentialError> {
    match entry.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn entry(id: Uuid, kind: CredentialKind) -> Result<Entry, CredentialError> {
    Ok(Entry::new(
        SERVICE,
        &format!("{}:{}", id, kind.account_suffix()),
    )?)
}

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("credential store operation failed: {0}")]
    Keyring(#[from] KeyringError),
}

/// Tests that touch Windows Credential Manager take turns, across the test
/// threads and the isolated-store child processes alike: concurrent writes
/// and deletes have been observed to make a read right after a delete still
/// return the old entry.
#[cfg(all(test, windows))]
pub(crate) struct TestLock(windows_sys::Win32::Foundation::HANDLE);

#[cfg(all(test, windows))]
impl Drop for TestLock {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::ReleaseMutex;
        // SAFETY: the handle came from CreateMutexW and is released once.
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

#[cfg(all(test, windows))]
pub(crate) fn test_lock() -> TestLock {
    use windows_sys::Win32::Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{CreateMutexW, INFINITE, WaitForSingleObject};
    let name = "Local\\codex-hosts-credential-tests-v1\0"
        .encode_utf16()
        .collect::<Vec<_>>();
    // SAFETY: a named mutex in the session namespace; the wait is unbounded
    // because the holders are other tests that finish.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    assert!(!handle.is_null(), "credential test mutex");
    let wait = unsafe { WaitForSingleObject(handle, INFINITE) };
    assert!(
        wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED,
        "credential test mutex wait {wait}"
    );
    TestLock(handle)
}

#[cfg(all(test, not(windows)))]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn empty_password_round_trips_as_a_saved_credential() {
        let _guard = crate::credentials::test_lock();
        let id = Uuid::new_v4();
        let result = store(id, CredentialKind::Password, "")
            .and_then(|()| load(id, CredentialKind::Password));
        let cleanup = delete(id, CredentialKind::Password);

        cleanup.expect("temporary empty-password credential should be removed");
        let password = result
            .expect("empty password should be stored")
            .expect("empty password should remain a present credential");
        assert!(password.is_empty());
    }

    #[test]
    fn sudo_password_keys_are_isolated_from_login_other_users_and_hosts() {
        let _guard = crate::credentials::test_lock();
        let host = Uuid::new_v4();
        let other_host = Uuid::new_v4();
        let first = CredentialKind::SudoPassword(Uuid::new_v4());
        let second = CredentialKind::SudoPassword(Uuid::new_v4());
        store(host, first, "  ").unwrap();
        let result = load(host, first).unwrap().unwrap();
        let isolated = [
            load(host, second).unwrap(),
            load(other_host, first).unwrap(),
            load(host, CredentialKind::Password).unwrap(),
        ];
        delete(host, first).unwrap();
        assert_eq!(result.as_str(), "  ");
        assert!(isolated.iter().all(Option::is_none));
    }

    #[test]
    fn multi_credential_failures_restore_all_modified_slots_and_report_rollback_failure() {
        let _guard = crate::credentials::test_lock();
        let a = CredentialKind::SudoPassword(Uuid::new_v4());
        let b = CredentialKind::SudoPassword(Uuid::new_v4());
        for fail_write in [true, false] {
            let slots = RefCell::new(vec![(a, Some("old-a".to_owned())), (b, None)]);
            let updates = [
                (a, Some(Zeroizing::new("new-a".into()))),
                (b, Some(Zeroizing::new("new-b".into()))),
            ];
            let result = transact(
                &updates,
                |kind| {
                    Ok(slots
                        .borrow()
                        .iter()
                        .find(|(key, _)| *key == kind)
                        .unwrap()
                        .1
                        .clone()
                        .map(Zeroizing::new))
                },
                |kind, secret| {
                    slots
                        .borrow_mut()
                        .iter_mut()
                        .find(|(key, _)| *key == kind)
                        .unwrap()
                        .1 = secret.map(str::to_owned);
                    if fail_write && kind == b && secret == Some("new-b") {
                        Err("partial credential write".into())
                    } else {
                        Ok(())
                    }
                },
                || {
                    if fail_write {
                        panic!("metadata must not be written after a failed credential write")
                    } else {
                        Err("metadata conflict".into())
                    }
                },
            );
            assert!(result.is_err());
            assert_eq!(
                *slots.borrow(),
                vec![(a, Some("old-a".to_owned())), (b, None)]
            );
        }
        let error = transact(
            &[(a, Some(Zeroizing::new("new".into())))],
            |_| Ok(None),
            |_, secret| {
                if secret.is_none() {
                    Err("rollback unavailable".into())
                } else {
                    Ok(())
                }
            },
            || Err("metadata conflict".into()),
        )
        .unwrap_err();
        assert!(error.contains("metadata conflict") && error.contains("rollback unavailable"));
    }

    #[test]
    fn deleting_a_host_cleans_sudo_slots_and_snapshot_restores_them() {
        let _guard = crate::credentials::test_lock();
        let mut host = crate::model::HostProfile::default();
        let record = Uuid::new_v4();
        host.advanced.sudo = Some(crate::model::SudoSettings {
            credentials: vec![crate::model::SudoCredential {
                id: record,
                username: "alice".into(),
            }],
            ..Default::default()
        });
        let sudo = CredentialKind::SudoPassword(record);
        store(host.id, CredentialKind::Password, "login-fixture").unwrap();
        store(host.id, sudo, "sudo-fixture").unwrap();
        let snapshot = snapshot_host(&host).unwrap();
        delete_host(&host).unwrap();
        let missing = load(host.id, sudo).unwrap().is_none()
            && load(host.id, CredentialKind::Password).unwrap().is_none();
        restore(host.id, &snapshot).unwrap();
        let restored = load(host.id, sudo).unwrap().unwrap();
        delete_host(&host).unwrap();
        assert!(missing);
        assert_eq!(restored.as_str(), "sudo-fixture");
    }
}

#[cfg(test)]
mod regression_tests;
