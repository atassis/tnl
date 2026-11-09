#![cfg(unix)]

use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn intentional_signals_release_name_before_exit_with_and_without_inspection() {
    tokio::time::timeout(Duration::from_secs(25), assert_signal_shutdown())
        .await
        .expect("bounded real signal/restart scenario");
}

async fn assert_signal_shutdown() {
    let tokens = tempfile::NamedTempFile::new().unwrap();
    let tf = tnld::auth::TokensFile {
        tokens: vec![tnld::auth::TokenEntry {
            name: "fixture".into(),
            hash: tnld::auth::hash_plaintext("tnl_U").unwrap(),
        }],
    };
    std::fs::write(tokens.path(), toml::to_string(&tf).unwrap()).unwrap();
    let (daemon, state) = tnld::serve::spawn_server_with_state(tnld::config::Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://x".into(),
        hostname_root: "t.x".into(),
        tokens_file: tokens.path().to_string_lossy().into_owned(),
        trusted_proxies: vec![],
        session_grace_sec: 3600,
    })
    .await
    .unwrap();
    let cfg = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        cfg.path(),
        format!(
            "endpoint = \"http://{}\"\ntoken = \"tnl_U\"\n",
            daemon.local_addr
        ),
    )
    .unwrap();
    let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = backend.local_addr().unwrap().to_string();
    for (signal, inspect) in [("-INT", false), ("-INT", true), ("-TERM", true)] {
        let mut args = vec!["http", &target, "demo", "--quiet"];
        if inspect {
            args.push("--inspect");
        }
        let mut child = Child(
            Command::new(env!("CARGO_BIN_EXE_tnl"))
                .args(args)
                .env("TNL_CONFIG", cfg.path())
                .env_remove("TNL_ENDPOINT")
                .env_remove("TNL_TOKEN")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        tokio::time::timeout(Duration::from_secs(4), async {
            while state.registry.find_by_subdomain("demo").is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("actual CLI registered");
        // Hold a real request open; shutdown must not wait for its response.
        let mut visitor = tokio::net::TcpStream::connect(daemon.local_addr)
            .await
            .unwrap();
        visitor
            .write_all(b"GET /held HTTP/1.1\r\nHost: demo.t.x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let (mut local, _) = tokio::time::timeout(Duration::from_secs(3), backend.accept())
            .await
            .unwrap()
            .unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(local.read_u8().await.unwrap());
        }
        assert!(Command::new("kill")
            .args([signal, &child.0.id().to_string()])
            .status()
            .unwrap()
            .success());
        let status = wait_exit(&mut child).await;
        assert!(
            status.success(),
            "handled signal must exit successfully: {signal}: {status}"
        );
        assert!(
            state.registry.find_by_subdomain("demo").is_none(),
            "name must be released before CLI exit, not after grace: {signal}, inspect={inspect}"
        );
        let mut byte = [0];
        let read = tokio::time::timeout(Duration::from_secs(1), local.read(&mut byte))
            .await
            .expect("active backend must close immediately");
        match read {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::BrokenPipe
                ) => {}
            other => panic!("unexpected backend shutdown result: {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ctrl_c_during_registration_closes_a_late_acknowledged_session() {
    assert_registration_shutdown(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_close_ack_is_bounded_and_never_reported_as_released() {
    assert_registration_shutdown(false).await;
}

async fn control_read(
    stream: &mut std::pin::Pin<Box<dyn tnl_protocol::Stream>>,
) -> tnl_protocol::ControlMsg {
    let n = stream.read_u32().await.unwrap();
    assert!(n < 4096);
    let mut payload = vec![0; n as usize];
    stream.read_exact(&mut payload).await.unwrap();
    serde_json::from_slice(&payload).unwrap()
}

async fn control_write(
    stream: &mut std::pin::Pin<Box<dyn tnl_protocol::Stream>>,
    msg: &tnl_protocol::ControlMsg,
) {
    let payload = serde_json::to_vec(msg).unwrap();
    stream
        .write_u32(u32::try_from(payload.len()).unwrap())
        .await
        .unwrap();
    stream.write_all(&payload).await.unwrap();
    stream.flush().await.unwrap();
}

#[allow(clippy::too_many_lines, clippy::result_large_err)] // Real CLI registration, signal and delayed-control boundaries.
async fn assert_registration_shutdown(acknowledge: bool) {
    use std::io::{BufRead, BufReader, Read};
    use tnl_protocol::{ClosingReason, ControlMsg, Session as _};
    tokio::time::timeout(Duration::from_secs(10), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cfg = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            cfg.path(),
            format!(
                "endpoint = \"http://{}\"\ntoken = \"fixture\"\n",
                listener.local_addr().unwrap()
            ),
        )
        .unwrap();
        let (ready, registered) = tokio::sync::oneshot::channel();
        let (finish, finished) = tokio::sync::oneshot::channel();
        let (closing, closed) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (io, _) = listener.accept().await.unwrap();
            let ws = async_tungstenite::tokio::accept_hdr_async(
                io,
                |_: &http::Request<()>, mut response: http::Response<()>| {
                    response.headers_mut().insert(
                        tnl_protocol::control::CONTROL_PROTOCOL_HEADER,
                        http::HeaderValue::from_static(
                            tnl_protocol::control::CONTROL_PROTOCOL_VERSION,
                        ),
                    );
                    Ok(response)
                },
            )
            .await
            .unwrap();
            let mut session = tnl_protocol::transport::server_session_from_ws_generic(ws);
            let mut control = session.accept_stream().await.unwrap().unwrap();
            assert!(matches!(
                control_read(&mut control).await,
                ControlMsg::CreateTunnel(_)
            ));
            ready.send(()).unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            control_write(
                &mut control,
                &ControlMsg::TunnelCreated(tnl_protocol::TunnelCreatedResp {
                    tunnel_id: ulid::Ulid::new(),
                    hostname: "demo.t.x".into(),
                    subdomain: "demo".into(),
                }),
            )
            .await;
            assert_eq!(control_read(&mut control).await, ControlMsg::Close);
            closing.send(()).unwrap();
            if acknowledge {
                control_write(
                    &mut control,
                    &ControlMsg::Closing {
                        reason: ClosingReason::ClientShutdown,
                    },
                )
                .await;
            } else {
                // Keep control alive: EOF must not accidentally satisfy the deadline test.
                finished.await.unwrap();
            }
        });
        let mut child = Child(
            Command::new(env!("CARGO_BIN_EXE_tnl"))
                .args(["http", "3000", "demo", "--quiet", "--inspect"])
                .env("TNL_CONFIG", cfg.path())
                .env_remove("TNL_ENDPOINT")
                .env_remove("TNL_TOKEN")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut diagnostic = BufReader::new(child.0.stderr.take().unwrap());
        let mut line = String::new();
        diagnostic.read_line(&mut line).unwrap();
        let mut inspector = url::Url::parse(
            line.split_whitespace()
                .find(|s| s.starts_with("http://"))
                .unwrap(),
        )
        .unwrap();
        let token = inspector
            .fragment()
            .unwrap()
            .strip_prefix("token=")
            .unwrap()
            .to_owned();
        inspector.set_fragment(None);
        let stats = inspector.join("api/v1/stats").unwrap();
        registered.await.unwrap();
        if !acknowledge {
            tokio::time::timeout(Duration::from_secs(3), async {
                while !inspector_connected(&stats, &token).await {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        assert!(Command::new("kill")
            .args(["-INT", &child.0.id().to_string()])
            .status()
            .unwrap()
            .success());
        closed.await.unwrap();
        if !acknowledge {
            assert!(
                !inspector_connected(&stats, &token).await,
                "shutdown must revoke inspection/replay before waiting for Close ACK"
            );
        }
        let outcome = wait_exit(&mut child).await;
        assert!(outcome.success());
        let mut stdout = String::new();
        child
            .0
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        let mut stderr = String::new();
        diagnostic.read_to_string(&mut stderr).unwrap();
        assert_eq!(stdout.contains("session released"), acknowledge);
        assert_eq!(
            stderr.contains("shutdown was not acknowledged"),
            !acknowledge
        );
        let _ = finish.send(());
        server.await.unwrap();
    })
    .await
    .expect("bounded late-registration/ack fixture");
}

async fn wait_exit(child: &mut Child) -> std::process::ExitStatus {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("bounded signal shutdown")
}

async fn inspector_connected(stats: &url::Url, token: &str) -> bool {
    reqwest::Client::new()
        .get(stats.clone())
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["tunnel"]["connected"]
        .as_bool()
        .unwrap()
}
