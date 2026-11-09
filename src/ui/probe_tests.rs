//! Real loopback SSH and forwarding, with synthetic CLI data and credentials.

use std::sync::atomic::{AtomicUsize, Ordering};

use russh::keys::ssh_key::{PrivateKey, private::Ed25519Keypair};
use russh::server::{self, Session};
use russh::{Channel, ChannelId};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

use super::*;

const PASSWORD: &str = "synthetic-probe-password";
const USER: &str = "probe-user";

#[derive(Default)]
struct Counts {
    authentications: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
    connections: AtomicUsize,
    forwarded: AtomicUsize,
}

struct Active(Arc<Counts>, bool);
impl Drop for Active {
    fn drop(&mut self) {
        if self.1 {
            self.0.connections.fetch_sub(1, Ordering::SeqCst);
        } else {
            self.0.active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

struct SshHandler {
    counts: Arc<Counts>,
    channels: HashMap<ChannelId, Channel<server::Msg>>,
    workers: JoinSet<()>,
    delay: Duration,
    auth_delay: Duration,
}

impl server::Handler for SshHandler {
    type Error = russh::Error;

    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<server::Auth, Self::Error> {
        if user == USER && password == PASSWORD {
            tokio::time::sleep(self.auth_delay).await;
            self.counts.authentications.fetch_add(1, Ordering::SeqCst);
            Ok(server::Auth::Accept)
        } else {
            Ok(server::Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            })
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<server::Msg>,
        host: &str,
        port: u32,
        _: &str,
        _: u32,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.counts.forwarded.fetch_add(1, Ordering::SeqCst);
        if host == "blocked.internal" || !host.ends_with(".internal") {
            return Ok(());
        }
        let Ok(port) = u16::try_from(port) else {
            return Ok(());
        };
        let Ok(mut socket) = TcpStream::connect(("127.0.0.1", port)).await else {
            return Ok(());
        };
        reply.accept().await;
        self.workers.spawn(async move {
            let mut tunnel = channel.into_stream();
            let _ = tokio::io::copy_bidirectional(&mut tunnel, &mut socket).await;
        });
        Ok(())
    }

    async fn exec_request(
        &mut self,
        id: ChannelId,
        command: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        assert_eq!(command, b"hostname");
        session.channel_success(id)?;
        let channel = self.channels.remove(&id).unwrap();
        let counts = Arc::clone(&self.counts);
        let delay = self.delay;
        self.workers.spawn(async move {
            let active = counts.active.fetch_add(1, Ordering::SeqCst) + 1;
            counts.peak.fetch_max(active, Ordering::SeqCst);
            let _active = Active(counts, false);
            tokio::time::sleep(delay).await;
            if channel.data(&b"loopback-host\n"[..]).await.is_ok() {
                let _ = channel.exit_status(0).await;
                let _ = channel.close().await;
            }
        });
        Ok(())
    }
}

struct SshServer {
    port: u16,
    fingerprint: String,
    counts: Arc<Counts>,
    task: tokio::task::JoinHandle<()>,
}

impl SshServer {
    fn start(delay: Duration) -> Self {
        Self::delayed(delay, Duration::ZERO)
    }

    fn delayed(delay: Duration, auth_delay: Duration) -> Self {
        crate::ssh::runtime().block_on(async {
            let key = PrivateKey::new(Ed25519Keypair::from_seed(&[7; 32]).into(), "loopback probe fixture").unwrap();
            let fingerprint = key.public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string();
            let config = Arc::new(server::Config { keys: vec![key], auth_rejection_time: Duration::ZERO, ..Default::default() });
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let counts = Arc::new(Counts::default());
            let shared = Arc::clone(&counts);
            let task = tokio::spawn(async move {
                let mut sessions = JoinSet::new();
                loop {
                    tokio::select! {
                        accepted = listener.accept() => {
                            let (socket, _) = accepted.unwrap();
                            let counts = Arc::clone(&shared);
                            let handler = SshHandler { counts: Arc::clone(&counts), channels: HashMap::new(), workers: JoinSet::new(), delay, auth_delay };
                            let config = Arc::clone(&config);
                            sessions.spawn(async move {
                                counts.connections.fetch_add(1, Ordering::SeqCst);
                                let _connection = Active(counts, true);
                                if let Ok(session) = server::run_stream(config, socket, handler).await { let _ = session.await; }
                            });
                        }
                        _ = sessions.join_next(), if !sessions.is_empty() => {}
                    }
                }
            });
            Self { port, fingerprint, counts, task }
        })
    }

    fn host(&self, alias: &str) -> HostProfile {
        HostProfile {
            alias: alias.into(),
            address: "127.0.0.1".into(),
            port: self.port,
            username: USER.into(),
            host_fingerprint: Some(self.fingerprint.clone()),
            host_key_algorithm: Some("ssh-ed25519".into()),
            ..Default::default()
        }
    }
}

impl Drop for SshServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn app(hosts: Vec<HostProfile>) -> HostsApp {
    let mut app = tests::metadata_test_app(&egui::Context::default(), "en", hosts[0].clone());
    app.store.hosts = hosts;
    app.editor = None;
    app
}

fn wait(app: &mut HostsApp) -> Duration {
    let start = Instant::now();
    while !app.tests_idle() {
        app.poll_tests();
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "GUI probes must finish"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    start.elapsed()
}

struct FakeBw {
    profile: crate::model::BitwardenProfile,
    log: PathBuf,
}
impl Drop for FakeBw {
    fn drop(&mut self) {
        crate::bitwarden::session::forget(self.profile.id).unwrap();
    }
}

fn configure_bw(app: &mut HostsApp, delay: u64) -> FakeBw {
    let profile = crate::model::BitwardenProfile {
        email: "probe@example.com".into(),
        folder_id: Some(Uuid::new_v4()),
        policy: crate::model::ReauthPolicy::Permanent,
        ..Default::default()
    };
    let directory = crate::bitwarden::session::data_dir(profile.id).unwrap();
    let executable = std::env::current_exe().unwrap();
    let cli = executable
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/fake_bw.exe");
    let items: Vec<_> = app
        .store
        .hosts
        .iter_mut()
        .map(|host| {
            host.vault = Some(VaultHostRef {
                provider: VaultProvider::Bitwarden,
                item_id: host.id,
                item_name: host.alias.clone(),
                revision_date: "2026-11-08T00:00:00.000Z".into(),
                external_id: None,
            });
            json!({ "id": host.id, "type": 1, "name": host.alias, "folderId": profile.folder_id,
            "revisionDate": "2026-11-08T00:00:00.000Z", "login": { "username": host.username,
                "password": PASSWORD, "uris": [{"uri": super::item_uri(host)}] } })
        })
        .collect();
    fs::write(
        directory.join("fake-vault.json"),
        json!({"session": "fake-session", "items": items, "delay_ms": delay}).to_string(),
    )
    .unwrap();
    crate::bitwarden::session::store(
        &profile,
        &codex_hosts_bw::SessionKey::new("fake-session".into()),
    )
    .unwrap();
    crate::bitwarden::session::mark_synced(profile.id).unwrap();
    app.store.bitwarden.cli_path = Some(cli);
    app.store.bitwarden.profile = Some(profile.clone());
    app.store.save().unwrap();
    FakeBw {
        profile,
        log: directory.join("fake-bw.log"),
    }
}

fn configure_op(app: &mut HostsApp, stream: bool) -> PathBuf {
    use crate::onepassword::cli::test_support::{ACCOUNT, VAULT};
    let directory = crate::storage::data_directory().unwrap().join("fake-op");
    fs::create_dir(&directory).unwrap();
    let cli = directory.join("op.exe");
    let executable = std::env::current_exe().unwrap();
    fs::copy(
        executable
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples/fake_op.exe"),
        &cli,
    )
    .unwrap();
    let mut replaced = HashMap::new();
    for (index, host) in app.store.hosts.iter_mut().enumerate() {
        let external = format!("{:026}", index + 1);
        let key = crate::vault_hosts::ItemKey::onepassword(ACCOUNT, &external);
        replaced.insert(host.id, key.id);
        host.id = key.id;
        host.vault = Some(VaultHostRef {
            provider: VaultProvider::OnePassword,
            item_id: key.id,
            external_id: Some(external),
            item_name: host.alias.clone(),
            revision_date: "1".into(),
        });
    }
    for host in &mut app.store.hosts {
        if let Some(jump) = host.jump_host {
            host.jump_host = Some(replaced[&jump]);
        }
    }
    let items: Vec<_> = app.store.hosts.iter().map(|host| json!({
        "id": host.vault().unwrap().external_id, "title": host.alias, "version": 1,
        "vault": {"id": VAULT, "name": "fake-op"}, "category": "LOGIN",
        "urls": [{"href": super::item_uri(host), "primary": true}],
        "fields": [{"purpose": "USERNAME", "value": host.username}, {"purpose": "PASSWORD", "value": PASSWORD}],
    })).collect();
    let path = directory.join("fake-op.json");
    fs::write(&path, json!({ "account": {"account_uuid": ACCOUNT, "url": "test.1password.com"},
        "vault": {"id": VAULT, "name": "fake-op"}, "items": items, "failures": [], "stream": stream }).to_string()).unwrap();
    app.store.onepassword.cli_path = Some(cli);
    app.store.onepassword.account = Some(crate::model::OnePasswordAccount {
        account_id: ACCOUNT.into(),
        vault_id: VAULT.into(),
        vault_name: "fake-op".into(),
        ..Default::default()
    });
    app.store.save().unwrap();
    path
}

fn op_calls(path: &Path) -> Vec<Value> {
    fs::read_to_string(path.with_file_name("fake-op-calls.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn state(app: &HostsApp, alias: &str) -> HostTestState {
    app.test_states[&app.store.find_alias(alias).unwrap().id]
}

#[test]
fn multi_hop_tests_share_jumps_and_isolate_a_failed_forward() {
    if !tests::in_isolated_store(
        "ui::probe_tests::multi_hop_tests_share_jumps_and_isolate_a_failed_forward",
    ) {
        return;
    }
    let root = SshServer::start(Duration::ZERO);
    let middle = SshServer::start(Duration::ZERO);
    let target = SshServer::start(Duration::from_millis(150));
    let root_host = root.host("root");
    let middle_host = HostProfile {
        address: "middle.internal".into(),
        jump_host: Some(root_host.id),
        ..middle.host("middle")
    };
    let good_a = HostProfile {
        address: "target.internal".into(),
        jump_host: Some(middle_host.id),
        ..target.host("good-a")
    };
    let good_b = HostProfile {
        address: "target.internal".into(),
        jump_host: Some(middle_host.id),
        ..target.host("good-b")
    };
    let bad = HostProfile {
        address: "blocked.internal".into(),
        jump_host: Some(middle_host.id),
        ..target.host("blocked")
    };
    let mut app = app(vec![root_host, middle_host, good_a, bad, good_b]);
    let fixture = configure_op(&mut app, true);
    app.start_all_tests();
    wait(&mut app);
    for alias in ["root", "middle", "good-a", "good-b"] {
        assert_eq!(state(&app, alias), HostTestState::Succeeded, "{alias}");
    }
    assert_eq!(state(&app, "blocked"), HostTestState::Failed);
    assert_eq!(root.counts.authentications.load(Ordering::SeqCst), 1);
    assert_eq!(middle.counts.authentications.load(Ordering::SeqCst), 1);
    assert_eq!(target.counts.authentications.load(Ordering::SeqCst), 2);
    assert_eq!(root.counts.forwarded.load(Ordering::SeqCst), 1);
    assert_eq!(middle.counts.forwarded.load(Ordering::SeqCst), 3);
    let calls = op_calls(&fixture);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["args"][1], "list");
    assert_eq!(calls[1]["args"][1], "get");
    assert_eq!(calls[1]["args"].as_array().unwrap().last().unwrap(), "-");
}

#[test]
fn credential_preparation_and_each_jump_have_separate_time_budgets() {
    if !tests::in_isolated_store(
        "ui::probe_tests::credential_preparation_and_each_jump_have_separate_time_budgets",
    ) {
        return;
    }
    let _credentials = credentials::test_lock();
    let root = SshServer::delayed(Duration::ZERO, Duration::from_millis(170));
    let middle = SshServer::delayed(Duration::ZERO, Duration::from_millis(170));
    let target = SshServer::delayed(Duration::ZERO, Duration::from_millis(170));
    let root_host = root.host("root");
    let middle_host = HostProfile {
        address: "middle.internal".into(),
        jump_host: Some(root_host.id),
        ..middle.host("middle")
    };
    let target_host = HostProfile {
        address: "target.internal".into(),
        jump_host: Some(middle_host.id),
        ..target.host("target")
    };
    let id = target_host.id;
    let mut app = app(vec![root_host, middle_host, target_host]);
    let _fixture = configure_bw(&mut app, 500);
    let (sender, receiver) = mpsc::channel();
    let start = Instant::now();
    let _worker = connection::probes::start(
        Arc::new(app.store.hosts.clone()),
        vec![id],
        connection::probes::Limits {
            network: Duration::from_millis(400),
            ..Default::default()
        },
        move |_, result| {
            sender.send(result).unwrap();
        },
    );
    let outcome = receiver.recv_timeout(Duration::from_secs(8)).unwrap();
    let result = outcome.result.unwrap();
    assert_eq!(result.verified_host_keys.len(), 3);
    assert_eq!(result.stdout.trim(), "loopback-host");
    assert!(start.elapsed() > Duration::from_millis(900));
}

#[test]
fn cancelling_a_batch_releases_its_target_and_jump_connections() {
    if !tests::in_isolated_store(
        "ui::probe_tests::cancelling_a_batch_releases_its_target_and_jump_connections",
    ) {
        return;
    }
    let root = SshServer::start(Duration::ZERO);
    let target = SshServer::start(Duration::from_secs(60));
    let jump = root.host("jump");
    let host = HostProfile {
        address: "target.internal".into(),
        jump_host: Some(jump.id),
        ..target.host("target")
    };
    let mut app = app(vec![jump, host]);
    let _fixture = configure_op(&mut app, false);
    app.start_all_tests();
    let start = Instant::now();
    while target.counts.active.load(Ordering::SeqCst) == 0 {
        app.poll_tests();
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(2));
    }
    app.test_operations.clear();
    let start = Instant::now();
    while root.counts.connections.load(Ordering::SeqCst)
        + target.counts.connections.load(Ordering::SeqCst)
        != 0
    {
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "cancelled SSH connections must close promptly"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_new_test_operation_reads_changed_credentials_again() {
    if !tests::in_isolated_store(
        "ui::probe_tests::a_new_test_operation_reads_changed_credentials_again",
    ) {
        return;
    }
    let server = SshServer::start(Duration::ZERO);
    let mut app = app(vec![server.host("first"), server.host("second")]);
    let path = configure_op(&mut app, false);
    app.start_all_tests();
    wait(&mut app);
    assert_eq!(state(&app, "first"), HostTestState::Succeeded);
    let mut data: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    data["items"][0]["fields"][1]["value"] = json!("changed-synthetic-password");
    data["items"][0]["version"] = json!(2);
    fs::write(&path, data.to_string()).unwrap();
    app.start_all_tests();
    wait(&mut app);
    assert_eq!(state(&app, "first"), HostTestState::Failed);
    assert_eq!(state(&app, "second"), HostTestState::Succeeded);
    assert_eq!(op_calls(&path).len(), 4);
}

#[test]
fn thirteen_vault_hosts_use_one_read_and_parallel_network_probes() {
    if !tests::in_isolated_store(
        "ui::probe_tests::thirteen_vault_hosts_use_one_read_and_parallel_network_probes",
    ) {
        return;
    }
    let _credentials = credentials::test_lock();
    let server = SshServer::start(Duration::from_millis(180));
    let mut app = app((0..13)
        .map(|index| server.host(&format!("host-{index}")))
        .collect());
    let fixture = configure_bw(&mut app, 100);
    let serial_hosts = app.store.hosts.clone();
    // The existing unoptimized vault/SSH future needs the runtime's stack
    // size; the benchmark thread matches it instead of the test harness's 2 MiB.
    let serial = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let start = Instant::now();
            for host in &serial_hosts {
                crate::ssh::runtime()
                    .block_on(connection::probe_async(
                        host,
                        &serial_hosts,
                        crate::ssh::OperationLimits {
                            total_timeout: Some(Duration::from_secs(10)),
                            connect_timeout: Some(Duration::from_secs(10)),
                            command_timeout: Some(Duration::from_secs(10)),
                            ..Default::default()
                        },
                    ))
                    .unwrap();
            }
            start.elapsed()
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(
        fs::read_to_string(&fixture.log)
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("get item"))
            .count(),
        13
    );
    fs::remove_file(&fixture.log).unwrap();
    server.counts.peak.store(0, Ordering::SeqCst);
    app.start_all_tests();
    let parallel = wait(&mut app);
    assert!(
        app.test_states
            .values()
            .all(|state| *state == HostTestState::Succeeded)
    );
    assert_eq!(app.test_states.len(), 13);
    let calls = fs::read_to_string(&fixture.log).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.starts_with("list items"))
            .count(),
        1
    );
    assert!(!calls.lines().any(|line| line.starts_with("get item")));
    assert!(server.counts.peak.load(Ordering::SeqCst) >= 2);
    assert!(server.counts.peak.load(Ordering::SeqCst) <= 8);
    assert!(
        parallel * 2 < serial,
        "serial {serial:?}; parallel {parallel:?}"
    );
    println!(
        "Probe benchmark: 13 hosts, serial {serial:?}, parallel {parallel:?}, bulk CLI reads 1"
    );
}

#[test]
fn waiting_for_a_vault_does_not_block_an_independent_local_host() {
    if !tests::in_isolated_store(
        "ui::probe_tests::waiting_for_a_vault_does_not_block_an_independent_local_host",
    ) {
        return;
    }
    let _credentials = credentials::test_lock();
    let server = SshServer::start(Duration::ZERO);
    let mut app = app(vec![server.host("vault"), server.host("local")]);
    let _fixture = configure_bw(&mut app, 1500);
    let local = &mut app.store.hosts[1];
    local.vault = None;
    let local_id = local.id;
    let kind = credentials::login_password_kind(local);
    credentials::store(local_id, kind, PASSWORD).unwrap();
    let (sender, receiver) = mpsc::channel();
    let _worker = connection::probes::start(
        Arc::new(app.store.hosts.clone()),
        app.store.hosts.iter().map(|host| host.id).collect(),
        connection::probes::Limits {
            concurrency: 1,
            ..Default::default()
        },
        move |id, outcome| {
            sender.send((id, outcome.result)).unwrap();
        },
    );
    let first = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    credentials::delete(local_id, kind).unwrap();
    let second = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(first.0, local_id);
    assert!(first.1.is_ok());
    assert!(second.1.is_ok());
}

#[test]
fn a_large_batch_reuses_its_jump_without_exhausting_connection_capacity() {
    if !tests::in_isolated_store(
        "ui::probe_tests::a_large_batch_reuses_its_jump_without_exhausting_connection_capacity",
    ) {
        return;
    }
    let root = SshServer::start(Duration::ZERO);
    let target = SshServer::start(Duration::from_millis(30));
    let jump = root.host("jump");
    let mut hosts = vec![jump.clone()];
    hosts.extend((0..40).map(|index| HostProfile {
        address: "target.internal".into(),
        jump_host: Some(jump.id),
        ..target.host(&format!("target-{index}"))
    }));
    let mut app = app(hosts);
    let _fixture = configure_op(&mut app, false);
    app.start_all_tests();
    wait(&mut app);
    assert_eq!(app.test_states.len(), 41);
    assert!(
        app.test_states
            .values()
            .all(|state| *state == HostTestState::Succeeded)
    );
    assert_eq!(root.counts.authentications.load(Ordering::SeqCst), 1);
    assert_eq!(target.counts.authentications.load(Ordering::SeqCst), 40);
    assert!(target.counts.peak.load(Ordering::SeqCst) <= 8);
}

#[test]
fn a_jump_key_prompt_updates_the_failed_hop_and_retries_the_original_target() {
    if !tests::in_isolated_store(
        "ui::probe_tests::a_jump_key_prompt_updates_the_failed_hop_and_retries_the_original_target",
    ) {
        return;
    }
    let jump = HostProfile {
        alias: "jump".into(),
        address: "127.0.0.1".into(),
        port: 1,
        ..Default::default()
    };
    let target = HostProfile {
        alias: "target".into(),
        jump_host: Some(jump.id),
        ..Default::default()
    };
    let mut app = app(vec![jump.clone(), target.clone()]);
    app.selected = Some(target.id);
    let mut error = RemoteFailure::new("HOSTKEY_UNKNOWN", "Untrusted jump host.");
    error.host_id = Some(jump.id);
    error.host_alias = Some("jump".into());
    error.host_key = Some(Box::new(crate::ssh::HostKeyFailure {
        observed_fingerprint: Some("SHA256:jump".into()),
        ..Default::default()
    }));
    app.finish_test(target.id, connection::probes::Outcome::failed(error));
    let prompt = app.fingerprint_prompt.as_ref().unwrap();
    assert_eq!(prompt.host_id, jump.id);
    assert_eq!(prompt.retry_test, Some(target.id));
    app.apply_fingerprint_choice(true, &egui::Context::default());
    assert_eq!(
        app.store.hosts[0].host_fingerprint.as_deref(),
        Some("SHA256:jump")
    );
    assert!(app.store.hosts[1].host_fingerprint.is_none());
    assert!(app.test_operations.contains_key(&target.id));
    assert!(!app.test_operations.contains_key(&jump.id));
    app.test_operations.clear();
}
