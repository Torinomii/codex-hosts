use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::Arc;

use russh::Signer;
use russh::keys::agent::AgentIdentity;
use russh::keys::ssh_encoding::{Decode, Encode};
use russh::keys::ssh_key::private::KeypairData;
use russh::keys::{HashAlg, PrivateKey, load_secret_key};
use serde::Serialize;
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command as TokioCommand;
use zeroize::Zeroizing;

#[cfg(windows)]
use p256::ecdsa::{RecoveryId, Signature, VerifyingKey};
#[cfg(windows)]
use russh::keys::ssh_key::{private, public};
#[cfg(windows)]
use sha2::{Digest, Sha256};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HWND, WAIT_ABANDONED, WAIT_OBJECT_0};
#[cfg(windows)]
use windows_sys::Win32::Networking::WindowsWebServices::{
    WEBAUTHN_ATTESTATION_CONVEYANCE_PREFERENCE_NONE,
    WEBAUTHN_AUTHENTICATOR_ATTACHMENT_CROSS_PLATFORM, WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS,
    WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS_VERSION_2,
    WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS,
    WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS_VERSION_2, WEBAUTHN_CLIENT_DATA,
    WEBAUTHN_CLIENT_DATA_CURRENT_VERSION, WEBAUTHN_COSE_ALGORITHM_ECDSA_P256_WITH_SHA256,
    WEBAUTHN_COSE_CREDENTIAL_PARAMETER, WEBAUTHN_COSE_CREDENTIAL_PARAMETER_CURRENT_VERSION,
    WEBAUTHN_COSE_CREDENTIAL_PARAMETERS, WEBAUTHN_CREDENTIAL, WEBAUTHN_CREDENTIAL_ATTESTATION,
    WEBAUTHN_CREDENTIAL_CURRENT_VERSION, WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY,
    WEBAUTHN_HASH_ALGORITHM_SHA_256, WEBAUTHN_RP_ENTITY_INFORMATION,
    WEBAUTHN_RP_ENTITY_INFORMATION_CURRENT_VERSION, WEBAUTHN_USER_ENTITY_INFORMATION,
    WEBAUTHN_USER_ENTITY_INFORMATION_CURRENT_VERSION,
    WEBAUTHN_USER_VERIFICATION_REQUIREMENT_REQUIRED, WebAuthNAuthenticatorGetAssertion,
    WebAuthNAuthenticatorMakeCredential, WebAuthNCancelCurrentOperation, WebAuthNFreeAssertion,
    WebAuthNFreeCredentialAttestation, WebAuthNGetCancellationId, WebAuthNGetErrorName,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CreateMutexW, INFINITE, ReleaseMutex, WaitForSingleObject,
};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
#[cfg(windows)]
use windows_sys::core::GUID;

use crate::storage;

const HELPER_VERSION: u8 = 5;
const HELPER_ERROR: u32 = 0;
const HELPER_SIGN: u32 = 1;
const HELPER_ENROLL: u32 = 2;
#[cfg(not(windows))]
const HELPER_LOAD_RESIDENT: u32 = 3;
const KEY_ECDSA_SK: u32 = 10;
pub const FLAG_USER_PRESENCE: u8 = 0x01;
pub const FLAG_RESIDENT: u8 = 0x20;
const MAX_HELPER_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const WINDOWS_MANAGED_USER_ID_SEED: &[u8] = b"codex-hosts:ssh:resident:v1";

#[derive(Debug, Error)]
pub enum FidoError {
    #[error(
        "the Windows OpenSSH FIDO system component is unavailable at {0}; this is a Windows optional component, not YubiKey/Pageant software"
    )]
    HelperUnavailable(PathBuf),
    #[error("failed to launch the Windows OpenSSH FIDO helper: {0}")]
    Launch(#[source] std::io::Error),
    #[error("FIDO helper I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("the Windows FIDO operation failed with OpenSSH error {code}: {meaning}")]
    Helper { code: u32, meaning: &'static str },
    #[error("a recoverable SSH credential already exists on the hardware key")]
    RecoverableCredentialExists,
    #[error("Windows hardware-key recovery failed: {0}")]
    WindowsRecovery(String),
    #[error("Windows Security prompt {prompt} of 2 was cancelled or timed out")]
    WindowsRecoveryCancelled { prompt: u8 },
    #[error("Windows Security credential creation was cancelled or timed out")]
    WindowsEnrollmentCancelled,
    #[error("failed to secure the recovered SSH file: {0}")]
    Permissions(String),
    #[error("the existing SSH handle file does not match the recovered hardware credential: {0}")]
    ExistingHandleMismatch(PathBuf),
    #[error("FIDO helper protocol error: {0}")]
    Protocol(&'static str),
    #[error("FIDO key data is invalid: {0}")]
    Key(#[from] russh::keys::ssh_key::Error),
    #[error("FIDO key encoding failed: {0}")]
    Encoding(#[from] russh::keys::ssh_encoding::Error),
    #[error("the selected file is not an OpenSSH ecdsa-sk or ed25519-sk FIDO handle")]
    NotSecurityKey,
    #[error("the FIDO signing task could not be completed: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("the SSH session closed before FIDO signing completed")]
    SessionClosed,
}

impl From<russh::SendError> for FidoError {
    fn from(_: russh::SendError) -> Self {
        Self::SessionClosed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FidoAlgorithm {
    EcdsaSk,
}

#[derive(Debug, Clone, Serialize)]
pub struct FidoKeyInfo {
    pub path: PathBuf,
    pub fingerprint: String,
    pub algorithm: String,
    pub public_key: String,
}

impl FidoAlgorithm {
    fn helper_key_type(self) -> u32 {
        match self {
            Self::EcdsaSk => KEY_ECDSA_SK,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FidoEnrollment {
    pub algorithm: FidoAlgorithm,
    pub application: &'static str,
    pub user_id: &'static str,
    pub flags: u8,
}

pub const fn recoverable_enrollment() -> FidoEnrollment {
    FidoEnrollment {
        algorithm: FidoAlgorithm::EcdsaSk,
        application: "ssh:",
        user_id: "",
        flags: FLAG_USER_PRESENCE | FLAG_RESIDENT,
    }
}

pub const fn compatible_enrollment() -> FidoEnrollment {
    FidoEnrollment {
        algorithm: FidoAlgorithm::EcdsaSk,
        application: "ssh:",
        user_id: "",
        flags: FLAG_USER_PRESENCE,
    }
}

#[derive(Clone)]
pub struct FidoSigner {
    key: Arc<PrivateKey>,
}

impl FidoSigner {
    pub fn new(key: Arc<PrivateKey>) -> Result<Self, FidoError> {
        ensure_security_key(&key)?;
        Ok(Self { key })
    }
}

impl Signer for FidoSigner {
    type Error = FidoError;

    fn auth_sign(
        &mut self,
        identity: &AgentIdentity,
        _hash_alg: Option<HashAlg>,
        to_sign: Vec<u8>,
    ) -> impl Future<Output = Result<Vec<u8>, Self::Error>> + Send {
        let key = Arc::clone(&self.key);
        let requested = identity.public_key().clone();
        async move {
            if key.public_key().key_data() != requested.key_data() {
                return Err(FidoError::Protocol(
                    "signer received a different public key",
                ));
            }
            let signature = sign(&key, &to_sign).await?;
            Ok(append_auth_signature(to_sign, &signature))
        }
    }
}

fn append_auth_signature(mut authentication_packet: Vec<u8>, signature: &[u8]) -> Vec<u8> {
    authentication_packet.extend_from_slice(signature);
    authentication_packet
}

pub fn helper_path() -> PathBuf {
    env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("OpenSSH")
        .join("ssh-sk-helper.exe")
}

pub fn helper_available() -> bool {
    helper_path().is_file()
}

pub fn is_security_key(key: &PrivateKey) -> bool {
    key.key_data().sk_ecdsa_p256().is_some() || key.key_data().sk_ed25519().is_some()
}

pub fn ensure_security_key(key: &PrivateKey) -> Result<(), FidoError> {
    is_security_key(key)
        .then_some(())
        .ok_or(FidoError::NotSecurityKey)
}

pub async fn sign(key: &PrivateKey, data: &[u8]) -> Result<Vec<u8>, FidoError> {
    ensure_security_key(key)?;
    let mut body = Zeroizing::new(Vec::new());
    let mut serialized = Vec::with_capacity(key.key_data().encoded_len()?);
    key.key_data().encode(&mut serialized)?;
    put_string(&mut body, &serialized);
    put_string(&mut body, b"internal");
    put_string(&mut body, data);
    put_string(&mut body, b"");
    put_u32(&mut body, 0);
    put_string(&mut body, b"");

    let response = helper_exchange(HELPER_SIGN, body).await?;
    let mut response = response.as_slice();
    let signature = take_string(&mut response)?;
    ensure_empty(response)?;

    let mut encoded = Vec::with_capacity(4 + signature.len());
    put_string(&mut encoded, &signature);
    Ok(encoded)
}

pub async fn enroll(
    algorithm: FidoAlgorithm,
    application: &str,
    user_id: &str,
    flags: u8,
    pin: Zeroizing<String>,
) -> Result<PrivateKey, FidoError> {
    if !application.starts_with("ssh:") {
        return Err(FidoError::Protocol(
            "FIDO SSH application must start with ssh:",
        ));
    }
    #[cfg(windows)]
    if flags & FLAG_RESIDENT != 0 {
        drop(pin);
        return enroll_resident_webauthn(application, user_id);
    }
    let mut body = Zeroizing::new(Vec::new());
    put_u32(&mut body, algorithm.helper_key_type());
    put_string(&mut body, b"internal");
    put_string(&mut body, b"");
    put_string(&mut body, application.as_bytes());
    put_string(&mut body, user_id.as_bytes());
    body.push(flags);
    put_string(&mut body, pin.as_bytes());
    put_string(&mut body, b"");

    let response = match helper_exchange(HELPER_ENROLL, body).await {
        Err(FidoError::Helper { code: 44, .. }) if flags & FLAG_RESIDENT != 0 => {
            return Err(FidoError::RecoverableCredentialExists);
        }
        result => result?,
    };
    let mut response = response.as_slice();
    let serialized = take_string(&mut response)?;
    let _attestation = take_string(&mut response)?;
    ensure_empty(response)?;
    decode_private_key(&serialized)
}

pub async fn load_resident(pin: Zeroizing<String>) -> Result<Vec<PrivateKey>, FidoError> {
    #[cfg(windows)]
    {
        drop(pin);
        load_resident_webauthn()
    }

    #[cfg(not(windows))]
    {
        let mut body = Zeroizing::new(Vec::new());
        put_string(&mut body, b"internal");
        put_string(&mut body, b"");
        put_string(&mut body, pin.as_bytes());
        put_u32(&mut body, 0);

        let response = helper_exchange(HELPER_LOAD_RESIDENT, body).await?;
        let mut response = response.as_slice();
        let mut keys = Vec::new();
        while !response.is_empty() {
            let serialized = take_string(&mut response)?;
            let _comment = take_string(&mut response)?;
            let _user_id = take_string(&mut response)?;
            keys.push(decode_private_key(&serialized)?);
        }
        Ok(keys)
    }
}

#[cfg(windows)]
fn private_key_from_recovered_public_key(
    application: &str,
    credential_id: Vec<u8>,
    public_key_bytes: &[u8],
) -> Result<PrivateKey, FidoError> {
    let ec_point = match public::EcdsaPublicKey::from_sec1_bytes(public_key_bytes)? {
        public::EcdsaPublicKey::NistP256(point) => point,
        _ => {
            return Err(FidoError::WindowsRecovery(
                "the recovered SSH credential is not an ECDSA P-256 key".to_owned(),
            ));
        }
    };
    let public_key = public::SkEcdsaSha2NistP256::new(ec_point, application.to_owned());
    let private_key = private::SkEcdsaSha2NistP256::new(
        public_key,
        FLAG_USER_PRESENCE | FLAG_RESIDENT,
        credential_id,
    )?;
    Ok(PrivateKey::new(private_key.into(), "codex-hosts")?)
}

#[cfg(windows)]
fn load_resident_webauthn() -> Result<Vec<PrivateKey>, FidoError> {
    const APPLICATION: &str = "ssh:";
    const CLIENT_DATA_ONE: &[u8] = br#"{"type":"webauthn.get","challenge":"Y29kZXgtaG9zdHMtcmVjb3ZlcnktMQ","origin":"https://ssh.invalid"}"#;
    const CLIENT_DATA_TWO: &[u8] = br#"{"type":"webauthn.get","challenge":"Y29kZXgtaG9zdHMtcmVjb3ZlcnktMg","origin":"https://ssh.invalid"}"#;

    let _hardware_guard = WindowsHardwareOperationGuard::acquire()?;
    let window = unsafe { GetForegroundWindow() };
    if window.is_null() {
        return Err(FidoError::WindowsRecovery(
            "the Codex Hosts window is unavailable".to_owned(),
        ));
    }
    let first = windows_get_assertion(window, APPLICATION, CLIENT_DATA_ONE, 1)?;
    std::thread::sleep(std::time::Duration::from_millis(350));
    let second = windows_get_assertion(window, APPLICATION, CLIENT_DATA_TWO, 2)?;
    if first.credential_id != second.credential_id {
        return Err(FidoError::WindowsRecovery(
            "Windows returned different credentials during verification".to_owned(),
        ));
    }

    let first_candidates = recover_ecdsa_public_keys(&first)?;
    let second_candidates = recover_ecdsa_public_keys(&second)?;
    let matches = first_candidates
        .into_iter()
        .filter(|candidate| second_candidates.contains(candidate))
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(FidoError::WindowsRecovery(
            "the ECDSA public key could not be identified uniquely".to_owned(),
        ));
    }

    Ok(vec![private_key_from_recovered_public_key(
        APPLICATION,
        first.credential_id,
        &matches[0],
    )?])
}

#[cfg(windows)]
struct WindowsAssertion {
    credential_id: Vec<u8>,
    authenticator_data: Vec<u8>,
    signature: Vec<u8>,
    client_data: Vec<u8>,
}

#[cfg(windows)]
fn recover_ecdsa_public_keys(assertion: &WindowsAssertion) -> Result<Vec<Vec<u8>>, FidoError> {
    let signature = Signature::from_der(&assertion.signature).map_err(|_| {
        FidoError::WindowsRecovery(
            "the selected credential is not a recoverable ECDSA-SK credential".to_owned(),
        )
    })?;
    let client_data_hash = Sha256::digest(&assertion.client_data);
    let mut signed_data = Vec::with_capacity(assertion.authenticator_data.len() + 32);
    signed_data.extend_from_slice(&assertion.authenticator_data);
    signed_data.extend_from_slice(&client_data_hash);

    let mut candidates = Vec::new();
    for y_is_odd in [false, true] {
        for x_is_reduced in [false, true] {
            let recovery_id = RecoveryId::new(y_is_odd, x_is_reduced);
            if let Ok(key) = VerifyingKey::recover_from_msg(&signed_data, &signature, recovery_id) {
                let encoded = key.to_sec1_point(false).as_bytes().to_vec();
                if !candidates.contains(&encoded) {
                    candidates.push(encoded);
                }
            }
        }
    }
    if candidates.is_empty() {
        return Err(FidoError::WindowsRecovery(
            "the ECDSA public key could not be recovered from the assertion".to_owned(),
        ));
    }
    Ok(candidates)
}

#[cfg(windows)]
fn windows_cancellation_id() -> Result<GUID, FidoError> {
    let mut cancellation_id = GUID::default();
    let result = unsafe { WebAuthNGetCancellationId(&mut cancellation_id) };
    if result != 0 {
        return Err(FidoError::WindowsRecovery(windows_webauthn_error(result)));
    }
    Ok(cancellation_id)
}

#[cfg(windows)]
fn run_webauthn_bounded(
    mut cancellation_id: GUID,
    operation: impl FnOnce(*mut GUID) -> i32,
) -> i32 {
    let cancellation_for_timer = cancellation_id;
    let (finished_sender, finished_receiver) = std::sync::mpsc::channel();
    let timer = std::thread::spawn(move || {
        if finished_receiver
            .recv_timeout(std::time::Duration::from_secs(45))
            .is_err()
        {
            unsafe {
                WebAuthNCancelCurrentOperation(&cancellation_for_timer);
            }
        }
    });
    let result = operation(&mut cancellation_id);
    let _ = finished_sender.send(());
    let _ = timer.join();
    result
}

#[cfg(windows)]
fn enroll_resident_webauthn(application: &str, user_name: &str) -> Result<PrivateKey, FidoError> {
    let _hardware_guard = WindowsHardwareOperationGuard::acquire()?;
    let rp_id = wide_string(application);
    let rp_name = wide_string("SSH");
    let display_name = wide_string(if user_name.is_empty() {
        "codex-hosts"
    } else {
        user_name
    });
    // A stable opaque user handle lets the authenticator recognize this app's
    // managed resident credential instead of accumulating indistinguishable
    // `ssh:` entries after every retry.
    let mut user_id: [u8; 32] = Sha256::digest(WINDOWS_MANAGED_USER_ID_SEED).into();
    let rp = WEBAUTHN_RP_ENTITY_INFORMATION {
        dwVersion: WEBAUTHN_RP_ENTITY_INFORMATION_CURRENT_VERSION,
        pwszId: rp_id.as_ptr(),
        pwszName: rp_name.as_ptr(),
        pwszIcon: std::ptr::null(),
    };
    let user = WEBAUTHN_USER_ENTITY_INFORMATION {
        dwVersion: WEBAUTHN_USER_ENTITY_INFORMATION_CURRENT_VERSION,
        cbId: user_id.len() as u32,
        pbId: user_id.as_mut_ptr(),
        pwszName: display_name.as_ptr(),
        pwszIcon: std::ptr::null(),
        pwszDisplayName: display_name.as_ptr(),
    };
    let mut parameter = WEBAUTHN_COSE_CREDENTIAL_PARAMETER {
        dwVersion: WEBAUTHN_COSE_CREDENTIAL_PARAMETER_CURRENT_VERSION,
        pwszCredentialType: WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY,
        lAlg: WEBAUTHN_COSE_ALGORITHM_ECDSA_P256_WITH_SHA256,
    };
    let parameters = WEBAUTHN_COSE_CREDENTIAL_PARAMETERS {
        cCredentialParameters: 1,
        pCredentialParameters: &mut parameter,
    };
    let challenge = uuid::Uuid::new_v4().simple().to_string();
    let mut client_json = format!(
        r#"{{"type":"webauthn.create","challenge":"{challenge}","origin":"https://ssh.invalid"}}"#
    )
    .into_bytes();
    let client_data = WEBAUTHN_CLIENT_DATA {
        dwVersion: WEBAUTHN_CLIENT_DATA_CURRENT_VERSION,
        cbClientDataJSON: client_json.len() as u32,
        pbClientDataJSON: client_json.as_mut_ptr(),
        pwszHashAlgId: WEBAUTHN_HASH_ALGORITHM_SHA_256,
    };
    let cancellation_id = windows_cancellation_id()?;
    let mut known_credential_ids = resident_credential_ids();
    let mut excluded_credentials = known_credential_ids
        .iter_mut()
        .map(|credential_id| WEBAUTHN_CREDENTIAL {
            dwVersion: WEBAUTHN_CREDENTIAL_CURRENT_VERSION,
            cbId: credential_id.len() as u32,
            pbId: credential_id.as_mut_ptr(),
            pwszCredentialType: WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY,
        })
        .collect::<Vec<_>>();
    let mut options = WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS {
        dwVersion: WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS_VERSION_2,
        dwTimeoutMilliseconds: 45_000,
        dwAuthenticatorAttachment: WEBAUTHN_AUTHENTICATOR_ATTACHMENT_CROSS_PLATFORM,
        bRequireResidentKey: 1,
        dwUserVerificationRequirement: WEBAUTHN_USER_VERIFICATION_REQUIREMENT_REQUIRED,
        dwAttestationConveyancePreference: WEBAUTHN_ATTESTATION_CONVEYANCE_PREFERENCE_NONE,
        ..Default::default()
    };
    options.CredentialList.cCredentials = excluded_credentials.len() as u32;
    options.CredentialList.pCredentials = if excluded_credentials.is_empty() {
        std::ptr::null_mut()
    } else {
        excluded_credentials.as_mut_ptr()
    };
    let mut attestation: *mut WEBAUTHN_CREDENTIAL_ATTESTATION = std::ptr::null_mut();
    let result = run_webauthn_bounded(cancellation_id, |cancellation_id| {
        options.pCancellationId = cancellation_id;
        unsafe {
            WebAuthNAuthenticatorMakeCredential(
                GetForegroundWindow(),
                &rp,
                &user,
                &parameters,
                &client_data,
                &options,
                &mut attestation,
            )
        }
    });
    if result as u32 == 0x8007_04C7 {
        return Err(FidoError::WindowsEnrollmentCancelled);
    }
    if result as u32 == 0x8009_000F {
        return Err(FidoError::RecoverableCredentialExists);
    }
    if result != 0 {
        return Err(FidoError::WindowsRecovery(windows_webauthn_error(result)));
    }
    if attestation.is_null() {
        return Err(FidoError::WindowsRecovery(
            "Windows returned an empty credential attestation".to_owned(),
        ));
    }

    let recovered = unsafe {
        (|| -> Result<PrivateKey, FidoError> {
            let value = &*attestation;
            if value.dwVersion >= 5 && value.bResidentKey == 0 {
                return Err(FidoError::WindowsRecovery(
                    "Windows did not create a recoverable credential on the hardware key"
                        .to_owned(),
                ));
            }
            let authenticator_data =
                copy_ffi_bytes(value.pbAuthenticatorData, value.cbAuthenticatorData)?;
            let returned_credential_id =
                copy_ffi_bytes(value.pbCredentialId, value.cbCredentialId)?;
            let (credential_id, public_key) = parse_attested_ecdsa_key(&authenticator_data)?;
            if credential_id != returned_credential_id {
                return Err(FidoError::WindowsRecovery(
                    "Windows returned inconsistent credential identifiers".to_owned(),
                ));
            }
            private_key_from_recovered_public_key(application, credential_id, &public_key)
        })()
    };
    unsafe { WebAuthNFreeCredentialAttestation(attestation) };
    recovered
}

#[cfg(windows)]
struct WindowsHardwareOperationGuard(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl WindowsHardwareOperationGuard {
    fn acquire() -> Result<Self, FidoError> {
        let name = "Local\\codex-hosts-hardware-auth-v1\0"
            .encode_utf16()
            .collect::<Vec<_>>();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(FidoError::WindowsRecovery(
                std::io::Error::last_os_error().to_string(),
            ));
        }
        let wait = unsafe { WaitForSingleObject(handle, INFINITE) };
        if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
            unsafe {
                CloseHandle(handle);
            }
            return Err(FidoError::WindowsRecovery(format!(
                "hardware-authentication lock wait failed with code {wait}"
            )));
        }
        Ok(Self(handle))
    }
}

#[cfg(windows)]
impl Drop for WindowsHardwareOperationGuard {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
fn parse_attested_ecdsa_key(authenticator_data: &[u8]) -> Result<(Vec<u8>, Vec<u8>), FidoError> {
    const PREFIX_LENGTH: usize = 32 + 1 + 4;
    const AAGUID_LENGTH: usize = 16;
    if authenticator_data.len() < PREFIX_LENGTH + AAGUID_LENGTH + 2
        || authenticator_data[32] & 0x40 == 0
    {
        return Err(FidoError::WindowsRecovery(
            "the authenticator did not return an attested credential public key".to_owned(),
        ));
    }
    let credential_length_offset = PREFIX_LENGTH + AAGUID_LENGTH;
    let credential_length = u16::from_be_bytes([
        authenticator_data[credential_length_offset],
        authenticator_data[credential_length_offset + 1],
    ]) as usize;
    let credential_offset = credential_length_offset + 2;
    let cose_offset = credential_offset
        .checked_add(credential_length)
        .ok_or_else(|| FidoError::WindowsRecovery("credential length overflow".to_owned()))?;
    if cose_offset >= authenticator_data.len() {
        return Err(FidoError::WindowsRecovery(
            "the authenticator returned a truncated credential".to_owned(),
        ));
    }
    let credential_id = authenticator_data[credential_offset..cose_offset].to_vec();
    let cose: ciborium::Value = ciborium::from_reader(&authenticator_data[cose_offset..])
        .map_err(|error| FidoError::WindowsRecovery(format!("invalid COSE public key: {error}")))?;
    let ciborium::Value::Map(fields) = cose else {
        return Err(FidoError::WindowsRecovery(
            "the authenticator returned a non-map COSE public key".to_owned(),
        ));
    };
    let mut x = None;
    let mut y = None;
    let mut algorithm = None;
    for (key, value) in fields {
        let ciborium::Value::Integer(key) = key else {
            continue;
        };
        match i128::from(key) {
            3 => {
                if let ciborium::Value::Integer(value) = value {
                    algorithm = Some(i128::from(value));
                }
            }
            -2 => {
                if let ciborium::Value::Bytes(value) = value {
                    x = Some(value);
                }
            }
            -3 => {
                if let ciborium::Value::Bytes(value) = value {
                    y = Some(value);
                }
            }
            _ => {}
        }
    }
    if algorithm != Some(i128::from(WEBAUTHN_COSE_ALGORITHM_ECDSA_P256_WITH_SHA256)) {
        return Err(FidoError::WindowsRecovery(
            "the hardware key did not create an ECDSA P-256 credential".to_owned(),
        ));
    }
    let (Some(x), Some(y)) = (x, y) else {
        return Err(FidoError::WindowsRecovery(
            "the COSE public key is missing its ECDSA coordinates".to_owned(),
        ));
    };
    if x.len() != 32 || y.len() != 32 {
        return Err(FidoError::WindowsRecovery(
            "the COSE public key has invalid ECDSA coordinates".to_owned(),
        ));
    }
    let mut public_key = Vec::with_capacity(65);
    public_key.push(0x04);
    public_key.extend_from_slice(&x);
    public_key.extend_from_slice(&y);
    Ok((credential_id, public_key))
}

#[cfg(windows)]
fn windows_get_assertion(
    window: HWND,
    application: &str,
    client_json: &[u8],
    prompt: u8,
) -> Result<WindowsAssertion, FidoError> {
    let relying_party_id = wide_string(application);
    let client_data = WEBAUTHN_CLIENT_DATA {
        dwVersion: WEBAUTHN_CLIENT_DATA_CURRENT_VERSION,
        cbClientDataJSON: client_json.len() as u32,
        pbClientDataJSON: client_json.as_ptr().cast_mut(),
        pwszHashAlgId: WEBAUTHN_HASH_ALGORITHM_SHA_256,
    };
    let cancellation_id = windows_cancellation_id()?;
    let mut options = WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS {
        dwVersion: WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS_VERSION_2,
        dwTimeoutMilliseconds: 45_000,
        dwAuthenticatorAttachment: WEBAUTHN_AUTHENTICATOR_ATTACHMENT_CROSS_PLATFORM,
        dwUserVerificationRequirement: WEBAUTHN_USER_VERIFICATION_REQUIREMENT_REQUIRED,
        ..Default::default()
    };
    let mut assertion = std::ptr::null_mut();
    let result = run_webauthn_bounded(cancellation_id, |cancellation_id| {
        options.pCancellationId = cancellation_id;
        unsafe {
            WebAuthNAuthenticatorGetAssertion(
                window,
                relying_party_id.as_ptr(),
                &client_data,
                &options,
                &mut assertion,
            )
        }
    });
    if result as u32 == 0x8007_04C7 {
        return Err(FidoError::WindowsRecoveryCancelled { prompt });
    }
    if result != 0 {
        return Err(FidoError::WindowsRecovery(windows_webauthn_error(result)));
    }
    if assertion.is_null() {
        return Err(FidoError::WindowsRecovery(
            "Windows returned an empty assertion".to_owned(),
        ));
    }

    let recovered = unsafe {
        (|| -> Result<WindowsAssertion, FidoError> {
            let value = &*assertion;
            Ok(WindowsAssertion {
                credential_id: copy_ffi_bytes(value.Credential.pbId, value.Credential.cbId)?,
                authenticator_data: copy_ffi_bytes(
                    value.pbAuthenticatorData,
                    value.cbAuthenticatorData,
                )?,
                signature: copy_ffi_bytes(value.pbSignature, value.cbSignature)?,
                client_data: client_json.to_vec(),
            })
        })()
    };
    unsafe { WebAuthNFreeAssertion(assertion) };
    recovered
}

#[cfg(windows)]
unsafe fn copy_ffi_bytes(pointer: *const u8, length: u32) -> Result<Vec<u8>, FidoError> {
    if length == 0 {
        return Ok(Vec::new());
    }
    if pointer.is_null() {
        return Err(FidoError::WindowsRecovery(
            "Windows returned a null data pointer".to_owned(),
        ));
    }
    Ok(unsafe { std::slice::from_raw_parts(pointer, length as usize) }.to_vec())
}

#[cfg(windows)]
fn wide_string(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn windows_webauthn_error(result: i32) -> String {
    let pointer = unsafe { WebAuthNGetErrorName(result) };
    if pointer.is_null() {
        return format!("WebAuthn error 0x{:08X}", result as u32);
    }
    let mut length = 0usize;
    while length < 256 && unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    let name = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, length) });
    format!("{name} (0x{:08X})", result as u32)
}

pub fn fingerprint(key: &PrivateKey) -> Result<String, FidoError> {
    ensure_security_key(key)?;
    Ok(key.public_key().fingerprint(HashAlg::Sha256).to_string())
}

pub fn key_info(path: PathBuf, key: &PrivateKey) -> Result<FidoKeyInfo, FidoError> {
    ensure_security_key(key)?;
    Ok(FidoKeyInfo {
        path,
        fingerprint: fingerprint(key)?,
        algorithm: key.public_key().algorithm().to_string(),
        public_key: key.public_key().to_openssh()?,
    })
}

pub fn save_handle(key: &PrivateKey) -> Result<FidoKeyInfo, FidoError> {
    ensure_security_key(key)?;
    let directory = storage::data_directory()
        .map_err(|_| FidoError::Protocol("application data directory is unavailable"))?
        .join("keys");
    fs::create_dir_all(&directory)?;
    let fingerprint = fingerprint(key)?;
    let short = fingerprint
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(16)
        .collect::<String>();
    let algorithm = if key.key_data().sk_ed25519().is_some() {
        "ed25519_sk"
    } else {
        "ecdsa_sk"
    };
    let path = directory.join(format!("id_{algorithm}_{short}"));
    if path.exists() {
        validate_existing_handle(&path, key)?;
        secure_handle_permissions(&path)?;
        return key_info(path, key);
    }

    let encoded = Zeroizing::new(key.to_openssh(russh::keys::ssh_key::LineEnding::LF)?);
    let temporary = directory.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("fido"),
        uuid::Uuid::new_v4().simple()
    ));
    let write_result = (|| -> Result<(), FidoError> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(encoded.as_bytes())?;
        file.sync_all()?;
        secure_handle_permissions(&temporary)?;
        match fs::hard_link(&temporary, &path) {
            Ok(()) => {
                fs::remove_file(&temporary)?;
                Ok(())
            }
            Err(_) if path.exists() => validate_existing_handle(&path, key),
            Err(error) => Err(error.into()),
        }
    })();
    if write_result.is_err() || temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    write_result?;
    key_info(path, key)
}

fn validate_existing_handle(path: &Path, expected: &PrivateKey) -> Result<(), FidoError> {
    let existing = load_secret_key(path, None)
        .map_err(|_| FidoError::ExistingHandleMismatch(path.to_owned()))?;
    if !is_security_key(&existing) || existing.public_key() != expected.public_key() {
        return Err(FidoError::ExistingHandleMismatch(path.to_owned()));
    }
    Ok(())
}

fn is_resident_handle(key: &PrivateKey) -> bool {
    key.key_data()
        .sk_ecdsa_p256()
        .map(|key| key.flags() & FLAG_RESIDENT != 0)
        .or_else(|| {
            key.key_data()
                .sk_ed25519()
                .map(|key| key.flags() & FLAG_RESIDENT != 0)
        })
        .unwrap_or(false)
}

#[cfg(windows)]
fn resident_credential_ids() -> Vec<Vec<u8>> {
    let mut ids: Vec<Vec<u8>> = Vec::new();
    for directory in handle_directories() {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || !looks_like_fido_handle(&path) {
                continue;
            }
            let Ok(key) = load_secret_key(&path, None) else {
                continue;
            };
            if !is_resident_handle(&key) {
                continue;
            }
            let key_handle = key
                .key_data()
                .sk_ecdsa_p256()
                .map(|key| key.key_handle())
                .or_else(|| key.key_data().sk_ed25519().map(|key| key.key_handle()));
            if let Some(key_handle) = key_handle
                && !ids.iter().any(|known| known.as_slice() == key_handle)
            {
                ids.push(key_handle.to_vec());
            }
        }
    }
    ids
}

#[cfg(windows)]
fn secure_handle_permissions(path: &Path) -> Result<(), FidoError> {
    let user = env::var("USERNAME")
        .map_err(|_| FidoError::Permissions("the Windows user name is unavailable".to_owned()))?;
    let domain = env::var("USERDOMAIN").unwrap_or_default();
    let identity = if domain.is_empty() {
        user
    } else {
        format!(r"{domain}\{user}")
    };
    let icacls = env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("icacls.exe");
    let status = StdCommand::new(icacls)
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{identity}:(F)"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(FidoError::Permissions(format!(
            "Windows ACL update exited with {status}"
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
fn secure_handle_permissions(_path: &Path) -> Result<(), FidoError> {
    Ok(())
}

pub fn discover_handles() -> Vec<FidoKeyInfo> {
    let directories = handle_directories();
    let mut found = Vec::new();
    for directory in directories {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || !looks_like_fido_handle(&path) {
                continue;
            }
            let Ok(key) = load_secret_key(&path, None) else {
                continue;
            };
            if let Ok(info) = key_info(path, &key) {
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok();
                found.push((is_resident_handle(&key), modified, info));
            }
        }
    }
    found.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| left.2.path.cmp(&right.2.path))
    });
    found.dedup_by(|left, right| left.2.fingerprint == right.2.fingerprint);
    found.into_iter().map(|(_, _, info)| info).collect()
}

fn handle_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Ok(directory) = storage::data_directory() {
        directories.push(directory.join("keys"));
    }
    if let Some(profile) = env::var_os("USERPROFILE") {
        directories.push(PathBuf::from(profile).join(".ssh"));
    }
    directories
}

fn decode_private_key(serialized: &[u8]) -> Result<PrivateKey, FidoError> {
    let mut reader = serialized;
    let key_data = KeypairData::decode(&mut reader)?;
    if !reader.is_empty() {
        return Err(FidoError::Protocol("trailing data in serialized FIDO key"));
    }
    let key = PrivateKey::new(key_data, "codex-hosts")?;
    ensure_security_key(&key)?;
    Ok(key)
}

async fn helper_exchange(
    request_type: u32,
    body: Zeroizing<Vec<u8>>,
) -> Result<Vec<u8>, FidoError> {
    let path = helper_path();
    if !path.is_file() {
        return Err(FidoError::HelperUnavailable(path));
    }

    let mut payload = Zeroizing::new(Vec::with_capacity(10 + body.len()));
    payload.push(HELPER_VERSION);
    put_u32(&mut payload, request_type);
    payload.push(0);
    put_u32(&mut payload, 0);
    payload.extend_from_slice(&body);

    let mut framed = Zeroizing::new(Vec::with_capacity(4 + payload.len()));
    put_string(&mut framed, &payload);

    let mut command = TokioCommand::new(&path);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let mut child = command.spawn().map_err(FidoError::Launch)?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or(FidoError::Protocol("helper stdin was not created"))?;
    stdin.write_all(&framed).await?;
    stdin.shutdown().await?;
    drop(stdin);

    let output = child.wait_with_output().await?;
    if !output.status.success() {
        return Err(FidoError::Protocol("helper exited unsuccessfully"));
    }
    if output.stdout.len() > MAX_HELPER_RESPONSE_BYTES {
        return Err(FidoError::Protocol(
            "helper response exceeded the size limit",
        ));
    }
    let mut frame = output.stdout.as_slice();
    let response = take_string(&mut frame)?;
    let mut response = response.as_slice();
    ensure_empty(frame)?;
    if take_u8(&mut response)? != HELPER_VERSION {
        return Err(FidoError::Protocol("helper protocol version mismatch"));
    }
    let response_type = take_u32(&mut response)?;
    if response_type == HELPER_ERROR {
        let code = take_u32(&mut response)?;
        ensure_empty(response)?;
        return Err(FidoError::Helper {
            code,
            meaning: helper_error_meaning(code),
        });
    }
    if response_type != request_type {
        return Err(FidoError::Protocol(
            "helper returned the wrong response type",
        ));
    }
    Ok(response.to_vec())
}

fn put_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn put_string(output: &mut Vec<u8>, value: &[u8]) {
    put_u32(output, value.len() as u32);
    output.extend_from_slice(value);
}

fn take_u8(input: &mut &[u8]) -> Result<u8, FidoError> {
    let Some((&value, rest)) = input.split_first() else {
        return Err(FidoError::Protocol("truncated u8"));
    };
    *input = rest;
    Ok(value)
}

fn take_u32(input: &mut &[u8]) -> Result<u32, FidoError> {
    if input.len() < 4 {
        return Err(FidoError::Protocol("truncated u32"));
    }
    let value = u32::from_be_bytes(input[..4].try_into().expect("four bytes"));
    *input = &input[4..];
    Ok(value)
}

fn take_string(input: &mut &[u8]) -> Result<Vec<u8>, FidoError> {
    let length = take_u32(input)? as usize;
    if input.len() < length {
        return Err(FidoError::Protocol("truncated string"));
    }
    let value = input[..length].to_vec();
    *input = &input[length..];
    Ok(value)
}

fn ensure_empty(input: &[u8]) -> Result<(), FidoError> {
    input
        .is_empty()
        .then_some(())
        .ok_or(FidoError::Protocol("trailing helper data"))
}

fn helper_error_meaning(code: u32) -> &'static str {
    match code {
        4 => "invalid request format or a generic FIDO provider failure",
        43 => "a FIDO PIN is required or was rejected",
        44 => "the credential already exists or replacement was denied",
        59 => "the requested FIDO feature is unsupported",
        60 => "no matching FIDO device was found",
        _ => "unclassified OpenSSH helper failure",
    }
}

pub fn looks_like_fido_handle(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains("_sk") && !name.ends_with(".pub"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_frame_round_trip_is_bounded_and_big_endian() {
        let mut encoded = Vec::new();
        put_string(&mut encoded, b"abc");
        assert_eq!(encoded, [0, 0, 0, 3, b'a', b'b', b'c']);
        let mut input = encoded.as_slice();
        assert_eq!(take_string(&mut input).unwrap(), b"abc");
        assert!(input.is_empty());
    }

    #[test]
    fn fido_handle_name_filter_excludes_public_keys() {
        assert!(looks_like_fido_handle(Path::new("id_ed25519_sk")));
        assert!(looks_like_fido_handle(Path::new("work_sk_backup")));
        assert!(!looks_like_fido_handle(Path::new("id_ed25519_sk.pub")));
        assert!(!looks_like_fido_handle(Path::new("id_ed25519")));
    }

    #[test]
    fn helper_error_codes_are_not_confused_with_sk_api_codes() {
        assert_eq!(
            helper_error_meaning(4),
            "invalid request format or a generic FIDO provider failure"
        );
        assert_eq!(
            helper_error_meaning(60),
            "no matching FIDO device was found"
        );
    }

    #[test]
    fn custom_signer_appends_signature_to_the_authentication_packet() {
        assert_eq!(
            append_auth_signature(vec![1, 2, 3], &[4, 5]),
            vec![1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn enrollment_profiles_match_windows_openssh_compatible_requests() {
        assert_eq!(
            recoverable_enrollment(),
            FidoEnrollment {
                algorithm: FidoAlgorithm::EcdsaSk,
                application: "ssh:",
                user_id: "",
                flags: FLAG_USER_PRESENCE | FLAG_RESIDENT,
            }
        );
        assert_eq!(
            compatible_enrollment(),
            FidoEnrollment {
                algorithm: FidoAlgorithm::EcdsaSk,
                application: "ssh:",
                user_id: "",
                flags: FLAG_USER_PRESENCE,
            }
        );
    }

    #[cfg(windows)]
    #[test]
    fn two_webauthn_assertions_identify_the_same_ecdsa_public_key() {
        use p256::ecdsa::{SigningKey, signature::Signer};

        fn assertion(signing_key: &SigningKey, client_data: &[u8]) -> WindowsAssertion {
            let authenticator_data = vec![0x5a; 37];
            let client_data_hash = Sha256::digest(client_data);
            let mut signed_data = authenticator_data.clone();
            signed_data.extend_from_slice(&client_data_hash);
            let signature: Signature = signing_key.sign(&signed_data);
            WindowsAssertion {
                credential_id: vec![1, 2, 3, 4],
                authenticator_data,
                signature: signature.to_der().as_bytes().to_vec(),
                client_data: client_data.to_vec(),
            }
        }

        let signing_key = SigningKey::from_bytes((&[7u8; 32]).into()).unwrap();
        let first = recover_ecdsa_public_keys(&assertion(&signing_key, b"first")).unwrap();
        let second = recover_ecdsa_public_keys(&assertion(&signing_key, b"second")).unwrap();
        let matches = first
            .into_iter()
            .filter(|candidate| second.contains(candidate))
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1);
        assert_eq!(
            matches[0],
            signing_key.verifying_key().to_sec1_point(false).as_bytes()
        );
    }

    #[cfg(windows)]
    #[test]
    fn webauthn_attestation_extracts_credential_and_ecdsa_public_key() {
        use p256::ecdsa::SigningKey;

        let signing_key = SigningKey::from_bytes((&[9u8; 32]).into()).unwrap();
        let point = signing_key.verifying_key().to_sec1_point(false);
        let credential_id = vec![1, 3, 3, 7];
        let cose = ciborium::Value::Map(vec![
            (
                ciborium::Value::Integer(3.into()),
                ciborium::Value::Integer((-7).into()),
            ),
            (
                ciborium::Value::Integer((-2).into()),
                ciborium::Value::Bytes(point.x().unwrap().to_vec()),
            ),
            (
                ciborium::Value::Integer((-3).into()),
                ciborium::Value::Bytes(point.y().unwrap().to_vec()),
            ),
        ]);
        let mut encoded_cose = Vec::new();
        ciborium::into_writer(&cose, &mut encoded_cose).unwrap();
        let mut authenticator_data = vec![0u8; 37];
        authenticator_data[32] = 0x40;
        authenticator_data.extend_from_slice(&[0u8; 16]);
        authenticator_data.extend_from_slice(&(credential_id.len() as u16).to_be_bytes());
        authenticator_data.extend_from_slice(&credential_id);
        authenticator_data.extend_from_slice(&encoded_cose);

        let (parsed_credential, parsed_public_key) =
            parse_attested_ecdsa_key(&authenticator_data).unwrap();
        assert_eq!(parsed_credential, credential_id);
        assert_eq!(parsed_public_key, point.as_bytes());
    }
}
