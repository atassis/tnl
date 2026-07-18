use std::pin::Pin;
use tnl::forwarder::{forward, ForwardCtx};
use tnl::target::Target;
use tnl_protocol::Stream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn synth_502_on_local_malformed() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 1024];
        let _ = s.read(&mut buf).await.unwrap();
        s.write_all(b"hello this is not http\r\n\r\n")
            .await
            .unwrap();
    });

    let (a, b) = tokio::io::duplex(64 * 1024);
    let driver = tokio::spawn(forward(
        Box::pin(b) as Pin<Box<dyn Stream>>,
        Target::LocalhostPort(port),
        ForwardCtx::new("demo".into(), None, env!("CARGO_PKG_VERSION")),
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
    assert!(s.starts_with("HTTP/1.1 502 Bad Gateway"), "{s}");
    assert!(s.contains("X-Tnl-Origin-Failure: local-malformed\r\n"));
}
