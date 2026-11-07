use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    #[default]
    Ssh,
    Telnet,
}

impl Protocol {
    pub fn default_port(self) -> u16 {
        match self {
            Self::Ssh => 22,
            Self::Telnet => 23,
        }
    }

    pub fn stable_name(self) -> &'static str {
        match self {
            Self::Ssh => "ssh",
            Self::Telnet => "telnet",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SshAuth {
    #[default]
    Password,
    PrivateKey,
    SshAgent,
}

impl SshAuth {
    pub fn stable_name(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::PrivateKey => "private_key",
            Self::SshAgent => "ssh_agent",
        }
    }
}

/// How long an authenticated session may outlive the tool call that opened it.
/// The default: every call authenticates again, so a hardware key is touched
/// once per action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum AuthPersistence {
    #[default]
    PerCall,
    Session,
    Idle {
        minutes: u32,
    },
}

pub const MAX_IDLE_PERSISTENCE_MINUTES: u32 = 24 * 60;

/// Retention decided for a whole connection (or jump chain): the strictest hop wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    Session,
    Idle(std::time::Duration),
}

impl AuthPersistence {
    pub fn retention(self) -> Option<Retention> {
        match self {
            Self::PerCall => None,
            Self::Session => Some(Retention::Session),
            Self::Idle { minutes } => Some(Retention::Idle(std::time::Duration::from_secs(
                u64::from(minutes.clamp(1, MAX_IDLE_PERSISTENCE_MINUTES)) * 60,
            ))),
        }
    }
}

/// Retention for a chain is the strictest of its hops; any per-call hop makes
/// the whole chain per-call, because a retained jump session would otherwise
/// let later calls skip that hop's authentication.
pub fn chain_retention<'a>(chain: impl IntoIterator<Item = &'a HostProfile>) -> Option<Retention> {
    let mut retention = None;
    for host in chain {
        let hop = host.effective_auth_persistence().retention()?;
        retention = Some(match (retention, hop) {
            (None, hop) => hop,
            (Some(Retention::Session), other) | (Some(other), Retention::Session) => other,
            (Some(Retention::Idle(a)), Retention::Idle(b)) => Retention::Idle(a.min(b)),
        });
    }
    retention
}

pub const MAX_CHANNELS_PER_HOST: u8 = 16;
pub const DEFAULT_CHANNELS_PER_HOST: u8 = 8;
pub const MIN_KEEPALIVE_SECONDS: u16 = 10;
pub const MAX_KEEPALIVE_SECONDS: u16 = 300;
pub const DEFAULT_KEEPALIVE_SECONDS: u16 = 30;

/// A hint for Codex about the remote shell family; codex-hosts itself never
/// acts on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RemoteEnv {
    #[default]
    Auto,
    Posix,
    Windows,
}

/// Login-flow prompts for Telnet devices whose banners differ from the defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TelnetPrompts {
    pub login: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SudoCredential {
    pub id: Uuid,
    pub username: String,
}

/// Secret values live only in Credential Manager, keyed by host and record ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SudoSettings {
    /// Allow saved-password supply for sudo; passwordless calls remain available.
    pub enabled: bool,
    /// Account passwords at the legacy storage location, not selectable sudo
    /// identities. The UI binds the login account read-only and edits other su
    /// target accounts. Runtime supply follows the actual authentication request.
    pub credentials: Vec<SudoCredential>,
}

pub fn valid_sudo_username(value: &str) -> bool {
    !value.trim().is_empty() && !value.contains(['\r', '\n', '\0'])
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SuSettings {
    /// Allow saved-password supply for su; passwordless calls remain available.
    pub enabled: bool,
}

fn no_su_settings(value: &Option<SuSettings>) -> bool {
    value
        .as_ref()
        .is_none_or(|settings| *settings == SuSettings::default())
}

fn no_sudo_settings(value: &Option<SudoSettings>) -> bool {
    value
        .as_ref()
        .is_none_or(|settings| *settings == SudoSettings::default())
}

/// Optional per-host tuning; every field left at its default means "use the
/// global value", and profiles saved before these existed read back unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AdvancedSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_channels: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connect_timeout_s: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_timeout_s: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keepalive_s: Option<u16>,
    #[serde(skip_serializing_if = "is_auto")]
    pub remote_env: RemoteEnv,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub codex_hidden: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub require_call_approval: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telnet_prompts: Option<TelnetPrompts>,
    #[serde(skip_serializing_if = "no_sudo_settings")]
    pub sudo: Option<SudoSettings>,
    #[serde(skip_serializing_if = "no_su_settings")]
    pub su: Option<SuSettings>,
}

fn is_auto(value: &RemoteEnv) -> bool {
    *value == RemoteEnv::Auto
}

impl AdvancedSettings {
    pub fn is_default(&self) -> bool {
        let mut settings = self.clone();
        if no_sudo_settings(&settings.sudo) {
            settings.sudo = None;
        }
        if no_su_settings(&settings.su) {
            settings.su = None;
        }
        settings == Self::default()
    }

    pub fn max_channels(&self) -> usize {
        usize::from(self.max_channels.unwrap_or(DEFAULT_CHANNELS_PER_HOST))
    }

    pub fn keepalive(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(
            self.keepalive_s.unwrap_or(DEFAULT_KEEPALIVE_SECONDS),
        ))
    }

    fn validation_issue(&self) -> Option<ValidationIssue> {
        if let Some(sudo) = &self.sudo {
            let mut users = HashSet::new();
            let mut ids = HashSet::new();
            if sudo.credentials.iter().any(|credential| {
                !valid_sudo_username(&credential.username)
                    || !users.insert(credential.username.trim())
                    || !ids.insert(credential.id)
            }) {
                return Some(ValidationIssue::Sudo);
            }
        }
        let channels_ok = self
            .max_channels
            .is_none_or(|value| (1..=MAX_CHANNELS_PER_HOST).contains(&value));
        let keepalive_ok = self
            .keepalive_s
            .is_none_or(|value| (MIN_KEEPALIVE_SECONDS..=MAX_KEEPALIVE_SECONDS).contains(&value));
        let timeouts_ok = self.connect_timeout_s.is_none_or(|value| value >= 1)
            && self.command_timeout_s.is_none_or(|value| value >= 1);
        (!(channels_ok && keepalive_ok && timeouts_ok)).then_some(ValidationIssue::Advanced)
    }
}

/// Where a host's password is kept, reported by `list_hosts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialSource {
    /// Windows Credential Manager, the original store.
    CredentialManager,
    /// A Login item in the user's Bitwarden vault, read through `bw.exe`.
    Bitwarden,
    /// A Login item in the user's 1Password vault, read through `op.exe`.
    OnePassword,
}

impl CredentialSource {
    pub fn stable_name(self) -> &'static str {
        match self {
            Self::CredentialManager => "credential_manager",
            Self::Bitwarden => "bitwarden",
            Self::OnePassword => "onepassword",
        }
    }
}

/// The password manager a vault-backed host comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VaultProvider {
    #[default]
    #[serde(rename = "bitwarden")]
    Bitwarden,
    #[serde(rename = "onepassword")]
    OnePassword,
}

impl VaultProvider {
    fn is_bitwarden(&self) -> bool {
        *self == Self::Bitwarden
    }

    pub fn credential_source(self) -> CredentialSource {
        match self {
            Self::Bitwarden => CredentialSource::Bitwarden,
            Self::OnePassword => CredentialSource::OnePassword,
        }
    }
}

/// The password-manager Login item a vault-backed host mirrors. The item is
/// the source of truth for the alias (its name), the endpoint (its `ssh://`
/// or `telnet://` URI), the username, the password, the description and the
/// tags (codex-hosts custom fields); the profile keeps only local state such
/// as host-key trust, privilege settings, and approval switches. Nothing
/// secret.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultHostRef {
    /// Which password manager holds the item; Bitwarden for older stores.
    #[serde(default, skip_serializing_if = "VaultProvider::is_bitwarden")]
    pub provider: VaultProvider,
    /// Item id; also the profile id. For 1Password, whose ids are not UUIDs,
    /// it is derived from the account and [`Self::external_id`].
    pub item_id: Uuid,
    /// The password manager's own item id when it is not `item_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
    /// Item name at the last refresh (the alias).
    pub item_name: String,
    /// `revisionDate` at the last refresh; a changed item is a new pool key.
    #[serde(default)]
    pub revision_date: String,
}

/// How long an unlocked Bitwarden session may serve calls before the master
/// password is asked for again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ReauthPolicy {
    /// Every call asks; nothing is stored between calls.
    PerCall,
    /// Stored until unused for this many minutes.
    Idle {
        #[serde(default = "default_idle_minutes")]
        minutes: u32,
    },
    /// Stored until the codex-hosts window exits.
    UntilExit,
    /// Stored until Bitwarden rejects it.
    Permanent,
}

impl Default for ReauthPolicy {
    fn default() -> Self {
        Self::Idle {
            minutes: DEFAULT_REAUTH_IDLE_MINUTES,
        }
    }
}

fn default_idle_minutes() -> u32 {
    DEFAULT_REAUTH_IDLE_MINUTES
}

pub const DEFAULT_REAUTH_IDLE_MINUTES: u32 = 15;
pub const MAX_REAUTH_IDLE_MINUTES: u32 = 24 * 60;
/// Seconds a `bw sync` may be old before a vault host is used; 0 syncs before
/// every use (subject to [`MIN_SYNC_SPACING_SECONDS`]).
pub const DEFAULT_SYNC_MAX_AGE_SECONDS: u64 = 0;
/// Calls that follow each other closely (a batch, a probe then an exec) share
/// one sync even under "sync before every use".
pub const MIN_SYNC_SPACING_SECONDS: u64 = 5;
/// The sync-window choices the Bitwarden settings offer, in seconds.
pub const SYNC_MAX_AGE_CHOICES: [u64; 4] = [0, 60, 5 * 60, 30 * 60];
/// Folder the wizard proposes for host credentials.
pub const DEFAULT_BITWARDEN_FOLDER: &str = "codex-hosts";

/// The 1Password account and vault whose Login items are hosts. Non-secret:
/// signing in and authorizing `op` are the 1Password app's business.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OnePasswordAccount {
    /// Account id as `op account list` reports it (`account_uuid`); passed
    /// to every call as `--account`.
    pub account_id: String,
    /// Sign-in address, for display.
    pub url: String,
    /// Account e-mail, for display.
    pub email: String,
    /// Vault holding host Login items, by id.
    pub vault_id: String,
    /// Display name of that vault.
    pub vault_name: String,
}

/// 1Password settings kept with the host store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OnePasswordSettings {
    /// Whether 1Password hosts are shown and served at all.
    pub enabled: bool,
    /// Absolute path of the native `op.exe` chosen by the user.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_path: Option<std::path::PathBuf>,
    /// The chosen account and vault.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<OnePasswordAccount>,
}

impl Default for OnePasswordSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            cli_path: None,
            account: None,
        }
    }
}

impl OnePasswordSettings {
    /// Whether 1Password hosts can be served right now.
    pub fn available(&self) -> bool {
        self.enabled && self.account.is_some() && self.cli_path.is_some()
    }
}

/// The one Bitwarden account codex-hosts uses. Non-secret; the login state
/// lives in the CLI data directory and the session key in Credential Manager.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BitwardenProfile {
    /// Stable id; also names the CLI data directory.
    pub id: Uuid,
    /// Account e-mail.
    pub email: String,
    /// `bw config server` target; `None` is the Bitwarden cloud (US).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_url: Option<String>,
    /// Folder holding host Login items, by id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<Uuid>,
    /// Display name of that folder.
    pub folder_name: String,
    /// When the master password is asked for again.
    pub policy: ReauthPolicy,
}

impl Default for BitwardenProfile {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            email: String::new(),
            server_url: None,
            folder_id: None,
            folder_name: DEFAULT_BITWARDEN_FOLDER.to_owned(),
            policy: ReauthPolicy::default(),
        }
    }
}

/// Bitwarden settings kept with the host store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BitwardenSettings {
    /// Whether vault-backed hosts are shown and served at all.
    pub enabled: bool,
    /// Absolute path of the native `bw.exe` chosen by the user.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_path: Option<std::path::PathBuf>,
    /// The configured account, once the wizard finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<BitwardenProfile>,
    /// How old the vault cache may be when a vault host is used, in seconds;
    /// 0 syncs before every use.
    pub sync_max_age_seconds: u64,
}

impl Default for BitwardenSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            cli_path: None,
            profile: None,
            sync_max_age_seconds: DEFAULT_SYNC_MAX_AGE_SECONDS,
        }
    }
}

impl BitwardenSettings {
    /// Whether vault-backed hosts can be served right now.
    pub fn available(&self) -> bool {
        self.enabled && self.profile.is_some() && self.cli_path.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostProfile {
    pub id: Uuid,
    pub alias: String,
    pub description: String,
    pub tags: Vec<String>,
    pub address: String,
    pub port: u16,
    pub username: String,
    /// Immutable credential record for this login identity. None reads legacy profiles.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_password_id: Option<Uuid>,
    /// Set for a vault-backed host: the password-manager item that defines it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vault: Option<VaultHostRef>,
    pub protocol: Protocol,
    pub ssh_auth: SshAuth,
    pub private_key_path: String,
    pub agent_key_fingerprint: String,
    pub host_fingerprint: Option<String>,
    pub host_key_algorithm: Option<String>,
    pub host_key_first_seen_unix: Option<u64>,
    pub host_key_last_verified_unix: Option<u64>,
    pub jump_host: Option<Uuid>,
    pub verified: bool,
    pub auth_persistence: AuthPersistence,
    pub advanced: AdvancedSettings,
}

impl Default for HostProfile {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            alias: String::new(),
            description: String::new(),
            tags: Vec::new(),
            address: String::new(),
            port: 22,
            username: String::new(),
            login_password_id: None,
            vault: None,
            protocol: Protocol::Ssh,
            ssh_auth: SshAuth::Password,
            private_key_path: String::new(),
            agent_key_fingerprint: String::new(),
            host_fingerprint: None,
            host_key_algorithm: None,
            host_key_first_seen_unix: None,
            host_key_last_verified_unix: None,
            jump_host: None,
            verified: false,
            auth_persistence: AuthPersistence::PerCall,
            advanced: AdvancedSettings::default(),
        }
    }
}

impl HostProfile {
    pub fn new(alias: String) -> Self {
        Self {
            alias,
            ..Self::default()
        }
    }

    pub fn validation_issue(&self) -> Option<ValidationIssue> {
        if self.alias.trim().is_empty() {
            return Some(ValidationIssue::Alias);
        }
        if self.address.trim().is_empty() {
            return Some(ValidationIssue::Address);
        }
        if self.username.trim().is_empty() {
            return Some(ValidationIssue::Username);
        }
        if self.port == 0 {
            return Some(ValidationIssue::Port);
        }
        if self.protocol == Protocol::Ssh
            && self.ssh_auth == SshAuth::PrivateKey
            && self.private_key_path.trim().is_empty()
        {
            return Some(ValidationIssue::PrivateKey);
        }
        if self.protocol == Protocol::Telnet && self.jump_host.is_some() {
            return Some(ValidationIssue::TelnetChain);
        }
        if let AuthPersistence::Idle { minutes } = self.auth_persistence
            && !(1..=MAX_IDLE_PERSISTENCE_MINUTES).contains(&minutes)
        {
            return Some(ValidationIssue::AuthPersistence);
        }
        self.advanced.validation_issue()
    }

    /// Whether logging in needs the login password at all.
    pub fn uses_login_password(&self) -> bool {
        match self.protocol {
            Protocol::Ssh => self.ssh_auth == SshAuth::Password,
            Protocol::Telnet => true,
        }
    }

    /// The password-manager item this host mirrors, for a vault-backed host.
    pub fn vault(&self) -> Option<&VaultHostRef> {
        self.vault.as_ref()
    }

    /// Whether the host is defined by a password-manager item rather than
    /// this profile and Credential Manager.
    pub fn is_vault_host(&self) -> bool {
        self.vault.is_some()
    }

    /// The password manager that defines the host, if any.
    pub fn vault_provider(&self) -> Option<VaultProvider> {
        self.vault.as_ref().map(|vault| vault.provider)
    }

    /// Whether the host is defined by a Bitwarden item.
    pub fn is_bitwarden_host(&self) -> bool {
        self.vault_provider() == Some(VaultProvider::Bitwarden)
    }

    /// Where the login password comes from, when one is used at all.
    pub fn credential_source(&self) -> Option<CredentialSource> {
        self.uses_login_password().then(|| {
            self.vault_provider().map_or(
                CredentialSource::CredentialManager,
                VaultProvider::credential_source,
            )
        })
    }

    /// Telnet has no session layer yet, so its profiles always authenticate per call.
    pub fn effective_auth_persistence(&self) -> AuthPersistence {
        match self.protocol {
            Protocol::Ssh => self.auth_persistence,
            Protocol::Telnet => AuthPersistence::PerCall,
        }
    }

    pub fn connection_details_equal(&self, other: &Self) -> bool {
        self.address.trim() == other.address.trim()
            && self.port == other.port
            && self.username.trim() == other.username.trim()
            && self.login_password_id == other.login_password_id
            && self.protocol == other.protocol
            && self.ssh_auth == other.ssh_auth
            && self.private_key_path.trim() == other.private_key_path.trim()
            && self.agent_key_fingerprint.trim() == other.agent_key_fingerprint.trim()
            && self.jump_host == other.jump_host
    }

    pub fn apply_prefill(&mut self, prefill: &Prefill) {
        if let Some(description) = &prefill.description {
            self.description.clone_from(description);
        }
        if let Some(tags) = &prefill.tags {
            self.tags = normalize_tags(tags);
        }
        if let Some(alias) = &prefill.alias {
            self.alias.clone_from(alias);
        }
        if let Some(address) = &prefill.address {
            self.address.clone_from(address);
        }
        if let Some(port) = prefill.port {
            self.port = port;
        }
        if let Some(username) = &prefill.username {
            self.username.clone_from(username);
        }
        if let Some(protocol) = prefill.protocol {
            self.protocol = protocol;
            if prefill.port.is_none() {
                self.port = protocol.default_port();
            }
        }
        if let Some(ssh_auth) = prefill.ssh_auth {
            self.ssh_auth = ssh_auth;
        }
        if let Some(path) = &prefill.private_key_path {
            self.private_key_path.clone_from(path);
        }
        if let Some(fingerprint) = &prefill.agent_key_fingerprint {
            self.agent_key_fingerprint.clone_from(fingerprint);
        }
        if prefill.su_enabled.is_some() {
            let settings = self.advanced.su.get_or_insert_with(Default::default);
            if let Some(enabled) = prefill.su_enabled {
                settings.enabled = enabled;
            }
        }
        if prefill.sudo_enabled.is_some() || prefill.sudo_users.is_some() {
            let settings = self.advanced.sudo.get_or_insert_with(Default::default);
            if let Some(enabled) = prefill.sudo_enabled {
                settings.enabled = enabled;
            }
            if let Some(users) = &prefill.sudo_users {
                settings.credentials = users
                    .iter()
                    .map(|username| {
                        settings
                            .credentials
                            .iter()
                            .find(|credential| credential.username.trim() == username.trim())
                            .cloned()
                            .unwrap_or_else(|| SudoCredential {
                                id: Uuid::new_v4(),
                                username: username.trim().to_owned(),
                            })
                    })
                    .collect();
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationIssue {
    Alias,
    Address,
    Username,
    Port,
    PrivateKey,
    Chain,
    TelnetChain,
    AuthPersistence,
    Advanced,
    Sudo,
}

impl ValidationIssue {
    /// A short English label for tool results, which are not localised.
    pub fn tool_label(self) -> &'static str {
        match self {
            Self::Alias => "alias is empty",
            Self::Address => "address is empty",
            Self::Username => "username is empty",
            Self::Port => "port is 0",
            Self::PrivateKey => "private key path is empty",
            Self::Chain => "jump-host chain is invalid",
            Self::TelnetChain => "a Telnet host cannot use a jump host",
            Self::AuthPersistence => "idle minutes are out of range",
            Self::Advanced => "advanced settings are out of range",
            Self::Sudo => "sudo usernames must be nonempty, unique and contain no CR, LF or NUL",
        }
    }

    pub fn translation_key(self) -> &'static str {
        match self {
            Self::Alias => "validation_alias",
            Self::Address => "validation_address",
            Self::Username => "validation_username",
            Self::Port => "validation_port",
            Self::PrivateKey => "validation_private_key",
            Self::Chain => "validation_chain",
            Self::TelnetChain => "validation_telnet_chain",
            Self::AuthPersistence => "validation_auth_persistence",
            Self::Advanced => "validation_advanced",
            Self::Sudo => "validation_sudo",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Prefill {
    pub alias: Option<String>,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    pub address: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub protocol: Option<Protocol>,
    pub ssh_auth: Option<SshAuth>,
    pub private_key_path: Option<String>,
    pub agent_key_fingerprint: Option<String>,
    pub jump_alias: Option<String>,
    pub sudo_enabled: Option<bool>,
    pub sudo_users: Option<Vec<String>>,
    pub su_enabled: Option<bool>,
}

/// Alias lookup shared by the GUI store and the tools: ASCII case and
/// surrounding whitespace are ignored.
pub fn find_alias<'a>(hosts: &'a [HostProfile], alias: &str) -> Option<&'a HostProfile> {
    let alias = alias.trim();
    hosts
        .iter()
        .find(|host| host.alias.eq_ignore_ascii_case(alias))
}

pub fn normalize_tags(tags: impl IntoIterator<Item = impl AsRef<str>>) -> Vec<String> {
    let mut seen = HashSet::new();
    tags.into_iter()
        .map(|tag| tag.as_ref().trim().to_owned())
        .filter(|tag| !tag.is_empty() && seen.insert(tag.to_lowercase()))
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostFilter {
    pub search: String,
    pub tags: Vec<String>,
}

impl HostFilter {
    pub fn matches(&self, host: &HostProfile) -> bool {
        let search = self.search.trim().to_lowercase();
        let text_matches = search.is_empty()
            || [
                &host.alias,
                &host.address,
                &host.username,
                &host.description,
            ]
            .into_iter()
            .chain(host.tags.iter())
            .any(|value| value.to_lowercase().contains(&search));
        text_matches
            && self.tags.iter().all(|required| {
                let required = required.trim().to_lowercase();
                required.is_empty()
                    || host
                        .tags
                        .iter()
                        .any(|tag| tag.trim().to_lowercase() == required)
            })
    }
}

pub fn resolve_ssh_chain<'a>(
    target: &'a HostProfile,
    hosts: &'a [HostProfile],
) -> Result<Vec<&'a HostProfile>, ValidationIssue> {
    if target.protocol != Protocol::Ssh {
        return Err(ValidationIssue::TelnetChain);
    }
    let mut resolved = Vec::new();
    let mut active = HashSet::new();
    resolve_one(target, hosts, &mut active, &mut resolved, 0)?;
    Ok(resolved)
}

fn resolve_one<'a>(
    host: &'a HostProfile,
    hosts: &'a [HostProfile],
    active: &mut HashSet<Uuid>,
    resolved: &mut Vec<&'a HostProfile>,
    depth: usize,
) -> Result<(), ValidationIssue> {
    if depth >= 8 || host.protocol != Protocol::Ssh || !active.insert(host.id) {
        return Err(ValidationIssue::Chain);
    }
    if let Some(jump_id) = host.jump_host {
        let jump = hosts
            .iter()
            .find(|candidate| candidate.id == jump_id)
            .ok_or(ValidationIssue::Chain)?;
        resolve_one(jump, hosts, active, resolved, depth + 1)?;
    }
    resolved.push(host);
    active.remove(&host.id);
    Ok(())
}

pub fn can_use_as_jump(
    candidate: &HostProfile,
    target: &HostProfile,
    hosts: &[HostProfile],
) -> bool {
    if candidate.id == target.id || candidate.protocol != Protocol::Ssh || !candidate.verified {
        return false;
    }
    let mut proposed = target.clone();
    proposed.protocol = Protocol::Ssh;
    proposed.jump_host = Some(candidate.id);
    resolve_ssh_chain(&proposed, hosts).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sudo_metadata_is_optional_secret_free_and_validates_unique_accounts() {
        let mut host: HostProfile =
            serde_json::from_str(r#"{"alias":"old","address":"host","username":"alice"}"#).unwrap();
        assert!(host.advanced.sudo.is_none());
        host.advanced.sudo = Some(SudoSettings::default());
        assert!(host.advanced.is_default());
        assert!(
            serde_json::to_value(&host).unwrap()["advanced"]
                .get("sudo")
                .is_none()
        );
        let id = Uuid::new_v4();
        host.advanced.sudo = Some(SudoSettings {
            enabled: true,
            credentials: vec![SudoCredential {
                id,
                username: "alice".into(),
            }],
        });
        assert!(host.validation_issue().is_none());
        let json = serde_json::to_value(&host).unwrap();
        assert_eq!(
            json["advanced"]["sudo"]["credentials"][0]["username"],
            "alice"
        );
        assert!(
            json["advanced"]["sudo"]["credentials"][0]
                .get("password")
                .is_none()
        );
        let mut prefill = Prefill {
            sudo_users: Some(vec!["alice".into(), "bob".into()]),
            ..Default::default()
        };
        host.apply_prefill(&prefill);
        assert_eq!(host.advanced.sudo.as_ref().unwrap().credentials[0].id, id);
        prefill.sudo_users = Some(vec!["alice".into(), " alice ".into()]);
        host.apply_prefill(&prefill);
        assert_eq!(host.validation_issue(), Some(ValidationIssue::Sudo));
        prefill.sudo_users = Some(vec![]);
        host.apply_prefill(&prefill);
        assert!(host.advanced.sudo.as_ref().unwrap().credentials.is_empty());
    }

    #[test]
    fn su_defaults_remain_off_and_can_be_enabled_without_password_records() {
        let mut host: HostProfile =
            serde_json::from_str(r#"{"alias":"old","address":"host","username":"root"}"#).unwrap();
        assert!(host.advanced.su.is_none());
        host.advanced.su = Some(SuSettings::default());
        assert!(host.advanced.is_default());
        assert!(
            serde_json::to_value(&host).unwrap()["advanced"]
                .get("su")
                .is_none()
        );
        host.apply_prefill(&Prefill {
            su_enabled: Some(true),
            ..Default::default()
        });
        assert!(host.advanced.sudo.is_none());
        assert!(host.validation_issue().is_none());
        let encoded = serde_json::to_value(&host).unwrap();
        assert_eq!(encoded["advanced"]["su"]["enabled"], true);
        assert!(encoded["advanced"]["su"].get("password").is_none());
    }

    #[test]
    fn call_approval_defaults_off_and_persists_independently_of_auth_retention() {
        let mut host: HostProfile =
            serde_json::from_str(r#"{"alias":"old","address":"host","username":"alice"}"#).unwrap();
        assert!(!host.advanced.require_call_approval);
        assert!(host.advanced.is_default());
        assert!(
            serde_json::to_value(&host).unwrap()["advanced"]
                .get("require_call_approval")
                .is_none()
        );
        for retention in [
            AuthPersistence::PerCall,
            AuthPersistence::Session,
            AuthPersistence::Idle { minutes: 5 },
        ] {
            host.auth_persistence = retention;
            host.advanced.require_call_approval = true;
            assert!(!host.advanced.is_default());
            let json = serde_json::to_value(&host).unwrap();
            assert_eq!(json["advanced"]["require_call_approval"], true);
            assert_eq!(serde_json::from_value::<HostProfile>(json).unwrap(), host);
        }
    }

    #[test]
    fn auth_persistence_migrates_old_profiles_and_round_trips() {
        let old: HostProfile = serde_json::from_str(r#"{"alias":"a","address":"h"}"#).unwrap();
        assert_eq!(old.auth_persistence, AuthPersistence::PerCall);
        let mut idle = old.clone();
        idle.auth_persistence = AuthPersistence::Idle { minutes: 15 };
        let json = serde_json::to_value(&idle).unwrap();
        assert_eq!(
            json["auth_persistence"],
            serde_json::json!({"mode": "idle", "minutes": 15})
        );
        let restored: HostProfile = serde_json::from_value(json).unwrap();
        assert_eq!(restored, idle);
        assert_eq!(
            serde_json::to_value(AuthPersistence::Session).unwrap(),
            serde_json::json!({"mode": "session"})
        );
    }

    #[test]
    fn telnet_profiles_always_authenticate_per_call() {
        let host = HostProfile {
            protocol: Protocol::Telnet,
            auth_persistence: AuthPersistence::Session,
            ..Default::default()
        };
        assert_eq!(host.effective_auth_persistence(), AuthPersistence::PerCall);
        assert_eq!(chain_retention([&host]), None);
    }

    #[test]
    fn chain_retention_takes_the_strictest_hop() {
        let session = HostProfile {
            auth_persistence: AuthPersistence::Session,
            ..Default::default()
        };
        let idle = HostProfile {
            auth_persistence: AuthPersistence::Idle { minutes: 5 },
            ..Default::default()
        };
        let per_call = HostProfile::default();
        assert_eq!(chain_retention([&session]), Some(Retention::Session));
        assert_eq!(
            chain_retention([&session, &idle]),
            Some(Retention::Idle(std::time::Duration::from_secs(300)))
        );
        assert_eq!(chain_retention([&idle, &session, &per_call]), None);
        assert_eq!(
            AuthPersistence::Idle { minutes: 0 }.retention(),
            Some(Retention::Idle(std::time::Duration::from_secs(60)))
        );
        assert_eq!(
            AuthPersistence::Idle { minutes: 99_999 }.retention(),
            Some(Retention::Idle(std::time::Duration::from_secs(
                u64::from(MAX_IDLE_PERSISTENCE_MINUTES) * 60
            )))
        );
    }

    #[test]
    fn metadata_is_backward_compatible_and_does_not_change_connection_identity() {
        let old: HostProfile = serde_json::from_str(
            r#"{"alias":"legacy","address":"server","username":"user","verified":true}"#,
        )
        .unwrap();
        assert!(old.description.is_empty());
        assert!(old.tags.is_empty());
        let mut edited = old.clone();
        edited.description = "  中文备注\nsecond line\n".into();
        edited.tags = normalize_tags([" Prod ", "prod", "", "WEB", "web", "日本語"]);
        assert_eq!(edited.tags, ["Prod", "WEB", "日本語"]);
        assert!(edited.connection_details_equal(&old));
        assert!(edited.verified);
        let restored: HostProfile =
            serde_json::from_str(&serde_json::to_string(&edited).unwrap()).unwrap();
        assert_eq!(restored, edited);
        edited.apply_prefill(&Prefill::default());
        assert_eq!(restored, edited);
        edited.apply_prefill(&Prefill {
            description: Some(String::new()),
            tags: Some(vec![]),
            ..Default::default()
        });
        assert!(edited.description.is_empty() && edited.tags.is_empty());
    }

    #[test]
    fn host_filter_combines_text_and_all_tags() {
        let host = HostProfile {
            alias: "gateway".into(),
            description: "Tokyo 日本語".into(),
            tags: vec!["Prod".into(), "Web".into()],
            ..Default::default()
        };
        assert!(HostFilter::default().matches(&host));
        assert!(
            HostFilter {
                search: "日本語".into(),
                tags: vec!["prod".into(), " WEB ".into()]
            }
            .matches(&host)
        );
        assert!(
            !HostFilter {
                search: String::new(),
                tags: vec!["prod".into(), "db".into()]
            }
            .matches(&host)
        );
        assert!(
            !HostFilter {
                search: "absent".into(),
                tags: vec![]
            }
            .matches(&host)
        );
    }

    fn verified_ssh(alias: &str) -> HostProfile {
        HostProfile {
            alias: alias.to_owned(),
            address: "127.0.0.1".to_owned(),
            username: "tester".to_owned(),
            verified: true,
            ..HostProfile::default()
        }
    }

    #[test]
    fn resolves_nested_jump_hosts_in_connection_order() {
        let first = verified_ssh("first");
        let mut second = verified_ssh("second");
        second.jump_host = Some(first.id);
        let mut target = verified_ssh("target");
        target.jump_host = Some(second.id);
        let hosts = vec![first, second, target.clone()];
        let aliases = resolve_ssh_chain(&target, &hosts)
            .unwrap()
            .into_iter()
            .map(|host| host.alias.as_str())
            .collect::<Vec<_>>();
        assert_eq!(aliases, ["first", "second", "target"]);
    }

    #[test]
    fn rejects_jump_host_cycles() {
        let mut first = verified_ssh("first");
        let mut second = verified_ssh("second");
        first.jump_host = Some(second.id);
        second.jump_host = Some(first.id);
        let hosts = vec![first.clone(), second];
        assert_eq!(
            resolve_ssh_chain(&first, &hosts),
            Err(ValidationIssue::Chain)
        );
    }

    #[test]
    fn older_profiles_gain_safe_agent_and_host_key_defaults() {
        let profile: HostProfile = serde_json::from_value(serde_json::json!({
            "alias": "legacy",
            "address": "127.0.0.1",
            "username": "tester"
        }))
        .unwrap();
        assert_eq!(profile.ssh_auth, SshAuth::Password);
        assert!(profile.agent_key_fingerprint.is_empty());
        assert!(profile.host_key_algorithm.is_none());
        assert!(profile.host_key_first_seen_unix.is_none());
        assert!(profile.host_key_last_verified_unix.is_none());
    }
}
