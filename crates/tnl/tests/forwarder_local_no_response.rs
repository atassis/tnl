//! Override `LOCAL_FIRST_BYTE_TIMEOUT` to 200ms so the test doesn't wait the
//! full 15 seconds. The override is a process-wide `AtomicU64` — running this
//! test in parallel with other phase-2 tests would taint their behavior.
//! Use `cargo test ... -- --test-threads=1` if flake.

use std::pin::Pin;
use std::sync::atomic::Ordering;
use tnl::forwarder::{forward, ForwardCtx};
use tnl::target::Target;
use tnl_protocol::Stream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn synth_502_on_local_no_response() {
    tnl::forwarder::TEST_FIRST_BYTE_TIMEOUT_MS.store(200, Ordering::Relaxed);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        // Read but never reply.
        let mut buf = vec![0u8; 1024];
        let _ = s.read(&mut buf).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    });

    let (a, b) = tokio::io::duplex(64 * 1024);
    let driver = tokio::spawn(forward(
        Box::pin(b) as Pin<Box<dyn Stream>>,
        Target::LocalhostPort(port),
        ForwardCtx {
            tunnel: "demo".into(),
            log_tx: None,
            version: env!("CARGO_PKG_VERSION"),
        },
    ));
    let (mut ar, mut aw) = tokio::io::split(a);
    aw.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    aw.shutdown().await.ok();
    let mut got = Vec::new();
    ar.read_to_end(&mut got).await.unwrap();
    driver.await.unwrap().unwrap();

    let s = std::str::from_utf8(&got).unwrap();
    assert!(
        s.contains("X-Tnl-Origin-Failure: local-no-response\r\n"),
        "{s}"
    );

    tnl::forwarder::TEST_FIRST_BYTE_TIMEOUT_MS.store(0, Ordering::Relaxed);
}
