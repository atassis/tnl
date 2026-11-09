use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::Request;
use tnl_protocol::request_metadata::{read_request_metadata, DataStreamProtocol, VisitorIpSource};
use tnl_protocol::transport::{Session, YamuxSession};
use tnld::serve::{spawn_server_with_state, AppState, ServerHandle, SessionHandle};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

const REQUEST: &str = "GET /socket?q=1 HTTP/1.1\r\nHost: ws.t.example.com\r\nConnection: keep-alive, UpGrAdE\r\nUpgrade: WebSocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n";
const RESPONSE: &str = "HTTP/1.1 101 Switching Protocols\r\nConnection: upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n";

async fn head(io: &mut (impl AsyncRead + Unpin)) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        bytes.push(io.read_u8().await.unwrap());
        assert!(bytes.len() < 32768);
    }
    String::from_utf8(bytes).unwrap()
}

fn peer() -> ConnectInfo<std::net::SocketAddr> {
    ConnectInfo("127.0.0.1:12345".parse().unwrap())
}

async fn backend_head(io: &mut (impl AsyncRead + Unpin)) -> String {
    let metadata = read_request_metadata(io).await.unwrap();
    assert_eq!(metadata.visitor.visitor_ip, peer().0.ip());
    assert_eq!(metadata.visitor.peer_ip, peer().0.ip());
    assert_eq!(metadata.visitor.source, VisitorIpSource::Peer);
    head(io).await
}

async fn fixture() -> (ServerHandle, AppState, YamuxSession) {
    let tokens = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tokens.path(), "tokens = []").unwrap();
    let (server, state) = spawn_server_with_state(tnld::config::Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://test".into(),
        hostname_root: "t.example.com".into(),
        tokens_file: tokens.path().to_string_lossy().into_owned(),
        session_grace_sec: 30,
        trusted_proxies: vec![],
    })
    .await
    .unwrap();
    let session = state.registry.register_session("test");
    state
        .registry
        .create_tunnel("ws", &session.id, "test")
        .unwrap();
    let (a, b) = tokio::io::duplex(1024);
    state.session_handles.insert(
        session.id.clone(),
        SessionHandle::new(
            &session,
            Arc::new(Mutex::new(Box::new(YamuxSession::new_client(a)))),
            DataStreamProtocol::Http1MetadataV1,
        ),
    );
    (server, state, YamuxSession::new_server(b))
}

#[tokio::test]
async fn socket_upgrade_preserves_both_read_ahead_buffers_and_half_close() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (server, _state, mut session) = fixture().await;
        let backend = tokio::spawn(async move {
            let mut io = session.accept_stream().await.unwrap().unwrap();
            let request = backend_head(&mut io).await.to_lowercase();
            assert!(request.contains("connection: upgrade\r\n"));
            assert!(!request.contains("x-hop-secret"));
            io.write_all(
                format!("{RESPONSE}Connection: x-hop-secret\r\nX-Hop-Secret: hidden\r\n\r\nfirst")
                    .as_bytes(),
            )
            .await
            .unwrap();
            let mut early = [0; 5];
            io.read_exact(&mut early).await.unwrap();
            assert_eq!(&early, b"early");
            assert_eq!(
                io.read_u8().await.unwrap_err().kind(),
                std::io::ErrorKind::UnexpectedEof
            );
            io.write_all(b"final").await.unwrap();
            io.shutdown().await.unwrap();
        });
        let mut visitor = TcpStream::connect(server.local_addr).await.unwrap();
        visitor
            .write_all(
                format!("{REQUEST}Connection: x-hop-secret\r\nX-Hop-Secret: hidden\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\nearly")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let response = head(&mut visitor).await;
        assert!(response.starts_with("HTTP/1.1 101"), "{response}");
        assert!(!response.to_lowercase().contains("x-hop-secret"));
        visitor.shutdown().await.unwrap();
        let mut bytes = Vec::new();
        visitor.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"firstfinal");
        backend.await.unwrap();
        server.join.abort();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn invalid_request_envelopes_never_open_substream() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (server, _state, mut session) = fixture().await;
        let mut requests = Vec::new();
        for field in [
            "host",
            "origin",
            "authorization",
            "cookie",
            "sec-websocket-key",
            "sec-websocket-extensions",
        ] {
            requests.push(format!("{REQUEST}Connection: {field}\r\n\r\n"));
        }
        for extra in [
            "Connection: close",
            "Connection: upgrade,,foo",
            "Connection: upgrade, invalid token",
            "Upgrade: websocket",
            "Sec-WebSocket-Key: other",
            "Sec-WebSocket-Version: 13",
            "Content-Length: 1",
            "Transfer-Encoding: chunked",
            "Sec-WebSocket-Protocol: bad protocol",
        ] {
            requests.push(format!("{REQUEST}{extra}\r\n\r\n"));
        }
        requests.push(REQUEST.replace("GET ", "POST ") + "\r\n");
        requests.push(REQUEST.replace("HTTP/1.1", "HTTP/1.0") + "\r\n");
        requests.push(REQUEST.replace("Sec-WebSocket-Version: 13\r\n", "") + "\r\n");
        requests.push(
            REQUEST.replace("Sec-WebSocket-Version: 13", "Sec-WebSocket-Version: 12") + "\r\n",
        );
        requests
            .push(REQUEST.replace("Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n", "") + "\r\n");
        requests.push(
            REQUEST.replace(
                "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==",
                "Sec-WebSocket-Key: ",
            ) + "\r\n",
        );
        requests.push(REQUEST.replace("Upgrade: WebSocket", "Upgrade: websocket, h2c") + "\r\n");
        requests.push(
            REQUEST.replace("Connection: keep-alive, UpGrAdE", "Connection: keep-alive") + "\r\n",
        );
        requests.push(REQUEST.replace("Upgrade: WebSocket", "Upgrade: h2c") + "\r\n");
        for request in requests {
            let mut visitor = TcpStream::connect(server.local_addr).await.unwrap();
            visitor.write_all(request.as_bytes()).await.unwrap();
            let response = head(&mut visitor).await;
            assert_eq!(
                response.split_whitespace().nth(1),
                Some("400"),
                "{request:?}: {response}"
            );
            assert!(response.to_lowercase().contains("cache-control: no-store"));
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(50), session.accept_stream())
                .await
                .is_err()
        );
        server.join.abort();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn missing_on_upgrade_returns_400_without_panicking() {
    let (server, state, mut session) = fixture().await;
    let req = Request::builder()
        .uri("/socket")
        .header("host", "ws.t.example.com")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-key", "key")
        .header("sec-websocket-version", "13")
        .body(Body::empty())
        .unwrap();
    let response = tokio::time::timeout(
        Duration::from_secs(1),
        tnld::data_plane::handler(State(state), peer(), req),
    )
    .await;
    assert_eq!(
        response
            .expect("missing socket ownership must fail before opening a stream")
            .status(),
        400
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), session.accept_stream())
            .await
            .is_err()
    );
    server.join.abort();
}

#[tokio::test]
async fn invalid_and_unsolicited_101_are_upstream_502() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (server, _state, mut session) = fixture().await;
        let mut responses = vec![
            RESPONSE.replace("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=", "wrong") + "\r\n",
            RESPONSE.replace("Connection: upgrade\r\n", "") + "\r\n",
            RESPONSE.replace("Upgrade: websocket", "Upgrade: h2c") + "\r\n",
            RESPONSE.replace("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n", "") + "\r\n",
        ];
        for extra in [
            "Connection: close",
            "Connection: upgrade,,bad",
            "Upgrade: websocket",
            "Connection: authorization",
            "Connection: origin",
            "Connection: sec-websocket-accept",
            "Connection: set-cookie",
            "Sec-WebSocket-Accept: duplicate",
            "Sec-WebSocket-Protocol: unoffered",
            "Sec-WebSocket-Protocol: chat, other",
            "Sec-WebSocket-Protocol: chat\r\nSec-WebSocket-Protocol: chat",
            "Content-Length: 0",
            "Transfer-Encoding: chunked",
        ] {
            responses.push(format!("{RESPONSE}{extra}\r\n\r\n"));
        }
        responses.push(RESPONSE.replace("HTTP/1.1", "HTTP/1.0") + "\r\n");
        responses.push(format!("{RESPONSE}\r\n")); // ordinary request, unsolicited 101
        let count = responses.len();
        let backend = tokio::spawn(async move {
            for response in responses {
                let mut io = session.accept_stream().await.unwrap().unwrap();
                backend_head(&mut io).await;
                io.write_all(response.as_bytes()).await.unwrap();
                io.shutdown().await.unwrap();
            }
        });
        for i in 0..count {
            let mut visitor = TcpStream::connect(server.local_addr).await.unwrap();
            let request = if i == count - 1 {
                "GET / HTTP/1.1\r\nHost: ws.t.example.com\r\n\r\n".to_string()
            } else {
                format!("{REQUEST}Sec-WebSocket-Protocol: chat, other\r\n\r\n")
            };
            visitor.write_all(request.as_bytes()).await.unwrap();
            let response = head(&mut visitor).await;
            assert!(response.starts_with("HTTP/1.1 502"), "case {i}: {response}");
            assert!(
                response
                    .to_lowercase()
                    .contains("x-tnl-component: upstream"),
                "{response}"
            );
        }
        backend.await.unwrap();
        server.join.abort();
    })
    .await
    .unwrap();
}

fn synthetic_upgrade_request() -> Request<Body> {
    let mut placeholder = Request::new(Body::empty());
    let socket = hyper::upgrade::on(&mut placeholder);
    let mut req = Request::builder()
        .uri("/socket")
        .header("host", "ws.t.example.com")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-key", "key")
        .header("sec-websocket-version", "13")
        .body(Body::empty())
        .unwrap();
    // A failed OnUpgrade capability is enough to exercise pre-101 ownership;
    // successful visitor upgrades above always use actual sockets.
    req.extensions_mut().insert(socket);
    req
}

#[tokio::test]
async fn complete_header_timeout_releases_stalled_backend() {
    // Include the deliberate 31-second virtual-time advance in the outer budget.
    tokio::time::timeout(Duration::from_secs(40), async {
        let (server, state, mut session) = fixture().await;
        let handler = tokio::spawn(tnld::data_plane::handler(
            State(state),
            peer(),
            synthetic_upgrade_request(),
        ));
        let mut backend = tokio::time::timeout(Duration::from_secs(2), session.accept_stream())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        backend_head(&mut backend).await;
        backend.write_all(b"HTTP/1.1 ").await.unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(31)).await;
        let response = handler.await.unwrap();
        assert_eq!(response.status(), 502);
        assert_eq!(response.headers()["x-tnl-component"], "upstream");
        assert_eq!(
            backend.read_u8().await.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        server.join.abort();
    })
    .await
    .expect("complete-header timeout or backend cleanup stalled");
}

#[tokio::test]
async fn cancelled_negotiation_releases_its_hyper_driver() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let (server, state, mut session) = fixture().await;
        let handler = tokio::spawn(tnld::data_plane::handler(
            State(state),
            peer(),
            synthetic_upgrade_request(),
        ));
        let mut backend = session.accept_stream().await.unwrap().unwrap();
        backend_head(&mut backend).await;
        handler.abort();
        assert!(handler.await.unwrap_err().is_cancelled());
        assert_eq!(
            backend.read_u8().await.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        server.join.abort();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn failed_visitor_rendezvous_closes_upstream_without_http_error_bytes() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let (server, state, mut session) = fixture().await;
        let handler = tokio::spawn(tnld::data_plane::handler(
            State(state),
            peer(),
            synthetic_upgrade_request(),
        ));
        let mut backend = session.accept_stream().await.unwrap().unwrap();
        backend_head(&mut backend).await;
        let accept = async_tungstenite::tungstenite::handshake::derive_accept_key(b"key");
        backend
            .write_all(
                RESPONSE
                    .replace("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=", &accept)
                    .as_bytes(),
            )
            .await
            .unwrap();
        backend.write_all(b"\r\n").await.unwrap();
        assert_eq!(handler.await.unwrap().status(), 101);
        assert_eq!(
            backend.read_u8().await.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        server.join.abort();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn visitor_rendezvous_timeout_drops_upstream_io() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (server, state, mut session) = fixture().await;
        let ready = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        // Hold the 101 in middleware: the handler can return it, but Hyper
        // cannot fulfill downstream OnUpgrade until the middleware lets it out.
        let app = axum::Router::new()
            .fallback(tnld::data_plane::handler)
            .with_state(state)
            .layer(axum::middleware::from_fn({
                let ready = ready.clone();
                let release = release.clone();
                move |req: axum::extract::Request, next: axum::middleware::Next| {
                    let ready = ready.clone();
                    let release = release.clone();
                    async move {
                        let response = next.run(req).await;
                        assert_eq!(response.status(), 101);
                        ready.notify_one();
                        release.notified().await;
                        response
                    }
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut visitor = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let gated_server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        visitor
            .write_all(format!("{REQUEST}\r\n").as_bytes())
            .await
            .unwrap();
        let mut backend = session.accept_stream().await.unwrap().unwrap();
        backend_head(&mut backend).await;
        backend
            .write_all(format!("{RESPONSE}\r\n").as_bytes())
            .await
            .unwrap();
        ready.notified().await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(6)).await;
        assert_eq!(
            backend.read_u8().await.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        release.notify_one();
        assert!(head(&mut visitor).await.starts_with("HTTP/1.1 101"));
        let mut bytes = Vec::new();
        visitor.read_to_end(&mut bytes).await.unwrap();
        assert!(bytes.is_empty());
        gated_server.abort();
        server.join.abort();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn upgrade_routing_and_reserved_control_auth_are_unchanged() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let (server, state, mut session) = fixture().await;
        for (request, status) in [
            (
                REQUEST.replace("ws.t.example.com", "unknown.t.example.com") + "\r\n",
                404,
            ),
            (REQUEST.replace("/socket?q=1", "/control") + "\r\n", 401),
        ] {
            let mut visitor = TcpStream::connect(server.local_addr).await.unwrap();
            visitor.write_all(request.as_bytes()).await.unwrap();
            assert!(head(&mut visitor)
                .await
                .starts_with(&format!("HTTP/1.1 {status}")));
        }
        let req = Request::builder()
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            tnld::data_plane::handler(State(state.clone()), peer(), req)
                .await
                .status(),
            400
        );
        let t = state.registry.find_by_hostname("ws.t.example.com").unwrap();
        let id = state.registry.current_session_id(&t.id).unwrap();
        state.registry.drop_session(&id, 30);
        state.session_handles.remove(&id);
        let mut visitor = TcpStream::connect(server.local_addr).await.unwrap();
        visitor
            .write_all(format!("{REQUEST}\r\n").as_bytes())
            .await
            .unwrap();
        assert!(head(&mut visitor).await.starts_with("HTTP/1.1 503"));
        assert!(matches!(
            tokio::time::timeout(Duration::from_millis(50), session.accept_stream()).await,
            Err(_) | Ok(Ok(None))
        ));
        server.join.abort();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn ambiguous_wire_framing_and_visible_duplicate_zero_length_are_rejected() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let (server, state, mut session) = fixture().await;
        for framing in [
            "Content-Length: 0\r\nContent-Length: 1",
            "Content-Length: 1",
            "Content-Length: 0, 0",
            "Transfer-Encoding: chunked\r\nContent-Length: 0",
        ] {
            let mut visitor = TcpStream::connect(server.local_addr).await.unwrap();
            visitor
                .write_all(format!("{REQUEST}{framing}\r\n\r\n").as_bytes())
                .await
                .unwrap();
            assert!(head(&mut visitor).await.starts_with("HTTP/1.1 400"));
        }
        let mut request = synthetic_upgrade_request();
        request
            .headers_mut()
            .append("content-length", "0".parse().unwrap());
        request
            .headers_mut()
            .append("content-length", "0".parse().unwrap());
        assert_eq!(
            tnld::data_plane::handler(State(state), peer(), request)
                .await
                .status(),
            400
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), session.accept_stream())
                .await
                .is_err()
        );
        server.join.abort();
    })
    .await
    .unwrap();
}
