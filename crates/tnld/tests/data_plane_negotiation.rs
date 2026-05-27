//! Integration tests: content negotiation on the 404 (no tunnel) path.
//!
//! Three Accept variants:
//!   - `text/html`        → HTML body + Content-Type: text/html; charset=utf-8
//!   - `application/json` → JSON body + Content-Type: application/json
//!   - (none)             → plain-text body + Content-Type: text/plain; charset=utf-8

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config;
use tnld::serve::spawn_server;

fn make_cfg(tokens_file: String) -> Config {
    Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://test".into(),
        hostname_root: "t.example.com".into(),
        tokens_file,
        session_grace_sec: 30,
    }
}

fn make_tokens_file() -> tempfile::NamedTempFile {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let tf = TokensFile {
        tokens: vec![TokenEntry {
            name: "smoke".into(),
            hash: hash_plaintext("tnl_TEST").unwrap(),
        }],
    };
    std::fs::write(tmp.path(), toml::to_string(&tf).unwrap()).unwrap();
    tmp
}

#[tokio::test(flavor = "multi_thread")]
async fn accept_html_returns_html_body() {
    let tmp = make_tokens_file();
    let handle = spawn_server(make_cfg(tmp.path().to_string_lossy().into_owned()))
        .await
        .unwrap();
    let addr = handle.local_addr.to_string();

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/"))
        .header("Host", "ghost.t.example.com")
        .header("Accept", "text/html")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 404);
    assert_eq!(
        resp.headers()
            .get("x-tnl-component")
            .and_then(|v| v.to_str().ok()),
        Some("registry"),
    );
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        ct.starts_with("text/html"),
        "expected text/html content-type, got {ct:?}"
    );

    let body = resp.text().await.unwrap();
    assert!(
        body.to_lowercase().starts_with("<!doctype html>"),
        "expected HTML body, got: {body:.80?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn accept_json_returns_json_body() {
    let tmp = make_tokens_file();
    let handle = spawn_server(make_cfg(tmp.path().to_string_lossy().into_owned()))
        .await
        .unwrap();
    let addr = handle.local_addr.to_string();

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/"))
        .header("Host", "ghost.t.example.com")
        .header("Accept", "application/json")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 404);
    assert_eq!(
        resp.headers()
            .get("x-tnl-component")
            .and_then(|v| v.to_str().ok()),
        Some("registry"),
    );
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert_eq!(
        ct, "application/json",
        "expected application/json, got {ct:?}"
    );

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "no_such_tunnel");
}

#[tokio::test(flavor = "multi_thread")]
async fn no_accept_header_returns_plain_text_body() {
    let tmp = make_tokens_file();
    let handle = spawn_server(make_cfg(tmp.path().to_string_lossy().into_owned()))
        .await
        .unwrap();
    let addr = handle.local_addr.to_string();

    // reqwest's default User-Agent does NOT start with "Mozilla", so the
    // daemon will select plain-text when no Accept header is set.
    let resp = reqwest::Client::builder()
        .user_agent("tnl-test/0.0")
        .build()
        .unwrap()
        .get(format!("http://{addr}/"))
        .header("Host", "ghost.t.example.com")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 404);
    assert_eq!(
        resp.headers()
            .get("x-tnl-component")
            .and_then(|v| v.to_str().ok()),
        Some("registry"),
    );
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        ct.starts_with("text/plain"),
        "expected text/plain content-type, got {ct:?}"
    );

    let body = resp.text().await.unwrap();
    assert!(
        body.starts_with("tnl:"),
        "expected plain-text body starting with 'tnl:', got: {body:.80?}"
    );
}
