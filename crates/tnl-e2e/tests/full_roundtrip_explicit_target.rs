use std::sync::Once;
use std::time::Duration;

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;

static INIT: Once = Once::new();
fn init_tracing() {
    INIT.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("debug,hyper=info,tower=info")
            .try_init();
    });
}

/// E2E test that exercises `Target::Explicit(addr)`: the tnl client is given
/// a concrete `SocketAddr` (e.g. `127.0.0.1:RANDOM`) instead of a bare port.
/// The end-user request must still receive a successful 200 response from the
/// local backend, confirming the explicit-target code path works end-to-end.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn end_to_end_http_request_reaches_explicit_target_backend() {
    init_tracing();

    // 1. Spawn dummy backend on a random local port (IPv4 only)
    let backend = axum::Router::new().route(
        "/api/ping",
        axum::routing::get(|| async { "pong from explicit target\n" }),
    );
    let backend_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_addr: std::net::SocketAddr = backend_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(backend_listener, backend).await.unwrap();
    });

    // 2. Spawn tnld with a fresh tokens.toml
    let hash = hash_plaintext("tnl_EXPLICITTOKEN").unwrap();
    let tokens = TokensFile {
        tokens: vec![TokenEntry {
            name: "e2e-explicit".into(),
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

    // 3. Spawn tnl client using Target::Explicit(addr) — single address,
    //    no DNS, no fallback.
    let session = tnl::client::connect_and_create(
        &format!("http://{tnld_addr}"),
        "tnl_EXPLICITTOKEN",
        "smoke-explicit",
    )
    .await
    .unwrap();
    assert_eq!(session.hostname, "smoke-explicit.t.example.com");
    let session_box = session.session;
    let _ctrl = session.control;
    let _accept = tokio::spawn(tnl::client::run_accept_loop(
        session_box,
        tnl::target::Target::Explicit(backend_addr),
        tnl::forwarder::ForwardCtx {
            tunnel: "smoke-explicit".into(),
            log_tx: None,
            version: env!("CARGO_PKG_VERSION"),
        },
    ));

    // give the daemon a moment to register
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 4. Send a request to tnld DATA-PLANE with Host: smoke-explicit.t.example.com
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();
    let resp = client
        .get(format!("http://{tnld_addr}/api/ping"))
        .header("Host", "smoke-explicit.t.example.com")
        .header("Connection", "close")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        200,
        "expected 200 from explicit-target backend"
    );
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        content_type.starts_with("text/plain"),
        "expected text/plain content-type, got: {content_type:?}"
    );
    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "pong from explicit target\n",
        "unexpected body: {body:?}"
    );
}
