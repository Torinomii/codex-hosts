//! Reference-only, same-user IPC. Secret values never enter this protocol.
use std::io;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::Duration;

use eframe::egui;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
use uuid::Uuid;

use super::{Envelope, Request, Response};

const MAX_FRAME: usize = 128 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);

fn pipe_name(session: Uuid) -> String {
    format!(r"\\.\pipe\codex-hosts-temporary-{session}")
}

pub(super) struct Server {
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Server {
    pub(super) fn start(
        session: Uuid,
        inbox: mpsc::SyncSender<Envelope>,
        context: egui::Context,
    ) -> io::Result<Self> {
        Self::start_named(session, pipe_name(session), inbox, context)
    }

    pub(super) fn start_discovery(
        session: Uuid,
        inbox: mpsc::SyncSender<Envelope>,
        context: egui::Context,
    ) -> io::Result<Self> {
        Self::start_named(session, discovery_name()?, inbox, context)
    }

    fn start_named(
        session: Uuid,
        name: String,
        inbox: mpsc::SyncSender<Envelope>,
        context: egui::Context,
    ) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            runtime.block_on(async move {
                let mut listener = match create_private_pipe(&name, true) {
                    Ok(listener) => listener,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                if ready_tx.send(Ok(())).is_err() {
                    return;
                }
                let permits = Arc::new(tokio::sync::Semaphore::new(8));
                while !flag.load(Ordering::Relaxed) {
                    let permit = match tokio::time::timeout(
                        Duration::from_millis(100),
                        permits.clone().acquire_owned(),
                    )
                    .await
                    {
                        Ok(Ok(permit)) => permit,
                        _ => continue,
                    };
                    match tokio::time::timeout(Duration::from_millis(100), listener.connect()).await
                    {
                        Err(_) => continue,
                        Ok(Err(_)) => break,
                        Ok(Ok(())) => {}
                    }
                    // Keep the connected handle alive while creating its replacement. No name gap.
                    let next = match create_private_pipe(&name, false) {
                        Ok(next) => next,
                        Err(_) => break,
                    };
                    let mut connected = std::mem::replace(&mut listener, next);
                    let inbox = inbox.clone();
                    let context = context.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        let _ = tokio::time::timeout(IO_TIMEOUT, async {
                            let bytes = read_frame(&mut connected).await?;
                            let request = serde_json::from_slice::<Request>(&bytes);
                            let response = match request {
                                Err(_) => Response::error(session, "TEMPORARY_REQUEST_INVALID"),
                                Ok(request) => {
                                    let (reply, receiver) = tokio::sync::oneshot::channel();
                                    match inbox.try_send(Envelope { request, reply }) {
                                        Ok(()) => {
                                            context.request_repaint();
                                            receiver
                                                .await
                                                .map_err(|_| io::Error::other("window closed"))?
                                        }
                                        Err(_) => Response::error(session, "TEMPORARY_BUSY"),
                                    }
                                }
                            };
                            let bytes = serde_json::to_vec(&response)
                                .map_err(|_| io::Error::other("response invalid"))?;
                            write_frame(&mut connected, &bytes).await
                        })
                        .await;
                    });
                }
            });
        });
        let server = Self {
            stop,
            worker: Some(worker),
        };
        ready_rx
            .recv_timeout(IO_TIMEOUT)
            .map_err(|_| io::Error::other("pipe startup failed"))??;
        Ok(server)
    }
}

pub(super) fn call(session: Uuid, request: Request) -> io::Result<Response> {
    call_named(pipe_name(session), Some(session), request)
}

pub(super) fn discover(request: Request) -> io::Result<Response> {
    call_named(discovery_name()?, None, request)
}

fn call_named(name: String, expected: Option<Uuid>, request: Request) -> io::Result<Response> {
    call_named_with_timeout(name, expected, request, IO_TIMEOUT)
}

fn call_named_with_timeout(
    name: String,
    expected: Option<Uuid>,
    request: Request,
    timeout: Duration,
) -> io::Result<Response> {
    let bytes = serde_json::to_vec(&request).map_err(|_| io::Error::other("request invalid"))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        tokio::time::timeout(timeout, async {
            // Default client QoS is identification-only, not impersonation.
            let mut client = loop {
                match ClientOptions::new().open(&name) {
                    Ok(client) => break client,
                    Err(error)
                        if error.raw_os_error()
                            == Some(windows_sys::Win32::Foundation::ERROR_PIPE_BUSY as i32) =>
                    {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    Err(error) => return Err(error),
                }
            };
            // Only connection acquisition is retried. A lost response may already have effects.
            write_frame(&mut client, &bytes).await?;
            let bytes = read_frame(&mut client).await?;
            let response: Response =
                serde_json::from_slice(&bytes).map_err(|_| io::Error::other("response invalid"))?;
            if expected.is_some_and(|session| response.session != session) {
                return Err(io::Error::other("session mismatch"));
            }
            Ok(response)
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "pipe timeout"))?
    })
}

async fn read_frame<T: AsyncRead + Unpin>(stream: &mut T) -> io::Result<Vec<u8>> {
    let len = stream.read_u32_le().await? as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame limit"));
    }
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes).await?;
    Ok(bytes)
}

async fn write_frame<T: AsyncWrite + Unpin>(stream: &mut T, bytes: &[u8]) -> io::Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame limit"));
    }
    stream.write_u32_le(bytes.len() as u32).await?;
    stream.write_all(bytes).await?;
    stream.flush().await
}

fn current_user_sid() -> io::Result<String> {
    use std::ptr;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, LocalFree},
        Security::{
            Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_QUERY, TOKEN_USER,
            TokenUser,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    // SAFETY: Windows handles and LocalAlloc strings are released; token storage is pointer-aligned.
    unsafe {
        let mut handle = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = Token(handle);
        let mut bytes = 0;
        GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut bytes);
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0usize; (bytes as usize).div_ceil(std::mem::size_of::<usize>())];
        if GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut sid_text = ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut sid_text) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut len = 0;
        while *sid_text.add(len) != 0 {
            len += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(sid_text, len));
        LocalFree(sid_text.cast());
        Ok(sid)
    }
}

fn create_private_pipe(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    use std::ptr;
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SECURITY_ATTRIBUTES,
        },
    };
    let sid = current_user_sid()?;
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: descriptor remains valid throughout synchronous CreateNamedPipe; it is then freed.
    unsafe {
        let mut descriptor = ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let result = ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                name,
                (&mut attrs as *mut SECURITY_ATTRIBUTES).cast(),
            );
        LocalFree(descriptor);
        result
    }
}

fn discovery_name() -> io::Result<String> {
    let directory = crate::storage::data_directory()
        .map_err(|_| io::Error::other("data directory unavailable"))?;
    let directory = std::path::absolute(&directory)?;
    let normalized = directory
        .to_string_lossy()
        .replace('/', "\\")
        .to_lowercase();
    let hash = normalized
        .bytes()
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    Ok(format!(
        r"\\.\pipe\codex-hosts-main-{}-{hash:016x}",
        current_user_sid()?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_pipe_connects_when_a_new_listener_becomes_available() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let session = Uuid::new_v4();
        let name = pipe_name(session);
        let _occupied = create_private_pipe(&name, true).unwrap();
        let _client = ClientOptions::new().open(&name).unwrap();
        assert_eq!(
            ClientOptions::new().open(&name).unwrap_err().raw_os_error(),
            Some(231)
        );
        let target = name.clone();
        let caller = thread::spawn(move || call_named(target, Some(session), Request::Status));
        runtime.block_on(async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut next = create_private_pipe(&name, false).unwrap();
            tokio::time::timeout(Duration::from_secs(2), next.connect())
                .await
                .unwrap()
                .unwrap();
            let request: Request =
                serde_json::from_slice(&read_frame(&mut next).await.unwrap()).unwrap();
            assert!(matches!(request, Request::Status));
            write_frame(
                &mut next,
                &serde_json::to_vec(&Response::error(session, "CONNECTED")).unwrap(),
            )
            .await
            .unwrap();
        });
        assert_eq!(
            caller.join().unwrap().unwrap().code.as_deref(),
            Some("CONNECTED")
        );
    }

    #[test]
    fn busy_connection_is_bounded_and_sent_requests_are_not_replayed() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let session = Uuid::new_v4();
        let name = pipe_name(session);
        let _occupied = create_private_pipe(&name, true).unwrap();
        let _client = ClientOptions::new().open(&name).unwrap();
        let target = name.clone();
        let started = std::time::Instant::now();
        let error = thread::spawn(move || {
            call_named_with_timeout(
                target,
                Some(session),
                Request::Status,
                Duration::from_millis(100),
            )
        })
        .join()
        .unwrap()
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));

        let mut next = create_private_pipe(&name, false).unwrap();
        let target = name.clone();
        let caller = thread::spawn(move || {
            call_named(target, Some(session), Request::Clear { fields: vec![] })
        });
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(2), next.connect())
                .await
                .unwrap()
                .unwrap();
            let request: Request =
                serde_json::from_slice(&read_frame(&mut next).await.unwrap()).unwrap();
            assert!(matches!(request, Request::Clear { .. }));
            let replacement = create_private_pipe(&name, false).unwrap();
            drop(next); // Request may have taken effect, but its response was lost.
            assert!(
                tokio::time::timeout(Duration::from_millis(150), replacement.connect())
                    .await
                    .is_err()
            );
        });
        assert!(caller.join().unwrap().is_err());
    }
    #[test]
    fn pipe_is_exclusive_and_session_expires_on_drop() {
        let session = Uuid::new_v4();
        let (sender, receiver) = mpsc::sync_channel(8);
        let server = Server::start(session, sender, egui::Context::default()).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        assert!(create_private_pipe(&pipe_name(session), true).is_err());
        let handler = thread::spawn(move || {
            let envelope = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
            let _ = envelope
                .reply
                .send(Response::error(session, "TEST_METADATA_ONLY"));
        });
        assert_eq!(
            call(session, Request::Status).unwrap().code.as_deref(),
            Some("TEST_METADATA_ONLY")
        );
        handler.join().unwrap();
        drop(server);
        assert!(call(session, Request::Status).is_err());
    }
    #[test]
    fn oversized_frames_are_rejected_before_allocation() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (mut reader, mut writer) = tokio::io::duplex(8);
            writer.write_u32_le((MAX_FRAME + 1) as u32).await.unwrap();
            assert_eq!(
                read_frame(&mut reader).await.unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        });
    }

    #[test]
    fn missing_pipe_fails_without_retrying_until_the_deadline() {
        let error = call_named_with_timeout(
            pipe_name(Uuid::new_v4()),
            None,
            Request::Status,
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
