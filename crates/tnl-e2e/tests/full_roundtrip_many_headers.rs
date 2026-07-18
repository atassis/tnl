//! Regression: a backend that returns MORE than 64 response headers must be
//! relayed to the end-user unchanged, not rejected as "malformed".
//!
//! The pre-hyper daemon parsed responses with a fixed `[httparse::EMPTY_HEADER;
//! 64]` array and turned `TooManyHeaders` into a 502 `client-malformed-response`
//! — blaming the client for a perfectly valid (if fat) backend response.
//! Real apps stacking `helmet`/CORS/many `Set-Cookie` headers hit this.

use std::fmt::Write as _;
use std::time::Duration;

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Raw-TCP backend that reads one request then writes a response carrying
/// `header_count` distinct `X-H-<n>` headers plus a small body, then closes.
async fn spawn_many_header_backend(header_count: usize) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                // Drain the request head (up to the blank line).
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;

                let mut resp = String::from("HTTP/1.1 200 OK\r\n");
                for i in 0..header_count {
                    let _ = write!(resp, "X-H-{i}: v{i}");
                    resp.push_str("\r\n");
                }
                resp.push_str("Content-Type: text/plain\r\n");
                resp.push_str("Content-Length: 13\r\n");
                resp.push_str("Connection: close\r\n\r\n");
                resp.push_str("hello, world\n");
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
                let _ = sock.shutdown().await;
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backend_with_more_than_64_headers_is_relayed_verbatim() {
    // 1. Backend that returns 65 custom headers (> the old 64-slot cap).
    let backend_port = spawn_many_header_backend(65).await;

    // 2. tnld
    let hash = hash_plaintext("tnl_HDRSECRET").unwrap();
    let tokens = TokensFile {
        tokens: vec![TokenEntry {
            name: "e2e".into(),
            hash,
        }],
    };
    let tmp_tokens = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp_tokens.path(), toml::to_string(&tokens).unwrap()).unwrap();
    let cfg = TnldConfig {
        listen: "127.0.0.1:0".into(),
        public_url: "http://test".into(),
        hostname_root: "t.example.com".into(),
        tokens_file: tmp_tokens.path().to_string_lossy().into_owned(),
        session_grace_sec: 30,
    };
    let tnld_handle = spawn_server(cfg).await.unwrap();
    let tnld_addr = tnld_handle.local_addr.to_string();

    // 3. tnl client
    let session =
        tnl::client::connect_and_create(&format!("http://{tnld_addr}"), "tnl_HDRSECRET", "fat")
            .await
            .unwrap();
    let session_box = session.session;
    let _ctrl = session.control;
    let _accept = tokio::spawn(tnl::client::run_accept_loop(
        session_box,
        tnl::target::Target::LocalhostPort(backend_port),
        tnl::forwarder::ForwardCtx {
            tunnel: "fat".into(),
            log_tx: None,
            version: env!("CARGO_PKG_VERSION"),
        },
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 4. End-user request through the data plane.
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();
    let resp = client
        .get(format!("http://{tnld_addr}/whatever"))
        .header("Host", "fat.t.example.com")
        .header("Connection", "close")
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status().as_u16(),
        200,
        "a 65-header backend response must be relayed, not rejected as malformed; \
         x-tnl-component={:?}",
        resp.headers()
            .get("x-tnl-component")
            .and_then(|v| v.to_str().ok()),
    );
    // A representative header from the fat set must survive the relay.
    assert_eq!(
        resp.headers().get("x-h-64").and_then(|v| v.to_str().ok()),
        Some("v64"),
        "the 65th header must be forwarded to the end-user",
    );
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hello, world\n", "got: {body:?}");
}
