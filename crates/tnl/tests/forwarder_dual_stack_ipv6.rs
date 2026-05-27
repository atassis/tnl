//! REGRESSION for the 2026-05-27 field bug: backend bound on `[::1]` only,
//! `Target::LocalhostPort` must still complete the roundtrip via `lookup_host`.

use std::pin::Pin;
use tnl::forwarder::{forward, ForwardCtx};
use tnl::target::Target;
use tnl_protocol::Stream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn ipv6_only_backend_forwarded_via_localhost_port() {
    let listener = match tokio::net::TcpListener::bind("[::1]:0").await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("skipping: ipv6 loopback not available: {e}");
            return;
        }
    };
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 1024];
        let _ = s.read(&mut buf).await.unwrap();
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\nFROM-V6")
            .await
            .unwrap();
    });

    let (a, b) = tokio::io::duplex(64 * 1024);
    let substream: Pin<Box<dyn Stream>> = Box::pin(b);
    let driver = tokio::spawn(forward(
        substream,
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
    // Shut down the write half so the forwarder's req_pump sees EOF and exits,
    // allowing try_join! to complete and yw.shutdown() to propagate EOF to ar.
    aw.shutdown().await.unwrap();
    let mut got = Vec::new();
    ar.read_to_end(&mut got).await.unwrap();

    driver.await.unwrap().unwrap();
    assert!(
        got.ends_with(b"FROM-V6"),
        "{}",
        String::from_utf8_lossy(&got)
    );
}
