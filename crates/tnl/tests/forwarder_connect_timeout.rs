//! Black-hole route: TEST-NET-1 (192.0.2.0/24, RFC 5737) is reserved for
//! documentation; routing to it normally times out. If your test environment
//! happens to short-circuit this range (some isolated containers), the test
//! will see connect-unreachable instead — both kinds satisfy the contract.

use std::pin::Pin;
use std::time::Duration;

use tnl::forwarder::{forward, ForwardCtx};
use tnl::target::Target;
use tnl_protocol::Stream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn synth_502_on_unreachable_or_timeout() {
    let addr: std::net::SocketAddr = "192.0.2.1:1".parse().unwrap();
    let target = Target::Explicit(addr);

    let (a, b) = tokio::io::duplex(64 * 1024);
    let driver = tokio::spawn(forward(
        Box::pin(b) as Pin<Box<dyn Stream>>,
        target,
        ForwardCtx::new("demo".into(), None, env!("CARGO_PKG_VERSION")),
    ));

    let (mut ar, mut aw) = tokio::io::split(a);
    aw.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    aw.shutdown().await.ok();

    // Cap at LOCAL_CONNECT_TIMEOUT (3s) + slack so a stuck driver doesn't hang the suite.
    let mut got = Vec::new();
    tokio::time::timeout(Duration::from_secs(8), ar.read_to_end(&mut got))
        .await
        .expect("read_to_end stalled")
        .unwrap();
    driver.await.unwrap().unwrap();

    let s = std::str::from_utf8(&got).unwrap();
    assert!(s.starts_with("HTTP/1.1 502 Bad Gateway"), "{s}");
    assert!(s.contains("X-Tnl-Component: client\r\n"));
    let kind_line = s
        .lines()
        .find(|l| l.starts_with("X-Tnl-Origin-Failure: "))
        .expect("kind header");
    assert!(
        kind_line.contains("connect-timeout") || kind_line.contains("connect-unreachable"),
        "{kind_line}"
    );
}
