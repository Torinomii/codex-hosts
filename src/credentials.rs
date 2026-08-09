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
}

pub struct CredentialSnapshot {
    password: Option<Zeroizing<String>>,
    key_passphrase: Option<Zeroizing<String>>,
}

impl CredentialKind {
    fn account_suffix(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::KeyPassphrase => "key-passphrase",
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
    })
}

pub fn restore(id: Uuid, snapshot: &CredentialSnapshot) -> Result<(), CredentialError> {
    let mut first_error = None;
    for (kind, secret) in [
        (CredentialKind::Password, snapshot.password.as_ref()),
        (
            CredentialKind::KeyPassphrase,
            snapshot.key_passphrase.as_ref(),
        ),
    ] {
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

fn delete(id: Uuid, kind: CredentialKind) -> Result<(), CredentialError> {
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
