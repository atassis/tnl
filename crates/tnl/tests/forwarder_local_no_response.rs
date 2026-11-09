//! Override `LOCAL_FIRST_BYTE_TIMEOUT` to 200ms so the test doesn't wait the
//! full 15 seconds. The override is a process-wide `AtomicU64` — running this
//! test in parallel with other phase-2 tests would taint their behavior.
//! Use `cargo test ... -- --test-threads=1` if flake.

use std::pin::Pin;
use std::sync::atomic::Ordering;
use tnl::{
    forwarder::{forward, ForwardCtx},
    inspection::{Inspection, PreviewMode, ResponseSource, TunnelGeneration},
    target::Target,
};
use tnl_protocol::Stream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn synth_502_on_local_no_response() {
    tnl::forwarder::TEST_FIRST_BYTE_TIMEOUT_MS.store(200, Ordering::Relaxed);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let backend = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        // Keep the read pending behind a fixture barrier; timeout is the actual
        // first-byte deadline, not a scheduling sleep.
        let mut buf = vec![0u8; 1024];
        let _ = s.read(&mut buf).await.unwrap();
        ready_tx.send(()).unwrap();
        release_rx.await.unwrap();
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
    ready_rx.await.unwrap();
    let mut got = Vec::new();
    ar.read_to_end(&mut got).await.unwrap();
    driver.await.unwrap().unwrap();
    release_tx.send(()).unwrap();
    backend.await.unwrap();

    let s = std::str::from_utf8(&got).unwrap();
    assert!(
        s.contains("X-Tnl-Origin-Failure: local-no-response\r\n"),
        "{s}"
    );
    let record = history.get(history.search("")[0].capture_id).unwrap();
    let timing = record.timing.snapshot().unwrap();
    assert_eq!(record.response_source, ResponseSource::ClientSynth);
    assert_eq!(timing.backend_first_byte_offset_us, None);
    assert_eq!(timing.backend_final_headers_offset_us, None);
    assert_eq!(timing.response_framing_end_offset_us, None);

    tnl::forwarder::TEST_FIRST_BYTE_TIMEOUT_MS.store(0, Ordering::Relaxed);
}
