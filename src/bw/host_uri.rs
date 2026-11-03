//! The one Login URI that turns a vault item into a host:
//! `ssh://[user@]host[:port]` or `telnet://host[:port]`.

use std::fmt;

/// Protocol named by the URI scheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UriScheme {
    /// `ssh://`, default port 22.
    Ssh,
    /// `telnet://`, default port 23.
    Telnet,
}

impl UriScheme {
    /// Port used when the URI names none.
    pub fn default_port(self) -> u16 {
        match self {
            Self::Ssh => 22,
            Self::Telnet => 23,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Ssh => "ssh",
            Self::Telnet => "telnet",
        }
    }
}

/// A parsed host URI. `username` is the optional user-info part; `port` is
/// `None` when the URI relies on the scheme default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostUri {
    pub scheme: UriScheme,
    pub username: Option<String>,
    pub host: String,
    pub port: Option<u16>,
}

impl HostUri {
    /// The port to connect to.
    pub fn port_or_default(&self) -> u16 {
        self.port.unwrap_or_else(|| self.scheme.default_port())
    }
}

impl fmt::Display for HostUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://", self.scheme.name())?;
        if let Some(user) = &self.username {
            write!(f, "{user}@")?;
        }
        if self.host.contains(':') {
            write!(f, "[{}]", self.host)?;
        } else {
            f.write_str(&self.host)?;
        }
        if let Some(port) = self.port {
            write!(f, ":{port}")?;
        }
        Ok(())
    }
}

/// Parses `ssh://` and `telnet://` URIs; anything else (other schemes, no
/// host, a bad port) is `None`. Only the authority part is read; a path,
/// query, or fragment is ignored.
pub fn parse_host_uri(text: &str) -> Option<HostUri> {
    let text = text.trim();
    let (scheme, rest) = text.split_once("://")?;
    let scheme = match scheme.to_ascii_lowercase().as_str() {
        "ssh" => UriScheme::Ssh,
        "telnet" => UriScheme::Telnet,
        _ => return None,
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .trim();
    let (username, host_port) = match authority.rsplit_once('@') {
        Some((user, host_port)) => {
            let user = user.trim();
            ((!user.is_empty()).then(|| user.to_owned()), host_port)
        }
        None => (None, authority),
    };
    let (host, port) = if let Some(bracketed) = host_port.strip_prefix('[') {
        let (host, after) = bracketed.split_once(']')?;
        let port = match after {
            "" => None,
            tail => Some(tail.strip_prefix(':')?),
        };
        (host, port)
    } else {
        match host_port.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => (host, Some(port)),
            _ => (host_port, None),
        }
    };
    let host = host.trim();
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return None;
    }
    let port = match port {
        None => None,
        Some(digits) => {
            let value: u16 = digits.parse().ok()?;
            Some((value != 0).then_some(value)?)
        }
    };
    Some(HostUri {
        scheme,
        username,
        host: host.to_owned(),
        port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_uris_parse_users_ports_and_ipv6_and_round_trip() {
        let uri = parse_host_uri(" SSH://deploy@10.0.0.5:2222/ignored?x#y ").unwrap();
        assert_eq!(uri.scheme, UriScheme::Ssh);
        assert_eq!(uri.username.as_deref(), Some("deploy"));
        assert_eq!(uri.host, "10.0.0.5");
        assert_eq!(uri.port, Some(2222));
        assert_eq!(uri.to_string(), "ssh://deploy@10.0.0.5:2222");

        let plain = parse_host_uri("telnet://switch.local").unwrap();
        assert_eq!(plain.scheme, UriScheme::Telnet);
        assert_eq!(plain.username, None);
        assert_eq!(plain.port, None);
        assert_eq!(plain.port_or_default(), 23);
        assert_eq!(plain.to_string(), "telnet://switch.local");

        let v6 = parse_host_uri("ssh://root@[fe80::1]:22").unwrap();
        assert_eq!(v6.host, "fe80::1");
        assert_eq!(v6.port, Some(22));
        assert_eq!(v6.to_string(), "ssh://root@[fe80::1]:22");
        assert_eq!(
            parse_host_uri("ssh://[fe80::1]").unwrap().port_or_default(),
            22
        );
    }

    #[test]
    fn other_schemes_and_malformed_authorities_are_not_host_uris() {
        for text in [
            "https://vault.example",
            "10.0.0.5",
            "ssh://",
            "ssh://user@",
            "ssh://host:0",
            "ssh://host:70000",
            "ssh://host:abc",
            "ssh://[fe80::1",
            "ssh://[fe80::1]x",
            "ssh://bad host",
            "sftp://host",
        ] {
            assert!(parse_host_uri(text).is_none(), "{text}");
        }
    }
}
