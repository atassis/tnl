use std::pin::Pin;
use tnl::forwarder::{forward, ForwardCtx};
use tnl::target::Target;
use tnl_protocol::Stream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn synth_502_on_connection_refused() {
    // OS-assigned port with no listener bound.
    let tmp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tmp.local_addr().unwrap().port();
    drop(tmp);

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
    aw.write_all(b"GET / HTTP/1.1\r\nHost: x\r\nAccept: application/json\r\n\r\n")
        .await
        .unwrap();
    aw.shutdown().await.ok();
    let mut got = Vec::new();
    ar.read_to_end(&mut got).await.unwrap();
    driver.await.unwrap().unwrap();

    let s = std::str::from_utf8(&got).unwrap();
    assert!(s.starts_with("HTTP/1.1 502 Bad Gateway"), "{s}");
    assert!(s.contains("X-Tnl-Component: client\r\n"));
    assert!(s.contains("X-Tnl-Origin-Failure: connect-refused\r\n"));
    assert!(s.contains("Content-Type: application/json\r\n"));
}
