use super::*;
use russh::keys::ssh_key::{PrivateKey, private::Ed25519Keypair};
use russh::server::{self, Session};
use russh::{Channel, ChannelId};
use tokio::net::TcpListener;
use tokio::sync::Notify;

#[derive(Default)]
struct Observations {
    received: Mutex<Vec<u8>>,
    closed: Notify,
    sudo_ready: std::sync::atomic::AtomicBool,
}

struct TestServer {
    channels: HashMap<ChannelId, Channel<server::Msg>>,
    workers: JoinSet<()>,
    observations: Arc<Observations>,
}

impl server::Handler for TestServer {
    type Error = russh::Error;

    async fn auth_none(&mut self, _: &str) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
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

    async fn channel_close(&mut self, _: ChannelId, _: &mut Session) -> Result<(), Self::Error> {
        self.observations.closed.notify_one();
        Ok(())
    }

    async fn exec_request(
        &mut self,
        id: ChannelId,
        command: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if command == b"reject" {
            session.channel_failure(id)?;
            return Ok(());
        }
        session.channel_success(id)?;
        let mut channel = self.channels.remove(&id).unwrap();
        let mode = String::from_utf8(command.to_vec()).unwrap();
        let observations = Arc::clone(&self.observations);
        if mode.contains("CH_SUDO_AUTH_") || mode.contains("CH_SU_BEGIN_") {
            self.workers
                .spawn(sudo_fixture(channel, mode, observations));
            return Ok(());
        }
        self.workers.spawn(async move {
            if mode == "early" {
                channel.exit_status(7).await.unwrap();
                channel.close().await.unwrap();
                return;
            }
            if mode == "legacy" {
                channel.data(&b"legacy"[..]).await.unwrap();
                channel.exit_status(0).await.unwrap();
                channel.close().await.unwrap();
                return;
            }
            if mode == "stall" {
                // Do not consume input. The client must cancel and close on timeout.
                std::future::pending::<()>().await;
                return;
            }
            if mode == "duplex" {
                let output = vec![b'x'; MAX_CAPTURE_BYTES + 65536];
                channel.data(output.as_slice()).await.unwrap();
                channel
                    .extended_data(1, &b"stderr-before-input"[..])
                    .await
                    .unwrap();
            }
            let mut input = Vec::new();
            while let Some(message) = channel.wait().await {
                match message {
                    ChannelMsg::Data { data } => input.extend_from_slice(&data),
                    ChannelMsg::Eof => break,
                    ChannelMsg::Close => return,
                    _ => {}
                }
            }
            *observations.received.lock().unwrap() = input.clone();
            channel.data(input.as_slice()).await.unwrap();
            channel.extended_data(1, &b"stderr"[..]).await.unwrap();
            channel.exit_status(0).await.unwrap();
            channel.eof().await.unwrap();
            channel.close().await.unwrap();
        });
        Ok(())
    }
}

async fn sudo_fixture(
    mut channel: Channel<server::Msg>,
    command: String,
    observations: Arc<Observations>,
) {
    let framed = |label: &str| {
        let start = command.find(label).unwrap();
        let end = command[start..].find('\x1f').unwrap() + start + 1;
        command[start..end].to_owned()
    };
    let su = command.contains("CH_SU_BEGIN_");
    let username = if su {
        "bob"
    } else if command.contains("/sudo-test/rootpw") {
        "root"
    } else if command.contains("/sudo-test/targetpw") {
        "bob"
    } else {
        "alice"
    };
    let (prompt, nonce) = if su {
        (
            "Password: ".to_owned(),
            framed("\x1eCH_SU_BEGIN_")
                .trim_start_matches("\x1eCH_SU_BEGIN_")
                .trim_end_matches('\x1f')
                .to_owned(),
        )
    } else {
        (
            framed("\x1eCH_SUDO_AUTH_").replace("%p", username),
            framed("\x1eCH_SUDO_AUTH_")
                .trim_start_matches("\x1eCH_SUDO_AUTH_")
                .split(':')
                .next()
                .unwrap()
                .to_owned(),
        )
    };
    let method = if su { "SU" } else { "SUDO" };
    let ready = format!("\x1eCH_{method}_READY_{nonce}\x1f");
    if su {
        for byte in framed("\x1eCH_SU_BEGIN_").as_bytes() {
            channel.extended_data(1, &[*byte][..]).await.unwrap();
        }
    }
    if command.contains("/sudo-test/unsupported") {
        channel
            .extended_data(1, &b"sudo: a terminal is required"[..])
            .await
            .unwrap();
        channel.exit_status(1).await.unwrap();
        channel.close().await.unwrap();
        return;
    }
    if command.contains("/sudo-test/truncated") {
        channel
            .data(vec![b'x'; MAX_CAPTURE_BYTES + 4096].as_slice())
            .await
            .unwrap();
    }
    if command.contains("/sudo-test/stall-before") {
        std::future::pending::<()>().await;
        return;
    }
    let passwordless = command.contains("/sudo-test/nopass");
    let mut input = Vec::new();
    if !passwordless {
        // Deliberately split control frames into single-byte SSH packets.
        for byte in prompt.as_bytes() {
            channel.extended_data(1, &[*byte][..]).await.unwrap();
        }
        if command.contains("/sudo-test/stall-input") {
            std::future::pending::<()>().await;
            return;
        }
        while !input.ends_with(b"\n") {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => input.extend_from_slice(&data),
                Some(ChannelMsg::Close) | None => return,
                _ => {}
            }
        }
        *observations.received.lock().unwrap() = input.clone();
        if command.contains("/sudo-test/repeat") {
            channel.extended_data(1, prompt.as_bytes()).await.unwrap();
            // Wait for the client to close, observing whether a second write occurred.
            while let Some(message) = channel.wait().await {
                match message {
                    ChannelMsg::Data { data } => observations
                        .received
                        .lock()
                        .unwrap()
                        .extend_from_slice(&data),
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
            return;
        }
        if command.contains("/sudo-test/echo") {
            for chunk in input[..input.len() - 1].chunks(2) {
                channel.data(chunk).await.unwrap();
                channel.extended_data(1, chunk).await.unwrap();
            }
        }
    }
    for byte in ready.as_bytes() {
        channel.extended_data(1, &[*byte][..]).await.unwrap();
    }
    // A privileged program can reproduce the prompt; it must receive only EOF.
    channel.extended_data(1, prompt.as_bytes()).await.unwrap();
    let mut acknowledgment = Vec::new();
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Data { data } => acknowledgment.extend_from_slice(&data),
            ChannelMsg::Eof => break,
            ChannelMsg::Close => return,
            _ => {}
        }
    }
    assert_eq!(
        acknowledgment,
        format!("CH_{method}_ACK_{nonce}\n").as_bytes()
    );
    *observations.received.lock().unwrap() = input;
    observations.sudo_ready.store(true, Ordering::SeqCst);
    if command.contains("/sudo-test/stall-ready") {
        std::future::pending::<()>().await;
        return;
    }
    channel.data(&b"done"[..]).await.unwrap();
    channel
        .exit_status(if command.contains("/sudo-test/exit-one") {
            1
        } else {
            0
        })
        .await
        .unwrap();
    channel.close().await.unwrap();
}

fn sudo_test_launch(mode: &str) -> crate::sudo::Launch {
    let mut profile = HostProfile::default();
    profile.advanced.sudo = Some(crate::model::SudoSettings {
        enabled: true,
        ..Default::default()
    });
    crate::sudo::launch(&profile, &format!("/sudo-test/{mode}"), &[], Some("root")).unwrap()
}

#[tokio::test]
async fn sudo_selects_actual_auth_user_once_and_ignores_prompts_after_ready() {
    let (client, server, observations) = connect_test_server().await;
    for (mode, username) in [
        ("correct", "alice"),
        ("rootpw", "root"),
        ("targetpw", "bob"),
        ("exit-one", "alice"),
    ] {
        let mut calls = 0;
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_sudo(
                &client,
                sudo_test_launch(mode),
                &CaptureBudget::new(MAX_CAPTURE_BYTES),
                |actual| {
                    calls += 1;
                    assert_eq!(actual, username);
                    Ok(zeroize::Zeroizing::new("test-secret".into()))
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(*observations.received.lock().unwrap(), b"test-secret\n");
        assert_eq!(result.exit_code, if mode == "exit-one" { 1 } else { 0 });
        assert_eq!(result.stdout, "done");
        assert!(
            result.stderr.contains("CH_SUDO_AUTH_"),
            "post-READY caller output is ordinary stderr"
        );
        assert!(!result.stderr.contains("CH_SUDO_READY_"));
    }
    let result = run_sudo(
        &client,
        sudo_test_launch("nopass"),
        &CaptureBudget::new(MAX_CAPTURE_BYTES),
        |_| panic!("passwordless sudo must not load credentials"),
    )
    .await
    .unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(observations.received.lock().unwrap().is_empty());
    stop(client, server).await;
}

#[tokio::test]
async fn sudo_truncated_output_does_not_hide_protocol_and_echo_is_redacted() {
    let (client, server, observations) = connect_test_server().await;
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        run_sudo(
            &client,
            sudo_test_launch("truncated"),
            &CaptureBudget::new(128),
            |_| Ok(zeroize::Zeroizing::new("secret".into())),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(result.output_truncated);
    assert_eq!(result.exit_code, 0);
    assert_eq!(*observations.received.lock().unwrap(), b"secret\n");
    let result = run_sudo(
        &client,
        sudo_test_launch("echo"),
        &CaptureBudget::new(MAX_CAPTURE_BYTES),
        |_| Ok(zeroize::Zeroizing::new("complex' secret".into())),
    )
    .await
    .unwrap();
    assert_eq!(result.stdout, "[REDACTED]done");
    assert!(result.stderr.starts_with("[REDACTED]"));
    assert!(!result.stderr.contains("complex' secret"));
    stop(client, server).await;
}

#[tokio::test]
async fn sudo_repeat_missing_and_policy_failure_close_without_guessing() {
    let (client, server, observations) = connect_test_server().await;
    let mut calls = 0;
    let error = run_sudo(
        &client,
        sudo_test_launch("repeat"),
        &CaptureBudget::new(MAX_CAPTURE_BYTES),
        |_| {
            calls += 1;
            Ok(zeroize::Zeroizing::new("wrong".into()))
        },
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.code, "SUDO_AUTH_REPEATED");
    assert_eq!(calls, 1);
    assert_eq!(*observations.received.lock().unwrap(), b"wrong\n");
    let error = run_sudo(
        &client,
        sudo_test_launch("correct"),
        &CaptureBudget::new(MAX_CAPTURE_BYTES),
        |_| Err(RemoteFailure::new("SUDO_CREDENTIAL_MISSING", "alice")),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.code, "SUDO_CREDENTIAL_MISSING");
    let error = run_sudo(
        &client,
        sudo_test_launch("unsupported"),
        &CaptureBudget::new(MAX_CAPTURE_BYTES),
        |_| panic!("no framed prompt"),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.code, "SUDO_START_FAILED");
    assert!(error.message.contains("terminal is required"));
    stop(client, server).await;
}

#[tokio::test]
async fn sudo_timeout_drops_input_and_releases_channel_in_each_auth_phase() {
    for mode in ["stall-before", "stall-input", "stall-ready"] {
        let (client, server, observations) = connect_test_server().await;
        let error = {
            let budget = CaptureBudget::new(0);
            let operation = run_sudo(&client, sudo_test_launch(mode), &budget, |_| {
                Ok(zeroize::Zeroizing::new("x".repeat(MAX_STDIN_BYTES)))
            });
            tokio::pin!(operation);
            if mode == "stall-ready" {
                // Time the stalled phase, not scheduling + transferring 2 MiB
                // under concurrent UI tests. Still bound setup independently.
                tokio::time::timeout(Duration::from_secs(10), async {
                    tokio::select! {
                        result = &mut operation => panic!("operation ended before READY (success: {})", result.is_ok()),
                        _ = async {
                            while !observations.sudo_ready.load(Ordering::SeqCst) {
                                tokio::time::sleep(Duration::from_millis(1)).await;
                            }
                        } => {}
                    }
                }).await.unwrap();
            }
            run_with_optional_timeout(
                Some(Duration::from_millis(500)),
                "COMMAND_TIMEOUT",
                "test deadline",
                &mut operation,
            )
            .await
            .err()
            .unwrap()
        };
        assert_eq!(error.code, "COMMAND_TIMEOUT");
        if mode == "stall-ready" {
            assert!(
                observations.sudo_ready.load(Ordering::SeqCst),
                "the timeout occurred after READY and EOF"
            );
        }
        tokio::time::timeout(Duration::from_secs(3), observations.closed.notified())
            .await
            .unwrap();
        let result = run_command(&client, "legacy", None, &CaptureBudget::new(128))
            .await
            .unwrap();
        assert_eq!(result.stdout, "legacy");
        stop(client, server).await;
    }
}

fn su_test_launch(mode: &str) -> crate::sudo::Launch<crate::su::AuthProtocol> {
    let nonce = uuid::Uuid::new_v4();
    crate::sudo::Launch {
        user: "bob".into(),
        command: format!("/sudo-test/{mode} \x1eCH_SU_BEGIN_{nonce}\x1f"),
        protocol: crate::su::AuthProtocol::new(nonce, "bob".into()),
        ack: format!("CH_SU_ACK_{nonce}\n").into_bytes(),
    }
}

#[tokio::test]
async fn su_passwordless_and_password_paths_redact_echoes_and_stop_after_ready() {
    for mode in ["nopass", "echo", "truncated"] {
        let (client, server, observations) = connect_test_server().await;
        let mut loads = 0;
        let result = run_sudo(
            &client,
            su_test_launch(mode),
            &CaptureBudget::new(128),
            |user| {
                loads += 1;
                assert_eq!(user, "bob");
                Ok(zeroize::Zeroizing::new("fixture-password".into()))
            },
        )
        .await
        .unwrap();
        assert_eq!(loads, if mode == "nopass" { 0 } else { 1 });
        assert!(!result.stdout.contains("fixture-password"));
        assert!(!result.stderr.contains("fixture-password"));
        assert!(observations.sudo_ready.load(Ordering::SeqCst));
        stop(client, server).await;
    }
}

#[tokio::test]
async fn su_timeout_closes_the_channel_in_each_authentication_phase() {
    for mode in ["stall-before", "stall-input", "stall-ready"] {
        let (client, server, observations) = connect_test_server().await;
        let result = run_with_optional_timeout(
            Some(Duration::from_millis(500)),
            "COMMAND_TIMEOUT",
            "test deadline",
            run_sudo(
                &client,
                su_test_launch(mode),
                &CaptureBudget::new(0),
                |_| Ok(zeroize::Zeroizing::new("x".repeat(MAX_STDIN_BYTES))),
            ),
        )
        .await;
        assert_eq!(result.err().unwrap().code, "COMMAND_TIMEOUT");
        tokio::time::timeout(Duration::from_secs(3), observations.closed.notified())
            .await
            .unwrap();
        let result = run_command(&client, "legacy", None, &CaptureBudget::new(128))
            .await
            .unwrap();
        assert_eq!(result.stdout, "legacy");
        stop(client, server).await;
    }
}

async fn connect_test_server() -> (
    client::Handle<ServerKeyObserver>,
    tokio::task::JoinHandle<()>,
    Arc<Observations>,
) {
    // Fresh OS-random UUIDs are sufficient for an ephemeral loopback test key.
    let mut seed = [0; 32];
    seed[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    seed[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let key = PrivateKey::new(Ed25519Keypair::from_seed(&seed).into(), "isolated test").unwrap();
    // russh forwards packets to a bounded application queue before dispatching
    // further control messages. Keep room for the full bounded input so a test
    // that deliberately delays consuming it does not stall the server itself.
    let config = Arc::new(server::Config {
        keys: vec![key],
        window_size: 4096,
        maximum_packet_size: 4096,
        channel_buffer_size: 1024,
        ..Default::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let observations = Arc::new(Observations::default());
    let handler = TestServer {
        channels: HashMap::new(),
        workers: JoinSet::new(),
        observations: Arc::clone(&observations),
    };
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let session = server::run_stream(config, socket, handler).await.unwrap();
        let _ = session.await;
    });
    let config = Arc::new(client::Config {
        window_size: 4096,
        maximum_packet_size: 4096,
        ..Default::default()
    });
    let observer = ServerKeyObserver {
        observed: Arc::new(Mutex::new(None)),
    };
    let mut client = client::connect(config, address, observer).await.unwrap();
    assert!(client.authenticate_none("test").await.unwrap().success());
    (client, server, observations)
}

async fn stop(client: client::Handle<ServerKeyObserver>, server: tokio::task::JoinHandle<()>) {
    let _ = client
        .disconnect(Disconnect::ByApplication, "test complete", "en")
        .await;
    drop(client);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn stdin_exact_utf8_empty_eof_and_legacy_requests() {
    let (client, server, observations) = connect_test_server().await;
    for input in [
        "",
        "中文\nsecond line\r\n",
        "no trailing newline",
        "\0binary-safe-text",
    ] {
        let budget = CaptureBudget::new(MAX_CAPTURE_BYTES);
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_command(&client, "echo", Some(input), &budget),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.stdout, input);
        assert_eq!(result.stderr, "stderr");
        assert_eq!(result.exit_code, 0);
        assert_eq!(*observations.received.lock().unwrap(), input.as_bytes());
    }
    let budget = CaptureBudget::new(MAX_CAPTURE_BYTES);
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run_command(&client, "legacy", None, &budget),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.stdout, "legacy");
    stop(client, server).await;
}

#[tokio::test]
async fn stdin_duplex_exceeds_both_windows_and_drains_truncated_output() {
    let (client, server, observations) = connect_test_server().await;
    let input = "a".repeat(MAX_STDIN_BYTES);
    let budget = CaptureBudget::new(MAX_CAPTURE_BYTES);
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        run_command(&client, "duplex", Some(&input), &budget),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(result.output_truncated);
    assert_eq!(result.stdout.len(), MAX_CAPTURE_BYTES);
    assert_eq!(*observations.received.lock().unwrap(), input.as_bytes());
    stop(client, server).await;
}

#[tokio::test]
async fn stdin_early_exit_and_rejected_exec_do_not_hang() {
    let (client, server, _) = connect_test_server().await;
    let input = "a".repeat(MAX_STDIN_BYTES);
    let budget = CaptureBudget::new(MAX_CAPTURE_BYTES);
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run_command(&client, "early", Some(&input), &budget),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.exit_code, 7);
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run_command(&client, "reject", Some(&input), &budget),
    )
    .await
    .unwrap();
    assert_eq!(result.err().unwrap().code, "REMOTE_EXEC_FAILED");
    stop(client, server).await;
}

#[tokio::test]
async fn stdin_timeout_closes_channel_and_session_can_run_another_command() {
    let (client, server, observations) = connect_test_server().await;
    let input = "a".repeat(MAX_STDIN_BYTES);
    let budget = CaptureBudget::new(MAX_CAPTURE_BYTES);
    let result = run_with_optional_timeout(
        Some(Duration::from_millis(200)),
        "COMMAND_TIMEOUT",
        "test deadline",
        run_command(&client, "stall", Some(&input), &budget),
    )
    .await;
    assert_eq!(result.err().unwrap().code, "COMMAND_TIMEOUT");
    tokio::time::timeout(Duration::from_secs(3), observations.closed.notified())
        .await
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run_command(&client, "legacy", None, &budget),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.exit_code, 0);
    stop(client, server).await;
}
