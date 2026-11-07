use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{ConnectInfo, State};
use tnl_protocol::request_metadata::DataStreamProtocol;
use tnl_protocol::{Session, Stream};
use tokio::io::DuplexStream;

struct OneStreamSession<T = DuplexStream>(Option<T>);
#[async_trait::async_trait]
impl<T: Stream + Sync + 'static> Session for OneStreamSession<T> {
    async fn open_stream(&mut self) -> anyhow::Result<Pin<Box<dyn Stream>>> {
        Ok(Box::pin(
            self.0
                .take()
                .ok_or_else(|| anyhow::anyhow!("already opened"))?,
        ))
    }
    async fn accept_stream(&mut self) -> anyhow::Result<Option<Pin<Box<dyn Stream>>>> {
        Ok(None)
    }
    async fn ping(&mut self) -> anyhow::Result<Duration> {
        Ok(Duration::ZERO)
    }
    async fn close(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn data_plane_request_metadata_write_failure_and_timeout_are_transport_502() {
    for stalled in [false, true] {
        let file = tempfile::NamedTempFile::new().unwrap();
        let (server, state) = tnld::serve::spawn_server_with_state(tnld::config::Config {
            listen: "127.0.0.1:0".into(),
            public_url: "http://x".into(),
            hostname_root: "t.x".into(),
            tokens_file: file.path().to_string_lossy().into_owned(),
            session_grace_sec: 30,
            trusted_proxies: vec![],
        })
        .await
        .unwrap();
        let session = state.registry.register_session("test");
        state
            .registry
            .create_tunnel("demo", &session.id, "test")
            .unwrap();
        let (writer, peer) = tokio::io::duplex(8);
        // A live unread peer stalls the write; a dropped peer fails immediately.
        let _peer = stalled.then_some(peer);
        state.session_handles.insert(
            session.id.clone(),
            tnld::serve::SessionHandle {
                session: Arc::new(tokio::sync::Mutex::new(Box::new(OneStreamSession(Some(
                    writer,
                ))))),
                data_stream_protocol: DataStreamProtocol::Http1MetadataV1,
            },
        );
        let req = axum::http::Request::builder()
            .uri("/failure")
            .header("Host", "demo.t.x")
            .header("Accept", "application/json")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = tokio::time::timeout(
            Duration::from_secs(6),
            tnld::data_plane::handler(
                State(state),
                ConnectInfo("127.0.0.1:12345".parse().unwrap()),
                req,
            ),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), 502);
        assert_eq!(response.headers()["x-tnl-component"], "transport");
        let body = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["kind"], "request-metadata-write");
        assert!(json["request_id"]
            .as_str()
            .unwrap()
            .parse::<ulid::Ulid>()
            .is_ok());
        server.join.abort();
    }
}

#[derive(Clone, Default)]
struct VecWriter(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for VecWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for VecWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

// Force a driver error only at shutdown, after a valid response was delivered.
struct ShutdownErrorStream(DuplexStream);

impl tokio::io::AsyncRead for ShutdownErrorStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for ShutdownErrorStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, bytes)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "test shutdown failure",
        )))
    }
}

#[tokio::test]
async fn data_plane_connection_error_diagnostic_keeps_request_identity_span() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let sink = VecWriter::default();
    tracing_subscriber::fmt()
        .with_env_filter("tnld=debug")
        .with_ansi(false)
        .with_writer(sink.clone())
        .try_init()
        .unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    let (server, state) = tnld::serve::spawn_server_with_state(tnld::config::Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://x".into(),
        hostname_root: "t.x".into(),
        tokens_file: file.path().to_string_lossy().into_owned(),
        session_grace_sec: 30,
        trusted_proxies: vec!["127.0.0.1/32".into()],
    })
    .await
    .unwrap();
    let session = state.registry.register_session("test");
    state
        .registry
        .create_tunnel("demo", &session.id, "test")
        .unwrap();
    let (writer, mut peer) = tokio::io::duplex(1024);
    state.session_handles.insert(
        session.id.clone(),
        tnld::serve::SessionHandle {
            session: Arc::new(tokio::sync::Mutex::new(Box::new(OneStreamSession(Some(
                ShutdownErrorStream(writer),
            ))))),
            data_stream_protocol: DataStreamProtocol::RawHttp1,
        },
    );
    let valid_response = tokio::spawn(async move {
        let mut head = Vec::new();
        while !head.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            head.push(peer.read_u8().await.unwrap());
        }
        peer.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        peer.shutdown().await.unwrap();
    });
    let req = axum::http::Request::builder()
        .uri("/connection-error")
        .header("Host", "demo.t.x")
        .header("X-Forwarded-For", "198.51.100.7")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        tnld::data_plane::handler(
            State(state),
            ConnectInfo("127.0.0.1:12345".parse().unwrap()),
            req,
        ),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), 200);
    assert!(axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap()
        .is_empty());
    valid_response.await.unwrap();
    let diagnostic = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let captured = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
            if let Some(line) = captured
                .lines()
                .find(|line| line.contains("tunnel client connection ended"))
            {
                break line.to_string();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("connection driver must emit its error diagnostic");
    assert!(
        diagnostic.contains("visitor_ip=198.51.100.7"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("peer_ip=127.0.0.1"), "{diagnostic}");
    assert!(
        diagnostic.contains("visitor_ip_source=\"x_forwarded_for\""),
        "{diagnostic}"
    );
    server.join.abort();
}
