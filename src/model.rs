use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    #[default]
    #[serde(alias = "ssh_password")]
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
/// The default keeps today's behaviour: every call authenticates again, so a
/// hardware key is touched once per action.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telnet_prompts: Option<TelnetPrompts>,
}

fn is_auto(value: &RemoteEnv) -> bool {
    *value == RemoteEnv::Auto
}

impl AdvancedSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
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
