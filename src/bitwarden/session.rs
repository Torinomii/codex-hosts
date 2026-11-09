//! Where the unlocked session key lives between calls, and for how long.
//!
//! `bw unlock --raw` returns a session key that decrypts the CLI's local
//! vault cache. codex-hosts keeps it in Windows Credential Manager under the
//! account profile, at the same trust level as the SSH passwords stored
//! there today, and removes it when the re-authentication policy says so.
//! `bw` itself has no timeout: its vault stays unlocked until `bw lock` (or
//! the next `bw unlock`, which replaces the one stored user key). So a key
//! the policy retires is handed back to the caller ([`take_expired`],
//! [`apply_policy`], [`take`]) to be locked with, not merely deleted.
//! Two small non-secret timestamps live next to the CLI data directory:
//! when the key was last used (idle policy) and when the vault last synced.

use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use codex_hosts_bw::SessionKey;
use uuid::Uuid;

use crate::credentials::{self, CredentialKind};
use crate::model::{BitwardenProfile, MIN_SYNC_SPACING_SECONDS, ReauthPolicy};

const USED_FILE: &str = "session-used";
const SYNCED_FILE: &str = "last-sync";
/// Marks a key stored under [`ReauthPolicy::UntilExit`], so a window that
/// starts after a crash can clear what the previous one left behind.
const SCOPE_FILE: &str = "session-until-exit";

/// The CLI data directory of one account profile
/// (`%LOCALAPPDATA%\CodexHosts\bitwarden\<profile id>`).
pub(crate) fn data_dir(profile_id: Uuid) -> Result<PathBuf, String> {
    let base = crate::storage::data_directory().map_err(|error| error.to_string())?;
    let dir = base.join("bitwarden").join(profile_id.to_string());
    fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    Ok(dir)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn read_stamp(dir: &Path, name: &str) -> Option<u64> {
    fs::read_to_string(dir.join(name))
        .ok()
        .and_then(|text| text.trim().parse().ok())
}

fn write_stamp(dir: &Path, name: &str) -> Result<(), String> {
    fs::write(dir.join(name), now_unix().to_string())
        .map_err(|error| format!("{}: {error}", dir.join(name).display()))
}

fn remove_stamp(dir: &Path, name: &str) {
    let _ = fs::remove_file(dir.join(name));
}

fn stamp_is_within(dir: &Path, name: &str, window: Duration) -> bool {
    read_stamp(dir, name).is_some_and(|at| now_unix().saturating_sub(at) < window.as_secs())
}

fn read_key(profile_id: Uuid) -> Result<Option<SessionKey>, String> {
    let stored = credentials::load(profile_id, CredentialKind::BitwardenSession(profile_id))
        .map_err(|error| error.to_string())?;
    Ok(stored
        .filter(|key| !key.is_empty())
        .map(|key| SessionKey::new(key.to_string())))
}

fn idle_window(minutes: u32) -> Duration {
    Duration::from_secs(u64::from(minutes.max(1)) * 60)
}

/// Whether the policy still allows the stored key. An expired idle key is
/// not served, and stays stored until [`take_expired`] hands it over to be
/// locked. A per-call key counts only until it is taken.
fn still_allowed(profile: &BitwardenProfile, dir: &Path) -> bool {
    match profile.policy {
        ReauthPolicy::PerCall | ReauthPolicy::UntilExit | ReauthPolicy::Permanent => true,
        ReauthPolicy::Idle { minutes } => stamp_is_within(dir, USED_FILE, idle_window(minutes)),
    }
}

/// Removes and returns a key the idle policy has expired, so the caller can
/// lock `bw` with it; `None` when no key is stored or it is still allowed.
pub(crate) fn take_expired(profile: &BitwardenProfile) -> Result<Option<SessionKey>, String> {
    let dir = data_dir(profile.id)?;
    if still_allowed(profile, &dir) {
        return Ok(None);
    }
    take(profile.id)
}

/// Removes and returns the stored key, for a caller that locks `bw` with it.
pub(crate) fn take(profile_id: Uuid) -> Result<Option<SessionKey>, String> {
    let key = read_key(profile_id)?;
    if key.is_some() {
        forget(profile_id)?;
    }
    Ok(key)
}

/// Brings the stored key in line with a policy the user just chose: a
/// switch to "every call" retires it (returned, to lock `bw` with); "until
/// exit" marks it for the window's exit; "idle" starts its clock now.
pub(crate) fn apply_policy(profile: &BitwardenProfile) -> Result<Option<SessionKey>, String> {
    let dir = data_dir(profile.id)?;
    match profile.policy {
        ReauthPolicy::PerCall => return take(profile.id),
        ReauthPolicy::UntilExit => {
            if read_key(profile.id)?.is_some() {
                write_stamp(&dir, SCOPE_FILE)?;
            }
        }
        ReauthPolicy::Idle { .. } => {
            remove_stamp(&dir, SCOPE_FILE);
            if read_key(profile.id)?.is_some() {
                write_stamp(&dir, USED_FILE)?;
            }
        }
        ReauthPolicy::Permanent => remove_stamp(&dir, SCOPE_FILE),
    }
    Ok(None)
}

/// Takes the stored session key for one operation, if the policy still
/// allows it. Under "every call" the key is handed over exactly once: the
/// window stores it for the call that asked, and the call consumes it.
pub(crate) fn load(profile: &BitwardenProfile) -> Result<Option<SessionKey>, String> {
    let dir = data_dir(profile.id)?;
    if !still_allowed(profile, &dir) {
        return Ok(None);
    }
    let key = read_key(profile.id)?;
    if profile.policy == ReauthPolicy::PerCall {
        forget(profile.id)?;
    }
    Ok(key)
}

/// Whether a usable key is stored, without taking it.
pub(crate) fn peek(profile: &BitwardenProfile) -> Result<bool, String> {
    let dir = data_dir(profile.id)?;
    if !still_allowed(profile, &dir) {
        return Ok(false);
    }
    Ok(read_key(profile.id)?.is_some())
}

/// The stored key for a background read (listing the folder for the host
/// list) that must not spend a per-call key or prompt: under "every call"
/// this is always `None`.
pub(crate) fn load_non_consuming(profile: &BitwardenProfile) -> Result<Option<SessionKey>, String> {
    if profile.policy == ReauthPolicy::PerCall {
        return Ok(None);
    }
    load(profile)
}

/// Keeps a freshly obtained key for the next call(s), as the policy allows.
pub(crate) fn store(profile: &BitwardenProfile, key: &SessionKey) -> Result<(), String> {
    let dir = data_dir(profile.id)?;
    credentials::store(
        profile.id,
        CredentialKind::BitwardenSession(profile.id),
        key.expose(),
    )
    .map_err(|error| error.to_string())?;
    write_stamp(&dir, USED_FILE)?;
    if profile.policy == ReauthPolicy::UntilExit {
        write_stamp(&dir, SCOPE_FILE)?;
    } else {
        remove_stamp(&dir, SCOPE_FILE);
    }
    Ok(())
}

/// Records that the key served a call, for the idle policy.
pub(crate) fn touch(profile_id: Uuid) -> Result<(), String> {
    let dir = data_dir(profile_id)?;
    write_stamp(&dir, USED_FILE)
}

/// Removes the stored key; the vault is locked for codex-hosts until the
/// master password is entered again.
pub(crate) fn forget(profile_id: Uuid) -> Result<(), String> {
    super::clear_cached_session(profile_id);
    credentials::delete(profile_id, CredentialKind::BitwardenSession(profile_id))
        .map_err(|error| error.to_string())?;
    if let Ok(dir) = data_dir(profile_id) {
        remove_stamp(&dir, USED_FILE);
        remove_stamp(&dir, SCOPE_FILE);
    }
    Ok(())
}

/// Removes and returns a key left behind by a window that did not exit
/// cleanly, when the policy was "until the window exits", to lock `bw` with.
pub(crate) fn take_if_window_scoped(
    profile: &BitwardenProfile,
) -> Result<Option<SessionKey>, String> {
    let dir = data_dir(profile.id)?;
    if !dir.join(SCOPE_FILE).exists() {
        return Ok(None);
    }
    let key = take(profile.id)?;
    remove_stamp(&dir, SCOPE_FILE);
    Ok(key)
}

/// Forgets the key and deletes the CLI data directory of an account that is
/// no longer configured, so its login tokens and vault cache do not linger.
pub(crate) fn remove_account(profile_id: Uuid) {
    let _ = forget(profile_id);
    if let Ok(base) = crate::storage::data_directory() {
        let _ = fs::remove_dir_all(base.join("bitwarden").join(profile_id.to_string()));
    }
}

/// The profile directory an interrupted setup left behind: the one whose
/// `bw` data file was written most recently. Reusing it lets the wizard
/// unlock an account the CLI is already logged in to instead of asking for
/// another login (and another two-step or new-device code).
pub(crate) fn leftover_profile() -> Option<Uuid> {
    let base = crate::storage::data_directory().ok()?.join("bitwarden");
    leftover_profile_in(&base)
}

fn leftover_profile_in(base: &Path) -> Option<Uuid> {
    let mut newest: Option<(SystemTime, Uuid)> = None;
    for entry in fs::read_dir(base).ok()?.flatten() {
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|name| Uuid::parse_str(name).ok())
        else {
            continue;
        };
        let Ok(written) =
            fs::metadata(entry.path().join("data.json")).and_then(|meta| meta.modified())
        else {
            continue;
        };
        if newest.is_none_or(|(at, _)| written > at) {
            newest = Some((written, id));
        }
    }
    newest.map(|(_, id)| id)
}

/// Whether the last `bw sync` is recent enough to serve from: younger than
/// `max_age_seconds`, and in any case younger than
/// [`MIN_SYNC_SPACING_SECONDS`] so a burst of calls shares one sync.
pub(crate) fn sync_is_fresh(profile_id: Uuid, max_age_seconds: u64) -> bool {
    data_dir(profile_id).is_ok_and(|dir| {
        stamp_is_within(
            &dir,
            SYNCED_FILE,
            Duration::from_secs(max_age_seconds.max(MIN_SYNC_SPACING_SECONDS)),
        )
    })
}

pub(crate) fn mark_synced(profile_id: Uuid) -> Result<(), String> {
    super::cache::clear();
    let dir = data_dir(profile_id)?;
    write_stamp(&dir, SYNCED_FILE)
}

/// A non-secret identity of the key for pool keys: it changes whenever the
/// key does, and reveals nothing about it.
pub(crate) fn fingerprint(key: &SessionKey) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.expose().hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(policy: ReauthPolicy) -> BitwardenProfile {
        BitwardenProfile {
            policy,
            ..BitwardenProfile::default()
        }
    }

    fn cleanup(profile: &BitwardenProfile) {
        let _ = forget(profile.id);
        if let Ok(dir) = data_dir(profile.id) {
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn per_call_hands_a_key_over_once_and_permanent_keeps_it() {
        let _guard = crate::credentials::test_lock();
        let per_call = profile(ReauthPolicy::PerCall);
        store(&per_call, &SessionKey::new("k1".into())).unwrap();
        let seen = peek(&per_call).unwrap();
        let first = load(&per_call).unwrap();
        let second = load(&per_call).unwrap();
        cleanup(&per_call);
        assert!(seen);
        assert_eq!(
            first.map(|key| key.expose().to_owned()).as_deref(),
            Some("k1")
        );
        assert!(second.is_none(), "the call that asked consumes the key");

        let permanent = profile(ReauthPolicy::Permanent);
        store(&permanent, &SessionKey::new("k2".into())).unwrap();
        let kept = load(&permanent).unwrap();
        let still = load(&permanent).unwrap();
        forget(permanent.id).unwrap();
        let gone = load(&permanent).unwrap();
        cleanup(&permanent);
        assert_eq!(
            kept.map(|key| key.expose().to_owned()).as_deref(),
            Some("k2")
        );
        assert!(still.is_some());
        assert!(gone.is_none());
    }

    #[test]
    fn idle_keys_expire_by_the_used_stamp_and_until_exit_keys_are_marked() {
        let _guard = crate::credentials::test_lock();
        let idle = profile(ReauthPolicy::Idle { minutes: 30 });
        store(&idle, &SessionKey::new("k3".into())).unwrap();
        assert!(load(&idle).unwrap().is_some());
        let dir = data_dir(idle.id).unwrap();
        fs::write(dir.join(USED_FILE), (now_unix() - 31 * 60).to_string()).unwrap();
        let expired = load(&idle).unwrap();
        let peeked = peek(&idle).unwrap();
        let handed = take_expired(&idle).unwrap();
        let entry_removed =
            credentials::load(idle.id, CredentialKind::BitwardenSession(idle.id)).unwrap();
        cleanup(&idle);
        assert!(expired.is_none() && !peeked, "an expired key is not served");
        assert_eq!(
            handed.map(|key| key.expose().to_owned()).as_deref(),
            Some("k3"),
            "the expired key is handed over to lock bw with"
        );
        assert!(entry_removed.is_none(), "and deleted, not kept");

        let scoped = profile(ReauthPolicy::UntilExit);
        store(&scoped, &SessionKey::new("k4".into())).unwrap();
        let marked = data_dir(scoped.id).unwrap().join(SCOPE_FILE).exists();
        let left = take_if_window_scoped(&scoped).unwrap();
        let gone = load(&scoped).unwrap();
        cleanup(&scoped);
        assert!(marked);
        assert!(left.is_some(), "the leftover key is handed over to lock bw");
        assert!(gone.is_none());
    }

    #[test]
    fn a_new_policy_applies_to_the_stored_key_at_once() {
        let _guard = crate::credentials::test_lock();
        let mut changed = profile(ReauthPolicy::Permanent);
        store(&changed, &SessionKey::new("k5".into())).unwrap();
        let dir = data_dir(changed.id).unwrap();

        changed.policy = ReauthPolicy::UntilExit;
        assert!(apply_policy(&changed).unwrap().is_none());
        let scoped = dir.join(SCOPE_FILE).exists();

        changed.policy = ReauthPolicy::Idle { minutes: 5 };
        fs::write(dir.join(USED_FILE), (now_unix() - 60 * 60).to_string()).unwrap();
        assert!(apply_policy(&changed).unwrap().is_none());
        let unscoped = !dir.join(SCOPE_FILE).exists();
        let idle_kept = peek(&changed).unwrap();

        changed.policy = ReauthPolicy::PerCall;
        let retired = apply_policy(&changed).unwrap();
        let after = peek(&changed).unwrap();
        cleanup(&changed);
        assert!(scoped, "until-exit marks the key for the window's exit");
        assert!(
            unscoped && idle_kept,
            "idle starts counting from the switch"
        );
        assert_eq!(
            retired.map(|key| key.expose().to_owned()).as_deref(),
            Some("k5"),
            "every-call retires the kept key so bw can be locked"
        );
        assert!(!after);
    }

    #[test]
    fn sync_freshness_and_fingerprints() {
        let _guard = crate::credentials::test_lock();
        let profile = profile(ReauthPolicy::Permanent);
        assert!(!sync_is_fresh(profile.id, 0));
        mark_synced(profile.id).unwrap();
        let fresh = sync_is_fresh(profile.id, 0);
        cleanup(&profile);
        assert!(fresh);
        assert_ne!(
            fingerprint(&SessionKey::new("a".into())),
            fingerprint(&SessionKey::new("b".into()))
        );
    }

    #[test]
    fn leftover_profile_is_the_newest_logged_in_directory() {
        let base = std::env::temp_dir().join(format!("codex-hosts-leftover-{}", Uuid::new_v4()));
        assert!(leftover_profile_in(&base).is_none(), "no base directory");
        fs::create_dir_all(&base).unwrap();
        assert!(leftover_profile_in(&base).is_none(), "empty base directory");

        let older = Uuid::new_v4();
        let newer = Uuid::new_v4();
        let stamps_only = Uuid::new_v4();
        for (id, data) in [(older, true), (newer, true), (stamps_only, false)] {
            let dir = base.join(id.to_string());
            fs::create_dir_all(&dir).unwrap();
            if data {
                fs::write(dir.join("data.json"), b"{}").unwrap();
            } else {
                fs::write(dir.join(SYNCED_FILE), b"1").unwrap();
            }
        }
        let not_a_profile = base.join("plugins");
        fs::create_dir_all(&not_a_profile).unwrap();
        fs::write(not_a_profile.join("data.json"), b"{}").unwrap();
        let written_at = |path: PathBuf, secs: u64| {
            fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(secs))
                .unwrap();
        };
        written_at(
            base.join(older.to_string()).join("data.json"),
            1_700_000_000,
        );
        written_at(
            base.join(newer.to_string()).join("data.json"),
            1_700_000_500,
        );
        written_at(not_a_profile.join("data.json"), 1_800_000_000);

        let found = leftover_profile_in(&base);
        let _ = fs::remove_dir_all(&base);
        assert_eq!(found, Some(newer));
    }
}
