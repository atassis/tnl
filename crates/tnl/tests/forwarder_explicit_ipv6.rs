use std::pin::Pin;
use tnl::forwarder::{forward, ForwardCtx};
use tnl::target::Target;
use tnl_protocol::Stream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn explicit_ipv6_target() {
    let listener = match tokio::net::TcpListener::bind("[::1]:0").await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("skipping: ipv6 loopback not available: {e}");
            return;
        }
    };
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 1024];
        let _ = s.read(&mut buf).await.unwrap();
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nV6")
            .await
            .unwrap();
    });

    let (a, b) = tokio::io::duplex(64 * 1024);
    let driver = tokio::spawn(forward(
        Box::pin(b) as Pin<Box<dyn Stream>>,
        Target::Explicit(addr),
        ForwardCtx::new("demo".into(), None, env!("CARGO_PKG_VERSION")),
    ));
    let (mut ar, mut aw) = tokio::io::split(a);
    aw.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    aw.shutdown().await.unwrap();
    let mut got = Vec::new();
    ar.read_to_end(&mut got).await.unwrap();
    driver.await.unwrap().unwrap();
    assert!(got.ends_with(b"V6"));
}
