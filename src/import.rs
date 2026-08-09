use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::model::{HostProfile, Protocol, SshAuth, resolve_ssh_chain};

const TEMPLATE_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostImportTemplate {
    version: u32,
    hosts: Vec<ImportHost>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportHost {
    alias: String,
    address: String,
    #[serde(default)]
    port: Option<u16>,
    username: String,
    #[serde(default)]
    protocol: Protocol,
    #[serde(default)]
    ssh_auth: SshAuth,
    #[serde(default)]
    private_key_path: String,
    #[serde(default)]
    jump_host: Option<String>,
}

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("the template JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported template version {0}")]
    Version(u32),
    #[error("the template contains no hosts")]
    Empty,
    #[error("host alias is empty")]
    EmptyAlias,
    #[error("host alias already exists: {0}")]
    DuplicateAlias(String),
    #[error("host {alias} references an unknown jump host: {jump}")]
    UnknownJump { alias: String, jump: String },
    #[error("host {alias} has invalid connection fields")]
    InvalidHost { alias: String },
    #[error("host {0} has an invalid jump-host chain")]
    InvalidChain(String),
}

pub fn template_bytes() -> Vec<u8> {
    let template = HostImportTemplate {
        version: TEMPLATE_VERSION,
        hosts: vec![ImportHost {
            alias: "example".to_owned(),
            address: "server.example.com".to_owned(),
            port: Some(22),
            username: "operator".to_owned(),
            protocol: Protocol::Ssh,
            ssh_auth: SshAuth::Password,
            private_key_path: String::new(),
            jump_host: None,
        }],
    };
    serde_json::to_vec_pretty(&template).expect("the built-in import template is serializable")
}

pub fn parse_template(
    bytes: &[u8],
    existing_hosts: &[HostProfile],
) -> Result<Vec<HostProfile>, ImportError> {
    let template: HostImportTemplate = serde_json::from_slice(bytes)?;
    if template.version != TEMPLATE_VERSION {
        return Err(ImportError::Version(template.version));
    }
    if template.hosts.is_empty() {
        return Err(ImportError::Empty);
    }

    let mut aliases = existing_hosts
        .iter()
        .map(|host| host.alias.trim().to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut imported = Vec::with_capacity(template.hosts.len());
    let mut jump_aliases = Vec::with_capacity(template.hosts.len());

    for item in template.hosts {
        let alias = item.alias.trim().to_owned();
        if alias.is_empty() {
            return Err(ImportError::EmptyAlias);
        }
        if !aliases.insert(alias.to_ascii_lowercase()) {
            return Err(ImportError::DuplicateAlias(alias));
        }
        let port = item.port.unwrap_or_else(|| item.protocol.default_port());
        let profile = HostProfile {
            id: Uuid::new_v4(),
            alias: alias.clone(),
            address: item.address.trim().to_owned(),
            port,
            username: item.username.trim().to_owned(),
            protocol: item.protocol,
            ssh_auth: item.ssh_auth,
            private_key_path: item.private_key_path.trim().to_owned(),
            host_fingerprint: None,
            jump_host: None,
            verified: false,
        };
        if profile.validation_issue().is_some() {
            return Err(ImportError::InvalidHost { alias });
        }
        imported.push(profile);
        jump_aliases.push(item.jump_host.map(|value| value.trim().to_owned()));
    }

    let ids_by_alias = existing_hosts
        .iter()
        .chain(imported.iter())
        .map(|host| (host.alias.trim().to_ascii_lowercase(), host.id))
        .collect::<HashMap<_, _>>();
    for (host, jump_alias) in imported.iter_mut().zip(jump_aliases) {
        let Some(jump_alias) = jump_alias.filter(|value| !value.is_empty()) else {
            continue;
        };
        let Some(jump_id) = ids_by_alias.get(&jump_alias.to_ascii_lowercase()) else {
            return Err(ImportError::UnknownJump {
                alias: host.alias.clone(),
                jump: jump_alias,
            });
        };
        host.jump_host = Some(*jump_id);
        if host.protocol == Protocol::Telnet {
            return Err(ImportError::InvalidHost {
                alias: host.alias.clone(),
            });
        }
    }

    let combined = existing_hosts
        .iter()
        .cloned()
        .chain(imported.iter().cloned())
        .collect::<Vec<_>>();
    for host in &imported {
        if host.protocol == Protocol::Ssh && resolve_ssh_chain(host, &combined).is_err() {
            return Err(ImportError::InvalidChain(host.alias.clone()));
        }
    }
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_hosts_with_fresh_ids_and_resolved_jump_aliases() {
        let bytes = br#"{
            "version": 1,
            "hosts": [
                {"alias":"jump","address":"10.0.0.1","username":"root","protocol":"ssh"},
                {"alias":"target","address":"10.0.0.2","username":"root","protocol":"ssh","jump_host":"jump"}
            ]
        }"#;
        let hosts = parse_template(bytes, &[]).unwrap();
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[1].jump_host, Some(hosts[0].id));
        assert!(!hosts[0].verified);
        assert!(hosts[0].host_fingerprint.is_none());
    }

    #[test]
    fn rejects_existing_aliases_without_partial_import() {
        let existing = HostProfile::new("server".to_owned());
        let bytes = br#"{"version":1,"hosts":[{"alias":"SERVER","address":"127.0.0.1","username":"root"}]}"#;
        assert!(matches!(
            parse_template(bytes, &[existing]),
            Err(ImportError::DuplicateAlias(_))
        ));
    }

    #[test]
    fn rejects_secret_fields() {
        let bytes = br#"{"version":1,"hosts":[{"alias":"server","address":"127.0.0.1","username":"root","password":"secret"}]}"#;
        assert!(matches!(
            parse_template(bytes, &[]),
            Err(ImportError::Json(_))
        ));
    }

    #[test]
    fn rejects_unknown_jump_aliases() {
        let bytes = br#"{"version":1,"hosts":[{"alias":"server","address":"127.0.0.1","username":"root","jump_host":"missing"}]}"#;
        assert!(matches!(
            parse_template(bytes, &[]),
            Err(ImportError::UnknownJump { .. })
        ));
    }
}
