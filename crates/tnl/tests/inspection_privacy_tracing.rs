//! Isolated process so unrelated tests cannot change tracing callsite interest.
use std::time::Duration;
use tnl::forwarder::{forward, ForwardCtx};
use tnl::inspection::{Inspection, PreviewMode, TunnelGeneration};
use tnl::target::Target;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone)]
struct TraceBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for TraceBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut output = self.0.lock().unwrap();
        if output.len() + bytes.len() > 65536 {
            return Err(std::io::ErrorKind::OutOfMemory.into());
        }
        output.extend_from_slice(bytes);
        drop(output);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TraceBuffer {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn full_summary_channel_and_tracing_never_print_capture_secrets() {
    let output = TraceBuffer(std::sync::Arc::new(std::sync::Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .without_time()
        .with_ansi(false)
        .with_writer(output.clone())
        .finish();
    // This test uses Tokio's current-thread runtime: keep the dispatcher alive
    // across the complete exchange, not only individual future polls.
    let dispatch = tracing::Dispatch::new(subscriber);
    let guard = tracing::dispatcher::set_default(&dispatch);
    tracing::callsite::rebuild_interest_cache();
    let history = Inspection::new(PreviewMode::Json);
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let reserved = tx.reserve().await.unwrap();
    let mut ctx = ForwardCtx::new("trace".into(), Some(tx.clone()), env!("CARGO_PKG_VERSION"));
    ctx.inspection = Some((history.clone(), TunnelGeneration::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (mut visitor, substream) = tokio::io::duplex(1024);
    async {
        let backend = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut received = Vec::new(); socket.read_to_end(&mut received).await.unwrap();
            socket.write_all(b"HTTP/1.1 204 No Content\r\n\r\n").await.unwrap();
        };
        let visitor = async {
            visitor.write_all(b"GET /api?token=TRACE_QUERY_CANARY HTTP/1.1\r\nHost: localhost\r\nAuthorization: TRACE_HEADER_CANARY\r\n\r\n").await.unwrap();
            visitor.shutdown().await.unwrap();
            let mut response = Vec::new(); visitor.read_to_end(&mut response).await.unwrap();
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            let (result, (), ()) = tokio::join!(Box::pin(forward(Box::pin(substream), Target::Explicit(addr), ctx)), backend, visitor);
            result.unwrap();
        }).await.unwrap();
    }.await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let mut ctx = ForwardCtx::new(
        "trace-failure".into(),
        Some(tx.clone()),
        env!("CARGO_PKG_VERSION"),
    );
    ctx.inspection = Some((history.clone(), TunnelGeneration::new()));
    let (mut visitor, stream) = tokio::io::duplex(8192);
    let client = async {
        visitor.write_all(b"GET /failure?token=FAILURE_QUERY_CANARY HTTP/1.1\r\nHost: local\r\nAuthorization: FAILURE_HEADER_CANARY\r\n\r\n").await.unwrap();
        visitor.shutdown().await.unwrap();
        let mut response = Vec::new();
        visitor.read_to_end(&mut response).await.unwrap();
    };
    let (result, ()) = tokio::join!(
        Box::pin(forward(Box::pin(stream), Target::Explicit(addr), ctx)),
        client
    );
    result.unwrap();
    let rows = history.search("");
    assert_eq!(rows.len(), 2);
    assert!(!format!("{rows:?}").contains("CANARY"));
    assert!(!serde_json::to_string(&rows).unwrap().contains("CANARY"));
    assert!(history.search("CANARY").is_empty());
    drop(guard);
    drop(reserved);
    let output = output.0.lock().unwrap();
    let trace = std::str::from_utf8(&output).unwrap();
    assert!(trace.contains("request head parsed"), "{trace}");
    assert!(trace.contains("dropping log line"));
    assert!(trace.contains("connect-refused"));
    assert!(!trace.contains("CANARY"));
    drop(output);
}
