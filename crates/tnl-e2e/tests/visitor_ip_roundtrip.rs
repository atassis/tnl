//! Authoritative identities across real daemon/control/yamux/backend paths.
use std::net::SocketAddr;
use std::time::Duration;

use tnl_protocol::request_metadata::{
    read_request_metadata, DataStreamProtocol, VisitorIpSource, DATA_STREAM_PROTOCOL_HEADER,
    HTTP1_METADATA_V1,
};
use tnl_protocol::{ControlMsg, CreateTunnelReq, Session as _};
use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn daemon(
    trusted_proxies: Vec<String>,
) -> (tempfile::NamedTempFile, tnld::serve::ServerHandle) {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        toml::to_string(&TokensFile {
            tokens: vec![TokenEntry {
                name: "test".into(),
                hash: hash_plaintext("tnl_TEST").unwrap(),
            }],
        })
        .unwrap(),
    )
    .unwrap();
    let handle = tnld::serve::spawn_server(tnld::config::Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://x".into(),
        hostname_root: "t.x".into(),
        tokens_file: file.path().to_string_lossy().into_owned(),
        session_grace_sec: 30,
        trusted_proxies,
    })
    .await
    .unwrap();
    (file, handle)
}

async fn backend() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = axum::Router::new().fallback(|req: axum::extract::Request| async move {
        let request_id = req.headers().get("x-tnl-request-id").map(|v| v.to_str().unwrap().to_string());
        let xff = req.headers().get("x-forwarded-for").map(|v| v.to_str().unwrap().to_string());
        let capability = req.headers().get(DATA_STREAM_PROTOCOL_HEADER).map(|v| v.to_str().unwrap().to_string());
        let claim = req.headers().get("x-tnl-visitor-ip").map(|v| v.to_str().unwrap().to_string());
        let body = axum::body::to_bytes(req.into_body(), 65536).await.unwrap();
        axum::Json(serde_json::json!({"backend_id": request_id, "xff": xff, "capability": capability, "claim": claim, "body": String::from_utf8_lossy(&body)}))
    });
    (
        port,
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        }),
    )
}

async fn request(addr: SocketAddr, host: &str, path: &str, headers: &str) -> String {
    let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    socket
        .write_all(
            format!(
                "GET {path} HTTP/1.1\r\nHost: {host}.t.x\r\nConnection: close\r\n{headers}\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(4), socket.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let response = String::from_utf8(bytes).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    response
}

async fn log(
    rx: &mut tokio::sync::mpsc::Receiver<tnl::inspector::LogLine>,
) -> tnl::inspector::LogLine {
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn visitor_ip_roundtrip_default_trust_ignores_public_spoofing() {
    let (_file, daemon) = daemon(vec![]).await;
    let (port, backend) = backend().await;
    let session = tnl::client::connect_and_create(
        &format!("http://{}", daemon.local_addr),
        "tnl_TEST",
        "direct",
    )
    .await
    .unwrap();
    assert_eq!(
        session.data_stream_protocol,
        DataStreamProtocol::Http1MetadataV1
    );
    let _control = session.control;
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let ctx = tnl::forwarder::ForwardCtx {
        data_stream_protocol: session.data_stream_protocol,
        ..tnl::forwarder::ForwardCtx::new("direct".into(), Some(tx), env!("CARGO_PKG_VERSION"))
    };
    let accept = tokio::spawn(tnl::client::run_accept_loop(
        session.session,
        tnl::target::Target::LocalhostPort(port),
        ctx,
    ));
    let response = request(daemon.local_addr, "direct", "/spoof", "X-Forwarded-For: 203.0.113.66\r\nForwarded: for=203.0.113.66\r\nX-Real-IP: 203.0.113.66\r\nX-Tnl-Visitor-IP: 203.0.113.66\r\nX-Tnl-Data-Stream-Protocol: fake\r\n").await;
    assert!(
        response.contains("203.0.113.66"),
        "ordinary application headers remain unchanged"
    );
    assert!(
        response.contains("fake"),
        "public capability field is not control negotiation"
    );
    let record = log(&mut rx).await;
    assert_eq!(record.visitor_ip, Some("127.0.0.1".parse().unwrap()));
    assert_eq!(record.peer_ip, record.visitor_ip);
    assert_eq!(record.visitor_ip_source, Some(VisitorIpSource::Peer));
    assert!(record.resolved_addr.is_some());
    assert!(tokio::time::timeout(Duration::from_millis(100), rx.recv())
        .await
        .is_err());
    accept.abort();
    backend.abort();
    daemon.join.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn visitor_ip_roundtrip_trusted_chains_duplicates_hop_stripping_and_concurrency() {
    let (_file, daemon) = daemon(vec!["127.0.0.1/32".into(), "192.0.2.0/24".into()]).await;
    let (port, backend) = backend().await;
    let session = tnl::client::connect_and_create(
        &format!("http://{}", daemon.local_addr),
        "tnl_TEST",
        "proxy",
    )
    .await
    .unwrap();
    let _control = session.control;
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let ctx = tnl::forwarder::ForwardCtx {
        data_stream_protocol: session.data_stream_protocol,
        ..tnl::forwarder::ForwardCtx::new("proxy".into(), Some(tx), env!("CARGO_PKG_VERSION"))
    };
    let accept = tokio::spawn(tnl::client::run_accept_loop(
        session.session,
        tnl::target::Target::LocalhostPort(port),
        ctx,
    ));
    let cases = [
        (
            "/chain",
            "X-Forwarded-For: 203.0.113.66, 198.51.100.7, 192.0.2.9\r\n",
            "198.51.100.7",
            VisitorIpSource::XForwardedFor,
        ),
        (
            "/duplicate",
            "X-Forwarded-For: 198.51.100.7\r\nx-forwarded-for: 198.51.100.7\r\n",
            "127.0.0.1",
            VisitorIpSource::PeerInvalidForwarding,
        ),
        (
            "/invalid",
            "X-Forwarded-For: unknown, 198.51.100.7\r\n",
            "127.0.0.1",
            VisitorIpSource::PeerInvalidForwarding,
        ),
        (
            "/missing",
            "Forwarded: for=203.0.113.66\r\n",
            "127.0.0.1",
            VisitorIpSource::PeerMissingForwarding,
        ),
        (
            "/all-trusted",
            "X-Forwarded-For: 192.0.2.9\r\n",
            "127.0.0.1",
            VisitorIpSource::PeerAllTrusted,
        ),
        (
            "/hop",
            "Connection: X-Forwarded-For\r\nX-Forwarded-For: 2001:db8::7\r\n",
            "2001:db8::7",
            VisitorIpSource::XForwardedFor,
        ),
    ];
    let mut requests = Vec::new();
    for (path, headers, _, _) in cases {
        requests.push(tokio::spawn(request(
            daemon.local_addr,
            "proxy",
            path,
            headers,
        )));
    }
    for task in requests {
        task.await.unwrap();
    }
    let mut pending: std::collections::HashMap<_, _> = cases
        .iter()
        .map(|&(path, _, expected, source)| (path, (expected, source)))
        .collect();
    for _ in cases {
        let record = log(&mut rx).await;
        let (expected, source) = pending
            .remove(record.path.as_str())
            .expect("duplicate or unexpected request path");
        assert_eq!(record.visitor_ip, Some(expected.parse().unwrap()));
        assert_eq!(record.peer_ip, Some("127.0.0.1".parse().unwrap()));
        assert_eq!(record.visitor_ip_source, Some(source));
        assert_eq!(record.status, Some(200));
    }
    assert!(pending.is_empty(), "missing request records: {pending:?}");
    assert!(tokio::time::timeout(Duration::from_millis(100), rx.recv())
        .await
        .is_err());
    accept.abort();
    backend.abort();
    daemon.join.abort();
}

async fn manual_session(
    addr: SocketAddr,
    subdomain: &str,
    metadata: bool,
) -> (
    tnl_protocol::transport::YamuxSession,
    std::pin::Pin<Box<dyn tnl_protocol::Stream>>,
) {
    let mut req = http::Request::builder()
        .uri(format!("ws://{addr}/control"))
        .header("Host", addr.to_string())
        .header("Authorization", "Bearer tnl_TEST")
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header(
            tnl_protocol::control::CONTROL_PROTOCOL_HEADER,
            tnl_protocol::control::CONTROL_PROTOCOL_VERSION,
        )
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            async_tungstenite::tungstenite::handshake::client::generate_key(),
        );
    if metadata {
        req = req.header(DATA_STREAM_PROTOCOL_HEADER, HTTP1_METADATA_V1);
    }
    let (ws, response) = async_tungstenite::tokio::connect_async(req.body(()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        response.headers().contains_key(DATA_STREAM_PROTOCOL_HEADER),
        metadata
    );
    let mut session = tnl_protocol::transport::client_session_from_ws_generic(ws);
    let mut ctrl = session.open_stream().await.unwrap();
    let message = serde_json::to_vec(&ControlMsg::CreateTunnel(CreateTunnelReq {
        subdomain: Some(subdomain.into()),
    }))
    .unwrap();
    ctrl.write_u32(u32::try_from(message.len()).unwrap())
        .await
        .unwrap();
    ctrl.write_all(&message).await.unwrap();
    ctrl.flush().await.unwrap();
    let n = ctrl.read_u32().await.unwrap();
    let mut bytes = vec![0; n as usize];
    ctrl.read_exact(&mut bytes).await.unwrap();
    assert!(matches!(
        serde_json::from_slice::<ControlMsg>(&bytes).unwrap(),
        ControlMsg::TunnelCreated(_)
    ));
    (session, ctrl)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn visitor_ip_roundtrip_legacy_and_metadata_sessions_do_not_share_mode() {
    let (_file, daemon) = daemon(vec![]).await;
    let (port, backend) = backend().await;
    let (raw, _raw_control) = manual_session(daemon.local_addr, "legacy", false).await;
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let accept = tokio::spawn(tnl::client::run_accept_loop(
        Box::new(raw),
        tnl::target::Target::LocalhostPort(port),
        tnl::forwarder::ForwardCtx::new("legacy".into(), Some(tx), env!("CARGO_PKG_VERSION")),
    ));
    let (mut negotiated, _control) = manual_session(daemon.local_addr, "negotiated", true).await;
    let addr = daemon.local_addr;
    let pending = tokio::spawn(async move {
        reqwest::Client::new()
            .get(format!("http://{addr}/broken"))
            .header("Host", "negotiated.t.x")
            .header("Accept", "application/json")
            .send()
            .await
            .unwrap()
    });
    let mut stream = negotiated.accept_stream().await.unwrap().unwrap();
    let metadata = read_request_metadata(&mut stream).await.unwrap();
    assert_eq!(metadata.visitor.source, VisitorIpSource::Peer);
    let mut head = Vec::new();
    while !head.windows(4).any(|v| v == b"\r\n\r\n") {
        head.push(stream.read_u8().await.unwrap());
    }
    assert!(head.starts_with(b"GET /broken HTTP/1.1\r\n"));
    stream.write_all(b"invalid response\r\n").await.unwrap();
    stream.shutdown().await.unwrap();
    let response = pending.await.unwrap();
    assert_eq!(response.status(), 502);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        body["request_id"],
        metadata.request_id.to_string(),
        "metadata must reuse the daemon's existing error-context ID"
    );
    let response = reqwest::Client::new()
        .get(format!("http://{}/raw", daemon.local_addr))
        .header("host", "legacy.t.x")
        .header("x-tnl-request-id", "01BX5ZZKBKACTAV9WEVGEMMVRZ")
        .header("x-tnl-data-stream-protocol", "http1-metadata-v1")
        .send()
        .await
        .unwrap();
    let daemon_id = response.headers()["x-tnl-request-id"]
        .to_str()
        .unwrap()
        .to_string();
    assert_ne!(daemon_id, "01BX5ZZKBKACTAV9WEVGEMMVRZ");
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["backend_id"], daemon_id);
    let record = log(&mut rx).await;
    assert_eq!(record.status, Some(200));
    assert_ne!(
        record.req_id.to_string(),
        daemon_id,
        "raw client IDs are local-only"
    );
    assert_ne!(record.req_id.to_string(), "01BX5ZZKBKACTAV9WEVGEMMVRZ");
    assert_eq!(
        (record.visitor_ip, record.peer_ip, record.visitor_ip_source),
        (None, None, None)
    );
    assert!(tokio::time::timeout(Duration::from_millis(100), rx.recv())
        .await
        .is_err());
    accept.abort();
    backend.abort();
    daemon.join.abort();
}
