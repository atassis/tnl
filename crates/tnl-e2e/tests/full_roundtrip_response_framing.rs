//! Response-framing regressions the hand-rolled daemon parser got wrong and
//! hyper gets right:
//!  * `Transfer-Encoding: chunked` — decoded and relayed (old coverage lived in
//!    a `data_plane.rs` unit test that died with `build_response_from_raw`).
//!  * A leading `1xx` informational response (103 Early Hints) — must be
//!    skipped so the end-user sees the *final* response, not the 1xx with the
//!    real response smuggled into its body.

use std::time::Duration;

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Spawn a raw-TCP backend that replies to every connection with `raw_response`
/// bytes verbatim, then closes.
async fn spawn_raw_backend(raw_response: &'static [u8]) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let _ = sock.write_all(raw_response).await;
                let _ = sock.flush().await;
                let _ = sock.shutdown().await;
            });
        }
    });
    port
}

/// Boilerplate: stand up tnld + a tnl client pointed at `backend_port`, return
/// the daemon's data-plane address and the tunnel subdomain host.
async fn tunnel_to(
    backend_port: u16,
    subdomain: &'static str,
) -> (String, tokio::task::JoinHandle<()>) {
    let hash = hash_plaintext("tnl_FRAMESECRET").unwrap();
    let tokens = TokensFile {
        tokens: vec![TokenEntry {
            name: "e2e".into(),
            hash,
        }],
    };
    let tmp_tokens = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp_tokens.path(), toml::to_string(&tokens).unwrap()).unwrap();
    // Leak the tempfile so it outlives this fn (daemon reads it lazily-ish).
    let tmp_path = tmp_tokens.into_temp_path();
    let tokens_file = tmp_path.to_string_lossy().into_owned();
    std::mem::forget(tmp_path);

    let cfg = TnldConfig {
        listen: "127.0.0.1:0".into(),
        public_url: "http://test".into(),
        hostname_root: "t.example.com".into(),
        tokens_file,
        session_grace_sec: 30,
    };
    let tnld_handle = spawn_server(cfg).await.unwrap();
    let tnld_addr = tnld_handle.local_addr.to_string();

    let session = tnl::client::connect_and_create(
        &format!("http://{tnld_addr}"),
        "tnl_FRAMESECRET",
        subdomain,
    )
    .await
    .unwrap();
    let session_box = session.session;
    let ctrl = session.control;
    let accept = tokio::spawn(async move {
        let _ctrl = ctrl;
        let _ = tnl::client::run_accept_loop(
            session_box,
            tnl::target::Target::LocalhostPort(backend_port),
            tnl::forwarder::ForwardCtx::new(subdomain.into(), None, env!("CARGO_PKG_VERSION")),
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    (tnld_addr, accept)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chunked_response_is_decoded_and_relayed() {
    let backend = spawn_raw_backend(
        b"HTTP/1.1 200 OK\r\n\
          Content-Type: text/plain\r\n\
          Transfer-Encoding: chunked\r\n\
          Connection: close\r\n\r\n\
          5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
    )
    .await;
    let (tnld_addr, _accept) = tunnel_to(backend, "chunk").await;

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();
    let resp = client
        .get(format!("http://{tnld_addr}/stream"))
        .header("Host", "chunk.t.example.com")
        .header("Connection", "close")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "hello world",
        "chunked body must be decoded; got {body:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leading_1xx_informational_response_is_skipped() {
    // A 103 Early Hints interim response precedes the real 200. The end-user
    // must receive the 200 and its body — never the 103, and never the real
    // response bytes smuggled into a 103's body.
    let backend = spawn_raw_backend(
        b"HTTP/1.1 103 Early Hints\r\n\
          Link: </style.css>; rel=preload\r\n\r\n\
          HTTP/1.1 200 OK\r\n\
          Content-Type: text/plain\r\n\
          Content-Length: 2\r\n\
          Connection: close\r\n\r\n\
          hi",
    )
    .await;
    let (tnld_addr, _accept) = tunnel_to(backend, "hints").await;

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();
    let resp = client
        .get(format!("http://{tnld_addr}/page"))
        .header("Host", "hints.t.example.com")
        .header("Connection", "close")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        200,
        "end-user must see the final 200, not the interim 103",
    );
    let body = resp.text().await.unwrap();
    assert_eq!(body, "hi", "got: {body:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unrelayable_response_is_attributed_to_upstream_not_client() {
    // The client's forwarder waves through any first line starting with
    // `HTTP/2` (peek.rs `looks_http`). hyper's HTTP/1 client then cannot relay
    // it. The end-user must get an HONEST attribution: `upstream` (the local
    // backend), NOT `client`, and with no "update your client" version-shaming.
    let backend = spawn_raw_backend(b"HTTP/2 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let (tnld_addr, _accept) = tunnel_to(backend, "nothttp").await;

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();
    let resp = client
        .get(format!("http://{tnld_addr}/x"))
        .header("Host", "nothttp.t.example.com")
        .header("Connection", "close")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 502);
    let component = resp
        .headers()
        .get("x-tnl-component")
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    assert_eq!(
        component.as_deref(),
        Some("upstream"),
        "an unrelayable backend response must be blamed on `upstream`, not the client",
    );
    let body = resp.text().await.unwrap();
    assert!(
        !body.contains("Update to tnl"),
        "the error must not shame the client version; got: {body:?}",
    );
    assert!(
        !body
            .to_lowercase()
            .contains("component <code>client</code>")
            && !body.contains("\"component\":\"client\""),
        "the error must not attribute to the client; got: {body:?}",
    );
}
