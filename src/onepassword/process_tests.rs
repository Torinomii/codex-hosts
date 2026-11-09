//! Exercises the production CLI runner against an isolated fake-op process.

use std::path::PathBuf;

use codex_hosts_bw::FieldEdit;
use serde_json::{Value, json};

use super::cli::test_support::{ACCOUNT, ITEM, VAULT, item_json};
use super::*;

const CREATED: &str = "cccccccccccccccccccccccccc";
const PASSWORD: &str = "fake-op-only-password";

struct Fixture {
    root: PathBuf,
    exe: OpExecutable,
}

impl Fixture {
    fn new(failures: &[&str]) -> Self {
        let test = std::env::current_exe().unwrap();
        let profile = test.parent().unwrap().parent().unwrap();
        let source = profile.join("examples/fake_op.exe");
        assert!(
            source.is_file(),
            "scripts/check.ps1 builds fake_op before testing"
        );
        let root = std::env::temp_dir().join(format!("codex-hosts-fake-op-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("op.exe");
        std::fs::copy(source, &path).unwrap();
        let item: Value =
            serde_json::from_str(&item_json(ITEM, "p0-fake-host", 1, PASSWORD)).unwrap();
        let state = json!({
            "account": {"account_uuid": ACCOUNT, "url": "test.1password.com", "email": "test@example.com"},
            "vault": {"id": VAULT, "name": "fake-op-test"},
            "items": [item], "new_item_id": CREATED, "failures": failures,
        });
        std::fs::write(
            root.join("fake-op.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
        Self {
            root,
            exe: OpExecutable::new(path).unwrap(),
        }
    }

    fn op(&self) -> Op {
        Op::new(
            RealRunner,
            self.exe.clone(),
            OnePasswordAccount {
                account_id: ACCOUNT.into(),
                url: "test.1password.com".into(),
                email: "test@example.com".into(),
                vault_id: VAULT.into(),
                vault_name: "fake-op-test".into(),
            },
        )
    }

    fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(self.root.join("fake-op-calls.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[tokio::test]
async fn setup_runs_the_fake_cli_with_the_selected_account() {
    let fixture = Fixture::new(&[]);
    let (version, accounts) = check_cli(&fixture.exe).await.unwrap();
    assert_eq!(version, "2.41.1");
    assert_eq!(accounts[0].account_uuid, ACCOUNT);
    let vaults = list_vaults(&fixture.exe, ACCOUNT).await.unwrap();
    assert_eq!(vaults[0].id, VAULT);
    let name = "test vault \"one\" ü";
    let vault = create_vault(&fixture.exe, ACCOUNT, name).await.unwrap();
    assert_eq!(vault.name, name);
    assert_eq!(fixture.calls().len(), 4);
}

#[tokio::test]
async fn fake_cli_retries_an_initial_connection_failure_then_lists_and_reads_hosts() {
    let fixture = Fixture::new(&["cannot connect to 1Password app"]);
    let op = fixture.op();
    let items = op.list_items().await.unwrap();
    assert_eq!(fixture.calls().len(), 2);
    let mut store = HostStore::default();
    let result = reconcile(&op, &mut store, &items);
    assert_eq!(result.hosts, 1);
    let host = store.find_alias("p0-fake-host").unwrap();
    assert_eq!(host.address, "192.0.2.10");
    assert_eq!(host.port, 2222);
    let item = op.get_item(ITEM).await.unwrap();
    assert_eq!(item.username().unwrap().as_deref(), Some("p0"));
    assert!(
        item.password()
            .unwrap()
            .is_some_and(|p| p.as_str() == PASSWORD)
    );
    assert_eq!(item_facts(&item).unwrap().description, "P0 fake host");
    assert_eq!(
        op.get_item(CREATED).await.unwrap_err().code(),
        "ONEPASSWORD_ITEM_MISSING"
    );
}

fn write() -> HostWrite {
    HostWrite {
        provider: VaultProvider::OnePassword,
        item_id: None,
        external_id: None,
        name: "fake round trip".into(),
        host_uri: "ssh://192.0.2.11:22".into(),
        username: "fake-user".into(),
        password: FieldEdit::Set(Zeroizing::new(PASSWORD.into())),
        description: "saved by fake-op".into(),
        tags: vec!["offline".into()],
        expected_revision: String::new(),
    }
}

#[tokio::test]
async fn fake_cli_writes_over_stdin_preserves_passwords_and_archives_items() {
    let fixture = Fixture::new(&[]);
    let op = fixture.op();
    let mut write = write();
    let Ok((key, facts)) = save_with(&op, &write).await else {
        panic!("create succeeds");
    };
    assert_eq!(key.external_id.as_deref(), Some(CREATED));
    assert_eq!(facts.revision, "1");
    let created = op.list_items().await.unwrap();
    let created = created.iter().find(|item| item.id == CREATED).unwrap();
    assert_eq!(created.additional_information.as_deref(), Some("fake-user"));
    write.external_id = Some(CREATED.into());
    write.expected_revision = facts.revision;
    write.name = "edited fake host".into();
    write.password = FieldEdit::Keep;
    let Ok((_, facts)) = save_with(&op, &write).await else {
        panic!("edit succeeds");
    };
    assert_eq!(facts.alias, "edited fake host");
    assert_eq!(facts.revision, "2");
    let saved = op.get_item(CREATED).await.unwrap();
    assert!(
        saved
            .password()
            .unwrap()
            .is_some_and(|p| p.as_str() == PASSWORD)
    );
    assert!(matches!(
        save_with(&op, &write).await,
        Err(WriteFailure::Conflict)
    ));
    op.archive(CREATED).await.unwrap();
    op.archive(CREATED).await.unwrap();
    assert_eq!(op.list_items().await.unwrap().len(), 1);

    let calls = fixture.calls();
    let writes: Vec<_> = calls
        .iter()
        .filter(|call| call["stdin_len"].as_u64().unwrap() > 0)
        .collect();
    assert_eq!(writes.len(), 2);
    for call in calls {
        assert!(!call["args"].to_string().contains(PASSWORD));
        for name in call["env_names"].as_array().unwrap() {
            let name = name.as_str().unwrap().to_ascii_uppercase();
            assert!(name != "PATH" && !name.starts_with("OP_") && name != "CODEX_HOSTS_OP_LIVE");
        }
    }
}

#[tokio::test]
async fn fake_cli_errors_are_classified_without_repeating_the_command() {
    for (diagnostic, code) in [
        ("authorization was denied", "ONEPASSWORD_LOCKED"),
        ("no accounts configured", "ONEPASSWORD_NOT_CONFIGURED"),
        ("LostConnectionToApp", "ONEPASSWORD_UNAVAILABLE"),
        ("command failed", "ONEPASSWORD_UNAVAILABLE"),
    ] {
        let fixture = Fixture::new(&[diagnostic]);
        assert_eq!(fixture.op().list_items().await.unwrap_err().code(), code);
        assert_eq!(fixture.calls().len(), 1);
    }
}

#[tokio::test]
async fn malformed_fake_cli_output_is_rejected() {
    let fixture = Fixture::new(&[]);
    let path = fixture.root.join("fake-op.json");
    let mut state: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    state["items"] = json!({"unexpected": "not an item list"});
    std::fs::write(path, serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(matches!(
        fixture.op().list_items().await,
        Err(OpError::Parse)
    ));
    assert_eq!(fixture.calls().len(), 1);
}
