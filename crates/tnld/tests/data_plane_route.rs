use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config;
use tnld::serve::spawn_server;

#[tokio::test]
async fn unknown_host_returns_404_with_component_header() {
    let hash = hash_plaintext("tnl_TESTSECRET").unwrap();
    let tokens = TokensFile {
        tokens: vec![TokenEntry {
            name: "smoke".into(),
            hash,
        }],
    };
    let tmp_tokens = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp_tokens.path(), toml::to_string(&tokens).unwrap()).unwrap();
    let cfg = Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://test".into(),
        hostname_root: "t.example.com".into(),
        tokens_file: tmp_tokens.path().to_string_lossy().into_owned(),
        session_grace_sec: 30,
    };
    let handle = spawn_server(cfg).await.unwrap();

    let resp = reqwest::Client::new()
        .get(format!("http://{}/some/path", handle.local_addr))
        .header("Host", "nonexistent.t.example.com")
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
    assert!(resp.headers().get("x-tnl-request-id").is_some());
    assert_eq!(
        resp.headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store"),
    );
}
