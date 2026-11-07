//! Real daemon + trusted TLS terminator + local backend. No certificate bypass.
use super::*;
use futures::StreamExt as _;
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::net::{TcpListener, TcpStream};

// Independent daemon fixtures share the process-wide four-worker Basic pool.
// Bound unrelated fixtures, not request concurrency inside each scenario.
static FIXTURE_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

struct Fixture {
    _auth_slot: tokio::sync::SemaphorePermit<'static>,
    server: tnld::serve::ServerHandle,
    state: tnld::serve::AppState,
    endpoint: String,
    connector: tokio_rustls::TlsConnector,
    tls_acceptor: tokio_rustls::TlsAcceptor,
    backend: SocketAddr,
    edge_task: tokio::task::JoinHandle<()>,
    backend_task: tokio::task::JoinHandle<()>,
    seen: tokio::sync::mpsc::UnboundedReceiver<String>,
    count: Arc<AtomicUsize>,
    release: tokio::sync::watch::Sender<bool>,
    _tokens: tempfile::NamedTempFile,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.edge_task.abort();
        self.backend_task.abort();
        self.server.join.abort();
    }
}

async fn head(io: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        bytes.push(io.read_u8().await.unwrap());
        assert!(bytes.len() < 32768);
    }
    String::from_utf8(bytes).unwrap()
}

#[allow(clippy::too_many_lines)]
async fn fixture() -> Fixture {
    let auth_slot = FIXTURE_SLOTS.acquire().await.unwrap();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let tokens = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        tokens.path(),
        toml::to_string(&tnld::auth::TokensFile {
            tokens: vec![tnld::auth::TokenEntry {
                name: "owner".into(),
                hash: tnld::auth::hash_plaintext("tnl_TEST").unwrap(),
            }],
        })
        .unwrap(),
    )
    .unwrap();
    let (server, state) = tnld::serve::spawn_server_with_state(tnld::config::Config {
        listen: "127.0.0.1:0".into(),
        public_url: "https://test".into(),
        hostname_root: "t.x".into(),
        tokens_file: tokens.path().to_str().unwrap().into(),
        trusted_proxies: vec![],
        session_grace_sec: 30,
    })
    .await
    .unwrap();
    let mut params = rcgen::CertificateParams::new(vec!["test-ca".into()]).unwrap();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let ca = params.self_signed(&ca_key).unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["localhost".into()])
        .unwrap()
        .signed_by(&key, &ca, &ca_key)
        .unwrap();
    let tls_server = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            rustls::pki_types::PrivateKeyDer::try_from(key.serialize_der()).unwrap(),
        )
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca.der().clone()).unwrap();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ));
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls_server));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    let daemon = server.local_addr;
    let tls_acceptor = acceptor.clone();
    let edge_task = tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let mut daemon = TcpStream::connect(daemon).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut tls, &mut daemon).await;
            });
        }
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend = listener.local_addr().unwrap();
    let (seen_tx, seen) = tokio::sync::mpsc::unbounded_channel();
    let count = Arc::new(AtomicUsize::new(0));
    let backend_count = count.clone();
    let (release, release_rx) = tokio::sync::watch::channel(false);
    let backend_task = tokio::spawn(async move {
        loop {
            let (mut io, _) = listener.accept().await.unwrap();
            let seen_tx = seen_tx.clone();
            let count = backend_count.clone();
            let mut release = release_rx.clone();
            tokio::spawn(async move {
                count.fetch_add(1, Ordering::SeqCst);
                let request = head(&mut io).await;
                let _ = seen_tx.send(request.clone());
                if request.to_ascii_lowercase().contains("upgrade: websocket") {
                    let key = request
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(name, _)| name.eq_ignore_ascii_case("sec-websocket-key"))
                                .map(|(_, value)| value.trim())
                        })
                        .unwrap();
                    let accept = async_tungstenite::tungstenite::handshake::derive_accept_key(
                        key.as_bytes(),
                    );
                    io.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nConnection: upgrade, X-Tnl-Request-Id\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\nX-Tnl-Request-Id: backend-forgery\r\nX-Tnl-Request-Id: 00000000000000000000000000\r\n\r\n").as_bytes()).await.unwrap();
                    let mut ws = async_tungstenite::WebSocketStream::from_raw_socket(
                        async_tungstenite::tokio::TokioAdapter::new(io),
                        async_tungstenite::tungstenite::protocol::Role::Server,
                        None,
                    )
                    .await;
                    while let Some(Ok(message)) = ws.next().await {
                        if message.is_close() {
                            let _ = ws.close(None).await;
                            break;
                        }
                        ws.send(message).await.unwrap();
                    }
                } else if request.starts_with("POST /early ") {
                    io.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                        .await
                        .unwrap();
                    let mut upload = Vec::new();
                    let _ = io.read_to_end(&mut upload).await;
                    let _ = seen_tx.send(format!("UPLOAD_COMPLETE:{}", upload.len()));
                } else if request.starts_with("GET /hold ") {
                    io.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\nTransfer-Encoding: chunked\r\nTrailer: X-Complete\r\n\r\n4\r\npart\r\n").await.unwrap();
                    if !*release.borrow_and_update() {
                        release.changed().await.unwrap();
                    }
                    let _ = io.write_all(b"0\r\nX-Complete: yes\r\n\r\n").await;
                } else {
                    io.write_all(
                        b"HTTP/1.1 200 OK\r\nConnection: close, X-Tnl-Request-Id\r\nX-Tnl-Request-Id: backend-forgery\r\nX-Tnl-Request-Id: 00000000000000000000000000\r\nContent-Length: 2\r\n\r\nok",
                    )
                    .await
                    .unwrap();
                }
            });
        }
    });
    Fixture {
        _auth_slot: auth_slot,
        server,
        state,
        endpoint,
        connector,
        tls_acceptor,
        backend,
        edge_task,
        backend_task,
        seen,
        count,
        release,
        _tokens: tokens,
    }
}

fn options(basic: bool) -> TunnelOptions {
    let basic = basic.then(|| {
        tnl_protocol::access::BasicVerifier::new(
            "user".into(),
            tnl_protocol::access::SecretVerifier::new(
                tnld::auth::hash_plaintext("VISITOR_SENTINEL").unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
    });
    TunnelOptions {
        access: Some(Arc::new(
            tnl_protocol::access::VisitorAccessPolicyV1::new(basic, &[], None, Some(1)).unwrap(),
        )),
    }
}

fn forward(connected: ConnectedSession, backend: SocketAddr) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let _control = connected.control;
        let mut ctx = crate::forwarder::ForwardCtx::new(
            connected.subdomain.clone(),
            None,
            env!("CARGO_PKG_VERSION"),
        );
        ctx.data_stream_protocol = connected.data_stream_protocol;
        let _ = run_accept_loop(
            connected.session,
            crate::target::Target::Explicit(backend),
            ctx,
        )
        .await;
    })
}

fn visitor(fixture: &Fixture, path: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .get(format!("http://{}{path}", fixture.server.local_addr))
        .header("Host", "guarded.t.x")
        .basic_auth("user", Some("VISITOR_SENTINEL"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_daemon_verified_wss_basic_http_body_and_websocket_lease_lifetimes() {
    let mut fixture = fixture().await;
    let options = options(true);
    let connected = connect_and_create_with_connector(
        &fixture.endpoint,
        "tnl_TEST",
        Some("guarded"),
        &options,
        Some(fixture.connector.clone()),
    )
    .await
    .unwrap();
    assert_eq!(
        fixture
            .state
            .registry
            .find_by_subdomain("guarded")
            .unwrap()
            .access
            .as_ref()
            .unwrap()
            .fingerprint(),
        options.access.as_ref().unwrap().fingerprint().unwrap()
    );
    let accept = forward(connected, fixture.backend);
    let mut response = visitor(&fixture, "/hold").send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.chunk().await.unwrap().unwrap(), "part");
    let denied = visitor(&fixture, "/other").send().await.unwrap();
    assert_eq!(denied.status(), 429);
    assert_eq!(denied.headers()["retry-after"], "1");
    assert_eq!(fixture.count.load(Ordering::SeqCst), 1);
    fixture.release.send(true).unwrap();
    assert!(response.bytes().await.unwrap().is_empty());
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if visitor(&fixture, "/ready").send().await.unwrap().status() == 200 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let request = http::Request::builder()
        .uri(format!("ws://{}/socket", fixture.server.local_addr))
        .header("Host", "guarded.t.x")
        .header("Connection", "upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Key", generate_key())
        .header("Sec-WebSocket-Version", "13")
        .header("Authorization", "Basic dXNlcjpWSVNJVE9SX1NFTlRJTkVM")
        .body(())
        .unwrap();
    let (mut ws, _) = async_tungstenite::tokio::connect_async(request)
        .await
        .unwrap();
    ws.send(async_tungstenite::tungstenite::Message::Text("echo".into()))
        .await
        .unwrap();
    assert_eq!(
        ws.next().await.unwrap().unwrap().into_text().unwrap(),
        "echo"
    );
    let count = fixture.count.load(Ordering::SeqCst);
    assert_eq!(
        visitor(&fixture, "/while-ws")
            .send()
            .await
            .unwrap()
            .status(),
        429
    );
    assert_eq!(
        fixture.count.load(Ordering::SeqCst),
        count,
        "101 body must not release the WS slot"
    );
    ws.close(None).await.unwrap();
    drop(ws);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if visitor(&fixture, "/after-ws")
                .send()
                .await
                .unwrap()
                .status()
                == 200
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    while let Ok(head) = fixture.seen.try_recv() {
        assert!(!head.to_ascii_lowercase().contains("authorization:"));
        assert!(!head.contains("VISITOR_SENTINEL"));
    }
    accept.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_daemon_verified_wss_reattach_retains_policy_slot_and_application_authorization() {
    let mut fixture = fixture().await;
    let options = options(false);
    let connected = connect_and_create_with_connector(
        &fixture.endpoint,
        "tnl_TEST",
        Some("guarded"),
        &options,
        Some(fixture.connector.clone()),
    )
    .await
    .unwrap();
    let id = connected.tunnel_id;
    let tunnel = fixture.state.registry.find_by_subdomain("guarded").unwrap();
    let old_session = fixture
        .state
        .registry
        .current_session_id(&tunnel.id)
        .unwrap();
    let access = tunnel.access.as_ref().unwrap().clone();
    let held = access
        .admit(None, &mut http::HeaderMap::new())
        .await
        .unwrap();
    drop(connected);
    tokio::time::timeout(Duration::from_secs(3), async {
        while fixture.state.session_handles.contains_key(&old_session) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let connected = connect_and_reattach_with_connector(
        &fixture.endpoint,
        "tnl_TEST",
        id,
        "guarded",
        &options,
        Some(fixture.connector.clone()),
    )
    .await
    .unwrap();
    assert_eq!(connected.tunnel_id, id);
    assert!(Arc::ptr_eq(
        fixture
            .state
            .registry
            .find_by_subdomain("guarded")
            .unwrap()
            .access
            .as_ref()
            .unwrap(),
        &access
    ));
    let accept = forward(connected, fixture.backend);
    assert_eq!(
        visitor(&fixture, "/retained-slot")
            .send()
            .await
            .unwrap()
            .status(),
        429
    );
    assert_eq!(fixture.count.load(Ordering::SeqCst), 0);
    drop(held);
    let response = reqwest::Client::new()
        .get(format!("http://{}/app", fixture.server.local_addr))
        .header("Host", "guarded.t.x")
        .header("Authorization", "Bearer APP_SENTINEL")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), "ok");
    let received_headers = fixture.seen.recv().await.unwrap();
    assert!(received_headers.contains("Bearer APP_SENTINEL"));
    accept.abort();
    drop(fixture);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_daemon_early_response_keeps_upload_lease_and_body_drop_cancels_cleanly() {
    let mut fixture = fixture().await;
    let options = options(true);
    let connected = connect_and_create_with_connector(
        &fixture.endpoint,
        "tnl_TEST",
        Some("guarded"),
        &options,
        Some(fixture.connector.clone()),
    )
    .await
    .unwrap();
    let accept = forward(connected, fixture.backend);
    let mut visitor_socket = TcpStream::connect(fixture.server.local_addr).await.unwrap();
    visitor_socket.write_all(b"POST /early HTTP/1.1\r\nHost: guarded.t.x\r\nAuthorization: Basic dXNlcjpWSVNJVE9SX1NFTlRJTkVM\r\nContent-Length: 1048576\r\n\r\nx").await.unwrap();
    let response = tokio::time::timeout(Duration::from_secs(3), head(&mut visitor_socket))
        .await
        .unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    let mut bytes = [0; 2];
    visitor_socket.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"ok");
    assert_eq!(
        visitor(&fixture, "/during-upload")
            .send()
            .await
            .unwrap()
            .status(),
        429,
        "response EOF must not release an unfinished upload driver"
    );
    drop(visitor_socket);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let response = visitor(&fixture, "/after-cancel").send().await.unwrap();
            if response.status() == 200 {
                response.bytes().await.unwrap();
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let response = visitor(&fixture, "/hold").send().await.unwrap();
    assert_eq!(response.status(), 200);
    drop(response);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let response = visitor(&fixture, "/after-body-drop").send().await.unwrap();
            if response.status() == 200 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fixture.release.send(true).unwrap();
    while let Ok(head) = fixture.seen.try_recv() {
        assert!(!head.contains("VISITOR_SENTINEL"));
    }
    accept.abort();
    drop(fixture);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_daemon_preserves_response_trailer_frames_under_a_concurrency_lease() {
    let fixture = fixture().await;
    fixture.release.send(true).unwrap();
    let connected = connect_and_create_with_connector(
        &fixture.endpoint,
        "tnl_TEST",
        Some("guarded"),
        &options(true),
        Some(fixture.connector.clone()),
    )
    .await
    .unwrap();
    let accept = forward(connected, fixture.backend);
    let mut io = TcpStream::connect(fixture.server.local_addr).await.unwrap();
    io.write_all(b"GET /hold HTTP/1.1\r\nHost: guarded.t.x\r\nConnection: close\r\nTE: trailers\r\nAuthorization: Basic dXNlcjpWSVNJVE9SX1NFTlRJTkVM\r\n\r\n").await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), io.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    let response = String::from_utf8(response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response.to_ascii_lowercase().contains("x-complete: yes"),
        "response trailer was lost"
    );
    accept.abort();
    drop(fixture);
}

#[path = "access_reconnect_tests.rs"]
mod access_reconnect_tests;

#[path = "access_observation_tests.rs"]
mod access_observation_tests;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn intentional_shutdown_closes_protected_websockets_and_releases_named_or_random_tunnels() {
    for requested in [Some("guarded"), None] {
        let fixture = fixture().await;
        let endpoint = fixture.endpoint.clone();
        let backend = fixture.backend;
        let options = options(true);
        let run_options = options.clone();
        let connector = fixture.connector.clone();
        let (stop, stopping) = tokio::sync::oneshot::channel();
        let run = tokio::spawn(TEST_TLS_CONNECTOR.scope(connector, async move {
            crate::reconnect::run_until_shutdown(
                &endpoint,
                "tnl_TEST",
                requested,
                crate::target::Target::Explicit(backend),
                crate::host_header::HostHeader::Preserve,
                &run_options,
                crate::reconnect::Hooks::default(),
                async { stopping.await.map_err(anyhow::Error::from) },
            )
            .await
        }));
        let tunnel = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if let Some(info) = fixture
                    .state
                    .registry
                    .snapshot_infos()
                    .into_iter()
                    .find(|t| t.active)
                {
                    break fixture
                        .state
                        .registry
                        .find_by_subdomain(&info.subdomain)
                        .unwrap();
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let request = http::Request::builder()
            .uri(format!("ws://{}/socket", fixture.server.local_addr))
            .header("Host", format!("{}.t.x", tunnel.subdomain))
            .header("Connection", "upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Key", generate_key())
            .header("Sec-WebSocket-Version", "13")
            .header("Authorization", "Basic dXNlcjpWSVNJVE9SX1NFTlRJTkVM")
            .body(())
            .unwrap();
        let (mut ws, _) = async_tungstenite::tokio::connect_async(request)
            .await
            .unwrap();
        ws.send(async_tungstenite::tungstenite::Message::Text("live".into()))
            .await
            .unwrap();
        assert_eq!(
            ws.next().await.unwrap().unwrap().into_text().unwrap(),
            "live"
        );
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(3), run)
            .await
            .expect("shutdown must join without draining WS")
            .unwrap()
            .unwrap();
        assert!(fixture
            .state
            .registry
            .find_by_subdomain(&tunnel.subdomain)
            .is_none());
        assert_shutdown_frame(&mut ws).await;
        let mut next = connect_and_create_with_connector(
            &fixture.endpoint,
            "tnl_TEST",
            Some(&tunnel.subdomain),
            &options,
            Some(fixture.connector.clone()),
        )
        .await
        .expect("released protected name immediately reusable");
        assert_ne!(next.tunnel_id.to_string(), tunnel.id);
        tokio::time::timeout(Duration::from_secs(2), close_control(&mut next.control))
            .await
            .unwrap()
            .unwrap();
        drop(fixture);
    }
}

async fn assert_shutdown_frame(
    stream: &mut (impl futures::Stream<
        Item = Result<
            async_tungstenite::tungstenite::Message,
            async_tungstenite::tungstenite::Error,
        >,
    > + Unpin),
) {
    use async_tungstenite::tungstenite::{error::ProtocolError, Error, Message};
    match tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("WS must close")
    {
        None
        | Some(
            Ok(Message::Close(_))
            | Err(
                Error::Protocol(ProtocolError::ResetWithoutClosingHandshake)
                | Error::ConnectionClosed
                | Error::AlreadyClosed,
            ),
        ) => {}
        Some(Err(Error::Io(error)))
            if matches!(
                error.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
            ) => {}
        other => panic!("unexpected shutdown WS result: {other:?}"),
    }
}
