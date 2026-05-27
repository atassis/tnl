//! Integration test: unknown host → 404 with X-Tnl-Component: registry and
//! JSON body `{"error": "no_such_tunnel", ...}`.

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
async fn unknown_host_returns_404_with_registry_attribution() {
    let tmp = make_tokens_file();
    let handle = spawn_server(make_cfg(tmp.path().to_string_lossy().into_owned()))
        .await
        .unwrap();
    let addr = handle.local_addr.to_string();

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/some/path"))
        .header("Host", "nope.t.example.com")
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

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "no_such_tunnel");
}
