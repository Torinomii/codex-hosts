use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::HostProfile;

const STORE_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HostStore {
    pub version: u32,
    pub preferred_locale: Option<String>,
    pub hosts: Vec<HostProfile>,
}

impl Default for HostStore {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            preferred_locale: None,
            hosts: Vec::new(),
        }
    }
}

impl HostStore {
    pub fn load() -> Result<Self, StorageError> {
        let primary = hosts_path()?;
        if primary.exists() {
            return read_store(&primary);
        }

        if env::var_os("CODEX_HOSTS_DATA_DIR").is_none()
            && let Some(legacy) = legacy_hosts_path()
            && legacy.exists()
        {
            let mut migrated = read_store(&legacy)?;
            migrated.version = STORE_VERSION;
            migrated.save()?;
            return Ok(migrated);
        }
        Ok(Self::default())
    }

    pub fn save(&self) -> Result<(), StorageError> {
        let path = hosts_path()?;
        let directory = path.parent().ok_or(StorageError::NoDataDirectory)?;
        fs::create_dir_all(directory)?;
        let temporary = path.with_extension("json.tmp");
        let backup = path.with_extension("json.bak");
        let mut normalized = self.clone();
        normalized.version = STORE_VERSION;
        let bytes = serde_json::to_vec_pretty(&normalized)?;
        fs::write(&temporary, bytes)?;
        if path.exists() {
            fs::copy(&path, &backup)?;
            fs::remove_file(&path)?;
        }
        fs::rename(&temporary, &path)?;
        Ok(())
    }

    pub fn find_alias(&self, alias: &str) -> Option<&HostProfile> {
        self.hosts
            .iter()
            .find(|host| host.alias.eq_ignore_ascii_case(alias.trim()))
    }

    pub fn find_alias_mut(&mut self, alias: &str) -> Option<&mut HostProfile> {
        self.hosts
            .iter_mut()
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
}
