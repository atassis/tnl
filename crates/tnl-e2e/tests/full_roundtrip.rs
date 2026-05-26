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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn end_to_end_http_request_reaches_local_backend() {
    init_tracing();

    // 1. Spawn dummy backend on a random local port
    let backend = axum::Router::new().route(
        "/api/ping",
        axum::routing::get(|| async { "pong from local backend\n" }),
    );
    let backend_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_port = backend_listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(backend_listener, backend).await.unwrap();
    });

    // 2. Spawn tnld with a fresh tokens.toml
    let hash = hash_plaintext("tnl_E2ESECRET").unwrap();
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
    };
    let tnld_handle = spawn_server(cfg).await.unwrap();
    let tnld_addr = tnld_handle.local_addr.to_string();

    // 3. Spawn tnl client: connect + create tunnel + run accept loop
    let session =
        tnl::client::connect_and_create(&format!("http://{tnld_addr}"), "tnl_E2ESECRET", "smoke")
            .await
            .unwrap();
    assert_eq!(session.hostname, "smoke.t.example.com");
    let session_box = session.session;
    let _ctrl = session.control;
    let _accept = tokio::spawn(tnl::client::run_accept_loop(session_box, backend_port));

    // give the daemon a moment to register
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 4. Send a request to tnld:DATA-PLANE with Host: smoke.t.example.com
    // Force connection: close on the upstream hop so tnld's data plane can
    // read-until-EOF cleanly. Disable pool to ensure the connection closes.
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();
    let resp = client
        .get(format!("http://{tnld_addr}/api/ping"))
        .header("Host", "smoke.t.example.com")
        .header("Connection", "close")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("pong from local backend"), "got: {body}");
}
