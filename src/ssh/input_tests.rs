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
