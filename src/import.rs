use std::collections::{HashMap, HashSet};

use thiserror::Error;
use uuid::Uuid;

use crate::model::{HostProfile, Protocol, SshAuth, resolve_ssh_chain};

#[derive(Debug)]
struct ImportHost {
    alias: String,
    address: String,
    port: Option<u16>,
    username: String,
    protocol: Protocol,
    ssh_auth: SshAuth,
    private_key_path: String,
    jump_host: Option<String>,
}

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("the template CSV is invalid: {0}")]
    Csv(#[from] csv::Error),
    #[error("the template is missing the required column: {0}")]
    MissingColumn(&'static str),
    #[error("the template contains the column more than once: {0}")]
    DuplicateColumn(String),
    #[error("host {alias} has an invalid port: {value}")]
    InvalidPort { alias: String, value: String },
    #[error("host {alias} has an invalid protocol: {value}")]
    InvalidProtocol { alias: String, value: String },
    #[error("host {alias} has an invalid SSH authentication method: {value}")]
    InvalidSshAuth { alias: String, value: String },
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
    b"\xEF\xBB\xBFalias,address,port,username,protocol,ssh_auth,private_key_path,jump_host\r\nexample,server.example.com,22,operator,ssh,password,,\r\n".to_vec()
}

pub fn parse_template(
    bytes: &[u8],
    existing_hosts: &[HostProfile],
) -> Result<Vec<HostProfile>, ImportError> {
    let (bytes, delimiter) = strip_excel_prefix(bytes);
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(bytes);
    let headers = reader.headers()?.clone();
    let columns = ImportColumns::from_headers(&headers)?;

    let mut aliases = existing_hosts
        .iter()
        .map(|host| host.alias.trim().to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut imported = Vec::new();
    let mut jump_aliases = Vec::new();

    for record in reader.records() {
        let record = record?;
        if record.iter().all(|value| value.trim().is_empty()) {
            continue;
        }
        let item = columns.read(&record)?;
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
    if imported.is_empty() {
        return Err(ImportError::Empty);
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

#[derive(Debug)]
struct ImportColumns {
    alias: usize,
    address: usize,
    port: Option<usize>,
    username: usize,
    protocol: Option<usize>,
    ssh_auth: Option<usize>,
    private_key_path: Option<usize>,
    jump_host: Option<usize>,
}

impl ImportColumns {
    fn from_headers(headers: &csv::StringRecord) -> Result<Self, ImportError> {
        let mut known = HashMap::new();
        for (index, header) in headers.iter().enumerate() {
            let name = header.trim().to_ascii_lowercase();
            if !matches!(
                name.as_str(),
                "alias"
                    | "address"
                    | "port"
                    | "username"
                    | "protocol"
                    | "ssh_auth"
                    | "private_key_path"
                    | "jump_host"
            ) {
                continue;
            }
            if known.insert(name.clone(), index).is_some() {
                return Err(ImportError::DuplicateColumn(name));
            }
        }
        let required = |name: &'static str| {
            known
                .get(name)
                .copied()
                .ok_or(ImportError::MissingColumn(name))
        };
        Ok(Self {
            alias: required("alias")?,
            address: required("address")?,
            port: known.get("port").copied(),
            username: required("username")?,
            protocol: known.get("protocol").copied(),
            ssh_auth: known.get("ssh_auth").copied(),
            private_key_path: known.get("private_key_path").copied(),
            jump_host: known.get("jump_host").copied(),
        })
    }

    fn read(&self, record: &csv::StringRecord) -> Result<ImportHost, ImportError> {
        let value = |index: usize| record.get(index).unwrap_or_default().trim();
        let optional = |index: Option<usize>| index.map(&value).unwrap_or_default();
        let alias = value(self.alias).to_owned();
        let protocol_text = optional(self.protocol);
        let protocol = match protocol_text.to_ascii_lowercase().as_str() {
            "" | "ssh" => Protocol::Ssh,
            "telnet" => Protocol::Telnet,
            _ => {
                return Err(ImportError::InvalidProtocol {
                    alias,
                    value: protocol_text.to_owned(),
                });
            }
        };
        let ssh_auth_text = optional(self.ssh_auth);
        let ssh_auth = match ssh_auth_text.to_ascii_lowercase().as_str() {
            "" | "password" => SshAuth::Password,
            "private_key" | "private-key" => SshAuth::PrivateKey,
            _ => {
                return Err(ImportError::InvalidSshAuth {
                    alias,
                    value: ssh_auth_text.to_owned(),
                });
            }
        };
        let port_text = optional(self.port);
        let port = if port_text.is_empty() {
            None
        } else {
            Some(
                port_text
                    .parse::<u16>()
                    .map_err(|_| ImportError::InvalidPort {
                        alias: alias.clone(),
                        value: port_text.to_owned(),
                    })?,
            )
        };
        Ok(ImportHost {
            alias,
            address: value(self.address).to_owned(),
            port,
            username: value(self.username).to_owned(),
            protocol,
            ssh_auth,
            private_key_path: optional(self.private_key_path).to_owned(),
            jump_host: self
                .jump_host
                .map(&value)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        })
    }
}

fn strip_excel_prefix(bytes: &[u8]) -> (&[u8], u8) {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let Some(line_end) = bytes.iter().position(|byte| *byte == b'\n') else {
        return (bytes, b',');
    };
    let first_line = bytes[..line_end]
        .strip_suffix(b"\r")
        .unwrap_or(&bytes[..line_end]);
    if first_line.len() == 5 && first_line[..4].eq_ignore_ascii_case(b"sep=") {
        let delimiter = first_line[4];
        if !matches!(delimiter, b'\r' | b'\n' | b'"') {
            return (&bytes[line_end + 1..], delimiter);
        }
    }
    (bytes, b',')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_hosts_with_fresh_ids_and_resolved_jump_aliases() {
        let bytes = b"alias,address,username,protocol,jump_host\njump,10.0.0.1,root,ssh,\ntarget,10.0.0.2,root,ssh,jump\n";
        let hosts = parse_template(bytes, &[]).unwrap();
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[1].jump_host, Some(hosts[0].id));
        assert!(!hosts[0].verified);
        assert!(hosts[0].host_fingerprint.is_none());
    }

    #[test]
    fn rejects_existing_aliases_without_partial_import() {
        let existing = HostProfile::new("server".to_owned());
        let bytes = b"alias,address,username\nSERVER,127.0.0.1,root\n";
        assert!(matches!(
            parse_template(bytes, &[existing]),
            Err(ImportError::DuplicateAlias(_))
        ));
    }

    #[test]
    fn ignores_excel_prefix_and_unrecognized_columns() {
        let bytes = b"\xEF\xBB\xBFsep=;\r\nalias;address;username;notes;password;ExcelGenerated\r\nserver;127.0.0.1;root;keep this secret;not imported;ignored\r\n";
        let hosts = parse_template(bytes, &[]).unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].alias, "server");
        assert_eq!(hosts[0].address, "127.0.0.1");
        assert_eq!(hosts[0].username, "root");
        assert!(hosts[0].host_fingerprint.is_none());
        assert!(!hosts[0].verified);
    }

    #[test]
    fn rejects_unknown_jump_aliases() {
        let bytes = b"alias,address,username,jump_host\nserver,127.0.0.1,root,missing\n";
        assert!(matches!(
            parse_template(bytes, &[]),
            Err(ImportError::UnknownJump { .. })
        ));
    }

    #[test]
    fn supports_quoted_values_and_protocol_default_ports() {
        let bytes = b"alias,address,username,protocol\n\"server, one\",127.0.0.1,root,ssh\ntelnet,127.0.0.2,operator,telnet\n";
        let hosts = parse_template(bytes, &[]).unwrap();
        assert_eq!(hosts[0].alias, "server, one");
        assert_eq!(hosts[0].port, 22);
        assert_eq!(hosts[1].port, 23);
    }

    #[test]
    fn requires_only_the_core_columns() {
        let bytes = b"alias,address\nserver,127.0.0.1\n";
        assert!(matches!(
            parse_template(bytes, &[]),
            Err(ImportError::MissingColumn("username"))
        ));
    }
}
