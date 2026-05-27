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

/// Assert attribution headers on a 502 response.
fn assert_attribution_headers(resp: &reqwest::Response) {
    let component = resp
        .headers()
        .get("x-tnl-component")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(component, "client", "X-Tnl-Component: {component:?}");

    let origin_failure = resp
        .headers()
        .get("x-tnl-origin-failure")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(
        origin_failure, "connect-refused",
        "X-Tnl-Origin-Failure: {origin_failure:?}"
    );
}

/// Fire one request to tnld's data plane with the given Accept header and
/// return the response. Panics if the request does not complete within 10 s.
async fn send_request(
    client: &reqwest::Client,
    tnld_addr: &str,
    accept: &str,
) -> reqwest::Response {
    tokio::time::timeout(Duration::from_secs(10), async {
        client
            .get(format!("http://{tnld_addr}/api/ping"))
            .header("Host", "smoke-down.t.example.com")
            .header("Connection", "close")
            .header("Accept", accept)
            .send()
            .await
            .unwrap()
    })
    .await
    .expect("request timed out")
}

/// E2E test: when the local backend is not running, an end-user request must
/// receive a 502 with proper `X-Tnl-Component: client` and
/// `X-Tnl-Origin-Failure: connect-refused` attribution headers.
///
/// Two sub-requests exercise content negotiation:
///  1. `Accept: application/json`  — JSON body with `error == "local_backend_failure"`.
///  2. `Accept: text/html`         — HTML body starting with `<!doctype html>`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_backend_down_returns_502_with_attribution() {
    init_tracing();

    // 1. Bind a listener, capture the port, then drop it — nothing listening.
    let tmp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_port = tmp_listener.local_addr().unwrap().port();
    drop(tmp_listener);

    // 2. Spawn tnld.
    let hash = hash_plaintext("tnl_DOWNSECRET").unwrap();
    let tokens = TokensFile {
        tokens: vec![TokenEntry {
            name: "e2e-down".into(),
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

    // 3. Spawn tnl client pointing at the dead port.
    let session = tnl::client::connect_and_create(
        &format!("http://{tnld_addr}"),
        "tnl_DOWNSECRET",
        "smoke-down",
    )
    .await
    .unwrap();
    assert_eq!(session.hostname, "smoke-down.t.example.com");
    let _ctrl = session.control;
    let _accept = tokio::spawn(tnl::client::run_accept_loop(
        session.session,
        tnl::target::Target::LocalhostPort(dead_port),
        tnl::forwarder::ForwardCtx {
            tunnel: "smoke-down".into(),
            log_tx: None,
            version: env!("CARGO_PKG_VERSION"),
        },
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();

    // ── Sub-request 1: JSON ───────────────────────────────────────────────────
    let resp_json = send_request(&client, &tnld_addr, "application/json").await;
    assert_eq!(resp_json.status().as_u16(), 502);
    assert_attribution_headers(&resp_json);
    let ct = resp_json
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("application/json"), "content-type: {ct:?}");
    let body: serde_json::Value = serde_json::from_str(&resp_json.text().await.unwrap()).unwrap();
    assert_eq!(body["error"], "local_backend_failure");
    assert_eq!(body["kind"], "connect-refused");

    // ── Sub-request 2: HTML ───────────────────────────────────────────────────
    let resp_html = send_request(&client, &tnld_addr, "text/html").await;
    assert_eq!(resp_html.status().as_u16(), 502);
    let ct = resp_html
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("text/html"), "content-type: {ct:?}");
    let html = resp_html.text().await.unwrap();
    assert!(
        html.to_ascii_lowercase().starts_with("<!doctype html>"),
        "body prefix: {:?}",
        &html[..html.len().min(80)]
    );
}
