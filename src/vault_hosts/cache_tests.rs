//! Production admissions against actual fake CLI processes and isolated stores.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use codex_hosts_bw::SessionKey;
use serde_json::{Value, json};

use super::*;
use crate::model::{BitwardenProfile, OnePasswordAccount, ReauthPolicy};

fn isolated(name: &str) -> bool {
    if std::env::var("CODEX_HOSTS_ISOLATED_TEST").as_deref() == Ok(name) {
        return true;
    }
    let directory =
        std::env::temp_dir().join(format!("codex-hosts-login-cache-{}", Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--test-threads=1", "--nocapture"])
        .env("CODEX_HOSTS_DATA_DIR", &directory)
        .env("CODEX_HOSTS_ISOLATED_TEST", name)
        .output()
        .unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    print!("{}", String::from_utf8_lossy(&output.stdout));
    false
}

fn example(name: &str) -> PathBuf {
    std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("examples/{name}.exe"))
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn write(path: &Path, state: &Value) {
    std::fs::write(path, serde_json::to_vec(state).unwrap()).unwrap();
}

fn stored_host() -> HostProfile {
    HostStore::load().unwrap().hosts[0].clone()
}

struct SessionCleanup(Uuid);

impl Drop for SessionCleanup {
    fn drop(&mut self) {
        bitwarden::session::forget(self.0).unwrap();
    }
}

#[test]
fn onepassword_reuses_logins_and_checks_refreshes_writes_and_accounts() {
    if !isolated(
        "vault_hosts::cache_tests::onepassword_reuses_logins_and_checks_refreshes_writes_and_accounts",
    ) {
        return;
    }
    use onepassword::cli::test_support::{ACCOUNT, ITEM, VAULT, item_json};
    let directory = crate::storage::data_directory().unwrap();
    let exe = directory.join("op.exe");
    std::fs::copy(example("fake_op"), &exe).unwrap();
    let path = directory.join("fake-op.json");
    let json = item_json(ITEM, "cache-host", 1, "synthetic-op-password");
    let item = onepassword::cli::parse_item(Zeroizing::new(json.as_bytes().to_vec())).unwrap();
    let host = mirror(
        &onepassword::item_facts(&item).unwrap(),
        &ItemKey::onepassword(ACCOUNT, ITEM),
    );
    write(
        &path,
        &json!({
            "account": {"account_uuid": ACCOUNT, "url": "test.1password.com", "email": "cache@example.com"},
            "vault": {"id": VAULT, "name": "cache-test"},
            "items": [serde_json::from_str::<Value>(&json).unwrap()], "failures": [], "delay_ms": 120,
        }),
    );
    let mut store = HostStore::default();
    store.hosts.push(host.clone());
    store.onepassword.cli_path = Some(exe);
    store.onepassword.account = Some(OnePasswordAccount {
        account_id: ACCOUNT.into(),
        vault_id: VAULT.into(),
        vault_name: "cache-test".into(),
        url: "test.1password.com".into(),
        email: "cache@example.com".into(),
    });
    store.save().unwrap();
    let calls = || -> usize {
        std::fs::read_to_string(directory.join("fake-op-calls.jsonl"))
            .unwrap()
            .lines()
            .filter(|line| {
                let call: Value = serde_json::from_str(line).unwrap();
                call["args"][0] == "item" && call["args"][1] == "get"
            })
            .count()
    };
    crate::ssh::runtime().block_on(async {
        let cold = Instant::now();
        let first = onepassword::admit(&host).await.unwrap();
        let cold = cold.elapsed();
        assert!(
            first
                .login
                .password()
                .is_some_and(|p| p.as_str() == "synthetic-op-password")
        );
        let warm = Instant::now();
        for _ in 0..8 {
            assert!(onepassword::admit(&host).await.is_ok());
        }
        println!(
            "fake-op: cold={}ms, warm mean={}us, item reads={}",
            cold.as_millis(),
            warm.elapsed().as_micros() / 8,
            calls()
        );
        assert_eq!(calls(), 1, "warm admissions must not launch op");
        let mut local = HostStore::load().unwrap();
        local.hosts[0].advanced.require_call_approval = true;
        local.save().unwrap();
        assert!(onepassword::admit(&stored_host()).await.is_ok());
        assert_eq!(
            calls(),
            1,
            "local host state changes must not discard login snapshots"
        );
        assert!(onepassword::resolve_login_password(&host).await.is_ok());
        assert_eq!(calls(), 1, "password reads share the admission snapshot");
        let mut state = read(&path);
        state["items"][0]["version"] = json!(2);
        for field in state["items"][0]["fields"].as_array_mut().unwrap() {
            if field["id"] == "password" {
                field["value"] = json!("changed-op-password");
            }
        }
        write(&path, &state);
        let stale_write = HostWrite {
            provider: VaultProvider::OnePassword,
            item_id: Some(host.id),
            external_id: Some(ITEM.into()),
            name: host.alias.clone(),
            host_uri: "ssh://192.0.2.10:2222".into(),
            username: host.username.clone(),
            password: codex_hosts_bw::FieldEdit::Keep,
            description: String::new(),
            tags: Vec::new(),
            expected_revision: "1".into(),
        };
        assert!(
            matches!(
                onepassword::save_item(&stale_write).await,
                Err(WriteFailure::Conflict)
            ),
            "cached data must never hide a conflicting write"
        );
        assert!(onepassword::discover().await.unwrap());
        let refreshed = stored_host();
        let admitted = onepassword::admit(&refreshed).await.unwrap();
        assert!(
            admitted
                .login
                .password()
                .is_some_and(|p| p.as_str() == "changed-op-password")
        );
        assert_eq!(calls(), 3);
        assert!(onepassword::discover().await.unwrap());
        let before = calls();
        let (left, right) = tokio::join!(
            onepassword::admit(&refreshed),
            onepassword::admit(&refreshed)
        );
        assert!(left.is_ok() && right.is_ok());
        assert_eq!(
            calls(),
            before + 1,
            "concurrent callers share the first read"
        );
        let before = calls();
        cache::reset(VaultProvider::OnePassword).unwrap();
        assert!(onepassword::admit(&refreshed).await.is_ok());
        assert_eq!(
            calls(),
            before + 1,
            "manual clearing forces a fresh CLI read"
        );
        let mut disabled = HostStore::load().unwrap();
        disabled.onepassword.login_cache_minutes = 0;
        disabled.save().unwrap();
        let before = calls();
        for _ in 0..2 {
            assert!(onepassword::admit(&refreshed).await.is_ok());
        }
        assert_eq!(
            calls(),
            before + 2,
            "disabling the cache reads the CLI every time"
        );
        let mut store = HostStore::load().unwrap();
        store.onepassword.enabled = false;
        store.save().unwrap();
        assert_eq!(
            onepassword::admit(&refreshed).await.err().unwrap().code,
            "ONEPASSWORD_DISABLED"
        );
        store.onepassword.enabled = true;
        store.onepassword.account.as_mut().unwrap().account_id =
            "otheraccount000000000000000".into();
        store.save().unwrap();
        let before = calls();
        assert!(onepassword::admit(&refreshed).await.is_err());
        assert_eq!(
            calls(),
            before,
            "another account cannot receive a cached password"
        );
    });
}

#[test]
fn bitwarden_reuses_logins_without_bypassing_unlock_policies() {
    if !isolated(
        "vault_hosts::cache_tests::bitwarden_reuses_logins_without_bypassing_unlock_policies",
    ) {
        return;
    }
    let _credentials = crate::credentials::test_lock();
    let mut profile = BitwardenProfile {
        folder_id: Some(Uuid::new_v4()),
        policy: ReauthPolicy::Permanent,
        email: "cache@example.com".into(),
        ..Default::default()
    };
    let _cleanup = SessionCleanup(profile.id);
    let facts = HostFacts {
        alias: "cache-host".into(),
        protocol: Protocol::Ssh,
        address: "192.0.2.10".into(),
        port: 22,
        username: "cache-user".into(),
        description: String::new(),
        tags: Vec::new(),
        revision: "1".into(),
    };
    let host = mirror(&facts, &ItemKey::bitwarden(Uuid::new_v4()));
    let directory = bitwarden::session::data_dir(profile.id).unwrap();
    let path = directory.join("fake-vault.json");
    write(
        &path,
        &json!({"session": "synthetic-session", "delay_ms": 120, "items": [{
            "id": host.id, "type": 1, "name": host.alias, "folderId": profile.folder_id, "revisionDate": "1",
            "login": {"username": host.username, "password": "synthetic-bw-password", "uris": [{"uri": "ssh://192.0.2.10:22"}]}
        }]}),
    );
    let key = SessionKey::new("synthetic-session".into());
    bitwarden::session::store(&profile, &key).unwrap();
    bitwarden::session::mark_synced(profile.id).unwrap();
    let mut store = HostStore::default();
    store.hosts.push(host.clone());
    store.bitwarden.cli_path = Some(example("fake_bw"));
    store.bitwarden.profile = Some(profile.clone());
    store.save().unwrap();
    let count = |prefix: &str| -> usize {
        std::fs::read_to_string(directory.join("fake-bw.log"))
            .unwrap()
            .lines()
            .filter(|line| line.starts_with(prefix))
            .count()
    };
    crate::ssh::runtime().block_on(async {
        let cold = Instant::now();
        let first = bitwarden::admit(&host).await.unwrap();
        let cold = cold.elapsed();
        assert!(
            first
                .into_login()
                .password
                .is_some_and(|p| p.as_str() == "synthetic-bw-password")
        );
        let warm = Instant::now();
        for _ in 0..8 {
            assert!(bitwarden::admit(&host).await.is_ok());
        }
        println!(
            "fake-bw: cold={}ms, warm mean={}us, item reads={}",
            cold.as_millis(),
            warm.elapsed().as_micros() / 8,
            count("get item")
        );
        assert_eq!(count("get item"), 1);
        assert!(bitwarden::resolve_login_password(&host, None).await.is_ok());
        assert_eq!(count("get item"), 1);
        assert!(bitwarden::refresh_quietly().await.unwrap().is_some());
        assert!(bitwarden::refresh_quietly().await.unwrap().is_none());
        assert_eq!(
            count("list items"),
            1,
            "repeated list_hosts must not launch bw"
        );
        let mut state = read(&path);
        state["items"][0]["revisionDate"] = json!("2");
        state["items"][0]["login"]["password"] = json!("changed-bw-password");
        write(&path, &state);
        std::fs::write(directory.join("data.json"), b"synthetic-cli-change").unwrap();
        let changed = bitwarden::admit(&host).await.unwrap();
        assert!(
            changed
                .into_login()
                .password
                .is_some_and(|p| p.as_str() == "changed-bw-password")
        );
        let refreshed = stored_host();
        assert!(bitwarden::admit(&refreshed).await.is_ok());
        assert_eq!(count("get item"), 2);
        bitwarden::session::mark_synced(profile.id).unwrap();
        let before = count("get item");
        let (left, right) =
            tokio::join!(bitwarden::admit(&refreshed), bitwarden::admit(&refreshed));
        assert!(left.is_ok() && right.is_ok());
        assert_eq!(count("get item"), before + 1);
        profile.policy = ReauthPolicy::PerCall;
        let mut store = HostStore::load().unwrap();
        store.bitwarden.profile = Some(profile.clone());
        store.save().unwrap();
        let before = count("get item");
        for _ in 0..2 {
            bitwarden::session::store(&profile, &key).unwrap();
            assert!(bitwarden::admit(&refreshed).await.is_ok());
            assert!(!bitwarden::session::peek(&profile).unwrap());
        }
        assert_eq!(
            count("get item"),
            before + 2,
            "per-call never reuses a login cache"
        );
        profile.policy = ReauthPolicy::Idle { minutes: 1 };
        let mut store = HostStore::load().unwrap();
        store.bitwarden.profile = Some(profile.clone());
        store.save().unwrap();
        bitwarden::session::store(&profile, &key).unwrap();
        assert!(bitwarden::admit(&refreshed).await.is_ok());
        let old = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 120;
        std::fs::write(directory.join("session-used"), old.to_string()).unwrap();
        let account = bitwarden::Account::from_settings(&store.bitwarden).unwrap();
        assert_eq!(
            account.unlocked_vault().err().unwrap().code,
            "BITWARDEN_LOCKED"
        );
        bitwarden::session::forget(profile.id).unwrap();
        assert!(!bitwarden::session::peek(&profile).unwrap());
    });
}
