//! Every password manager behind one interface. The window's jobs that
//! write items (saving or deleting a host, a transfer, a batch delete) ask
//! [`Vaults`] for what they need, and [`LiveVaults`] hands each request to
//! the backend of the item's provider. What is the same for every provider
//! stays out of the backends: the conflict rule
//! ([`vault_hosts::check_revision`]), the host facts an item describes, and
//! the jobs and their lock handling. Supporting another password manager
//! is one more [`VaultBackend`] and its arm in [`LiveVaults::backend`].

use codex_hosts_bw::{FieldEdit, LoginPatch, SessionKey};
use eframe::egui;
use uuid::Uuid;
use zeroize::Zeroizing;

use super::bitwarden::{
    Job, JobError, JobResult, block_on, explain_remote, spawn_job, sync_if_stale, unlocked,
};
use crate::bitwarden::{Account, Vault, VaultError, hosts};
use crate::i18n::Catalog;
use crate::model::{BitwardenSettings, HostProfile, VaultProvider};
use crate::vault_hosts::{self, HostFacts, HostWrite, ItemKey};

/// What a window job asks of the password managers, whichever holds the
/// item. The window's worker passes the real CLIs; tests pass a fake.
pub(super) trait Vaults {
    /// The login password the item behind `host` holds, `None` when it has
    /// none.
    fn read_password(&mut self, host: &HostProfile) -> JobResult<Option<Zeroizing<String>>>;
    /// The host items of `provider`'s folder or vault.
    fn list(&mut self, provider: VaultProvider) -> JobResult<Vec<(ItemKey, HostFacts)>>;
    /// Writes the item `write` names (or creates it) and reports the host
    /// it describes now. An item changed since the write's revision is
    /// left alone.
    fn save(&mut self, write: HostWrite) -> JobResult<(ItemKey, HostFacts)>;
    /// Moves the item behind `host` to its trash or archive.
    fn remove(&mut self, host: &HostProfile) -> JobResult<()>;
}

/// One password manager as a job reaches it: only what its CLI needs.
trait VaultBackend {
    fn read_password(&mut self, host: &HostProfile) -> JobResult<Option<Zeroizing<String>>>;
    fn list(&mut self) -> JobResult<Vec<(ItemKey, HostFacts)>>;
    fn save(&mut self, write: HostWrite) -> JobResult<(ItemKey, HostFacts)>;
    fn remove(&mut self, host: &HostProfile) -> JobResult<()>;
}

/// Bitwarden through the vault the job unlocked, limited to the hosts
/// folder.
struct BitwardenBackend {
    vault: Vault,
    folder: Option<Uuid>,
}

impl VaultBackend for BitwardenBackend {
    fn read_password(&mut self, host: &HostProfile) -> JobResult<Option<Zeroizing<String>>> {
        let Some(item) = host.vault() else {
            return Ok(None);
        };
        match block_on(
            self.vault
                .resolve_password(item.item_id, Some(host.username.trim())),
        )? {
            Ok(resolved) => Ok(Some(resolved.password)),
            Err(VaultError::PasswordMissing) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn list(&mut self) -> JobResult<Vec<(ItemKey, HostFacts)>> {
        let folder = self.folder;
        let items = block_on(self.vault.list_items(folder))??;
        Ok(items
            .iter()
            .filter(|item| item.folder_id == folder && item.deleted_date.is_none())
            .filter_map(|item| {
                hosts::facts(item)
                    .ok()
                    .map(|facts| (ItemKey::bitwarden(item.id), facts))
            })
            .collect())
    }

    fn save(&mut self, write: HostWrite) -> JobResult<(ItemKey, HostFacts)> {
        let item = match write.item_id {
            None => block_on(self.vault.create_item(host_item_json(&write, self.folder)))??,
            Some(id) => {
                let current = block_on(self.vault.get_item(id))??;
                vault_hosts::check_revision(
                    &write.expected_revision,
                    current.summary().revision_date.as_deref(),
                )?;
                let fields = write.codex_fields();
                let name = write.name.trim().to_owned();
                let HostWrite {
                    username,
                    password,
                    host_uri,
                    ..
                } = write;
                let summary = current.summary();
                let patch = LoginPatch {
                    name: (name != summary.name).then_some(name),
                    username: if Some(username.as_str()) == summary.username.as_deref() {
                        FieldEdit::Keep
                    } else {
                        FieldEdit::Set(username)
                    },
                    password,
                    host_uri: Some(host_uri),
                    codex_fields: Some(fields),
                };
                let json = current.apply_login_patch(&patch)?;
                block_on(self.vault.edit_item(id, json))??
            }
        };
        let facts = hosts::facts(item.summary()).map_err(JobError::Problem)?;
        Ok((ItemKey::bitwarden(item.summary().id), facts))
    }

    fn remove(&mut self, host: &HostProfile) -> JobResult<()> {
        if let Some(item) = host.vault() {
            block_on(self.vault.delete_item(item.item_id))??;
        }
        Ok(())
    }
}

/// The JSON of a new Bitwarden host item: a Login item in the folder with
/// the host URI and the codex-hosts fields.
pub(super) fn host_item_json(write: &HostWrite, folder: Option<Uuid>) -> Zeroizing<Vec<u8>> {
    let password = match &write.password {
        FieldEdit::Set(password) => serde_json::Value::String(password.as_str().to_owned()),
        FieldEdit::Keep | FieldEdit::Clear => serde_json::Value::Null,
    };
    let item = serde_json::json!({
        "type": 1,
        "name": write.name,
        "notes": null,
        "favorite": false,
        "reprompt": 0,
        "folderId": folder,
        "fields": write.codex_fields().iter().map(|(key, value)| serde_json::json!({
            "name": format!("{}{key}", codex_hosts_bw::CODEX_FIELD_PREFIX),
            "value": value,
            "type": 0,
            "linkedId": null,
        })).collect::<Vec<_>>(),
        "login": {
            "username": write.username,
            "password": password,
            "uris": [{ "match": null, "uri": write.host_uri }]
        }
    });
    Zeroizing::new(item.to_string().into_bytes())
}

/// 1Password through `op`, which waits for the app's own prompt and keeps
/// its own gate; there is nothing to open first.
struct OnePasswordBackend;

impl VaultBackend for OnePasswordBackend {
    fn read_password(&mut self, host: &HostProfile) -> JobResult<Option<Zeroizing<String>>> {
        Ok(block_on(crate::onepassword::login_password(host))??)
    }

    fn list(&mut self) -> JobResult<Vec<(ItemKey, HostFacts)>> {
        Ok(block_on(crate::onepassword::list_hosts())??)
    }

    fn save(&mut self, write: HostWrite) -> JobResult<(ItemKey, HostFacts)> {
        Ok(block_on(crate::onepassword::save_item(&write))??)
    }

    fn remove(&mut self, host: &HostProfile) -> JobResult<()> {
        block_on(crate::onepassword::archive_item(host))??;
        Ok(())
    }
}

/// The password managers one job reaches.
pub(super) struct LiveVaults {
    /// Opened only when the job touches Bitwarden.
    bitwarden: Option<BitwardenBackend>,
    onepassword: OnePasswordBackend,
}

impl LiveVaults {
    /// The backend of `provider`; one the job did not open reads as locked.
    fn backend(&mut self, provider: VaultProvider) -> JobResult<&mut dyn VaultBackend> {
        match provider {
            VaultProvider::Bitwarden => match self.bitwarden.as_mut() {
                Some(backend) => Ok(backend),
                None => Err(JobError::Vault(VaultError::Locked)),
            },
            VaultProvider::OnePassword => Ok(&mut self.onepassword),
        }
    }
}

impl Vaults for LiveVaults {
    fn read_password(&mut self, host: &HostProfile) -> JobResult<Option<Zeroizing<String>>> {
        match host.vault_provider() {
            Some(provider) => self.backend(provider)?.read_password(host),
            None => Ok(None),
        }
    }

    fn list(&mut self, provider: VaultProvider) -> JobResult<Vec<(ItemKey, HostFacts)>> {
        self.backend(provider)?.list()
    }

    fn save(&mut self, write: HostWrite) -> JobResult<(ItemKey, HostFacts)> {
        self.backend(write.provider)?.save(write)
    }

    fn remove(&mut self, host: &HostProfile) -> JobResult<()> {
        match host.vault_provider() {
            Some(provider) => self.backend(provider)?.remove(host),
            None => Ok(()),
        }
    }
}

/// What a worker needs from the window to open the password managers.
pub(super) struct VaultAccess {
    pub(super) settings: BitwardenSettings,
    /// The Bitwarden session the window holds, if any.
    pub(super) memory: Option<SessionKey>,
    pub(super) max_age: u64,
    /// The Bitwarden hosts folder.
    pub(super) folder: Option<Uuid>,
}

impl VaultAccess {
    /// The password managers in `providers`, ready for one job: Bitwarden
    /// is unlocked (and synced per the window). The inner error is a lock
    /// the window can ask about.
    fn open(
        self,
        providers: &[VaultProvider],
        catalog: &Catalog,
    ) -> Result<JobResult<LiveVaults>, String> {
        let mut vaults = LiveVaults {
            bitwarden: None,
            onepassword: OnePasswordBackend,
        };
        if providers.contains(&VaultProvider::Bitwarden) {
            let account = Account::from_settings(&self.settings)
                .map_err(|error| explain_remote(catalog, &error))?;
            let opened = unlocked(&account, self.memory).and_then(|mut vault| {
                sync_if_stale(&account, &mut vault, self.max_age)?;
                Ok(vault)
            });
            match opened {
                Ok(vault) => {
                    vaults.bitwarden = Some(BitwardenBackend {
                        vault,
                        folder: self.folder,
                    });
                }
                Err(error) => return Ok(Err(error)),
            }
        }
        Ok(Ok(vaults))
    }
}

/// Runs `work` on a worker thread against the password managers in
/// `providers`. One that is locked ends the job with the lock, which the
/// window answers with its unlock prompt and a retry.
pub(super) fn vaults_job<T: Send + 'static>(
    repaint: egui::Context,
    catalog: Catalog,
    access: VaultAccess,
    providers: Vec<VaultProvider>,
    work: impl FnOnce(&mut LiveVaults, &Catalog) -> JobResult<T> + Send + 'static,
) -> Job<Result<T, JobError>> {
    spawn_job(repaint, catalog.clone(), move || {
        let mut vaults = match access.open(&providers, &catalog)? {
            Ok(vaults) => vaults,
            Err(error) => return Ok(Err(error)),
        };
        Ok(work(&mut vaults, &catalog))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_host_item_carries_the_uri_the_fields_and_the_folder() {
        let write = HostWrite {
            provider: VaultProvider::Bitwarden,
            item_id: None,
            external_id: None,
            name: "build-box".to_owned(),
            host_uri: "ssh://10.0.0.5:2222".to_owned(),
            username: "deploy".to_owned(),
            password: FieldEdit::Set(Zeroizing::new("pw".to_owned())),
            description: " front door ".to_owned(),
            tags: vec!["prod".to_owned(), "web".to_owned()],
            expected_revision: String::new(),
        };
        let folder = Uuid::from_u128(7);
        let json: serde_json::Value =
            serde_json::from_slice(&host_item_json(&write, Some(folder))).unwrap();
        assert_eq!(json["type"], 1);
        assert_eq!(json["name"], "build-box");
        assert_eq!(json["folderId"], folder.to_string());
        assert_eq!(json["login"]["username"], "deploy");
        assert_eq!(json["login"]["password"], "pw");
        assert_eq!(json["login"]["uris"][0]["uri"], "ssh://10.0.0.5:2222");
        let fields = json["fields"].as_array().unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0]["name"], "codex-hosts:description");
        assert_eq!(fields[0]["value"], "front door");
        assert_eq!(fields[1]["name"], "codex-hosts:tags");
        assert_eq!(fields[1]["value"], "prod, web");
    }

    #[test]
    fn a_password_manager_the_job_did_not_open_reads_as_locked() {
        let mut vaults = LiveVaults {
            bitwarden: None,
            onepassword: OnePasswordBackend,
        };
        let write = HostWrite {
            provider: VaultProvider::Bitwarden,
            item_id: None,
            external_id: None,
            name: "build-box".to_owned(),
            host_uri: "ssh://10.0.0.5:22".to_owned(),
            username: "deploy".to_owned(),
            password: FieldEdit::Keep,
            description: String::new(),
            tags: Vec::new(),
            expected_revision: String::new(),
        };
        assert!(vaults.save(write).is_err_and(|error| error.is_locked()));
        assert!(
            vaults
                .list(VaultProvider::Bitwarden)
                .is_err_and(|error| error.is_locked())
        );
        // A local host has no item to read or remove.
        let local = HostProfile::new("local".to_owned());
        assert!(
            vaults
                .read_password(&local)
                .is_ok_and(|password| password.is_none())
        );
        assert!(vaults.remove(&local).is_ok());
    }
}
