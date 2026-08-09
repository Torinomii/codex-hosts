use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

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
    password: Zeroizing<String>,
    key_passphrase: Zeroizing<String>,
}

pub struct ImportedHost {
    pub profile: HostProfile,
    pub password: Zeroizing<String>,
    pub key_passphrase: Zeroizing<String>,
}

pub struct ImportBatch {
    pub hosts: Vec<ImportedHost>,
    pub contains_sensitive_values: bool,
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
    b"\xEF\xBB\xBFalias,address,port,username,protocol,ssh_auth,private_key_path,jump_host,password,key_passphrase\r\nexample,server.example.com,22,operator,ssh,password,,,,\r\n".to_vec()
}

pub fn parse_template(
    bytes: &[u8],
    existing_hosts: &[HostProfile],
) -> Result<ImportBatch, ImportError> {
    let (bytes, delimiter) = strip_excel_prefix(bytes);
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .trim(csv::Trim::Headers)
        .from_reader(bytes);
    let headers = reader.headers()?.clone();
    let columns = ImportColumns::from_headers(&headers)?;

    let mut aliases = existing_hosts
        .iter()
        .map(|host| host.alias.trim().to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut imported = Vec::new();
    let mut jump_aliases = Vec::new();
    let mut contains_sensitive_values = false;

    for record in reader.records() {
        let record = record?;
        if record.iter().all(|value| value.trim().is_empty()) {
            continue;
        }
        let item = columns.read(&record)?;
        contains_sensitive_values |= !item.password.is_empty() || !item.key_passphrase.is_empty();
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
        imported.push(ImportedHost {
            profile,
            password: item.password,
            key_passphrase: item.key_passphrase,
        });
        jump_aliases.push(item.jump_host.map(|value| value.trim().to_owned()));
    }
    if imported.is_empty() {
        return Err(ImportError::Empty);
    }

    let ids_by_alias = existing_hosts
        .iter()
        .chain(imported.iter().map(|item| &item.profile))
        .map(|host| (host.alias.trim().to_ascii_lowercase(), host.id))
        .collect::<HashMap<_, _>>();
    for (item, jump_alias) in imported.iter_mut().zip(jump_aliases) {
        let host = &mut item.profile;
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
        .chain(imported.iter().map(|item| item.profile.clone()))
        .collect::<Vec<_>>();
    for item in &imported {
        let host = &item.profile;
        if host.protocol == Protocol::Ssh && resolve_ssh_chain(host, &combined).is_err() {
            return Err(ImportError::InvalidChain(host.alias.clone()));
        }
    }
    Ok(ImportBatch {
        hosts: imported,
        contains_sensitive_values,
    })
}

pub fn export_bytes(selected: &[HostProfile], all_hosts: &[HostProfile]) -> csv::Result<Vec<u8>> {
    let mut bytes = b"\xEF\xBB\xBF".to_vec();
    {
        let mut writer = csv::WriterBuilder::new()
            .terminator(csv::Terminator::CRLF)
            .from_writer(&mut bytes);
        writer.write_record([
            "alias",
            "address",
            "port",
            "username",
            "protocol",
            "ssh_auth",
            "private_key_path",
            "jump_host",
            "host_fingerprint",
            "verified",
        ])?;
        for host in selected {
            let jump_alias = host
                .jump_host
                .and_then(|id| all_hosts.iter().find(|candidate| candidate.id == id))
                .map(|host| host.alias.as_str())
                .unwrap_or_default();
            let alias = excel_safe_cell(&host.alias);
            let address = excel_safe_cell(&host.address);
            let username = excel_safe_cell(&host.username);
            let private_key_path = excel_safe_cell(&host.private_key_path);
            let jump_alias = excel_safe_cell(jump_alias);
            let host_fingerprint =
                excel_safe_cell(host.host_fingerprint.as_deref().unwrap_or_default());
            writer.write_record([
                alias.as_ref(),
                address.as_ref(),
                &host.port.to_string(),
                username.as_ref(),
                host.protocol.stable_name(),
                host.ssh_auth.stable_name(),
                private_key_path.as_ref(),
                jump_alias.as_ref(),
                host_fingerprint.as_ref(),
                if host.verified { "true" } else { "false" },
            ])?;
        }
        writer.flush()?;
    }
    Ok(bytes)
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
    password: Option<usize>,
    key_passphrase: Option<usize>,
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
                    | "password"
                    | "key_passphrase"
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
            password: known.get("password").copied(),
            key_passphrase: known.get("key_passphrase").copied(),
        })
    }

    fn read(&self, record: &csv::StringRecord) -> Result<ImportHost, ImportError> {
        let raw_value = |index: usize| record.get(index).unwrap_or_default();
        let value = |index: usize| decode_excel_safe_cell(raw_value(index).trim());
        let optional = |index: Option<usize>| index.map(&value).unwrap_or_default();
        let raw_optional = |index: Option<usize>| index.map(&raw_value).unwrap_or_default();
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
            password: Zeroizing::new(raw_optional(self.password).to_owned()),
            key_passphrase: Zeroizing::new(raw_optional(self.key_passphrase).to_owned()),
        })
    }
}

fn excel_safe_cell(value: &str) -> Cow<'_, str> {
    if value
        .as_bytes()
        .first()
        .is_some_and(|byte| matches!(*byte, b'=' | b'+' | b'-' | b'@' | b'\t' | b'\r'))
    {
        Cow::Owned(format!("'{value}"))
    } else {
        Cow::Borrowed(value)
    }
}

fn decode_excel_safe_cell(value: &str) -> &str {
    value
        .strip_prefix('\'')
        .filter(|rest| {
            rest.as_bytes()
                .first()
                .is_some_and(|byte| matches!(*byte, b'=' | b'+' | b'-' | b'@' | b'\t' | b'\r'))
        })
        .unwrap_or(value)
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
    fn built_in_template_is_importable_and_contains_no_credentials() {
        let batch = parse_template(&template_bytes(), &[]).unwrap();
        assert_eq!(batch.hosts.len(), 1);
        assert!(!batch.contains_sensitive_values);
        assert!(batch.hosts[0].password.is_empty());
        assert!(batch.hosts[0].key_passphrase.is_empty());
    }

    #[test]
    fn imports_hosts_with_fresh_ids_and_resolved_jump_aliases() {
        let bytes = b"alias,address,username,protocol,jump_host\njump,10.0.0.1,root,ssh,\ntarget,10.0.0.2,root,ssh,jump\n";
        let batch = parse_template(bytes, &[]).unwrap();
        let hosts = batch.hosts;
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[1].profile.jump_host, Some(hosts[0].profile.id));
        assert!(!hosts[0].profile.verified);
        assert!(hosts[0].profile.host_fingerprint.is_none());
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
    fn imports_credentials_and_ignores_unrecognized_columns() {
        let bytes = b"\xEF\xBB\xBFsep=;\r\nalias;address;username;notes;password;key_passphrase;ExcelGenerated\r\nserver;127.0.0.1;root;ignored;example-password;example-passphrase;ignored\r\n";
        let batch = parse_template(bytes, &[]).unwrap();
        assert!(batch.contains_sensitive_values);
        assert_eq!(batch.hosts.len(), 1);
        let host = &batch.hosts[0];
        assert_eq!(host.profile.alias, "server");
        assert_eq!(host.profile.address, "127.0.0.1");
        assert_eq!(host.profile.username, "root");
        assert_eq!(host.password.as_str(), "example-password");
        assert_eq!(host.key_passphrase.as_str(), "example-passphrase");
        assert!(host.profile.host_fingerprint.is_none());
        assert!(!host.profile.verified);
        let stored_profile = serde_json::to_string(&host.profile).unwrap();
        assert!(!stored_profile.contains("example-password"));
        assert!(!stored_profile.contains("example-passphrase"));
    }

    #[test]
    fn preserves_credential_whitespace_exactly() {
        let bytes = b"alias,address,username,password,key_passphrase\nserver,127.0.0.1,root,\" password \",\"  passphrase  \"\nspaces,127.0.0.2,root,\"   \",\n";
        let batch = parse_template(bytes, &[]).unwrap();
        assert!(batch.contains_sensitive_values);
        assert_eq!(batch.hosts[0].password.as_str(), " password ");
        assert_eq!(batch.hosts[0].key_passphrase.as_str(), "  passphrase  ");
        assert_eq!(batch.hosts[1].password.as_str(), "   ");
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
        let batch = parse_template(bytes, &[]).unwrap();
        assert_eq!(batch.hosts[0].profile.alias, "server, one");
        assert_eq!(batch.hosts[0].profile.port, 22);
        assert_eq!(batch.hosts[1].profile.port, 23);
    }

    #[test]
    fn requires_only_the_core_columns() {
        let bytes = b"alias,address\nserver,127.0.0.1\n";
        assert!(matches!(
            parse_template(bytes, &[]),
            Err(ImportError::MissingColumn("username"))
        ));
    }

    #[test]
    fn exports_all_profile_fields_without_credentials() {
        let jump = HostProfile {
            alias: "jump".to_owned(),
            address: "10.0.0.1".to_owned(),
            username: "root".to_owned(),
            verified: true,
            ..HostProfile::default()
        };
        let target = HostProfile {
            alias: "target".to_owned(),
            address: "10.0.0.2".to_owned(),
            username: "operator".to_owned(),
            ssh_auth: SshAuth::PrivateKey,
            private_key_path: r"C:\keys\target".to_owned(),
            host_fingerprint: Some("SHA256:example".to_owned()),
            jump_host: Some(jump.id),
            verified: true,
            ..HostProfile::default()
        };
        let all_hosts = vec![jump, target.clone()];
        let bytes = export_bytes(std::slice::from_ref(&target), &all_hosts).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with('\u{feff}'));
        assert!(text.contains("target,10.0.0.2,22,operator,ssh,private_key"));
        assert!(text.contains(r"C:\keys\target,jump,SHA256:example,true"));
        assert!(!text.contains("password,key_passphrase"));
    }

    #[test]
    fn export_prevents_spreadsheet_formulas_and_import_restores_the_value() {
        let host = HostProfile {
            alias: "=SUM(1,1)".to_owned(),
            address: "127.0.0.1".to_owned(),
            username: "root".to_owned(),
            ..HostProfile::default()
        };
        let bytes = export_bytes(std::slice::from_ref(&host), std::slice::from_ref(&host)).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains("\"'=SUM(1,1)\""));
        let imported = parse_template(&bytes, &[]).unwrap();
        assert_eq!(imported.hosts[0].profile.alias, "=SUM(1,1)");
    }
}
