//! Parsers for the small, non-item outputs of `bw`: the version string and
//! the raw session token. Item and status JSON live in [`crate::parse`] and [`crate::item`].

use std::fmt;
use std::str::FromStr;

use zeroize::Zeroizing;

use super::policy::SessionKey;

/// CLI versions whose login, unlock, stdin `create`/`edit`, and item shapes
/// were verified end to end. Writes are refused on any other version until it
/// is verified and added here (design: 未驗證前不允許寫入).
pub const VERIFIED_VERSIONS: &[&str] = &["2026.10.1"];

/// A `bw --version` value, `YEAR.MONTH.PATCH`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BwVersion {
    /// Release year.
    pub year: u16,
    /// Release month.
    pub month: u8,
    /// Patch number.
    pub patch: u16,
}

impl fmt::Display for BwVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.year, self.month, self.patch)
    }
}

impl FromStr for BwVersion {
    type Err = OutputError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut parts = text.trim().split('.');
        let year = parts.next().and_then(|part| part.parse().ok());
        let month = parts.next().and_then(|part| part.parse().ok());
        let patch = parts.next().and_then(|part| part.parse().ok());
        match (year, month, patch, parts.next()) {
            (Some(year), Some(month), Some(patch), None) if (1..=12).contains(&month) => {
                Ok(Self { year, month, patch })
            }
            _ => Err(OutputError::BadVersion),
        }
    }
}

/// Whether a CLI version is on the verified list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionSupport {
    /// Verified end to end; reads and writes allowed.
    Verified,
    /// Not verified; diagnostics and reads only, no vault writes.
    Unverified,
}

/// Classifies `version` against [`VERIFIED_VERSIONS`].
pub fn classify_version(version: &BwVersion) -> VersionSupport {
    let text = version.to_string();
    if VERIFIED_VERSIONS.contains(&text.as_str()) {
        VersionSupport::Verified
    } else {
        VersionSupport::Unverified
    }
}

/// Why a small `bw` output could not be used. Messages never echo the output.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OutputError {
    /// The output is not UTF-8.
    #[error("bw output is not UTF-8")]
    NotUtf8,
    /// The output is empty or whitespace.
    #[error("bw output is empty")]
    Empty,
    /// The output is not `YEAR.MONTH.PATCH`.
    #[error("bw output is not a version number")]
    BadVersion,
    /// The `--raw` output is not a single session token.
    #[error("bw --raw output is not a single session token")]
    BadSession,
}

/// Parses `bw --version` stdout.
pub fn parse_version_output(stdout: &[u8]) -> Result<BwVersion, OutputError> {
    let text = std::str::from_utf8(stdout).map_err(|_| OutputError::NotUtf8)?;
    if text.trim().is_empty() {
        return Err(OutputError::Empty);
    }
    text.parse()
}

/// Parses the session token printed by `bw unlock --raw` or `bw login --raw`.
/// The buffer and every intermediate copy are zeroized; the token itself only
/// survives inside the returned [`SessionKey`].
pub fn parse_raw_session(stdout: Zeroizing<Vec<u8>>) -> Result<SessionKey, OutputError> {
    let text =
        Zeroizing::new(String::from_utf8(stdout.to_vec()).map_err(|_| OutputError::NotUtf8)?);
    let token = text.trim();
    if token.is_empty() {
        return Err(OutputError::Empty);
    }
    let single_printable_token = token.chars().all(|character| character.is_ascii_graphic());
    if !single_printable_token {
        return Err(OutputError::BadSession);
    }
    Ok(SessionKey::new(token.to_owned()))
}

/// What a non-zero `bw` exit meant, classified from stderr without exposing
/// it. Only a handful of known phrases are recognised; everything else is
/// [`CliFailure::Other`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliFailure {
    /// `You are not logged in.`
    NotLoggedIn,
    /// `Vault is locked.` — the session key is invalid or expired.
    Locked,
    /// `Not found.`
    NotFound,
    /// Wrong master password or credentials.
    InvalidCredentials,
    /// Two-step login or new-device verification needs a code the CLI only
    /// takes from a terminal (`Code is required.`, or no usable provider).
    VerificationRequired,
    /// The server rejected the two-step or new-device code that was entered.
    InvalidCode,
    /// The CLI could not reach the server.
    Network,
    /// Anything else.
    Other,
}

pub fn classify_failure(stderr: &[u8]) -> CliFailure {
    let text = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| text.contains(needle));
    if text.contains("not logged in") {
        CliFailure::NotLoggedIn
    } else if text.contains("vault is locked")
        || (text.contains("session") && text.contains("invalid"))
    {
        CliFailure::Locked
    } else if has(&[
        "new device otp",
        "token is invalid",
        "invalid otp",
        "otp is invalid",
        "invalid code",
        "code is invalid",
    ]) {
        CliFailure::InvalidCode
    } else if has(&[
        "code is required",
        "no providers available",
        "two-step login",
        "device verification",
        "verification code",
    ]) {
        CliFailure::VerificationRequired
    } else if has(&[
        "fetch failed",
        "failed to fetch",
        "enotfound",
        "econnrefused",
        "econnreset",
        "etimedout",
        "getaddrinfo",
        "network error",
        "certificate",
        "socket hang up",
    ]) {
        CliFailure::Network
    } else if text.contains("not found") {
        CliFailure::NotFound
    } else if text.contains("invalid master password")
        || text.contains("username or password is incorrect")
        || text.contains("incorrect")
        || text.contains("email address is")
        || text.contains("master password is required")
    {
        CliFailure::InvalidCredentials
    } else {
        CliFailure::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parses_and_classifies() {
        let version = parse_version_output(b"2026.10.1\n").unwrap();
        assert_eq!(
            version,
            BwVersion {
                year: 2026,
                month: 10,
                patch: 1
            }
        );
        assert_eq!(version.to_string(), "2026.10.1");
        assert_eq!(classify_version(&version), VersionSupport::Verified);
        let newer: BwVersion = "2026.11.0".parse().unwrap();
        assert!(newer > version);
        assert_eq!(classify_version(&newer), VersionSupport::Unverified);
    }

    #[test]
    fn bad_versions_are_rejected_without_echoing_them() {
        assert_eq!(parse_version_output(b""), Err(OutputError::Empty));
        assert_eq!(parse_version_output(b"   \n"), Err(OutputError::Empty));
        assert_eq!(
            parse_version_output(b"v2026.10"),
            Err(OutputError::BadVersion)
        );
        assert_eq!(
            parse_version_output(b"2026.13.1"),
            Err(OutputError::BadVersion)
        );
        assert_eq!(
            parse_version_output(b"2026.10.1.2"),
            Err(OutputError::BadVersion)
        );
        assert_eq!(parse_version_output(&[0xff]), Err(OutputError::NotUtf8));
        assert!(!OutputError::BadVersion.to_string().contains("2026"));
    }

    #[test]
    fn raw_session_must_be_one_printable_token() {
        let ok = parse_raw_session(Zeroizing::new(b"abc123+/=\r\n".to_vec()));
        assert!(ok.is_ok());
        assert!(matches!(
            parse_raw_session(Zeroizing::new(b"\n".to_vec())),
            Err(OutputError::Empty)
        ));
        assert!(matches!(
            parse_raw_session(Zeroizing::new(b"two tokens".to_vec())),
            Err(OutputError::BadSession)
        ));
        assert!(matches!(
            parse_raw_session(Zeroizing::new(b"? Master password: [hidden]".to_vec())),
            Err(OutputError::BadSession)
        ));
        assert!(matches!(
            parse_raw_session(Zeroizing::new(vec![0xff, 0xfe])),
            Err(OutputError::NotUtf8)
        ));
    }

    #[test]
    fn stderr_classification_recognises_known_phrases_only() {
        assert_eq!(
            classify_failure(b"You are not logged in."),
            CliFailure::NotLoggedIn
        );
        assert_eq!(classify_failure(b"Vault is locked."), CliFailure::Locked);
        assert_eq!(classify_failure(b"Not found."), CliFailure::NotFound);
        assert_eq!(
            classify_failure(b"Invalid master password."),
            CliFailure::InvalidCredentials
        );
        assert_eq!(
            classify_failure(b"Email address is required."),
            CliFailure::InvalidCredentials
        );
        assert_eq!(
            classify_failure(b"Code is required."),
            CliFailure::VerificationRequired
        );
        assert_eq!(
            classify_failure(b"No providers available for this client."),
            CliFailure::VerificationRequired
        );
        assert_eq!(
            classify_failure(b"TypeError: fetch failed"),
            CliFailure::Network
        );
        assert_eq!(
            classify_failure(b"Error: getaddrinfo ENOTFOUND vault.example"),
            CliFailure::Network,
            "a DNS failure is not a missing item"
        );
        assert_eq!(
            classify_failure(b"Two-step token is invalid. Try again."),
            CliFailure::InvalidCode
        );
        assert_eq!(
            classify_failure(b"invalid new device otp"),
            CliFailure::InvalidCode
        );
        assert_eq!(classify_failure(b"something else"), CliFailure::Other);
    }
}
