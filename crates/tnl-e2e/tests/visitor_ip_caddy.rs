//! Credential-free ingress proof against an operator-supplied local Caddy.
//! Never downloads a binary, reads production config, enables ACME, or uses DNS.
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use tnl_protocol::request_metadata::{DataStreamProtocol, VisitorIpSource};
use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug)]
struct Caddy(Child);
impl Drop for Caddy {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an installed TNL_TEST_CADDY_BIN and 127.0.0.2 source binding"]
async fn visitor_ip_caddy_single_edge_defaults_and_explicit_append_hop() {
    let binary = std::env::var_os("TNL_TEST_CADDY_BIN").expect(
        "set TNL_TEST_CADDY_BIN to an already installed local Caddy; no download is performed",
    );
    let version = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::process::Command::new(&binary)
            .arg("version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("bounded Caddy version")
    .expect("run local Caddy version");
    assert!(version.status.success());
    eprintln!(
        "Caddy version: {}",
        String::from_utf8_lossy(&version.stdout).trim()
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        for (append, trust_upstream, expected) in [
            (false, false, "127.0.0.2"),
            (true, false, "127.0.0.2"),
            (true, true, "198.51.100.7"),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let daemon = spawn_daemon(temp.path(), trust_upstream).await;
            let daemon_addr = daemon.local_addr;
            let _daemon = Task(daemon.join);
            let backend_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let backend_addr = backend_listener.local_addr().unwrap();
            let (observed_tx, mut observed_rx) = tokio::sync::mpsc::channel(4);
            let router = axum::Router::new().fallback(move |headers: axum::http::HeaderMap| {
                let tx = observed_tx.clone();
                async move {
                    assert_eq!(headers.get_all("x-tnl-request-id").iter().count(), 1);
                    tx.send((
                        headers["x-forwarded-for"].to_str().unwrap().to_string(),
                        headers["x-tnl-request-id"].to_str().unwrap().to_string(),
                    ))
                    .await
                    .unwrap();
                    axum::http::Response::builder()
                        .header("x-tnl-request-id", "backend-forgery")
                        .body("ok".to_string())
                        .unwrap()
                }
            });
            let _backend = Task(tokio::spawn(async move {
                axum::serve(backend_listener, router).await.unwrap();
            }));
            let (_caddy, caddy_addr) =
                start_caddy(&binary, temp.path(), daemon_addr, append, false).await;
            let (_accept, mut rx) = forwarder(caddy_addr, backend_addr).await;
            let response = visitor_request(caddy_addr).await;
            let response_id = canonical_id(response.split_once("\r\n\r\n").unwrap().0);
            let (observed, backend_id) =
                tokio::time::timeout(Duration::from_secs(2), observed_rx.recv())
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(backend_id, response_id);
            eprintln!("Observed upstream XFF: {observed}");
            assert_eq!(
                observed,
                if append {
                    "203.0.113.66, 198.51.100.7, 127.0.0.2"
                } else {
                    "127.0.0.2"
                }
            );
            let record = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(record.req_id.to_string(), response_id);
            assert_eq!(record.visitor_ip, Some(expected.parse().unwrap()));
            assert_eq!(record.peer_ip, Some("127.0.0.1".parse().unwrap()));
            assert_eq!(
                record.visitor_ip_source,
                Some(VisitorIpSource::XForwardedFor)
            );
            assert!(tokio::time::timeout(Duration::from_millis(100), rx.recv())
                .await
                .is_err());
        }
    })
    .await
    .expect("bounded Caddy visitor chain fixture");
}

async fn spawn_daemon(temp: &std::path::Path, trust_upstream: bool) -> tnld::serve::ServerHandle {
    let tokens = temp.join("tokens.toml");
    std::fs::write(
        &tokens,
        toml::to_string(&TokensFile {
            tokens: vec![TokenEntry {
                name: "synthetic".into(),
                hash: hash_plaintext("tnl_CADDY_TEST").unwrap(),
            }],
        })
        .unwrap(),
    )
    .unwrap();
    let mut trusted_proxies = vec!["127.0.0.1/32".into()];
    if trust_upstream {
        trusted_proxies.push("127.0.0.2/32".into());
    }
    let daemon = tnld::serve::spawn_server(tnld::config::Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://x".into(),
        hostname_root: "t.x".into(),
        tokens_file: tokens.to_string_lossy().into_owned(),
        session_grace_sec: 30,
        trusted_proxies,
    })
    .await
    .unwrap();
    daemon
}

async fn start_caddy(
    binary: &std::ffi::OsStr,
    temp: &std::path::Path,
    upstream: SocketAddr,
    append: bool,
    forced_close: bool,
) -> (Caddy, SocketAddr) {
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let caddy_addr = reserved.local_addr().unwrap();
    drop(reserved);
    let header_up = if append {
        "header_up X-Forwarded-For \"{http.request.header.X-Forwarded-For}, {http.request.remote.host}\""
    } else {
        ""
    };
    let extra = format!(
        "{header_up}\n{}",
        if forced_close {
            "header_up Connection close"
        } else {
            ""
        }
    );
    let config = local_config(caddy_addr, upstream, &extra);
    eprintln!("Effective test Caddyfile (append={append}):\n{config}");
    let config_path = temp.join("Caddyfile");
    std::fs::write(&config_path, config).unwrap();
    let stderr_path = temp.join("caddy.stderr");
    let mut caddy = Caddy(
        Command::new(binary)
            .args(["run", "--adapter", "caddyfile", "--config"])
            .arg(&config_path)
            .env("HOME", temp)
            .env("XDG_CONFIG_HOME", temp.join("config"))
            .env("XDG_DATA_HOME", temp.join("data"))
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&stderr_path).unwrap())
            .spawn()
            .expect("start local HTTP-only Caddy"),
    );
    let ready = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            assert!(
                caddy.0.try_wait().unwrap().is_none(),
                "Caddy exited early: {}",
                std::fs::read_to_string(&stderr_path).unwrap()
            );
            if tokio::net::TcpStream::connect(caddy_addr).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    assert!(
        ready.is_ok(),
        "Caddy failed to listen: {}",
        std::fs::read_to_string(&stderr_path).unwrap()
    );
    (caddy, caddy_addr)
}

async fn visitor_socket(caddy_addr: SocketAddr) -> tokio::net::TcpStream {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket
        .bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)), 0))
        .expect("prerequisite: bind visitor source 127.0.0.2");
    socket
        .connect(caddy_addr)
        .await
        .expect("prerequisite: reach Caddy from 127.0.0.2")
}

async fn visitor_request(caddy_addr: SocketAddr) -> String {
    let mut socket = visitor_socket(caddy_addr).await;
    socket.write_all(b"GET /visitor HTTP/1.1\r\nHost: demo.t.x\r\nConnection: close\r\nX-Forwarded-For: 203.0.113.66, 198.51.100.7\r\nForwarded: for=203.0.113.66\r\nX-Tnl-Visitor-IP: 203.0.113.66\r\nX-Tnl-Request-Id: 01BX5ZZKBKACTAV9WEVGEMMVRZ\r\n\r\n").await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), socket.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(
        response.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&response)
    );
    String::from_utf8(response).unwrap()
}

// Copy the reviewed proxy bodies, rather than maintaining an independent proxy.
// Only sites, listener/upstream ports, TLS and admin differ in this local fixture.
fn local_config(addr: SocketAddr, upstream: SocketAddr, extra: &str) -> String {
    let production = include_str!("../../../deploy/Caddyfile");
    let (_, data) = production
        .split_once("reverse_proxy 127.0.0.1:7777 {")
        .expect("repository data proxy block");
    let (directives, _) = data.split_once('}').unwrap();
    assert!(
        !directives.contains('{'),
        "nested directives need explicit fixture reconciliation"
    );
    assert!(
        production.contains("reverse_proxy 127.0.0.1:7777\n}"),
        "repository API proxy must remain unmodified"
    );
    format!("{{\n admin off\n auto_https off\n}}\nhttp://*.t.x:{} {{\n bind 127.0.0.1\n reverse_proxy {upstream} {{{directives}\n {extra}\n }}\n}}\nhttp://127.0.0.1:{} {{\n bind 127.0.0.1\n reverse_proxy {upstream}\n}}\n", addr.port(), addr.port())
}

#[test]
fn local_caddy_config_preserves_repository_proxy_directives() {
    let config = local_config(
        "127.0.0.1:1234".parse().unwrap(),
        "127.0.0.1:5678".parse().unwrap(),
        "",
    );
    assert!(
        config.contains("flush_interval -1"),
        "must preserve production streaming directive"
    );
    assert!(
        !config.contains("header_up Connection close"),
        "must not replace WebSocket upgrade with close"
    );
}

#[derive(Debug)]
struct Task<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn head(io: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        bytes.push(io.read_u8().await.unwrap());
        assert!(bytes.len() < 32768);
    }
    String::from_utf8(bytes).unwrap()
}

fn canonical_id(head: &str) -> String {
    let ids: Vec<_> = head
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("x-tnl-request-id")
                .then(|| value.trim())
        })
        .collect();
    assert_eq!(ids.len(), 1, "one authoritative ID: {head}");
    let id = ids[0].to_string();
    assert_eq!(id.len(), 26);
    assert_ne!(id, "01BX5ZZKBKACTAV9WEVGEMMVRZ");
    id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an installed TNL_TEST_CADDY_BIN"]
async fn actual_caddy_websocket_binary_read_ahead_ids_and_parallel_http() {
    application_fixture(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an installed TNL_TEST_CADDY_BIN"]
async fn actual_caddy_legacy_forced_close_breaks_upgrade_but_not_http() {
    application_fixture(true).await;
}

async fn application_backend_connection(
    mut io: tokio::net::TcpStream,
    tx: tokio::sync::mpsc::Sender<String>,
) {
    use async_tungstenite::{
        tokio::TokioAdapter,
        tungstenite::{protocol::Role, Message},
        WebSocketStream,
    };
    use futures::{SinkExt, StreamExt};
    let request = head(&mut io).await;
    let id = canonical_id(&request);
    tx.send(id).await.unwrap();
    let lower = request.to_ascii_lowercase();
    if lower.contains("connection: upgrade\r\n") {
        assert!(lower.contains("upgrade: websocket\r\n"));
        assert!(!lower.contains("connection: close"));
        // Coalesce the 101 and first binary frame in one write.
        io.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\nX-Tnl-Request-Id: backend-forgery\r\n\r\n\x82\x05first").await.unwrap();
        let mut ws =
            WebSocketStream::from_raw_socket(TokioAdapter::new(io), Role::Server, None).await;
        while let Some(message) = ws.next().await {
            match message.unwrap() {
                Message::Close(_) => {
                    ws.flush().await.unwrap();
                    break;
                }
                Message::Ping(bytes) => ws.send(Message::Pong(bytes)).await.unwrap(),
                message => ws.send(message).await.unwrap(),
            }
        }
    } else {
        assert!(lower.contains("connection: close"));
        assert!(!lower.contains("x-request-hop:"));
        io.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close, x-hop\r\nX-Hop: hidden\r\nSet-Cookie: a=1\r\nSet-Cookie: b=2\r\nX-Tnl-Request-Id: backend-forgery\r\n\r\n2\r\nok\r\n0\r\n\r\n").await.unwrap();
        io.shutdown().await.unwrap();
    }
}

async fn application_fixture(forced_close: bool) {
    use async_tungstenite::{
        tokio::TokioAdapter,
        tungstenite::{protocol::Role, Message},
        WebSocketStream,
    };
    use futures::StreamExt;
    tokio::time::timeout(Duration::from_secs(20), async {
        let binary = std::env::var_os("TNL_TEST_CADDY_BIN").expect("set TNL_TEST_CADDY_BIN");
        let temp = tempfile::tempdir().unwrap();
        let daemon = spawn_daemon(temp.path(), false).await;
        let daemon_addr = daemon.local_addr;
        let _daemon = Task(daemon.join);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = listener.local_addr().unwrap();
        let (observed_tx, mut observed_rx) = tokio::sync::mpsc::channel(8);
        let _backend = Task(tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                let (io, _) = listener.accept().await.unwrap();
                let tx = observed_tx.clone();
                tasks.spawn(async move {
                    application_backend_connection(io, tx).await;
                });
            }
        }));
        let (_caddy, addr) = start_caddy(&binary, temp.path(), daemon_addr, false, forced_close).await;
        let (_accept, mut logs) = forwarder(addr, backend_addr).await;
        let mut io = visitor_socket(addr).await;
        // Coalesce a real masked binary frame with the request head too.
        io.write_all(b"GET /socket HTTP/1.1\r\nHost: demo.t.x\r\nConnection: upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nX-Tnl-Request-Id: 01BX5ZZKBKACTAV9WEVGEMMVRZ\r\n\r\n\x82\x85\x00\x00\x00\x00early").await.unwrap();
        let response = head(&mut io).await;
        if forced_close {
            assert!(response.starts_with("HTTP/1.1 400"), "legacy close must break application upgrade: {response}");
            canonical_id(&response);
            assert!(observed_rx.try_recv().is_err(), "rejected upgrade must not reach backend");
            assert!(logs.try_recv().is_err(), "rejected upgrade has no client record");
            drop(io);
            let http = visitor_request(addr).await;
            let id = canonical_id(http.split_once("\r\n\r\n").unwrap().0);
            assert_eq!(observed_rx.recv().await.unwrap(), id);
            let record = logs.recv().await.unwrap();
            assert_eq!(record.req_id.to_string(), id);
            assert_eq!(record.status, Some(200));
            return;
        }
        assert!(response.starts_with("HTTP/1.1 101"), "{response}");
        assert!(!response.to_ascii_lowercase().contains("connection: close"));
        let ws_id = canonical_id(&response);
        assert_eq!(observed_rx.recv().await.unwrap(), ws_id);
        let mut ws = WebSocketStream::from_raw_socket(TokioAdapter::new(io), Role::Client, None).await;
        assert_eq!(ws.next().await.unwrap().unwrap(), Message::Binary(b"first".to_vec().into()));
        assert_eq!(ws.next().await.unwrap().unwrap(), Message::Binary(b"early".to_vec().into()));
        // One public keepalive socket, two ordinary requests while WS is live.
        let mut http = visitor_socket(addr).await;
        let mut ids = std::collections::HashSet::from([ws_id]);
        for _ in 0..2 {
            http.write_all(b"GET /ordinary HTTP/1.1\r\nHost: demo.t.x\r\nConnection: x-request-hop\r\nX-Request-Hop: hidden\r\nX-Tnl-Request-Id: 01BX5ZZKBKACTAV9WEVGEMMVRZ\r\n\r\n").await.unwrap();
            let response = head(&mut http).await;
            let lower = response.to_ascii_lowercase();
            assert!(lower.starts_with("http/1.1 200"), "{response}");
            assert!(!lower.contains("connection: close"));
            assert!(!lower.contains("x-hop:"));
            assert_eq!(lower.matches("set-cookie:").count(), 2);
            let id = canonical_id(&response);
            assert_eq!(observed_rx.recv().await.unwrap(), id);
            assert!(ids.insert(id));
            let mut body = [0; 12];
            http.read_exact(&mut body).await.unwrap();
            assert_eq!(&body, b"2\r\nok\r\n0\r\n\r\n");
        }
        for message in [Message::Text("hello".into()), Message::Binary(vec![0, 255, 128, 1].into()), Message::Ping(b"ping".to_vec().into())] {
            ws.send(message.clone()).await.unwrap();
            let expected = if let Message::Ping(bytes) = message { Message::Pong(bytes) } else { message };
            assert_eq!(ws.next().await.unwrap().unwrap(), expected);
        }
        ws.close(None).await.unwrap();
        assert!(matches!(ws.next().await.unwrap().unwrap(), Message::Close(_)));
        drop(ws);
        for _ in 0..3 {
            let record = logs.recv().await.unwrap();
            assert!(ids.remove(&record.req_id.to_string()));
            assert_eq!(record.status, Some(if record.path == "/socket" { 101 } else { 200 }));
            assert_eq!(record.visitor_ip, Some("127.0.0.2".parse().unwrap()));
            assert_eq!(record.peer_ip, Some("127.0.0.1".parse().unwrap()));
            assert_eq!(record.visitor_ip_source, Some(VisitorIpSource::XForwardedFor));
        }
        assert!(ids.is_empty());
    }).await.expect("bounded actual-Caddy application WS/HTTP fixture");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires an installed TNL_TEST_CADDY_BIN"]
async fn actual_caddy_cleanup_after_fixture_panic_releases_listener() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let binary = std::env::var_os("TNL_TEST_CADDY_BIN").expect("set TNL_TEST_CADDY_BIN");
        let temp = tempfile::tempdir().unwrap();
        let (caddy, addr) = start_caddy(
            &binary,
            temp.path(),
            "127.0.0.1:1".parse().unwrap(),
            false,
            false,
        )
        .await;
        let failure = std::panic::catch_unwind(move || {
            let _caddy = caddy;
            panic!("intentional fixture failure to exercise child kill/wait");
        });
        assert!(failure.is_err());
        let _rebound = tokio::net::TcpListener::bind(addr)
            .await
            .expect("Caddy must release its listener even after a fixture panic");
    })
    .await
    .expect("bounded panic-cleanup regression");
}

// Keep the real control WebSocket alive for the full forwarding task lifetime.
async fn forwarder(
    addr: SocketAddr,
    backend: SocketAddr,
) -> (
    Task<()>,
    tokio::sync::mpsc::Receiver<tnl::inspector::LogLine>,
) {
    let connected =
        tnl::client::connect_and_create(&format!("http://{addr}"), "tnl_CADDY_TEST", "demo")
            .await
            .unwrap();
    assert_eq!(
        connected.data_stream_protocol,
        DataStreamProtocol::Http1MetadataV1
    );
    let (tx, logs) = tokio::sync::mpsc::channel(8);
    let ctx = tnl::forwarder::ForwardCtx {
        data_stream_protocol: connected.data_stream_protocol,
        ..tnl::forwarder::ForwardCtx::new("demo".into(), Some(tx), env!("CARGO_PKG_VERSION"))
    };
    let accept = Task(tokio::spawn(async move {
        let _control = connected.control;
        tnl::client::run_accept_loop(
            connected.session,
            tnl::target::Target::Explicit(backend),
            ctx,
        )
        .await
        .unwrap();
    }));
    (accept, logs)
}
