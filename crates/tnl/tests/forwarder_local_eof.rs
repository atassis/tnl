use std::pin::Pin;
use tnl::forwarder::{forward, ForwardCtx};
use tnl::inspection::{Inspection, PreviewMode, ResponseSource, TunnelGeneration};
use tnl::target::Target;
use tnl_protocol::Stream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn synth_502_on_local_eof() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let backend = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        // Only close after a complete valid request has reached the backend;
        // unlike the no-head fixture this is an admitted forwarding exchange.
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(s.read_u8().await.unwrap());
        }
        assert_eq!(head, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        s.shutdown().await.unwrap();
    });

    let history = Inspection::new(PreviewMode::Off);
    let mut ctx = ForwardCtx::new("demo".into(), None, env!("CARGO_PKG_VERSION"));
    ctx.inspection = Some((history.clone(), TunnelGeneration::new()));
    let (a, b) = tokio::io::duplex(64 * 1024);
    let driver = tokio::spawn(forward(
        Box::pin(b) as Pin<Box<dyn Stream>>,
        Target::LocalhostPort(port),
        ctx,
    ));
    let (mut ar, mut aw) = tokio::io::split(a);
    aw.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    aw.shutdown().await.ok();
    let mut got = Vec::new();
    ar.read_to_end(&mut got).await.unwrap();
    driver.await.unwrap().unwrap();
    backend.await.unwrap();

    let s = std::str::from_utf8(&got).unwrap();
    assert!(s.starts_with("HTTP/1.1 502 Bad Gateway"), "{s}");
    assert!(s.contains("X-Tnl-Origin-Failure: local-eof\r\n"));
    let record = history.get(history.search("")[0].capture_id).unwrap();
    let timing = record.timing.snapshot().unwrap();
    assert_eq!(record.response_source, ResponseSource::ClientSynth);
    assert!(timing.terminal);
    assert_eq!(timing.backend_first_byte_offset_us, None);
    assert_eq!(timing.backend_final_headers_offset_us, None);
    assert_eq!(timing.response_framing_end_offset_us, None);
    assert_eq!(
        timing.intervals.request_head.outcome,
        tnl_inspector_api::timing::Outcome::Completed
    );
    assert!(timing.request_framing_end_offset_us.is_some());
}
