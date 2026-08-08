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
}

impl SshAuth {
    pub fn stable_name(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::PrivateKey => "private_key",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostProfile {
    pub id: Uuid,
    pub alias: String,
    pub address: String,
    pub port: u16,
    pub username: String,
    pub protocol: Protocol,
    pub ssh_auth: SshAuth,
    pub private_key_path: String,
    pub host_fingerprint: Option<String>,
    pub jump_host: Option<Uuid>,
    pub verified: bool,
}

impl Default for HostProfile {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            alias: String::new(),
            address: String::new(),
            port: 22,
            username: String::new(),
            protocol: Protocol::Ssh,
            ssh_auth: SshAuth::Password,
            private_key_path: String::new(),
            host_fingerprint: None,
            jump_host: None,
            verified: false,
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
        None
    }

    pub fn connection_details_equal(&self, other: &Self) -> bool {
        self.address.trim() == other.address.trim()
            && self.port == other.port
            && self.username.trim() == other.username.trim()
            && self.protocol == other.protocol
            && self.ssh_auth == other.ssh_auth
            && self.private_key_path.trim() == other.private_key_path.trim()
            && self.jump_host == other.jump_host
    }

    pub fn apply_prefill(&mut self, prefill: &Prefill) {
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
}

impl ValidationIssue {
    pub fn translation_key(self) -> &'static str {
        match self {
            Self::Alias => "validation_alias",
            Self::Address => "validation_address",
            Self::Username => "validation_username",
            Self::Port => "validation_port",
            Self::PrivateKey => "validation_private_key",
            Self::Chain => "validation_chain",
            Self::TelnetChain => "validation_telnet_chain",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Prefill {
    pub alias: Option<String>,
    pub address: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub protocol: Option<Protocol>,
    pub ssh_auth: Option<SshAuth>,
    pub private_key_path: Option<String>,
    pub jump_alias: Option<String>,
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
}
