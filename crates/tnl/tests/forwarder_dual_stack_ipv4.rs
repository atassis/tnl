//! Listener bound IPv4-only; `Target::LocalhostPort` must still resolve and
//! connect. Asserts the request body is forwarded and the backend response
//! returns intact.

use std::pin::Pin;
use tnl::forwarder::{forward, ForwardCtx};
use tnl::target::Target;
use tnl_protocol::Stream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn ipv4_only_backend_forwarded_via_localhost_port() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 1024];
        let n = s.read(&mut buf).await.unwrap();
        assert!(n > 0);
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nHELLO")
            .await
            .unwrap();
    });

    let (a, b) = tokio::io::duplex(64 * 1024);
    let substream: Pin<Box<dyn Stream>> = Box::pin(b);
    let target = Target::LocalhostPort(port);
    let ctx = ForwardCtx {
        tunnel: "demo".into(),
        log_tx: None,
        version: env!("CARGO_PKG_VERSION"),
    };

    let driver = tokio::spawn(forward(substream, target, ctx));

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
        std::str::from_utf8(&got)
            .unwrap()
            .starts_with("HTTP/1.1 200 OK"),
        "got: {:?}",
        String::from_utf8_lossy(&got)
    );
    assert!(got.ends_with(b"HELLO"));
}
