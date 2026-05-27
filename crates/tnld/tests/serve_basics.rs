use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config;
use tnld::serve::{spawn_server, ServerHandle};

async fn boot_test_server() -> (ServerHandle, String /*token*/) {
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
    // Keep tmp_tokens alive for the duration of the test by leaking it (it's small).
    Box::leak(Box::new(tmp_tokens));
    (handle, "tnl_TESTSECRET".to_string())
}

#[tokio::test]
async fn healthz_is_public_and_returns_200() {
    let (handle, _token) = boot_test_server().await;
    let url = format!("http://{}/healthz", handle.local_addr);
    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
}

#[tokio::test]
async fn whoami_requires_bearer_token() {
    let (handle, token) = boot_test_server().await;
    let url = format!("http://{}/whoami", handle.local_addr);

    let resp = reqwest::Client::new().get(&url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 401);

    let resp = reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("smoke"),
        "body should mention token name: {body}"
    );
}
