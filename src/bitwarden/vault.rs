//! Vault operations over `bw.exe` with a session key. A [`Vault`] is built
//! for one operation from the store's settings and the stored session; it
//! never outlives the call that made it, and every error it returns is a
//! classification, never CLI output.

use std::future::Future;

use codex_hosts_bw::{
    BwExecutable, BwInvocation, BwOutput, BwProfile, BwRunError, BwVersion, CliFailure, CodePrompt,
    ItemKind, ItemSummary, LoginError, LoginOutcome, MasterPassword, OutputError, SessionKey,
    StatusReport, TwoFactorMethod, VaultFolder, VaultItem, VaultParseError, VersionSupport,
    classify_failure, classify_version, encode_item_json, folder_json, interactive, parse_folder,
    parse_folders, parse_item_list, parse_raw_session, parse_status, parse_version_output,
};
use tokio::sync::mpsc::UnboundedReceiver;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::ssh::RemoteFailure;

/// Versions already read from an executable in this process, by path.
static KNOWN_VERSIONS: std::sync::Mutex<Vec<(std::path::PathBuf, (BwVersion, VersionSupport))>> =
    std::sync::Mutex::new(Vec::new());

fn known_version(exe: &std::path::Path) -> Option<(BwVersion, VersionSupport)> {
    // The scripted tests answer every `--version` themselves.
    if cfg!(test) {
        return None;
    }
    let known = KNOWN_VERSIONS.lock().ok()?;
    known
        .iter()
        .find(|(path, _)| path == exe)
        .map(|(_, version)| *version)
}

fn remember_version(exe: &std::path::Path, version: (BwVersion, VersionSupport)) {
    if let Ok(mut known) = KNOWN_VERSIONS.lock()
        && !known.iter().any(|(path, _)| path == exe)
    {
        known.push((exe.to_path_buf(), version));
    }
}

/// Runs one `bw` invocation; tests script answers instead of spawning.
pub(crate) trait CliRunner: Send + Sync {
    fn run(
        &self,
        invocation: BwInvocation,
    ) -> impl Future<Output = Result<BwOutput, BwRunError>> + Send;
}

/// The real CLI.
#[derive(Clone, Copy, Default)]
pub(crate) struct RealRunner;

impl CliRunner for RealRunner {
    fn run(
        &self,
        invocation: BwInvocation,
    ) -> impl Future<Output = Result<BwOutput, BwRunError>> + Send {
        codex_hosts_bw::run(invocation)
    }
}

/// Why a vault operation failed. Never carries CLI output or secrets.
#[derive(Debug, thiserror::Error)]
pub(crate) enum VaultError {
    /// The session key is missing or `bw` no longer accepts it.
    #[error("the Bitwarden vault is locked")]
    Locked,
    /// Not logged in.
    #[error("not logged in to Bitwarden")]
    SignedOut,
    /// Writes are refused on a CLI version that is not on the verified list.
    #[error("bw version is not verified for writes")]
    VersionUnverified,
    /// The cache is stale and the sync that would refresh it failed.
    #[error("Bitwarden sync required")]
    SyncRequired,
    /// The vault item does not exist.
    #[error("Bitwarden item missing")]
    ItemMissing,
    /// The vault item is in the trash.
    #[error("Bitwarden item is in the trash")]
    ItemDeleted,
    /// The item is not a Login item.
    #[error("Bitwarden item is not a Login item")]
    ItemKindMismatch,
    /// The item's username differs from the host's account.
    #[error("Bitwarden item username does not match the host account")]
    UsernameMismatch,
    /// The Login item has no password.
    #[error("Bitwarden item has no password")]
    PasswordMissing,
    /// Two-step login or new-device verification needs a code.
    #[error("Bitwarden needs a verification code")]
    VerificationRequired,
    /// The server rejected the verification code that was entered.
    #[error("Bitwarden rejected the verification code")]
    InvalidCode,
    /// `bw` could not reach the server.
    #[error("Bitwarden server unreachable")]
    Network,
    /// Wrong e-mail or master password.
    #[error("Bitwarden rejected the credentials")]
    InvalidCredentials,
    /// `bw` exited non-zero for another reason.
    #[error("bw failed (exit {exit_code:?})")]
    Cli {
        /// Exit code.
        exit_code: Option<i32>,
    },
    /// The process could not be run.
    #[error(transparent)]
    Run(#[from] BwRunError),
    /// `--version` or the raw session could not be read.
    #[error(transparent)]
    Output(#[from] OutputError),
    /// JSON output was not in the expected shape.
    #[error(transparent)]
    Parse(#[from] VaultParseError),
}

impl VaultError {
    /// The stable code a tool result carries.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Locked => "BITWARDEN_LOCKED",
            Self::SignedOut => "BITWARDEN_SIGNED_OUT",
            Self::VersionUnverified => "BITWARDEN_CLI_UNSUPPORTED",
            Self::SyncRequired => "BITWARDEN_SYNC_REQUIRED",
            Self::ItemMissing | Self::ItemDeleted | Self::PasswordMissing => {
                "BITWARDEN_ITEM_MISSING"
            }
            Self::ItemKindMismatch | Self::UsernameMismatch => "BITWARDEN_ITEM_MISMATCH",
            Self::VerificationRequired | Self::InvalidCode => "BITWARDEN_VERIFICATION_REQUIRED",
            Self::Network => "BITWARDEN_UNREACHABLE",
            Self::InvalidCredentials => "BITWARDEN_INVALID_CREDENTIALS",
            Self::Cli { .. } | Self::Run(_) | Self::Output(_) | Self::Parse(_) => {
                "BITWARDEN_UNAVAILABLE"
            }
        }
    }

    /// The failure as a tool call reports it, with the step the user takes
    /// in the codex-hosts window where there is one.
    pub(crate) fn into_remote(self, alias: &str) -> RemoteFailure {
        let hint = match &self {
            Self::Locked => {
                "Unlock Bitwarden in the codex-hosts window (Credential services), then retry."
            }
            Self::SignedOut => {
                "Log in to Bitwarden again under Credential services in the codex-hosts window."
            }
            Self::SyncRequired => {
                "Sync the vault under Credential services in the codex-hosts window, then retry."
            }
            Self::ItemMissing
            | Self::ItemDeleted
            | Self::PasswordMissing
            | Self::ItemKindMismatch
            | Self::UsernameMismatch => {
                "The host's Bitwarden item no longer matches; open_host_editor for this alias lets the user bind it again."
            }
            Self::VersionUnverified => {
                "The bw CLI is not on the verified list; update codex-hosts or the CLI."
            }
            Self::Network => "bw could not reach the Bitwarden server; check the network or proxy.",
            _ => "",
        };
        RemoteFailure::new(
            self.code(),
            format!("Bitwarden could not deliver the login for {alias}: {self}. {hint}")
                .trim_end()
                .to_owned(),
        )
    }
}

/// Where an interactive login stopped without an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoginStep {
    /// The vault now holds the new session.
    LoggedIn,
    /// The account has several two-step methods; log in again with one.
    MethodRequired,
    /// `bw` asked for something the window cannot answer.
    UnexpectedPrompt,
    /// The window withdrew.
    Cancelled,
}

/// A password read from a Login item.
pub(crate) struct ResolvedPassword {
    pub(crate) password: Zeroizing<String>,
}

/// One account's vault, for the duration of one operation.
pub(crate) struct Vault<R: CliRunner = RealRunner> {
    runner: R,
    exe: BwExecutable,
    profile: BwProfile,
    session: Option<SessionKey>,
    version: Option<(BwVersion, VersionSupport)>,
}

impl<R: CliRunner> Vault<R> {
    pub(crate) fn new(
        runner: R,
        exe: BwExecutable,
        profile: BwProfile,
        session: Option<SessionKey>,
    ) -> Self {
        Self {
            runner,
            exe,
            profile,
            session,
            version: None,
        }
    }

    pub(crate) fn session(&self) -> Option<&SessionKey> {
        self.session.as_ref()
    }

    pub(crate) fn take_session(&mut self) -> Option<SessionKey> {
        self.session.take()
    }

    /// Runs an invocation and turns a non-zero exit into a classified error.
    async fn run_ok(&mut self, invocation: BwInvocation) -> Result<BwOutput, VaultError> {
        let output = self.runner.run(invocation).await?;
        if output.exit_code == Some(0) {
            return Ok(output);
        }
        Err(self.failure(classify_failure(&output.stderr), output.exit_code))
    }

    /// The vault error for a classified non-zero exit; a rejected session is
    /// dropped.
    fn failure(&mut self, failure: CliFailure, exit_code: Option<i32>) -> VaultError {
        match failure {
            CliFailure::NotLoggedIn => {
                self.session = None;
                VaultError::SignedOut
            }
            CliFailure::Locked => {
                self.session = None;
                VaultError::Locked
            }
            CliFailure::NotFound => VaultError::ItemMissing,
            CliFailure::InvalidCredentials => VaultError::InvalidCredentials,
            CliFailure::VerificationRequired => VaultError::VerificationRequired,
            CliFailure::InvalidCode => VaultError::InvalidCode,
            CliFailure::Network => VaultError::Network,
            CliFailure::Other => VaultError::Cli { exit_code },
        }
    }

    pub(crate) async fn version(&mut self) -> Result<(BwVersion, VersionSupport), VaultError> {
        if let Some(version) = &self.version {
            return Ok(*version);
        }
        // Every `bw` start costs about a second; the version of one
        // executable does not change while the process runs.
        if let Some(version) = known_version(self.exe.path()) {
            self.version = Some(version);
            return Ok(version);
        }
        let output = self
            .run_ok(BwInvocation::version(&self.exe, &self.profile))
            .await?;
        let version = parse_version_output(&output.stdout)?;
        let support = classify_version(&version);
        self.version = Some((version, support));
        remember_version(self.exe.path(), (version, support));
        Ok((version, support))
    }

    async fn require_verified_for_write(&mut self) -> Result<(), VaultError> {
        match self.version().await?.1 {
            VersionSupport::Verified => Ok(()),
            VersionSupport::Unverified => Err(VaultError::VersionUnverified),
        }
    }

    fn require_session(&self) -> Result<&SessionKey, VaultError> {
        self.session.as_ref().ok_or(VaultError::Locked)
    }

    pub(crate) async fn status(&mut self) -> Result<StatusReport, VaultError> {
        let output = self
            .run_ok(BwInvocation::status(
                &self.exe,
                &self.profile,
                self.session.as_ref(),
            ))
            .await?;
        Ok(parse_status(&output.stdout)?)
    }

    pub(crate) async fn config_server(&mut self, url: &str) -> Result<(), VaultError> {
        self.run_ok(BwInvocation::config_server(&self.exe, &self.profile, url))
            .await
            .map(drop)
    }

    /// `bw login` through bw's own prompts: each code prompt goes to
    /// `on_prompt` and the next string on `answers` is typed at it; closing
    /// `answers` cancels. On success the printed key becomes this vault's
    /// session.
    pub(crate) async fn login(
        &mut self,
        email: &str,
        password: &MasterPassword,
        method: Option<TwoFactorMethod>,
        on_prompt: impl FnMut(CodePrompt),
        answers: &mut UnboundedReceiver<Zeroizing<String>>,
    ) -> Result<LoginStep, VaultError> {
        self.version().await?;
        let invocation =
            BwInvocation::interactive_login(&self.exe, &self.profile, email, password, method);
        let outcome = interactive::login(invocation, on_prompt, answers)
            .await
            .map_err(|error| match error {
                LoginError::Run(error) => VaultError::Run(error),
                LoginError::Output(error) => VaultError::Output(error),
            })?;
        Ok(match outcome {
            LoginOutcome::LoggedIn(key) => {
                self.session = Some(key);
                LoginStep::LoggedIn
            }
            LoginOutcome::MethodRequired => LoginStep::MethodRequired,
            LoginOutcome::UnexpectedPrompt => LoginStep::UnexpectedPrompt,
            LoginOutcome::Cancelled => LoginStep::Cancelled,
            LoginOutcome::Failed(failure) => return Err(self.failure(failure, Some(1))),
        })
    }

    pub(crate) async fn unlock(&mut self, password: &MasterPassword) -> Result<(), VaultError> {
        self.version().await?;
        let output = self
            .run_ok(BwInvocation::unlock(&self.exe, &self.profile, password))
            .await?;
        self.session = Some(parse_raw_session(output.stdout)?);
        Ok(())
    }

    /// Best effort: the session is dropped even if `bw lock` fails.
    pub(crate) async fn lock(&mut self) {
        if let Some(session) = self.session.take() {
            let _ = self
                .runner
                .run(BwInvocation::lock(&self.exe, &self.profile, &session))
                .await;
        }
    }

    pub(crate) async fn logout(&mut self) -> Result<(), VaultError> {
        let session = self.session.take();
        self.run_ok(BwInvocation::logout(
            &self.exe,
            &self.profile,
            session.as_ref(),
        ))
        .await
        .map(drop)
    }

    pub(crate) async fn sync(&mut self) -> Result<(), VaultError> {
        let invocation = {
            let session = self.require_session()?;
            BwInvocation::sync(&self.exe, &self.profile, session)
        };
        self.run_ok(invocation).await.map(drop)
    }

    pub(crate) async fn list_folders(&mut self) -> Result<Vec<VaultFolder>, VaultError> {
        let invocation = {
            let session = self.require_session()?;
            BwInvocation::list_folders(&self.exe, &self.profile, session)
        };
        let output = self.run_ok(invocation).await?;
        Ok(parse_folders(&output.stdout)?)
    }

    pub(crate) async fn create_folder(&mut self, name: &str) -> Result<VaultFolder, VaultError> {
        self.require_verified_for_write().await?;
        let invocation = {
            let session = self.require_session()?;
            BwInvocation::create_folder(
                &self.exe,
                &self.profile,
                session,
                encode_item_json(&folder_json(name)),
            )
        };
        let output = self.run_ok(invocation).await?;
        Ok(parse_folder(&output.stdout)?)
    }

    pub(crate) async fn list_items(
        &mut self,
        folder: Option<Uuid>,
    ) -> Result<Vec<ItemSummary>, VaultError> {
        let invocation = {
            let session = self.require_session()?;
            BwInvocation::list_items(&self.exe, &self.profile, session, folder)
        };
        let output = self.run_ok(invocation).await?;
        Ok(parse_item_list(&output.stdout)?)
    }

    pub(crate) async fn get_item(&mut self, id: Uuid) -> Result<VaultItem, VaultError> {
        let invocation = {
            let session = self.require_session()?;
            BwInvocation::get_item(&self.exe, &self.profile, session, id)
        };
        let output = self.run_ok(invocation).await?;
        Ok(VaultItem::parse(output.stdout)?)
    }

    /// One CLI read for a connection-test snapshot, retaining full items in
    /// zeroizing buffers rather than discarding their login fields.
    pub(crate) async fn read_items(&mut self, folder: Uuid) -> Result<Vec<VaultItem>, VaultError> {
        let session = self.require_session()?;
        let invocation = BwInvocation::list_items(&self.exe, &self.profile, session, Some(folder));
        let output = self.run_ok(invocation).await?;
        let items: Vec<&serde_json::value::RawValue> = serde_json::from_slice(&output.stdout)
            .map_err(|_| VaultParseError::UnexpectedShape("an item array"))?;
        items
            .into_iter()
            .map(|item| {
                VaultItem::parse(Zeroizing::new(item.get().as_bytes().to_vec()))
                    .map_err(VaultError::from)
            })
            .collect()
    }

    pub(crate) async fn create_item(
        &mut self,
        json: Zeroizing<Vec<u8>>,
    ) -> Result<VaultItem, VaultError> {
        self.require_verified_for_write().await?;
        let invocation = {
            let session = self.require_session()?;
            BwInvocation::create_item(&self.exe, &self.profile, session, encode_item_json(&json))
        };
        let output = self.run_ok(invocation).await?;
        Ok(VaultItem::parse(output.stdout)?)
    }

    pub(crate) async fn edit_item(
        &mut self,
        id: Uuid,
        json: Zeroizing<Vec<u8>>,
    ) -> Result<VaultItem, VaultError> {
        self.require_verified_for_write().await?;
        let invocation = {
            let session = self.require_session()?;
            BwInvocation::edit_item(
                &self.exe,
                &self.profile,
                session,
                id,
                encode_item_json(&json),
            )
        };
        let output = self.run_ok(invocation).await?;
        Ok(VaultItem::parse(output.stdout)?)
    }

    pub(crate) async fn delete_item(&mut self, id: Uuid) -> Result<(), VaultError> {
        self.require_verified_for_write().await?;
        let invocation = {
            let session = self.require_session()?;
            BwInvocation::delete_item(&self.exe, &self.profile, session, id)
        };
        self.run_ok(invocation).await.map(drop)
    }

    pub(crate) async fn restore_item(&mut self, id: Uuid) -> Result<(), VaultError> {
        self.require_verified_for_write().await?;
        let invocation = {
            let session = self.require_session()?;
            BwInvocation::restore_item(&self.exe, &self.profile, session, id)
        };
        self.run_ok(invocation).await.map(drop)
    }

    /// Reads one Login item's password, refusing trashed items, other kinds,
    /// and an account name that no longer matches the host.
    pub(crate) async fn resolve_password(
        &mut self,
        item: Uuid,
        expected_username: Option<&str>,
    ) -> Result<ResolvedPassword, VaultError> {
        let item = self.get_item(item).await?;
        if item.is_deleted() {
            return Err(VaultError::ItemDeleted);
        }
        item.expect_kind(ItemKind::Login)
            .map_err(|_| VaultError::ItemKindMismatch)?;
        let summary = item.summary();
        if let Some(expected) = expected_username
            && summary.username.as_deref() != Some(expected)
        {
            return Err(VaultError::UsernameMismatch);
        }
        let password = item.login_password()?.ok_or(VaultError::PasswordMissing)?;
        Ok(ResolvedPassword { password })
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::{Debug, Display};

    use super::*;
    use crate::bitwarden::test_support::{Scripted, stub_bw};

    static_assertions::assert_not_impl_any!(ResolvedPassword: Debug, Display, Clone);

    const LOGIN_JSON: &str = r#"{"object":"item","id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","type":1,"name":"host","login":{"username":"deploy","password":"pw-1"},"revisionDate":"2026-11-02T01:00:00.000Z","deletedDate":null}"#;

    fn vault(script: Scripted, session: Option<&str>) -> Vault<Scripted> {
        let (exe, profile) = stub_bw("vault");
        Vault::new(
            script,
            exe,
            profile,
            session.map(|key| SessionKey::new(key.to_owned())),
        )
    }

    #[tokio::test]
    async fn resolve_checks_the_account() {
        let script = Scripted::new(vec![
            (vec!["get", "item"], Some(0), LOGIN_JSON, ""),
            (vec!["get", "item"], Some(0), LOGIN_JSON, ""),
        ]);
        let mut vault = vault(script, Some("SESSION-KEY"));

        let item = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let resolved = vault.resolve_password(item, Some("deploy")).await.unwrap();
        assert_eq!(resolved.password.as_str(), "pw-1");
        let mismatch = vault.resolve_password(item, Some("root")).await;
        assert!(matches!(mismatch, Err(VaultError::UsernameMismatch)));
        assert_eq!(
            VaultError::UsernameMismatch.into_remote("box").code,
            "BITWARDEN_ITEM_MISMATCH"
        );
    }

    #[tokio::test]
    async fn a_rejected_session_locks_and_writes_need_a_verified_cli() {
        let script = Scripted::new(vec![
            (vec!["sync"], Some(1), "", "Vault is locked."),
            (vec!["--version"], Some(0), "1999.1.0\n", ""),
        ]);
        let mut vault = vault(script, Some("stale"));
        let locked = vault.sync().await;
        assert!(matches!(locked, Err(VaultError::Locked)));
        assert!(vault.session().is_none(), "the rejected key is dropped");
        let mut vault = vault_with_version_unverified();
        let refused = vault.create_folder("codex-hosts").await;
        assert!(matches!(refused, Err(VaultError::VersionUnverified)));
    }

    fn vault_with_version_unverified() -> Vault<Scripted> {
        vault(
            Scripted::new(vec![(vec!["--version"], Some(0), "1999.1.0\n", "")]),
            Some("key"),
        )
    }
}
