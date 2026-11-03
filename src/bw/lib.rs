//! Adapter for the official Bitwarden CLI (`bw.exe`), kept free of the rest
//! of codex-hosts so it can be built and tested on its own.
//!
//! Every subprocess is described by a [`BwInvocation`] and executed by
//! [`run`]. The invocation fixes the rules: a user-chosen absolute native
//! `.exe`, arguments as an argv array (never a shell string), the session key
//! and master password only in the child's environment (never `--session`,
//! never a password argument), item JSON only on stdin, a cleared environment
//! with a short allow-list, no console window, bounded output, and a kill on
//! timeout. Whole `bw` responses never leave this crate as plain strings:
//! [`parse`] and [`item`] project them into non-secret summaries and hand out
//! secrets one field at a time.

#![forbid(unsafe_code)]

pub mod host_uri;
pub mod interactive;
pub mod item;
pub mod output;
pub mod parse;
pub mod policy;
pub mod runner;

pub use host_uri::{HostUri, UriScheme, parse_host_uri};
pub use interactive::{CodePrompt, LoginError, LoginOutcome};
pub use item::{
    CODEX_FIELD_PREFIX, FieldEdit, ItemKind, ItemSummary, LoginPatch, VaultItem, parse_item_list,
};
pub use output::{
    BwVersion, CliFailure, OutputError, VERIFIED_VERSIONS, VersionSupport, classify_failure,
    classify_version, parse_raw_session, parse_version_output,
};
pub use parse::{
    StatusReport, VaultFolder, VaultParseError, VaultStatus, exact_folder_matches, folder_json,
    parse_folder, parse_folders, parse_status,
};
pub use policy::{
    BwExecutable, BwInvocation, BwPolicyError, BwProfile, DATA_DIR_ENV, MAX_OUTPUT_BYTES,
    MasterPassword, NO_INTERACTION_ENV, PASSWORD_ENV, SESSION_ENV, SessionKey, TwoFactorMethod,
    encode_item_json,
};
pub use runner::{BwOutput, BwRunError, run, run_command};
