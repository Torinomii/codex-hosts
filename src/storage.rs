use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::model::HostProfile;

const STORE_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HostStore {
    pub version: u32,
    pub preferred_locale: Option<String>,
    pub hosts: Vec<HostProfile>,
    #[serde(skip)]
    write_blocked: Option<String>,
}

fn load_primary_strict(path: &Path) -> Result<Option<HostStore>, StorageError> {
    if path.exists() {
        return read_store(path).map(Some);
    }
    let backup = path.with_extension("json.bak");
    if backup.exists() {
        return Err(StorageError::BackupRecoveryRequired(backup));
    }
    Ok(None)
}

impl Default for HostStore {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            preferred_locale: None,
            hosts: Vec::new(),
            write_blocked: None,
        }
    }
}

impl HostStore {
    pub fn load() -> Result<Self, StorageError> {
        let primary = hosts_path()?;
        if let Some(store) = load_primary_strict(&primary)? {
            return Ok(store);
        }
        load_legacy_or_default()
    }

    pub fn load_recovering() -> Result<Self, StorageError> {
        let primary = hosts_path()?;
        if let Some(store) = load_primary_or_backup(&primary)? {
            return Ok(store);
        }

        load_legacy_or_default()
    }

    pub fn blocked(reason: impl Into<String>) -> Self {
        Self {
            write_blocked: Some(reason.into()),
            ..Self::default()
        }
    }

    pub fn save(&self) -> Result<(), StorageError> {
        if let Some(reason) = &self.write_blocked {
            return Err(StorageError::WriteBlocked(reason.clone()));
        }
        let path = hosts_path()?;
        save_store_to_path(self, &path)
    }

    pub fn save_recovery_baseline(&self) -> Result<(), StorageError> {
        let path = hosts_path()?;
        save_recovery_baseline_to_path(self, &path)
    }

    pub fn find_alias(&self, alias: &str) -> Option<&HostProfile> {
        self.hosts
            .iter()
            .find(|host| host.alias.eq_ignore_ascii_case(alias.trim()))
    }

    pub fn next_neutral_alias(&self) -> String {
        let mut number = 1_u32;
        loop {
            let alias = format!("host-{number}");
            if self.find_alias(&alias).is_none() {
                return alias;
            }
            number += 1;
        }
    }
}

fn load_legacy_or_default() -> Result<HostStore, StorageError> {
    if env::var_os("CODEX_HOSTS_DATA_DIR").is_none()
        && let Some(legacy) = legacy_hosts_path()
        && legacy.exists()
    {
        let mut migrated = read_store(&legacy)?;
        migrated.version = STORE_VERSION;
        migrated.save()?;
        return Ok(migrated);
    }
    Ok(HostStore::default())
}

fn save_store_to_path(store: &HostStore, path: &Path) -> Result<(), StorageError> {
    let directory = path.parent().ok_or(StorageError::NoDataDirectory)?;
    fs::create_dir_all(directory)?;
    let temporary = unique_temporary_path(directory, "hosts");
    let backup = path.with_extension("json.bak");
    let result = (|| {
        let mut normalized = store.clone();
        normalized.version = STORE_VERSION;
        normalized.write_blocked = None;
        let bytes = serde_json::to_vec_pretty(&normalized)?;
        write_synced_file(&temporary, &bytes)?;
        if path.exists() {
            if read_store(path).is_ok() {
                refresh_backup(path, &backup)?;
            }
            fs::remove_file(path)?;
        }
        if let Err(error) = fs::rename(&temporary, path) {
            if !path.exists()
                && backup.exists()
                && let Err(recovery) = restore_backup(path, &backup)
            {
                return Err(StorageError::RecoveryFailed {
                    write: error.to_string(),
                    recovery: recovery.to_string(),
                });
            }
            return Err(StorageError::Io(error));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn save_recovery_baseline_to_path(store: &HostStore, path: &Path) -> Result<(), StorageError> {
    if let Some(reason) = &store.write_blocked {
        return Err(StorageError::WriteBlocked(reason.clone()));
    }
    save_store_to_path(store, path)?;
    let backup = path.with_extension("json.bak");
    refresh_backup(path, &backup)
}

fn load_primary_or_backup(path: &Path) -> Result<Option<HostStore>, StorageError> {
    let backup = path.with_extension("json.bak");
    if path.exists() {
        match read_store(path) {
            Ok(store) => return Ok(Some(store)),
            Err(primary_error) => {
                if !backup.exists() {
                    return Err(primary_error);
                }
                let store = match read_store(&backup) {
                    Ok(store) => store,
                    Err(_) => return Err(primary_error),
                };
                restore_backup(path, &backup)?;
                return Ok(Some(store));
            }
        }
    }
    if backup.exists() {
        let store = read_store(&backup)?;
        restore_backup(path, &backup)?;
        return Ok(Some(store));
    }
    Ok(None)
}

fn refresh_backup(primary: &Path, backup: &Path) -> Result<(), StorageError> {
    let directory = primary.parent().ok_or(StorageError::NoDataDirectory)?;
    let temporary = unique_temporary_path(directory, "hosts-backup");
    let bytes = fs::read(primary)?;
    let result = (|| {
        write_synced_file(&temporary, &bytes)?;
        if backup.exists() {
            fs::remove_file(backup)?;
        }
        fs::rename(&temporary, backup)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn restore_backup(primary: &Path, backup: &Path) -> Result<(), StorageError> {
    let directory = primary.parent().ok_or(StorageError::NoDataDirectory)?;
    let temporary = unique_temporary_path(directory, "hosts-recovery");
    let bytes = fs::read(backup)?;
    let preserved_primary = primary
        .exists()
        .then(|| directory.join(format!("hosts.json.corrupt.{}.bak", Uuid::new_v4())));
    let result = (|| {
        write_synced_file(&temporary, &bytes)?;
        if let Some(preserved) = &preserved_primary {
            fs::rename(primary, preserved)?;
        }
        if let Err(error) = fs::rename(&temporary, primary) {
            if let Some(preserved) = &preserved_primary
                && preserved.exists()
                && !primary.exists()
                && let Err(recovery) = fs::rename(preserved, primary)
            {
                return Err(StorageError::RecoveryFailed {
                    write: error.to_string(),
                    recovery: recovery.to_string(),
                });
            }
            return Err(StorageError::Io(error));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn write_synced_file(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

fn unique_temporary_path(directory: &Path, stem: &str) -> PathBuf {
    directory.join(format!("{stem}.{}.tmp", Uuid::new_v4()))
}

fn read_store(path: &Path) -> Result<HostStore, StorageError> {
    let bytes = fs::read(path)?;
    let mut store: HostStore = serde_json::from_slice(&bytes)?;
    store.version = STORE_VERSION;
    Ok(store)
}

pub fn data_directory() -> Result<PathBuf, StorageError> {
    if let Some(path) = env::var_os("CODEX_HOSTS_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    if let Some(local) = env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(local).join("CodexHosts"));
    }
    Err(StorageError::NoDataDirectory)
}

fn hosts_path() -> Result<PathBuf, StorageError> {
    Ok(data_directory()?.join("hosts.json"))
}

fn legacy_hosts_path() -> Option<PathBuf> {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("CodexRemoteGui").join("hosts.json"))
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("no application data directory is available")]
    NoDataDirectory,
    #[error("file operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("stored host data is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("host storage primary is missing; recovery is available from {0}")]
    BackupRecoveryRequired(PathBuf),
    #[error("host storage writes are blocked because loading failed: {0}")]
    WriteBlocked(String),
    #[error("host storage update failed: {write}; backup recovery also failed: {recovery}")]
    RecoveryFailed { write: String, recovery: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_alias_does_not_mix_interface_languages() {
        let mut store = HostStore::default();
        assert_eq!(store.next_neutral_alias(), "host-1");
        store.hosts.push(HostProfile::new("host-1".to_owned()));
        assert_eq!(store.next_neutral_alias(), "host-2");
    }

    #[test]
    fn corrupt_primary_recovers_from_valid_backup() {
        let directory = std::env::temp_dir().join(format!("codex-hosts-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let primary = directory.join("hosts.json");
        let backup = directory.join("hosts.json.bak");
        fs::write(&primary, b"not json").unwrap();
        let mut expected = HostStore::default();
        expected
            .hosts
            .push(HostProfile::new("recovered".to_owned()));
        fs::write(&backup, serde_json::to_vec_pretty(&expected).unwrap()).unwrap();

        let recovered = load_primary_or_backup(&primary).unwrap().unwrap();
        assert_eq!(recovered.hosts[0].alias, "recovered");
        assert_eq!(read_store(&primary).unwrap().hosts[0].alias, "recovered");
        let corrupt_copies = fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("hosts.json.corrupt.")
            })
            .collect::<Vec<_>>();
        assert_eq!(corrupt_copies.len(), 1);
        assert_eq!(fs::read(corrupt_copies[0].path()).unwrap(), b"not json");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn recovery_baseline_replaces_a_rolled_back_candidate_in_backup() {
        let directory = std::env::temp_dir().join(format!("codex-hosts-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let primary = directory.join("hosts.json");
        let backup = directory.join("hosts.json.bak");

        let mut original = HostStore::default();
        original.hosts.push(HostProfile::new("original".to_owned()));
        let mut candidate = HostStore::default();
        candidate
            .hosts
            .push(HostProfile::new("candidate".to_owned()));

        save_store_to_path(&original, &primary).unwrap();
        save_store_to_path(&candidate, &primary).unwrap();
        save_recovery_baseline_to_path(&original, &primary).unwrap();

        assert_eq!(read_store(&primary).unwrap().hosts[0].alias, "original");
        assert_eq!(read_store(&backup).unwrap().hosts[0].alias, "original");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn strict_load_rejects_corrupt_primary_even_with_valid_backup() {
        let directory = std::env::temp_dir().join(format!("codex-hosts-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let primary = directory.join("hosts.json");
        let backup = directory.join("hosts.json.bak");
        fs::write(&primary, b"not json").unwrap();
        fs::write(
            &backup,
            serde_json::to_vec_pretty(&HostStore::default()).unwrap(),
        )
        .unwrap();

        let error = load_primary_strict(&primary).unwrap_err();
        assert!(matches!(error, StorageError::Json(_)));

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn strict_load_requires_recovery_when_only_backup_exists() {
        let directory = std::env::temp_dir().join(format!("codex-hosts-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let primary = directory.join("hosts.json");
        let backup = directory.join("hosts.json.bak");
        fs::write(
            &backup,
            serde_json::to_vec_pretty(&HostStore::default()).unwrap(),
        )
        .unwrap();

        let error = load_primary_strict(&primary).unwrap_err();
        assert!(matches!(error, StorageError::BackupRecoveryRequired(_)));

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn missing_primary_recovers_from_valid_backup() {
        let directory = std::env::temp_dir().join(format!("codex-hosts-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let primary = directory.join("hosts.json");
        let backup = directory.join("hosts.json.bak");
        let mut expected = HostStore::default();
        expected
            .hosts
            .push(HostProfile::new("recovered".to_owned()));
        fs::write(&backup, serde_json::to_vec_pretty(&expected).unwrap()).unwrap();

        let recovered = load_primary_or_backup(&primary).unwrap().unwrap();
        assert_eq!(recovered.hosts[0].alias, "recovered");
        assert!(primary.exists());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn save_keeps_the_previous_valid_store_as_backup() {
        let directory = std::env::temp_dir().join(format!("codex-hosts-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let primary = directory.join("hosts.json");
        let backup = directory.join("hosts.json.bak");

        let mut first = HostStore::default();
        first.hosts.push(HostProfile::new("first".to_owned()));
        save_store_to_path(&first, &primary).unwrap();

        let mut second = HostStore::default();
        second.hosts.push(HostProfile::new("second".to_owned()));
        save_store_to_path(&second, &primary).unwrap();

        assert_eq!(read_store(&primary).unwrap().hosts[0].alias, "second");
        assert_eq!(read_store(&backup).unwrap().hosts[0].alias, "first");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn blocked_store_refuses_writes() {
        let error = HostStore::blocked("load failed").save().unwrap_err();
        assert!(matches!(error, StorageError::WriteBlocked(_)));
    }
}
