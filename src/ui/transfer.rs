//! Copying or moving hosts from one store to another: the built-in store,
//! Bitwarden and 1Password. The login password goes from the source to the
//! target without passing through the form, the local secrets kept in
//! Credential Manager (key passphrase, sudo and su passwords) are copied to
//! the new profile, and the local settings travel with the host. A move
//! removes an original only once its copy is saved. When the target already
//! has the host (same server and user) or its name, the chosen policy skips
//! it, overwrites the existing host, or keeps both under a free name.
//!
//! The batch delete's password-manager part runs through the same access.

use std::collections::{HashMap, HashSet};

use codex_hosts_bw::FieldEdit;
use eframe::egui::{self, RichText};
use uuid::Uuid;
use zeroize::Zeroizing;

use super::HostsApp;
use super::bitwarden::{JobError, JobResult, VaultTask};
use super::dialogs::{dialog_button_row, dialog_shell};
use super::services::HostGroup;
use super::status::StatusKind;
use crate::credentials::{self, CredentialKind};
use crate::model::{HostProfile, VaultProvider, normalize_tags};
use crate::vault_hosts::{self, HostFacts, HostWrite, ItemKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TransferMode {
    /// The original stays; the copy is named `<alias>-copy`.
    Copy,
    /// The original is removed once the copy is saved; the name stays.
    Move,
}

/// What happens when the target already has the host or its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OnConflict {
    /// Leave the host where it is and the target as it is.
    Skip,
    /// Write the host over the existing one, which keeps its id.
    Overwrite,
    /// Add the host under a free name next to the existing one.
    KeepBoth,
}

/// Why a host in the target counts as the one being transferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Clash {
    /// It has the name the transferred host would take.
    SameName,
    /// It reaches the same server as the same user.
    SameHost,
}

/// What a transfer job does; it holds no secret, so it can wait for an
/// unlock and run again.
#[derive(Debug, Clone)]
pub(super) struct TransferRequest {
    pub(super) mode: TransferMode,
    pub(super) target: HostGroup,
    pub(super) on_conflict: OnConflict,
    /// The hosts to transfer, in list order; none is in the target yet.
    pub(super) sources: Vec<HostProfile>,
    /// Every alias in the store, lower case.
    taken: HashSet<String>,
    /// The target's hosts as the store holds them: the candidates of a
    /// local target, and the local secrets of an overwritten vault host.
    target_hosts: Vec<HostProfile>,
}

impl TransferRequest {
    /// The hosts of `ids` that are not in `target` yet.
    pub(super) fn new(
        mode: TransferMode,
        target: HostGroup,
        on_conflict: OnConflict,
        ids: &[Uuid],
        hosts: &[HostProfile],
    ) -> Self {
        Self {
            mode,
            target,
            on_conflict,
            sources: hosts
                .iter()
                .filter(|host| ids.contains(&host.id) && HostGroup::of(host) != target)
                .cloned()
                .collect(),
            taken: hosts
                .iter()
                .map(|host| host.alias.trim().to_ascii_lowercase())
                .collect(),
            target_hosts: hosts
                .iter()
                .filter(|host| HostGroup::of(host) == target)
                .cloned()
                .collect(),
        }
    }

    /// Whether the job reads from or writes to `provider`.
    pub(super) fn involves(&self, provider: VaultProvider) -> bool {
        self.target.provider() == Some(provider)
            || self
                .sources
                .iter()
                .any(|host| host.vault_provider() == Some(provider))
    }

    /// The clashes the store already shows, before the job lists the
    /// target itself: alias and kind, per source.
    pub(super) fn known_clashes(&self) -> Vec<(String, Clash)> {
        let existing: Vec<Existing> = self.target_hosts.iter().map(Existing::local).collect();
        self.sources
            .iter()
            .filter_map(|source| {
                find_clash(&base_alias(self.mode, source), source, &existing)
                    .map(|(clash, _)| (source.alias.clone(), clash))
            })
            .collect()
    }
}

/// The name a transferred host asks for: its own for a move,
/// `<alias>-copy` for a copy.
fn base_alias(mode: TransferMode, source: &HostProfile) -> String {
    match mode {
        TransferMode::Move => source.alias.trim().to_owned(),
        TransferMode::Copy => format!("{}-copy", source.alias.trim()),
    }
}

/// `base`, else `base-2`, `base-3`, … whichever `taken` does not hold
/// (compared in lower case).
pub(super) fn free_alias(base: &str, taken: impl Fn(&str) -> bool) -> String {
    (1u32..)
        .map(|number| match number {
            1 => base.to_owned(),
            _ => format!("{base}-{number}"),
        })
        .find(|candidate| !taken(&candidate.to_ascii_lowercase()))
        .expect("an unbounded counter always finds a free alias")
}

/// A host the target already has.
#[derive(Debug, Clone)]
struct Existing {
    /// Its profile id (the item key's id for a vault host).
    id: Uuid,
    /// The item, for a vault host.
    key: Option<ItemKey>,
    /// Name, endpoint and login; the revision guards an overwrite.
    facts: HostFacts,
    /// The store's profile of it, when there is one.
    profile: Option<HostProfile>,
}

impl Existing {
    fn local(host: &HostProfile) -> Self {
        Self {
            id: host.id,
            key: vault_hosts::ItemKey::of(host),
            facts: HostFacts {
                alias: host.alias.trim().to_owned(),
                protocol: host.protocol,
                address: host.address.trim().to_owned(),
                port: host.port,
                username: host.username.trim().to_owned(),
                description: host.description.clone(),
                tags: host.tags.clone(),
                revision: host
                    .vault()
                    .map(|vault| vault.revision_date.clone())
                    .unwrap_or_default(),
            },
            profile: Some(host.clone()),
        }
    }
}

fn same_host(facts: &HostFacts, host: &HostProfile) -> bool {
    facts.protocol == host.protocol
        && facts.address.eq_ignore_ascii_case(host.address.trim())
        && facts.port == host.port
        && facts.username == host.username.trim()
}

/// The target host `alias` or `source` clashes with: one with that name
/// first, else one reaching the same server as the same user.
fn find_clash<'a>(
    alias: &str,
    source: &HostProfile,
    existing: &'a [Existing],
) -> Option<(Clash, &'a Existing)> {
    existing
        .iter()
        .find(|host| host.facts.alias.eq_ignore_ascii_case(alias))
        .map(|host| (Clash::SameName, host))
        .or_else(|| {
            existing
                .iter()
                .find(|host| same_host(&host.facts, source))
                .map(|host| (Clash::SameHost, host))
        })
}

/// What a transfer asks of the password managers. The window's worker
/// passes the real CLIs; tests pass a fake.
pub(super) trait Vaults {
    /// The login password the item behind `host` holds, `None` when it has
    /// none.
    fn read_password(&mut self, host: &HostProfile) -> JobResult<Option<Zeroizing<String>>>;
    /// The host items of `provider`'s folder or vault.
    fn list(&mut self, provider: VaultProvider) -> JobResult<Vec<(ItemKey, HostFacts)>>;
    /// Writes the item `write` names (or creates it) and reports the host
    /// it describes now.
    fn save(&mut self, write: HostWrite) -> JobResult<(ItemKey, HostFacts)>;
    /// Moves the item behind `host` to its trash or archive.
    fn remove(&mut self, host: &HostProfile) -> JobResult<()>;
}

/// One host that is now in the target store. No `Debug`: it carries the
/// secrets to restore if the store cannot be saved.
pub(super) struct Transferred {
    /// The host as the request read it.
    pub(super) source: HostProfile,
    /// The new profile, not saved in the store yet.
    pub(super) profile: HostProfile,
    /// Whether `profile` overwrote a host the target had.
    pub(super) overwrote: bool,
    /// Credential Manager records written for `profile`, with what they
    /// held before, to restore if the store cannot be saved.
    pub(super) written: Vec<(CredentialKind, Option<Zeroizing<String>>)>,
    /// Records of an overwritten host that the new settings no longer use,
    /// removed once the store is saved.
    pub(super) stale: Vec<CredentialKind>,
    /// Why a moved host's original item is still in its password manager.
    pub(super) source_left: Option<String>,
}

/// What a transfer job did, host by host.
pub(super) struct TransferReport {
    pub(super) mode: TransferMode,
    pub(super) target: HostGroup,
    pub(super) done: Vec<Transferred>,
    /// Hosts left alone because the target already had them.
    pub(super) skipped: Vec<(String, Clash)>,
    /// Alias and reason of each host that was not transferred.
    pub(super) failed: Vec<(String, String)>,
}

/// What a step of the job does with one host.
enum Plan<'a> {
    Create {
        alias: String,
    },
    Overwrite {
        alias: String,
        existing: &'a Existing,
    },
}

/// Transfers the hosts one by one; a failure stops only its own host. A
/// locked Bitwarden vault before anything was written fails the whole job,
/// so the window can ask for the master password and run it again.
pub(super) fn run(
    request: &TransferRequest,
    vaults: &mut impl Vaults,
    word: impl Fn(&JobError) -> String,
) -> JobResult<TransferReport> {
    let mut existing: Vec<Existing> = match request.target.provider() {
        None => request.target_hosts.iter().map(Existing::local).collect(),
        Some(provider) => vaults
            .list(provider)?
            .into_iter()
            .map(|(key, facts)| Existing {
                id: key.id,
                profile: request
                    .target_hosts
                    .iter()
                    .find(|host| host.id == key.id)
                    .cloned(),
                key: Some(key),
                facts,
            })
            .collect(),
    };
    // An item the store does not serve (its name is used elsewhere) still
    // holds that name in the target.
    let mut taken = request.taken.clone();
    taken.extend(
        existing
            .iter()
            .map(|host| host.facts.alias.to_ascii_lowercase()),
    );
    let mut report = TransferReport {
        mode: request.mode,
        target: request.target,
        done: Vec::new(),
        skipped: Vec::new(),
        failed: Vec::new(),
    };
    for (index, source) in request.sources.iter().enumerate() {
        let base = base_alias(request.mode, source);
        let own = source.alias.trim().to_ascii_lowercase();
        let target_names: HashSet<String> = existing
            .iter()
            .map(|host| host.facts.alias.to_ascii_lowercase())
            .collect();
        // A moving host may keep its own name unless the target holds it.
        let is_taken = |name: &str| {
            taken.contains(name)
                && (request.mode == TransferMode::Copy
                    || name != own
                    || target_names.contains(name))
        };
        let plan = match (find_clash(&base, source, &existing), request.on_conflict) {
            (Some((clash, _)), OnConflict::Skip) => {
                report.skipped.push((source.alias.clone(), clash));
                continue;
            }
            (Some((_, found)), OnConflict::Overwrite) => Plan::Overwrite {
                alias: match request.mode {
                    TransferMode::Move => base,
                    TransferMode::Copy => found.facts.alias.clone(),
                },
                existing: found,
            },
            _ => Plan::Create {
                alias: free_alias(&base, is_taken),
            },
        };
        let replaced = match &plan {
            Plan::Overwrite { existing, .. } => Some(existing.id),
            Plan::Create { .. } => None,
        };
        match transfer_one(request, source, plan, vaults, &word) {
            Ok(done) => {
                taken.insert(done.profile.alias.to_ascii_lowercase());
                let entry = Existing::local(&done.profile);
                match replaced.and_then(|id| existing.iter_mut().find(|host| host.id == id)) {
                    Some(slot) => *slot = entry,
                    None => existing.push(entry),
                }
                report.done.push(done);
            }
            Err(error) if error.is_locked() => {
                if report.done.is_empty() && report.failed.is_empty() {
                    return Err(error);
                }
                let reason = word(&error);
                report.failed.extend(
                    request.sources[index..]
                        .iter()
                        .map(|host| (host.alias.clone(), reason.clone())),
                );
                break;
            }
            Err(error) => report.failed.push((source.alias.clone(), word(&error))),
        }
    }
    Ok(report)
}

fn credential_failure(error: credentials::CredentialError) -> JobError {
    JobError::Other(error.to_string())
}

/// The Credential Manager records kept for `host` besides its login
/// password.
fn local_kinds(host: &HostProfile) -> Vec<CredentialKind> {
    std::iter::once(CredentialKind::KeyPassphrase)
        .chain(
            host.advanced
                .sudo
                .iter()
                .flat_map(|sudo| &sudo.credentials)
                .map(|credential| CredentialKind::SudoPassword(credential.id)),
        )
        .collect()
}

/// The item a host becomes in a password manager; over `existing` when
/// given. A password the host does not have leaves the item's alone.
fn item_write(
    provider: VaultProvider,
    profile: &HostProfile,
    password: Option<&Zeroizing<String>>,
    existing: Option<&Existing>,
) -> HostWrite {
    let key = existing.and_then(|existing| existing.key.as_ref());
    HostWrite {
        provider,
        item_id: key.map(|key| key.id),
        external_id: key.and_then(|key| key.external_id.clone()),
        name: profile.alias.clone(),
        host_uri: super::item_uri(profile),
        username: profile.username.trim().to_owned(),
        password: match password {
            Some(password) if !password.is_empty() => FieldEdit::Set(password.clone()),
            _ => FieldEdit::Keep,
        },
        description: profile.description.trim().to_owned(),
        tags: normalize_tags(&profile.tags),
        expected_revision: existing
            .map(|existing| existing.facts.revision.clone())
            .unwrap_or_default(),
    }
}

/// Puts each record back as it was before the job wrote it.
fn restore(id: Uuid, written: &[(CredentialKind, Option<Zeroizing<String>>)]) {
    for (kind, before) in written {
        let _ = match before {
            Some(secret) => credentials::store(id, *kind, secret),
            None => credentials::delete(id, *kind),
        };
    }
}

fn transfer_one(
    request: &TransferRequest,
    source: &HostProfile,
    plan: Plan<'_>,
    vaults: &mut impl Vaults,
    word: &impl Fn(&JobError) -> String,
) -> JobResult<Transferred> {
    // A local "no password" choice is an empty record; it travels as is.
    let password = match source.vault_provider() {
        None => credentials::load(source.id, credentials::login_password_kind(source))
            .map_err(credential_failure)?,
        Some(_) => vaults.read_password(source)?,
    };
    let (alias, existing) = match plan {
        Plan::Create { alias } => (alias, None),
        Plan::Overwrite { alias, existing } => (alias, Some(existing)),
    };
    let mut profile = source.clone();
    profile.alias = alias;
    // Fresh record ids, so the new host never shares a record with another.
    let mut secrets = Vec::new();
    if let Some(sudo) = profile.advanced.sudo.as_mut() {
        for credential in &mut sudo.credentials {
            let next = Uuid::new_v4();
            if let Some(secret) =
                credentials::load(source.id, CredentialKind::SudoPassword(credential.id))
                    .map_err(credential_failure)?
            {
                secrets.push((CredentialKind::SudoPassword(next), secret));
            }
            credential.id = next;
        }
    }
    if let Some(secret) =
        credentials::load(source.id, CredentialKind::KeyPassphrase).map_err(credential_failure)?
    {
        secrets.push((CredentialKind::KeyPassphrase, secret));
    }
    match request.target.provider() {
        None => {
            profile.id = existing.map_or_else(Uuid::new_v4, |existing| existing.id);
            profile.vault = None;
            let login = Uuid::new_v4();
            profile.login_password_id = Some(login);
            if let Some(password) = password {
                secrets.push((CredentialKind::LoginPassword(login), password));
            }
        }
        Some(provider) => {
            let write = item_write(provider, &profile, password.as_ref(), existing);
            let (key, facts) = vaults.save(write)?;
            profile.id = key.id;
            vault_hosts::apply(&facts, &key, &mut profile);
        }
    }
    let mut written = Vec::new();
    for (kind, secret) in &secrets {
        let stored = credentials::load(profile.id, *kind).and_then(|before| {
            credentials::store(profile.id, *kind, secret)?;
            Ok(before)
        });
        match stored {
            Ok(before) => written.push((*kind, before)),
            Err(error) => {
                restore(profile.id, &written);
                // A new item goes again; an overwritten one keeps the write.
                if existing.is_none() && profile.is_vault_host() {
                    let _ = vaults.remove(&profile);
                }
                return Err(credential_failure(error));
            }
        }
    }
    let stale = existing
        .and_then(|existing| existing.profile.as_ref())
        .map(|old| {
            let mut kinds = local_kinds(old);
            if !old.is_vault_host() {
                kinds.push(credentials::login_password_kind(old));
            }
            kinds.retain(|kind| !written.iter().any(|(new, _)| new == kind));
            kinds
        })
        .unwrap_or_default();
    let source_left = (request.mode == TransferMode::Move && source.is_vault_host())
        .then(|| vaults.remove(source).err().map(|error| word(&error)))
        .flatten();
    Ok(Transferred {
        source: source.clone(),
        profile,
        overwrote: existing.is_some(),
        written,
        stale,
        source_left,
    })
}

/// Puts the transferred hosts into `hosts`. A move drops each original and
/// points every jump-host reference at its replacement. A host with the new
/// profile's id (an overwritten host, or a mirror a refresh already made of
/// a new item) is replaced. Returns the originals that were removed.
pub(super) fn merge(hosts: &mut Vec<HostProfile>, report: &TransferReport) -> Vec<HostProfile> {
    let replaced: HashMap<Uuid, Uuid> = match report.mode {
        TransferMode::Copy => HashMap::new(),
        TransferMode::Move => report
            .done
            .iter()
            .map(|done| (done.source.id, done.profile.id))
            .collect(),
    };
    let mut removed = Vec::new();
    hosts.retain(|host| {
        let keep = !replaced.contains_key(&host.id);
        if !keep {
            removed.push(host.clone());
        }
        keep
    });
    for done in &report.done {
        match hosts.iter_mut().find(|host| host.id == done.profile.id) {
            Some(stored) => stored.clone_from(&done.profile),
            None => hosts.push(done.profile.clone()),
        }
    }
    if !replaced.is_empty() {
        for host in hosts.iter_mut() {
            if let Some(next) = host.jump_host.and_then(|jump| replaced.get(&jump)) {
                host.jump_host = Some(*next);
            }
        }
    }
    removed
}

/// What the batch delete's password-manager part did.
pub(super) struct RemovalReport {
    /// The hosts whose items went to the trash or archive.
    pub(super) removed: Vec<Uuid>,
    /// Alias and reason of each host whose item stayed.
    pub(super) failed: Vec<(String, String)>,
}

/// Moves the items of `hosts` to their trash or archive, one by one; a lock
/// before the first one fails the whole job, so it can run again after the
/// unlock.
pub(super) fn remove_items(
    hosts: &[HostProfile],
    vaults: &mut impl Vaults,
    word: impl Fn(&JobError) -> String,
) -> JobResult<RemovalReport> {
    let mut report = RemovalReport {
        removed: Vec::new(),
        failed: Vec::new(),
    };
    for host in hosts {
        match vaults.remove(host) {
            Ok(()) => report.removed.push(host.id),
            Err(error) => {
                if error.is_locked() && report.removed.is_empty() && report.failed.is_empty() {
                    return Err(error);
                }
                report.failed.push((host.alias.clone(), word(&error)));
            }
        }
    }
    Ok(report)
}

/// The copy-or-move dialog, while open.
pub(super) struct TransferPrompt {
    pub(super) ids: Vec<Uuid>,
    pub(super) mode: TransferMode,
    pub(super) target: HostGroup,
    pub(super) on_conflict: OnConflict,
}

impl HostsApp {
    /// The stores a host can go to now: the built-in one and every password
    /// manager that is set up and switched on.
    pub(super) fn transfer_stores(&self) -> Vec<HostGroup> {
        HostGroup::ALL
            .into_iter()
            .filter(|group| self.service_ready(*group))
            .collect()
    }

    /// Opens the dialog for `ids`, on `target` when given, else on the first
    /// store that is not where all of them already are.
    pub(super) fn open_transfer(
        &mut self,
        ids: Vec<Uuid>,
        mode: TransferMode,
        target: Option<HostGroup>,
    ) {
        let stores = self.transfer_stores();
        let groups: Vec<HostGroup> = self
            .store
            .hosts
            .iter()
            .filter(|host| ids.contains(&host.id))
            .map(HostGroup::of)
            .collect();
        let target = target.filter(|group| stores.contains(group)).or_else(|| {
            stores
                .iter()
                .copied()
                .find(|group| groups.iter().any(|source| source != group))
        });
        match target {
            Some(target) if stores.len() > 1 && !groups.is_empty() => {
                self.transfer_prompt = Some(TransferPrompt {
                    ids,
                    mode,
                    target,
                    on_conflict: OnConflict::Skip,
                });
            }
            _ => self.set_status(
                StatusKind::Warning,
                self.catalog.text("transfer_no_target").to_owned(),
            ),
        }
    }

    pub(super) fn transfer_dialog(&mut self, context: &egui::Context) {
        let Some(mut prompt) = self.transfer_prompt.take() else {
            return;
        };
        let stores = self.transfer_stores();
        let catalog = self.catalog.clone();
        let chosen: Vec<&HostProfile> = self
            .store
            .hosts
            .iter()
            .filter(|host| prompt.ids.contains(&host.id))
            .collect();
        let subject = match chosen.as_slice() {
            [host] => catalog.format("transfer_one", &[("alias", host.alias.as_str())]),
            hosts => catalog.format("transfer_count", &[("count", &hosts.len().to_string())]),
        };
        let choice = dialog_shell(
            context,
            "transfer_dialog",
            540.0,
            catalog.text("transfer_title"),
            |ui| {
                let target = catalog.text(prompt.target.label_key());
                ui.add(egui::Label::new(subject).wrap());
                ui.add_space(10.0);
                ui.label(RichText::new(catalog.text("transfer_target")).strong());
                ui.horizontal_wrapped(|ui| {
                    for group in &stores {
                        ui.radio_value(&mut prompt.target, *group, catalog.text(group.label_key()));
                    }
                });
                ui.add_space(8.0);
                ui.label(RichText::new(catalog.text("transfer_mode")).strong());
                ui.horizontal_wrapped(|ui| {
                    ui.radio_value(
                        &mut prompt.mode,
                        TransferMode::Copy,
                        catalog.text("transfer_copy"),
                    );
                    ui.radio_value(
                        &mut prompt.mode,
                        TransferMode::Move,
                        catalog.text("transfer_move"),
                    );
                });
                let mode_hint = match prompt.mode {
                    TransferMode::Copy => "transfer_copy_hint",
                    TransferMode::Move => "transfer_move_hint",
                };
                ui.add(egui::Label::new(RichText::new(catalog.text(mode_hint)).weak()).wrap());
                ui.add_space(8.0);
                ui.label(
                    RichText::new(catalog.format("transfer_conflict", &[("target", target)]))
                        .strong(),
                );
                ui.horizontal_wrapped(|ui| {
                    for (policy, key) in [
                        (OnConflict::Skip, "transfer_on_skip"),
                        (OnConflict::Overwrite, "transfer_on_overwrite"),
                        (OnConflict::KeepBoth, "transfer_on_keep"),
                    ] {
                        ui.radio_value(&mut prompt.on_conflict, policy, catalog.text(key));
                    }
                });
                let conflict_hint = match prompt.on_conflict {
                    OnConflict::Skip => "transfer_on_skip_hint",
                    OnConflict::Overwrite => "transfer_on_overwrite_hint",
                    OnConflict::KeepBoth => "transfer_on_keep_hint",
                };
                ui.add(
                    egui::Label::new(
                        RichText::new(catalog.format(conflict_hint, &[("target", target)])).weak(),
                    )
                    .wrap(),
                );
                let request = TransferRequest::new(
                    prompt.mode,
                    prompt.target,
                    prompt.on_conflict,
                    &prompt.ids,
                    &self.store.hosts,
                );
                let warn = ui.visuals().warn_fg_color;
                let clashes = request.known_clashes();
                if !clashes.is_empty() {
                    let names = clashes
                        .iter()
                        .map(|(alias, clash)| {
                            let why = catalog.text(match clash {
                                Clash::SameName => "transfer_clash_name",
                                Clash::SameHost => "transfer_clash_host",
                            });
                            format!("{alias} ({why})")
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    ui.add_space(6.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(catalog.format(
                                "transfer_clashes",
                                &[("target", target), ("names", names.as_str())],
                            ))
                            .color(warn),
                        )
                        .wrap(),
                    );
                }
                let skipped = chosen.len() - request.sources.len();
                if skipped > 0 {
                    ui.add_space(6.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(catalog.format(
                                "transfer_skipped",
                                &[("count", &skipped.to_string()), ("target", target)],
                            ))
                            .color(warn),
                        )
                        .wrap(),
                    );
                }
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(RichText::new(catalog.text("transfer_secrets_hint")).weak())
                        .wrap(),
                );
                let start = match prompt.mode {
                    TransferMode::Copy => "transfer_start_copy",
                    TransferMode::Move => "transfer_start_move",
                };
                dialog_button_row(
                    ui,
                    catalog.text(start),
                    prompt.mode == TransferMode::Move
                        || prompt.on_conflict == OnConflict::Overwrite,
                    catalog.text("cancel"),
                )
            },
        );
        match choice {
            Some(true) => self.start_transfer(prompt),
            Some(false) => {}
            None => self.transfer_prompt = Some(prompt),
        }
    }

    fn start_transfer(&mut self, prompt: TransferPrompt) {
        let target = self.catalog.text(prompt.target.label_key()).to_owned();
        // A move takes the open host away, and an overwrite may write over it.
        if self.editor.as_ref().is_some_and(|editor| {
            editor.has_unsaved_changes()
                && ((prompt.mode == TransferMode::Move && prompt.ids.contains(&editor.profile.id))
                    || prompt.on_conflict == OnConflict::Overwrite)
        }) {
            self.set_status(
                StatusKind::Warning,
                self.catalog.text("transfer_unsaved").to_owned(),
            );
            return;
        }
        if self.vault_job_running() {
            self.set_status_key("busy");
            return;
        }
        let request = TransferRequest::new(
            prompt.mode,
            prompt.target,
            prompt.on_conflict,
            &prompt.ids,
            &self.store.hosts,
        );
        if request.sources.is_empty() {
            self.set_status(
                StatusKind::Warning,
                self.catalog
                    .format("transfer_nothing", &[("target", target.as_str())]),
            );
            return;
        }
        let count = request.sources.len().to_string();
        self.start_vault_task(VaultTask::Transfer(request));
        self.set_status(
            StatusKind::Info,
            self.catalog.format(
                "transfer_running",
                &[("count", count.as_str()), ("target", target.as_str())],
            ),
        );
    }

    /// The job finished: the store takes the new hosts and, for a move,
    /// loses the originals and their local secrets.
    pub(super) fn finish_transfer(&mut self, report: TransferReport) {
        let target = self.catalog.text(report.target.label_key()).to_owned();
        if !report.done.is_empty() {
            let saved = self.refresh_hosts().and_then(|()| {
                let mut store = self.store.clone();
                let removed = merge(&mut store.hosts, &report);
                store.save().map_err(|error| error.to_string())?;
                Ok((store, removed))
            });
            let (store, removed) = match saved {
                Ok(saved) => saved,
                Err(error) => {
                    for done in &report.done {
                        restore(done.profile.id, &done.written);
                    }
                    self.set_status_format("storage_error", &[("error", &error)]);
                    return;
                }
            };
            self.store = store;
            for done in &report.done {
                for kind in &done.stale {
                    let _ = credentials::delete(done.profile.id, *kind);
                }
                if done.overwrote {
                    crate::ssh::invalidate_profile(done.profile.id);
                    self.test_states.remove(&done.profile.id);
                }
            }
            for host in &removed {
                let _ = credentials::delete_host(host);
                crate::ssh::invalidate_profile(host.id);
                self.test_operations.remove(&host.id);
                self.pending_tests.retain(|pending| *pending != host.id);
                self.test_states.remove(&host.id);
            }
            let open = self.selected;
            let moved_to = open.and_then(|selected| {
                removed
                    .iter()
                    .any(|host| host.id == selected)
                    .then(|| {
                        report
                            .done
                            .iter()
                            .find(|done| done.source.id == selected)
                            .map(|done| done.profile.id)
                    })
                    .flatten()
            });
            let overwritten_open = open.is_some_and(|selected| {
                report
                    .done
                    .iter()
                    .any(|done| done.overwrote && done.profile.id == selected)
            });
            if let Some(next) = moved_to.or(open.filter(|_| overwritten_open)) {
                self.select(next);
                self.scroll_to_selected = true;
            }
            self.collapsed_groups.unfold(report.target);
            if self.batch_mode {
                self.batch_mode = false;
                self.batch_selected.clear();
            }
        }
        let count = report.done.len().to_string();
        let overwritten = report.done.iter().filter(|done| done.overwrote).count();
        let details = if overwritten + report.skipped.len() > 0 {
            format!(
                " {}",
                self.catalog.format(
                    "transfer_details",
                    &[
                        ("overwritten", &overwritten.to_string()),
                        ("skipped", &report.skipped.len().to_string()),
                    ],
                )
            )
        } else {
            String::new()
        };
        if let Some((alias, error)) = report.failed.first() {
            let total =
                (report.done.len() + report.skipped.len() + report.failed.len()).to_string();
            let text = self.catalog.format(
                "transfer_failed_some",
                &[
                    ("done", count.as_str()),
                    ("total", total.as_str()),
                    ("target", target.as_str()),
                    ("alias", alias.as_str()),
                    ("error", error.as_str()),
                ],
            );
            self.set_status(StatusKind::Error, format!("{text}{details}"));
        } else if let Some(done) = report.done.iter().find(|done| done.source_left.is_some()) {
            let error = done.source_left.as_deref().unwrap_or_default();
            let source = self
                .catalog
                .text(HostGroup::of(&done.source).label_key())
                .to_owned();
            self.set_status(
                StatusKind::Warning,
                self.catalog.format(
                    "transfer_source_left",
                    &[
                        ("alias", done.source.alias.as_str()),
                        ("target", target.as_str()),
                        ("source", source.as_str()),
                        ("error", error),
                    ],
                ),
            );
        } else if report.done.is_empty() {
            self.set_status(
                StatusKind::Warning,
                self.catalog
                    .format("transfer_all_skipped", &[("target", target.as_str())]),
            );
        } else {
            let key = match report.mode {
                TransferMode::Copy => "transfer_copied",
                TransferMode::Move => "transfer_moved",
            };
            let text = self.catalog.format(
                key,
                &[("count", count.as_str()), ("target", target.as_str())],
            );
            self.set_status(StatusKind::Success, format!("{text}{details}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitwarden::VaultError;
    use crate::model::{Protocol, SshAuth, SudoCredential, SudoSettings, VaultHostRef};

    /// Password managers in memory: `items` is what a listing shows, reads
    /// answer from `passwords`, saves and removals are recorded.
    #[derive(Default)]
    struct FakeVaults {
        items: Vec<(ItemKey, HostFacts)>,
        passwords: HashMap<Uuid, &'static str>,
        /// Provider, item written over (`None` creates), name, password.
        saved: Vec<(VaultProvider, Option<Uuid>, String, Option<String>)>,
        revisions: Vec<String>,
        removed: Vec<Uuid>,
        refuse_removal: bool,
        /// Calls after this many succeed fail as a locked vault.
        locked_after: Option<usize>,
        calls: usize,
    }

    impl FakeVaults {
        fn call(&mut self) -> JobResult<()> {
            self.calls += 1;
            if self.locked_after.is_some_and(|after| self.calls > after) {
                return Err(JobError::Vault(VaultError::Locked));
            }
            Ok(())
        }
    }

    impl Vaults for FakeVaults {
        fn read_password(&mut self, host: &HostProfile) -> JobResult<Option<Zeroizing<String>>> {
            self.call()?;
            Ok(self
                .passwords
                .get(&host.id)
                .map(|password| Zeroizing::new((*password).to_owned())))
        }

        fn list(&mut self, provider: VaultProvider) -> JobResult<Vec<(ItemKey, HostFacts)>> {
            self.call()?;
            Ok(self
                .items
                .iter()
                .filter(|(key, _)| key.provider == provider)
                .cloned()
                .collect())
        }

        fn save(&mut self, write: HostWrite) -> JobResult<(ItemKey, HostFacts)> {
            self.call()?;
            let password = match &write.password {
                FieldEdit::Set(password) => Some(password.as_str().to_owned()),
                FieldEdit::Keep | FieldEdit::Clear => None,
            };
            self.saved
                .push((write.provider, write.item_id, write.name.clone(), password));
            self.revisions.push(write.expected_revision.clone());
            let uri = codex_hosts_bw::parse_host_uri(&write.host_uri).unwrap();
            let key = ItemKey {
                provider: write.provider,
                id: write.item_id.unwrap_or_else(Uuid::new_v4),
                external_id: write.external_id.clone(),
            };
            let facts = HostFacts {
                alias: write.name.clone(),
                protocol: match uri.scheme {
                    codex_hosts_bw::UriScheme::Ssh => Protocol::Ssh,
                    codex_hosts_bw::UriScheme::Telnet => Protocol::Telnet,
                },
                port: uri.port_or_default(),
                address: uri.host,
                username: write.username.clone(),
                description: write.description.clone(),
                tags: write.tags.clone(),
                revision: "2".to_owned(),
            };
            Ok((key, facts))
        }

        fn remove(&mut self, host: &HostProfile) -> JobResult<()> {
            self.call()?;
            if self.refuse_removal {
                return Err(JobError::Other("refused".to_owned()));
            }
            self.removed.push(host.id);
            Ok(())
        }
    }

    fn word(error: &JobError) -> String {
        match error {
            JobError::Other(text) => text.clone(),
            error if error.is_locked() => "locked".to_owned(),
            _ => "other".to_owned(),
        }
    }

    fn finished<T>(result: JobResult<T>) -> T {
        match result {
            Ok(report) => report,
            Err(error) => panic!("the job failed: {}", word(&error)),
        }
    }

    fn failed<T>(result: JobResult<T>) -> JobError {
        match result {
            Ok(_) => panic!("the job should have failed"),
            Err(error) => error,
        }
    }

    fn local_host(alias: &str) -> HostProfile {
        HostProfile {
            alias: alias.to_owned(),
            address: "10.0.0.5".to_owned(),
            port: 2222,
            username: "alice".to_owned(),
            ssh_auth: SshAuth::Password,
            tags: vec!["prod".to_owned()],
            host_fingerprint: Some("SHA256:abc".to_owned()),
            verified: true,
            login_password_id: Some(Uuid::new_v4()),
            ..HostProfile::default()
        }
    }

    fn vault_host(alias: &str, provider: VaultProvider) -> HostProfile {
        let mut host = local_host(alias);
        host.login_password_id = None;
        host.vault = Some(VaultHostRef {
            provider,
            item_id: host.id,
            external_id: None,
            item_name: alias.to_owned(),
            revision_date: "1".to_owned(),
        });
        host
    }

    /// A listed Bitwarden item: `server` "same" reaches the hosts above.
    fn item(alias: &str, server: &str) -> (ItemKey, HostFacts) {
        (
            ItemKey::bitwarden(Uuid::new_v4()),
            HostFacts {
                alias: alias.to_owned(),
                protocol: Protocol::Ssh,
                address: if server == "same" { "10.0.0.5" } else { server }.to_owned(),
                port: 2222,
                username: "alice".to_owned(),
                description: String::new(),
                tags: Vec::new(),
                revision: "r1".to_owned(),
            },
        )
    }

    fn secret(id: Uuid, kind: CredentialKind) -> Option<String> {
        credentials::load(id, kind)
            .unwrap()
            .map(|secret| secret.as_str().to_owned())
    }

    fn request(
        mode: TransferMode,
        target: HostGroup,
        on_conflict: OnConflict,
        sources: &[&HostProfile],
        others: &[HostProfile],
    ) -> TransferRequest {
        let hosts: Vec<HostProfile> = sources
            .iter()
            .map(|host| (*host).clone())
            .chain(others.iter().cloned())
            .collect();
        let ids: Vec<Uuid> = sources.iter().map(|host| host.id).collect();
        TransferRequest::new(mode, target, on_conflict, &ids, &hosts)
    }

    #[test]
    fn free_aliases_count_up_ignoring_case() {
        let taken: HashSet<String> = ["web-copy", "WEB-COPY-2"]
            .into_iter()
            .map(str::to_ascii_lowercase)
            .collect();
        assert_eq!(
            free_alias("Web-copy", |name| taken.contains(name)),
            "Web-copy-3"
        );
        assert_eq!(free_alias("db", |name| taken.contains(name)), "db");
    }

    #[test]
    fn a_request_skips_hosts_already_in_the_target_and_sees_known_clashes() {
        let web = local_host("web");
        let mut elsewhere = local_host("api");
        elsewhere.address = "10.9.9.9".to_owned();
        let vault = vault_host("vault", VaultProvider::Bitwarden);
        let named = vault_host("api-copy", VaultProvider::Bitwarden);
        let mut copy = request(
            TransferMode::Copy,
            HostGroup::Bitwarden,
            OnConflict::Skip,
            &[&web, &elsewhere, &vault],
            std::slice::from_ref(&named),
        );
        assert_eq!(copy.sources.len(), 2, "the Bitwarden host stays out");
        assert!(copy.involves(VaultProvider::Bitwarden));
        assert!(!copy.involves(VaultProvider::OnePassword));
        // `vault` reaches the same server as `web`; `api-copy` is the name
        // the copy of `api` would take.
        assert_eq!(
            copy.known_clashes(),
            [
                ("web".to_owned(), Clash::SameHost),
                ("api".to_owned(), Clash::SameName)
            ]
        );
        copy.target_hosts.clear();
        assert!(copy.known_clashes().is_empty());
    }

    fn report(mode: TransferMode, done: Vec<(HostProfile, HostProfile)>) -> TransferReport {
        TransferReport {
            mode,
            target: HostGroup::OnePassword,
            done: done
                .into_iter()
                .map(|(source, profile)| Transferred {
                    source,
                    profile,
                    overwrote: false,
                    written: Vec::new(),
                    stale: Vec::new(),
                    source_left: None,
                })
                .collect(),
            skipped: Vec::new(),
            failed: Vec::new(),
        }
    }

    #[test]
    fn a_move_replaces_the_originals_and_repoints_jump_hosts() {
        let jump = local_host("jump");
        let mut target = local_host("target");
        target.jump_host = Some(jump.id);
        let mut user = local_host("user");
        user.jump_host = Some(jump.id);
        let moved_jump = vault_host("jump", VaultProvider::OnePassword);
        let mut moved_target = vault_host("target", VaultProvider::OnePassword);
        moved_target.jump_host = Some(jump.id);
        let mut hosts = vec![jump.clone(), target.clone(), user.clone()];
        let removed = merge(
            &mut hosts,
            &report(
                TransferMode::Move,
                vec![
                    (jump.clone(), moved_jump.clone()),
                    (target.clone(), moved_target.clone()),
                ],
            ),
        );
        let removed: Vec<Uuid> = removed.iter().map(|host| host.id).collect();
        assert_eq!(removed, [jump.id, target.id]);
        let ids: Vec<Uuid> = hosts.iter().map(|host| host.id).collect();
        assert_eq!(ids, [user.id, moved_jump.id, moved_target.id]);
        assert!(
            hosts
                .iter()
                .filter(|host| host.id != moved_jump.id)
                .all(|host| host.jump_host == Some(moved_jump.id)),
            "every reference follows the moved jump host"
        );
    }

    #[test]
    fn a_copy_keeps_the_originals_and_replaces_a_host_with_the_same_id() {
        let jump = local_host("jump");
        let mut target = local_host("target");
        target.jump_host = Some(jump.id);
        let mut copy = vault_host("jump-copy", VaultProvider::OnePassword);
        copy.advanced.require_call_approval = true;
        let early_mirror = HostProfile {
            id: copy.id,
            ..vault_host("jump-copy", VaultProvider::OnePassword)
        };
        let mut hosts = vec![jump.clone(), target.clone(), early_mirror];
        let removed = merge(
            &mut hosts,
            &report(TransferMode::Copy, vec![(jump.clone(), copy.clone())]),
        );
        assert!(removed.is_empty());
        assert_eq!(hosts.len(), 3);
        assert_eq!(hosts[1].jump_host, Some(jump.id));
        assert!(hosts[2].advanced.require_call_approval);
    }

    #[test]
    fn a_local_host_copied_to_a_vault_carries_its_password_and_local_secrets() {
        let _guard = credentials::test_lock();
        let mut source = local_host("web");
        let sudo = Uuid::new_v4();
        source.advanced.sudo = Some(SudoSettings {
            enabled: true,
            credentials: vec![SudoCredential {
                id: sudo,
                username: "root".to_owned(),
            }],
        });
        let login = credentials::login_password_kind(&source);
        credentials::store(source.id, login, "login-secret").unwrap();
        credentials::store(source.id, CredentialKind::KeyPassphrase, "key-secret").unwrap();
        credentials::store(source.id, CredentialKind::SudoPassword(sudo), "root-secret").unwrap();
        let request = request(
            TransferMode::Copy,
            HostGroup::Bitwarden,
            OnConflict::Skip,
            &[&source],
            &[],
        );
        let mut vaults = FakeVaults::default();
        let result = finished(run(&request, &mut vaults, word));
        let done = &result.done[0];
        let copy = &done.profile;
        let copied_sudo = copy.advanced.sudo.as_ref().unwrap().credentials[0].clone();
        let checks = (
            secret(copy.id, CredentialKind::KeyPassphrase),
            secret(copy.id, CredentialKind::SudoPassword(copied_sudo.id)),
            secret(source.id, login),
            secret(source.id, CredentialKind::SudoPassword(sudo)),
        );
        credentials::delete_host(&source).unwrap();
        credentials::delete_host(copy).unwrap();

        assert_eq!(
            vaults.saved,
            [(
                VaultProvider::Bitwarden,
                None,
                "web-copy".to_owned(),
                Some("login-secret".to_owned())
            )]
        );
        assert!(result.failed.is_empty() && result.skipped.is_empty());
        assert!(vaults.removed.is_empty() && !done.overwrote);
        assert!(copy.is_bitwarden_host() && copy.login_password_id.is_none());
        assert_ne!(copy.id, source.id);
        assert_eq!((copy.address.as_str(), copy.port), ("10.0.0.5", 2222));
        assert_eq!(copy.host_fingerprint, source.host_fingerprint);
        assert_eq!(copied_sudo.username, "root");
        assert_ne!(copied_sudo.id, sudo, "the copy gets its own record");
        assert_eq!(
            checks,
            (
                Some("key-secret".to_owned()),
                Some("root-secret".to_owned()),
                Some("login-secret".to_owned()),
                Some("root-secret".to_owned())
            ),
            "the copy has the local secrets and the original keeps its own"
        );
        let kinds: Vec<CredentialKind> = done.written.iter().map(|(kind, _)| *kind).collect();
        assert_eq!(
            kinds,
            [
                CredentialKind::SudoPassword(copied_sudo.id),
                CredentialKind::KeyPassphrase
            ]
        );
        assert!(done.written.iter().all(|(_, before)| before.is_none()));
    }

    #[test]
    fn a_vault_host_moved_to_local_stores_its_password_and_trashes_the_item() {
        let _guard = credentials::test_lock();
        let source = vault_host("db", VaultProvider::Bitwarden);
        let request = request(
            TransferMode::Move,
            HostGroup::Local,
            OnConflict::Skip,
            &[&source],
            &[],
        );
        let mut vaults = FakeVaults::default();
        vaults.passwords.insert(source.id, "vault-secret");
        let result = finished(run(&request, &mut vaults, word));
        let moved = result.done[0].profile.clone();
        let stored = secret(moved.id, credentials::login_password_kind(&moved));
        credentials::delete_host(&moved).unwrap();

        assert_eq!(stored.as_deref(), Some("vault-secret"));
        assert!(!moved.is_vault_host() && moved.login_password_id.is_some());
        assert_eq!(moved.alias, "db", "a move keeps the name");
        assert_ne!(moved.id, source.id);
        assert_eq!(vaults.removed, [source.id]);
        assert!(result.done[0].source_left.is_none());
        assert!(vaults.saved.is_empty());
    }

    #[test]
    fn a_clash_is_skipped_by_default_and_nothing_is_written() {
        let _guard = credentials::test_lock();
        let source = local_host("web");
        let request = request(
            TransferMode::Copy,
            HostGroup::Bitwarden,
            OnConflict::Skip,
            &[&source],
            &[],
        );
        let mut vaults = FakeVaults {
            items: vec![item("web-prod", "same")],
            ..FakeVaults::default()
        };
        let result = finished(run(&request, &mut vaults, word));
        assert!(result.done.is_empty() && result.failed.is_empty());
        assert_eq!(result.skipped, [("web".to_owned(), Clash::SameHost)]);
        assert!(vaults.saved.is_empty());
    }

    #[test]
    fn keep_both_takes_the_next_free_name_beside_a_same_named_item() {
        let _guard = credentials::test_lock();
        let source = local_host("web");
        let mut vaults = FakeVaults {
            // Unrelated servers, but the names a copy and a move would take.
            items: vec![item("web-copy", "10.1.1.1"), item("WEB", "10.2.2.2")],
            ..FakeVaults::default()
        };
        for (mode, wanted) in [
            (TransferMode::Copy, "web-copy-2"),
            (TransferMode::Move, "web-2"),
        ] {
            let request = request(
                mode,
                HostGroup::Bitwarden,
                OnConflict::KeepBoth,
                &[&source],
                &[],
            );
            let result = finished(run(&request, &mut vaults, word));
            assert_eq!(result.done[0].profile.alias, wanted, "{mode:?}");
            assert!(!result.done[0].overwrote);
        }
        assert!(vaults.saved.iter().all(|(_, over, _, _)| over.is_none()));
    }

    #[test]
    fn overwrite_writes_over_the_existing_item_and_retires_its_old_secrets() {
        let _guard = credentials::test_lock();
        let source = local_host("web");
        let (key, facts) = item("web-copy", "10.1.1.1");
        let mut mirror = vault_host("web-copy", VaultProvider::Bitwarden);
        mirror.id = key.id;
        mirror.vault.as_mut().unwrap().item_id = key.id;
        let old_sudo = Uuid::new_v4();
        mirror.advanced.sudo = Some(SudoSettings {
            enabled: true,
            credentials: vec![SudoCredential {
                id: old_sudo,
                username: "admin".to_owned(),
            }],
        });
        credentials::store(key.id, CredentialKind::KeyPassphrase, "old-key").unwrap();
        credentials::store(key.id, CredentialKind::SudoPassword(old_sudo), "old-sudo").unwrap();
        let request = request(
            TransferMode::Copy,
            HostGroup::Bitwarden,
            OnConflict::Overwrite,
            &[&source],
            std::slice::from_ref(&mirror),
        );
        let mut vaults = FakeVaults {
            items: vec![(key.clone(), facts)],
            ..FakeVaults::default()
        };
        let result = finished(run(&request, &mut vaults, word));
        let done = &result.done[0];
        let stale = done.stale.clone();
        credentials::delete_host(&mirror).unwrap();

        assert!(done.overwrote);
        assert_eq!(done.profile.id, key.id, "the existing host keeps its id");
        assert_eq!(done.profile.alias, "web-copy");
        assert_eq!(
            done.profile.address, "10.0.0.5",
            "it takes the copied server"
        );
        assert_eq!(
            vaults.saved,
            [(
                VaultProvider::Bitwarden,
                Some(key.id),
                "web-copy".to_owned(),
                None
            )]
        );
        assert_eq!(
            vaults.revisions,
            ["r1"],
            "an item changed meanwhile is refused"
        );
        assert_eq!(
            stale,
            [
                CredentialKind::KeyPassphrase,
                CredentialKind::SudoPassword(old_sudo)
            ],
            "records the new settings do not use go once the store is saved"
        );
        let mut hosts = vec![source.clone(), mirror.clone()];
        merge(&mut hosts, &result);
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[1].address, "10.0.0.5");
    }

    #[test]
    fn a_move_over_a_local_host_takes_its_id_and_the_moved_name() {
        let _guard = credentials::test_lock();
        let source = vault_host("db", VaultProvider::OnePassword);
        let mut local = local_host("db-old");
        let old_login = credentials::login_password_kind(&local);
        credentials::store(local.id, old_login, "old-login").unwrap();
        local.jump_host = None;
        let mut user = local_host("user");
        user.address = "10.7.7.7".to_owned();
        user.jump_host = Some(source.id);
        let request = request(
            TransferMode::Move,
            HostGroup::Local,
            OnConflict::Overwrite,
            &[&source],
            &[local.clone(), user.clone()],
        );
        let mut vaults = FakeVaults::default();
        vaults.passwords.insert(source.id, "new-login");
        let result = finished(run(&request, &mut vaults, word));
        let done = &result.done[0];
        let new_login = secret(
            done.profile.id,
            credentials::login_password_kind(&done.profile),
        );
        let mut hosts = vec![source.clone(), local.clone(), user.clone()];
        let removed = merge(&mut hosts, &result);
        credentials::delete_host(&done.profile).unwrap();
        credentials::delete_host(&local).unwrap();

        assert_eq!(done.profile.id, local.id);
        assert_eq!(done.profile.alias, "db");
        assert_eq!(new_login.as_deref(), Some("new-login"));
        assert!(done.stale.contains(&old_login));
        assert_eq!(vaults.removed, [source.id]);
        assert_eq!(removed.len(), 1);
        let aliases: Vec<&str> = hosts.iter().map(|host| host.alias.as_str()).collect();
        assert_eq!(aliases, ["db", "user"]);
        assert_eq!(hosts[1].jump_host, Some(local.id));
    }

    #[test]
    fn an_item_that_cannot_be_removed_is_reported_and_the_copy_stays() {
        let _guard = credentials::test_lock();
        let source = vault_host("db", VaultProvider::OnePassword);
        let request = request(
            TransferMode::Move,
            HostGroup::Bitwarden,
            OnConflict::Skip,
            &[&source],
            &[],
        );
        let mut vaults = FakeVaults {
            refuse_removal: true,
            ..FakeVaults::default()
        };
        let result = finished(run(&request, &mut vaults, word));
        assert_eq!(result.done.len(), 1);
        assert_eq!(result.done[0].source_left.as_deref(), Some("refused"));
        assert_eq!(
            vaults.saved[0].3, None,
            "an item without a password stays without one"
        );
        let mut hosts = vec![source.clone()];
        merge(&mut hosts, &result);
        assert_eq!(
            hosts.len(),
            1,
            "the original mirror goes so names stay unique"
        );
        assert!(hosts[0].is_bitwarden_host());
    }

    #[test]
    fn a_lock_before_any_write_fails_the_job_and_a_later_one_stops_the_rest() {
        let _guard = credentials::test_lock();
        let hosts = [
            vault_host("a", VaultProvider::Bitwarden),
            vault_host("b", VaultProvider::Bitwarden),
            vault_host("c", VaultProvider::Bitwarden),
        ];
        let request = request(
            TransferMode::Copy,
            HostGroup::OnePassword,
            OnConflict::KeepBoth,
            &[&hosts[0], &hosts[1], &hosts[2]],
            &[],
        );

        let mut locked = FakeVaults {
            locked_after: Some(0),
            ..FakeVaults::default()
        };
        assert!(failed(run(&request, &mut locked, word)).is_locked());
        assert!(locked.saved.is_empty());

        // The listing, then a read and a save for `a`, then `b`'s read locks.
        let mut later = FakeVaults {
            locked_after: Some(3),
            ..FakeVaults::default()
        };
        let result = finished(run(&request, &mut later, word));
        assert_eq!(result.done.len(), 1);
        assert_eq!(
            result.failed,
            [
                ("b".to_owned(), "locked".to_owned()),
                ("c".to_owned(), "locked".to_owned())
            ]
        );
    }

    #[test]
    fn a_batch_removal_retries_after_an_early_lock_and_reports_each_failure() {
        let hosts = [
            vault_host("a", VaultProvider::Bitwarden),
            vault_host("b", VaultProvider::OnePassword),
        ];
        let mut locked = FakeVaults {
            locked_after: Some(0),
            ..FakeVaults::default()
        };
        assert!(failed(remove_items(&hosts, &mut locked, word)).is_locked());

        let mut refusing = FakeVaults {
            refuse_removal: true,
            ..FakeVaults::default()
        };
        let report = finished(remove_items(&hosts, &mut refusing, word));
        assert!(report.removed.is_empty());
        assert_eq!(report.failed.len(), 2);

        let mut fine = FakeVaults::default();
        let report = finished(remove_items(&hosts, &mut fine, word));
        assert_eq!(report.removed, [hosts[0].id, hosts[1].id]);
    }

    fn window(app: &mut HostsApp, context: &egui::Context, size: [f32; 2]) -> egui::FullOutput {
        let mut output = None;
        for frame in 0..4 {
            output = Some(context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(size[0], size[1]),
                    )),
                    time: Some(f64::from(frame) * 0.1),
                    ..Default::default()
                },
                |ui| app.render(ui),
            ));
        }
        output.unwrap()
    }

    #[test]
    fn without_a_password_manager_there_is_nowhere_to_transfer_to() {
        let context = egui::Context::default();
        let host = local_host("web");
        let mut app = super::super::tests::metadata_test_app(&context, "en", host.clone());
        app.open_transfer(vec![host.id], TransferMode::Copy, None);
        assert!(app.transfer_prompt.is_none());
        assert_eq!(app.status.kind, StatusKind::Warning);
        assert_eq!(app.status.text, app.catalog.text("transfer_no_target"));
    }

    #[test]
    fn the_transfer_dialog_offers_stores_policies_and_known_clashes_in_every_locale() {
        for locale in ["en", "ja", "zh-CN", "zh-TW"] {
            let context = egui::Context::default();
            super::super::theme::configure_fonts(&context, locale);
            super::super::theme::apply_style(&context);
            let host = local_host("web");
            let mut app = super::super::tests::metadata_test_app(&context, locale, host.clone());
            app.store
                .hosts
                .push(vault_host("web-prod", VaultProvider::Bitwarden));
            app.store.bitwarden.cli_path = Some(std::path::PathBuf::from("C:/bw/bw.exe"));
            app.store.bitwarden.profile = Some(crate::model::BitwardenProfile::default());
            app.store.onepassword.cli_path = Some(std::path::PathBuf::from("C:/op/op.exe"));
            app.store.onepassword.account = Some(crate::model::OnePasswordAccount::default());
            app.open_transfer(vec![host.id], TransferMode::Move, None);
            let prompt = app.transfer_prompt.as_ref().unwrap();
            assert_eq!(
                prompt.target,
                HostGroup::Bitwarden,
                "{locale}: first other store"
            );
            assert_eq!(
                prompt.on_conflict,
                OnConflict::Skip,
                "{locale}: safe default"
            );
            let size = [820.0, 620.0];
            let output = window(&mut app, &context, size);
            let texts = super::super::tests::painted_texts(&output.shapes);
            let catalog = app.catalog.clone();
            let target = catalog.text("group_bitwarden");
            let clashes = catalog.format(
                "transfer_clashes",
                &[
                    ("target", target),
                    (
                        "names",
                        &format!("web ({})", catalog.text("transfer_clash_host")),
                    ),
                ],
            );
            let conflict = catalog.format("transfer_conflict", &[("target", target)]);
            let mut wanted: Vec<&str> = [
                "transfer_title",
                "group_local",
                "group_onepassword",
                "transfer_copy",
                "transfer_move_hint",
                "transfer_on_skip",
                "transfer_on_overwrite",
                "transfer_on_keep",
                "transfer_start_move",
                "cancel",
            ]
            .into_iter()
            .map(|key| catalog.text(key))
            .collect();
            wanted.extend([clashes.as_str(), conflict.as_str()]);
            for text in wanted {
                let (_, rect, _) = texts
                    .iter()
                    .find(|(painted, _, _)| painted.contains(text))
                    .unwrap_or_else(|| panic!("{locale}: missing {text:?}"));
                assert!(
                    rect.bottom() <= size[1],
                    "{locale}: {text:?} is below the window"
                );
            }
            for (text, rect, clip) in &texts {
                if rect.intersect(*clip).is_negative() {
                    continue;
                }
                assert!(
                    rect.right() <= clip.right() + 1.0,
                    "{locale}: text {text:?} is cut horizontally"
                );
            }
            assert!(app.transfer_prompt.is_some(), "the dialog stays open");
        }
    }
}
