//! Application WebSocket relay, not the owner's distinct `/control` connection.
use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use async_tungstenite::tokio::{client_async, TokioAdapter};
use async_tungstenite::tungstenite::{protocol::Role, Message};
use async_tungstenite::WebSocketStream;
use futures::{SinkExt, StreamExt};
use tnl::forwarder::ForwardCtx;
use tnl::host_header::HostHeader;
use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::serve::{spawn_server_with_state, AppState, ServerHandle};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";
const RESPONSE: &str = "HTTP/1.1 101 Switching Protocols\r\nConnection: upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n";

struct Tunnel {
    server: ServerHandle,
    state: AppState,
    accept: JoinHandle<()>,
    _tokens: tempfile::NamedTempFile,
    logs: tokio::sync::mpsc::Receiver<tnl::inspector::LogLine>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.accept.abort();
        self.server.join.abort();
    }
}

async fn tunnel(backend: SocketAddr, host_header: HostHeader, rewrite_active: bool) -> Tunnel {
    let tokens = tempfile::NamedTempFile::new().unwrap();
    let file = TokensFile {
        tokens: vec![TokenEntry {
            name: "ws-test".into(),
            hash: hash_plaintext("tnl_WSSECRET").unwrap(),
        }],
    };
    std::fs::write(tokens.path(), toml::to_string(&file).unwrap()).unwrap();
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
    let connected = tnl::client::connect_and_create(
        &format!("http://{}", server.local_addr),
        "tnl_WSSECRET",
        "ws",
    )
    .await
    .unwrap();
    let (log_tx, logs) = tokio::sync::mpsc::channel(32);
    assert_eq!(
        connected.data_stream_protocol,
        tnl_protocol::request_metadata::DataStreamProtocol::Http1MetadataV1
    );
    let ctx = ForwardCtx {
        data_stream_protocol: connected.data_stream_protocol,
        host_header,
        rewrite_active: Arc::new(AtomicBool::new(rewrite_active)),
        ..ForwardCtx::new("ws".into(), Some(log_tx), env!("CARGO_PKG_VERSION"))
    };
    let accept = tokio::spawn(async move {
        let _control = connected.control;
        tnl::client::run_accept_loop(
            connected.session,
            tnl::target::Target::Explicit(backend),
            ctx,
        )
        .await
        .ok();
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(t) = state.registry.find_by_hostname("ws.t.example.com") {
                if let Some(id) = state.registry.current_session_id(&t.id) {
                    if state.session_handles.contains_key(&id) {
                        break;
                    }
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    Tunnel {
        server,
        state,
        accept,
        _tokens: tokens,
        logs,
    }
}

async fn head(io: &mut (impl AsyncRead + Unpin)) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        bytes.push(io.read_u8().await.unwrap());
        assert!(bytes.len() < 32768);
    }
    String::from_utf8(bytes).unwrap()
}

fn request(extra: &str) -> String {
    format!("GET /socket?q=1 HTTP/1.1\r\nHost: ws.t.example.com\r\nConnection: upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: {KEY}\r\nSec-WebSocket-Version: 13\r\n{extra}\r\n")
}

async fn websocket(addr: SocketAddr) -> WebSocketStream<TokioAdapter<TcpStream>> {
    let req = http::Request::builder()
        .uri(format!("ws://{addr}/socket?q=1"))
        .header("host", "ws.t.example.com")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-key", KEY)
        .header("sec-websocket-version", "13")
        .body(())
        .unwrap();
    client_async(req, TcpStream::connect(addr).await.unwrap())
        .await
        .unwrap()
        .0
}

async fn echo_connection(mut io: TcpStream) {
    let req = head(&mut io).await;
    if req.to_lowercase().contains("upgrade: websocket") {
        io.write_all(RESPONSE.as_bytes()).await.unwrap();
        let mut ws =
            WebSocketStream::from_raw_socket(TokioAdapter::new(io), Role::Server, None).await;
        ws.send(Message::Text("server-first".into())).await.unwrap();
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
        assert!(req.to_lowercase().contains("connection: close"));
        assert!(!req.to_lowercase().contains("x-request-hop"));
        io.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close, x-hop\r\nX-Hop: secret\r\nSet-Cookie: a=1\r\nSet-Cookie: b=2\r\n\r\n2\r\nok\r\n0\r\n\r\n").await.unwrap();
        io.shutdown().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn application_websocket_text_binary_ping_close_and_parallel_http_keepalive() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut tunnel = tunnel(listener.local_addr().unwrap(), HostHeader::Auto, false).await;
        let backend = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            for _ in 0..4 {
                tasks.spawn(echo_connection(listener.accept().await.unwrap().0));
            }
            while let Some(result) = tasks.join_next().await {
                result.unwrap();
            }
        });
        let mut ws = websocket(tunnel.server.local_addr).await;
        assert_eq!(
            ws.next().await.unwrap().unwrap(),
            Message::Text("server-first".into())
        );
        assert!(
            matches!(
                tunnel.logs.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ),
            "an open WebSocket has no close-time summary yet"
        );
        let mut ws2 = websocket(tunnel.server.local_addr).await;
        assert_eq!(
            ws2.next().await.unwrap().unwrap(),
            Message::Text("server-first".into())
        );
        // Both ordinary requests share ONE downstream keepalive socket, but
        // must use two independent fresh tunnel/backend connections.
        let mut http = TcpStream::connect(tunnel.server.local_addr).await.unwrap();
        for _ in 0..2 {
            http.write_all(b"GET /ordinary HTTP/1.1\r\nHost: ws.t.example.com\r\nConnection: x-request-hop\r\nX-Request-Hop: hidden\r\nSec-WebSocket-Key: not-an-upgrade\r\n\r\n")
                .await
                .unwrap();
            let response = head(&mut http).await.to_lowercase();
            assert!(response.starts_with("http/1.1 200"));
            assert!(!response.contains("x-hop:"));
            assert_eq!(response.matches("set-cookie:").count(), 2);
            assert!(!response.contains("connection: close"));
            let mut chunked = [0; 12];
            http.read_exact(&mut chunked).await.unwrap();
            assert_eq!(&chunked, b"2\r\nok\r\n0\r\n\r\n");
        }
        for message in [
            Message::Text("hello".into()),
            Message::Binary(vec![0; 256 * 1024].into()),
            Message::Ping(b"ping".to_vec().into()),
        ] {
            ws.send(message.clone()).await.unwrap();
            let expected = if let Message::Ping(bytes) = message {
                Message::Pong(bytes)
            } else {
                message
            };
            assert_eq!(ws.next().await.unwrap().unwrap(), expected);
        }
        let close = async_tungstenite::tungstenite::protocol::CloseFrame {
            code: async_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal,
            reason: "done".into(),
        };
        ws.send(Message::Close(Some(close.clone()))).await.unwrap();
        assert_eq!(
            ws.next().await.unwrap().unwrap(),
            Message::Close(Some(close))
        );
        ws2.close(None).await.unwrap();
        assert!(matches!(
            ws2.next().await.unwrap().unwrap(),
            Message::Close(_)
        ));
        drop((ws, ws2));
        backend.await.unwrap();
        let mut upgrades = 0;
        for _ in 0..4 {
            let log = tunnel.logs.recv().await.unwrap();
            assert!(log.bytes_in > 0 && log.bytes_out > 0);
            assert_eq!(log.method, "GET");
            if log.path == "/socket?q=1" {
                upgrades += 1;
                assert_eq!(log.status, Some(101));
            } else {
                assert_eq!(log.path, "/ordinary");
                assert_eq!(log.status, Some(200));
            }
        }
        assert_eq!(upgrades, 2);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upgrade_read_ahead_large_bidirectional_and_half_close() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tunnel = tunnel(listener.local_addr().unwrap(), HostHeader::Preserve, false).await;
        let backend = tokio::spawn(async move {
            let (mut io, _) = listener.accept().await.unwrap();
            assert!(head(&mut io).await.starts_with("GET /socket?q=1"));
            // Real frames coalesced with the 101 and with the request head.
            io.write_all([RESPONSE.as_bytes(), b"\x81\x05first"].concat().as_slice())
                .await
                .unwrap();
            let mut early = [0; 11];
            io.read_exact(&mut early).await.unwrap();
            assert_eq!(&early, b"\x81\x85\x00\x00\x00\x00early");
            let (mut r, mut w) = io.into_split();
            let download = async {
                w.write_all(&vec![0x5a; 4 * 1024 * 1024]).await.unwrap();
                w.shutdown().await.unwrap();
            };
            let upload = async {
                let mut bytes = Vec::new();
                r.read_to_end(&mut bytes).await.unwrap();
                assert_eq!(bytes, vec![0xa5; 4 * 1024 * 1024]);
            };
            tokio::join!(download, upload);
        });
        let mut visitor = TcpStream::connect(tunnel.server.local_addr).await.unwrap();
        let req = request("");
        // Fragmented head, then head terminator plus first frame in one write.
        visitor.write_all(&req.as_bytes()[..20]).await.unwrap();
        visitor
            .write_all(
                [&req.as_bytes()[20..], b"\x81\x85\x00\x00\x00\x00early"]
                    .concat()
                    .as_slice(),
            )
            .await
            .unwrap();
        assert!(head(&mut visitor).await.starts_with("HTTP/1.1 101"));
        let mut first = [0; 7];
        visitor.read_exact(&mut first).await.unwrap();
        assert_eq!(&first, b"\x81\x05first");
        let (mut r, mut w) = visitor.into_split();
        let upload = async {
            w.write_all(&vec![0xa5; 4 * 1024 * 1024]).await.unwrap();
            w.shutdown().await.unwrap();
        };
        let download = async {
            let mut bytes = Vec::new();
            r.read_to_end(&mut bytes).await.unwrap();
            assert_eq!(bytes, vec![0x5a; 4 * 1024 * 1024]);
        };
        tokio::join!(upload, download);
        backend.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handshake_headers_host_modes_origin_and_credentials() {
    tokio::time::timeout(Duration::from_secs(20), async {
        for (mode, active, expected) in [
            (HostHeader::Preserve, false, "ws.t.example.com"),
            (HostHeader::Auto, false, "ws.t.example.com"),
            (HostHeader::Rewrite, false, "resolved"),
            (HostHeader::Fixed("fixed.example".into()), false, "fixed.example"),
            (HostHeader::Auto, true, "resolved"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let tunnel = tunnel(addr, mode, active).await;
            let backend = tokio::spawn(async move {
                let (mut io, _) = listener.accept().await.unwrap();
                let req = head(&mut io).await.to_lowercase();
                assert!(req.starts_with("get /socket?q=1 http/1.1"));
                let expected = if expected == "resolved" { addr.to_string() } else { expected.into() };
                for line in [format!("host: {expected}\r\n"), "origin: https://public.example\r\n".into(),
                    "authorization: basic visitor\r\n".into(), "cookie: visitor=yes\r\n".into(),
                    "sec-websocket-protocol: chat, other\r\n".into(), "sec-websocket-extensions: permessage-deflate\r\n".into()] {
                    assert!(req.contains(&line), "missing {line:?}: {req}");
                }
                assert!(!req.contains("tnl_wssecret"));
                io.write_all(RESPONSE.replace("\r\n\r\n", "\r\nSec-WebSocket-Protocol: chat\r\nSec-WebSocket-Extensions: permessage-deflate\r\nSet-Cookie: backend=yes\r\n\r\n").as_bytes()).await.unwrap();
                io.shutdown().await.unwrap();
                assert_eq!(io.read_u8().await.unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
            });
            let mut visitor = TcpStream::connect(tunnel.server.local_addr).await.unwrap();
            visitor.write_all(request("Origin: https://public.example\r\nAuthorization: Basic visitor\r\nCookie: visitor=yes\r\nSec-WebSocket-Protocol: chat, other\r\nSec-WebSocket-Extensions: permessage-deflate\r\n").as_bytes()).await.unwrap();
            let response = head(&mut visitor).await.to_lowercase();
            assert!(response.starts_with("http/1.1 101"), "{response}");
            assert!(response.contains("sec-websocket-protocol: chat\r\n"));
            assert!(response.contains("sec-websocket-extensions: permessage-deflate\r\n"));
            assert!(response.contains("set-cookie: backend=yes\r\n"));
            visitor.shutdown().await.unwrap();
            backend.await.unwrap();
        }
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backend_rejection_remains_streamed_http_without_retry() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tunnel = tunnel(listener.local_addr().unwrap(), HostHeader::Rewrite, false).await;
        let statuses = [200, 401, 403, 426, 302];
        let backend = tokio::spawn(async move {
            for status in statuses {
                let (mut io, _) = listener.accept().await.unwrap();
                let req = head(&mut io).await.to_lowercase();
                if status == 403 {
                    assert!(req.contains("origin: https://denied.example\r\n"));
                } else {
                    assert!(!req.contains("origin:")); // do not fabricate missing Origin
                }
                io.write_all(format!("HTTP/1.1 {status} Refused\r\nContent-Length: 7\r\nSet-Cookie: refusal=yes\r\nConnection: close\r\n\r\nrefused").as_bytes()).await.unwrap();
                io.shutdown().await.unwrap();
            }
            assert!(tokio::time::timeout(Duration::from_millis(100), listener.accept()).await.is_err(), "a refusal must never retry");
        });
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
        for status in statuses {
            let mut req = client.get(format!("http://{}/socket", tunnel.server.local_addr))
                .header("host", "ws.t.example.com").header("connection", "upgrade").header("upgrade", "websocket")
                .header("sec-websocket-key", KEY).header("sec-websocket-version", "13");
            if status == 403 { req = req.header("origin", "https://denied.example"); }
            let response = req.send().await.unwrap();
            assert_eq!(response.status().as_u16(), status);
            assert_eq!(response.headers()["set-cookie"], "refusal=yes");
            assert_eq!(response.text().await.unwrap(), "refused");
        }
        backend.await.unwrap();
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_session_drop_closes_existing_websocket_and_backend() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tunnel = tunnel(listener.local_addr().unwrap(), HostHeader::Preserve, false).await;
        let backend = tokio::spawn(async move {
            let (mut io, _) = listener.accept().await.unwrap();
            head(&mut io).await;
            io.write_all(RESPONSE.as_bytes()).await.unwrap();
            let mut bytes = Vec::new();
            io.read_to_end(&mut bytes).await.unwrap();
            assert!(bytes.is_empty());
        });
        let mut visitor = TcpStream::connect(tunnel.server.local_addr).await.unwrap();
        visitor.write_all(request("").as_bytes()).await.unwrap();
        assert!(head(&mut visitor).await.starts_with("HTTP/1.1 101"));
        // Exercise the real daemon Session.close(), not a detached reconnect
        // test hook. Both the visitor and client backend must see EOF.
        let t = tunnel
            .state
            .registry
            .find_by_hostname("ws.t.example.com")
            .unwrap();
        let id = tunnel.state.registry.current_session_id(&t.id).unwrap();
        let handle = tunnel.state.session_handles.get(&id).unwrap().clone();
        handle.session.lock().await.close().await.unwrap();
        let mut bytes = Vec::new();
        let _ = visitor.read_to_end(&mut bytes).await;
        assert!(bytes.is_empty()); // after101: close only, never an HTTP error body
        backend.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_reader_backpressures_without_blocking_reverse_or_parallel_http() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tunnel = tunnel(listener.local_addr().unwrap(), HostHeader::Preserve, false).await;
        let (blocked_tx, blocked_rx) = tokio::sync::oneshot::channel();
        let (reverse_tx, reverse_rx) = tokio::sync::oneshot::channel();
        let backend = tokio::spawn(async move {
            let (mut io, _) = listener.accept().await.unwrap();
            head(&mut io).await;
            io.write_all(RESPONSE.as_bytes()).await.unwrap();
            let writer = tokio::spawn(async move {
                let (mut r, mut w) = io.into_split();
                let bytes = vec![0x5a; 16 * 1024 * 1024];
                let download = w.write_all(&bytes);
                tokio::pin!(download);
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), &mut download)
                        .await
                        .is_err(),
                    "paused visitor must backpressure the producer"
                );
                blocked_tx.send(()).unwrap();
                let reverse = async {
                    let mut bytes = Vec::new();
                    r.read_to_end(&mut bytes).await.unwrap();
                    assert_eq!(bytes, b"reverse");
                    reverse_tx.send(()).unwrap();
                };
                let (result, ()) = tokio::join!(&mut download, reverse);
                result.unwrap();
                w.shutdown().await.unwrap();
            });
            echo_connection(listener.accept().await.unwrap().0).await;
            writer.await.unwrap();
        });
        let mut visitor = TcpStream::connect(tunnel.server.local_addr).await.unwrap();
        visitor.write_all(request("").as_bytes()).await.unwrap();
        assert!(head(&mut visitor).await.starts_with("HTTP/1.1 101"));
        blocked_rx.await.unwrap();
        visitor.write_all(b"reverse").await.unwrap();
        visitor.shutdown().await.unwrap();
        reverse_rx.await.unwrap();
        let response = reqwest::Client::new()
            .get(format!("http://{}/ordinary", tunnel.server.local_addr))
            .header("host", "ws.t.example.com")
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "ok");
        let mut bytes = Vec::new();
        visitor.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, vec![0x5a; 16 * 1024 * 1024]);
        backend.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_resets_do_not_poison_other_websockets_or_http() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tunnel = tunnel(listener.local_addr().unwrap(), HostHeader::Preserve, false).await;
        let (reset_tx, reset_rx) = tokio::sync::oneshot::channel();
        let backend = tokio::spawn(async move {
            // Visitor aborts the first upgraded connection.
            let (mut io, _) = listener.accept().await.unwrap();
            head(&mut io).await;
            io.write_all(RESPONSE.as_bytes()).await.unwrap();
            let mut bytes = Vec::new();
            let _ = io.read_to_end(&mut bytes).await;
            assert!(bytes.is_empty());
            io.shutdown().await.ok();
            drop(io);
            // Backend aborts the next one, but only after the visitor has 101.
            let (mut io, _) = listener.accept().await.unwrap();
            head(&mut io).await;
            io.write_all(RESPONSE.as_bytes()).await.unwrap();
            reset_rx.await.unwrap();
            io.set_zero_linger().unwrap();
            drop(io);
            // New upgrades and HTTP still use the original owner session.
            echo_connection(listener.accept().await.unwrap().0).await;
            echo_connection(listener.accept().await.unwrap().0).await;
        });
        let mut visitor = TcpStream::connect(tunnel.server.local_addr).await.unwrap();
        visitor.write_all(request("").as_bytes()).await.unwrap();
        assert!(head(&mut visitor).await.starts_with("HTTP/1.1 101"));
        visitor.set_zero_linger().unwrap();
        drop(visitor);
        let mut visitor = TcpStream::connect(tunnel.server.local_addr).await.unwrap();
        visitor.write_all(request("").as_bytes()).await.unwrap();
        assert!(head(&mut visitor).await.starts_with("HTTP/1.1 101"));
        reset_tx.send(()).unwrap();
        let mut bytes = Vec::new();
        let _ = visitor.read_to_end(&mut bytes).await;
        assert!(bytes.is_empty());
        drop(visitor);
        let mut ws = websocket(tunnel.server.local_addr).await;
        assert_eq!(
            ws.next().await.unwrap().unwrap(),
            Message::Text("server-first".into())
        );
        ws.close(None).await.unwrap();
        assert!(matches!(
            ws.next().await.unwrap().unwrap(),
            Message::Close(_)
        ));
        drop(ws);
        let response = reqwest::Client::new()
            .get(format!("http://{}/ordinary", tunnel.server.local_addr))
            .header("host", "ws.t.example.com")
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "ok");
        backend.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upgrade_local_eof_malformed_and_connect_refusal_remain_safe_http_errors() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tunnel = tunnel(listener.local_addr().unwrap(), HostHeader::Preserve, false).await;
        let mut backend = Some(tokio::spawn(async move {
            for bytes in [b"".as_slice(), b"not HTTP\r\n", b"HTTP/2 200 OK\r\n\r\n"] {
                let (mut io, _) = listener.accept().await.unwrap();
                head(&mut io).await;
                io.write_all(bytes).await.unwrap();
                io.shutdown().await.unwrap();
            }
            // Drops listener so the fourth request exercises connect-refused.
        }));
        for (index, component) in ["client", "client", "upstream", "client"]
            .into_iter()
            .enumerate()
        {
            if index == 3 {
                backend.take().unwrap().await.unwrap();
            }
            let response = reqwest::Client::new()
                .get(format!("http://{}/socket", tunnel.server.local_addr))
                .header("host", "ws.t.example.com")
                .header("connection", "upgrade")
                .header("upgrade", "websocket")
                .header("sec-websocket-key", KEY)
                .header("sec-websocket-version", "13")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), 502);
            assert_eq!(response.headers()["x-tnl-component"], component);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert!(!response.text().await.unwrap().is_empty());
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn websocket_request_id_overrides_duplicates_and_connection_nominations() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut tunnel = tunnel(listener.local_addr().unwrap(), HostHeader::Preserve, false).await;
        let backend = tokio::spawn(async move {
            let mut ids = Vec::new();
            for nominate_response_id in [false, true, false, true] {
                let (mut io, _) = listener.accept().await.unwrap();
                let req = head(&mut io).await;
                let ids_in_head: Vec<_> = req.lines().filter_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("x-tnl-request-id").then(|| value.trim())
                }).collect();
                assert_eq!(ids_in_head.len(), 1, "{req}");
                ids.push(ids_in_head[0].to_string());
                assert!(req.to_lowercase().contains("connection: upgrade\r\n"));
                let nomination = if nominate_response_id {
                    "Connection: X-Tnl-Request-Id\r\n"
                } else {
                    ""
                };
                let response = RESPONSE.replace("\r\n\r\n", &format!(
                    "\r\n{nomination}X-Tnl-Request-Id: backend-forgery\r\nX-Tnl-Request-Id: 01BX5ZZKBKACTAV9WEVGEMMVRZ\r\nSec-WebSocket-Protocol: chat\r\n\r\n"
                ));
                io.write_all(response.as_bytes()).await.unwrap();
                let mut ws = WebSocketStream::from_raw_socket(TokioAdapter::new(io), Role::Server, None).await;
                let payload = Message::Binary(vec![0, 255, 128, 1, 2].into());
                assert_eq!(ws.next().await.unwrap().unwrap(), payload);
                ws.send(payload).await.unwrap();
                assert!(matches!(ws.next().await.unwrap().unwrap(), Message::Close(_)));
                ws.flush().await.unwrap();
            }
            ids
        });
        let mut public_ids = Vec::new();
        for nominate_request_id in [false, true] {
            for _ in [false, true] {
                let mut req = http::Request::builder()
                    .uri(format!("ws://{}/socket?q=1", tunnel.server.local_addr))
                    .header("host", "ws.t.example.com")
                    .header("connection", "upgrade")
                    .header("upgrade", "websocket")
                    .header("sec-websocket-key", KEY)
                    .header("sec-websocket-version", "13")
                    .header("sec-websocket-protocol", "chat, other")
                    .header("X-Tnl-Request-Id", "visitor-forgery")
                    .header("x-tnl-request-id", "01BX5ZZKBKACTAV9WEVGEMMVRZ")
                    .body(())
                    .unwrap();
                if nominate_request_id {
                    req.headers_mut().append("connection", "X-Tnl-Request-Id".parse().unwrap());
                }
                let (mut ws, response) = client_async(req, TcpStream::connect(tunnel.server.local_addr).await.unwrap()).await.unwrap();
                assert_eq!(response.status(), 101);
                assert_eq!(response.headers()["sec-websocket-accept"], "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
                assert_eq!(response.headers()["sec-websocket-protocol"], "chat");
                assert_eq!(response.headers().get_all("x-tnl-request-id").iter().count(), 1);
                let id = response.headers()["x-tnl-request-id"].to_str().unwrap().to_string();
                assert_eq!(id.len(), 26);
                assert_ne!(id, "01BX5ZZKBKACTAV9WEVGEMMVRZ");
                assert!(!public_ids.contains(&id));
                public_ids.push(id.clone());
                let payload = Message::Binary(vec![0, 255, 128, 1, 2].into());
                ws.send(payload.clone()).await.unwrap();
                assert_eq!(ws.next().await.unwrap().unwrap(), payload);
                ws.close(None).await.unwrap();
                assert!(matches!(ws.next().await.unwrap().unwrap(), Message::Close(_)));
                drop(ws);
                let log = tunnel.logs.recv().await.unwrap();
                assert_eq!(log.req_id.to_string(), id);
                assert_eq!(log.status, Some(101));
                assert_eq!(log.path, "/socket?q=1");
            }
        }
        assert_eq!(backend.await.unwrap(), public_ids);
    }).await.expect("WS request-ID correlation or opaque echo stalled");
}
