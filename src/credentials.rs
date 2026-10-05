use keyring::{Entry, Error as KeyringError};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

const SERVICE: &str = "Codex Hosts";
const LEGACY_SERVICE: &str = "Codex Remote GUI";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    Password,
    KeyPassphrase,
    SudoPassword(Uuid),
}

pub struct CredentialSnapshot {
    password: Option<Zeroizing<String>>,
    key_passphrase: Option<Zeroizing<String>>,
    sudo: Vec<(Uuid, Option<Zeroizing<String>>)>,
}

impl CredentialKind {
    fn account_suffix(self) -> String {
        match self {
            Self::Password => "password".into(),
            Self::KeyPassphrase => "key-passphrase".into(),
            Self::SudoPassword(id) => format!("sudo-password:{id}"),
        }
    }
}

pub fn store(id: Uuid, kind: CredentialKind, secret: &str) -> Result<(), CredentialError> {
    entry(id, kind)?.set_password(secret)?;
    Ok(())
}

pub fn load(id: Uuid, kind: CredentialKind) -> Result<Option<Zeroizing<String>>, CredentialError> {
    match entry(id, kind)?.get_password() {
        Ok(secret) => Ok(Some(Zeroizing::new(secret))),
        Err(KeyringError::NoEntry) if kind == CredentialKind::Password => {
            load_and_migrate_legacy_password(id)
        }
        Err(KeyringError::NoEntry) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn has(id: Uuid, kind: CredentialKind) -> Result<bool, CredentialError> {
    Ok(load(id, kind)?.is_some())
}

pub fn snapshot(id: Uuid) -> Result<CredentialSnapshot, CredentialError> {
    Ok(CredentialSnapshot {
        password: load(id, CredentialKind::Password)?,
        key_passphrase: load(id, CredentialKind::KeyPassphrase)?,
        sudo: Vec::new(),
    })
}

pub fn snapshot_host(
    host: &crate::model::HostProfile,
) -> Result<CredentialSnapshot, CredentialError> {
    let mut snapshot = snapshot(host.id)?;
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
        (CredentialKind::Password, snapshot.password.as_ref()),
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
    match entry(id, kind)?.delete_credential() {
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

fn load_and_migrate_legacy_password(
    id: Uuid,
) -> Result<Option<Zeroizing<String>>, CredentialError> {
    let legacy = Entry::new(LEGACY_SERVICE, &id.to_string())?;
    match legacy.get_password() {
        Ok(secret) => {
            let secret = Zeroizing::new(secret);
            entry(id, CredentialKind::Password)?.set_password(secret.as_str())?;
            Ok(Some(secret))
        }
        Err(KeyringError::NoEntry) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("credential store operation failed: {0}")]
    Keyring(#[from] KeyringError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn empty_password_round_trips_as_a_saved_credential() {
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
