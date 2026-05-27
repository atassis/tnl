//! Daemon picks up tokens.toml changes within ~500ms of mtime change.

use std::time::Duration;

use reqwest::StatusCode;
use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn whoami_reflects_tokens_file_changes() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let tf = TokensFile {
        tokens: vec![TokenEntry {
            name: "alpha".into(),
            hash: hash_plaintext("tnl_ALPHA").unwrap(),
        }],
    };
    std::fs::write(tmp.path(), toml::to_string(&tf).unwrap()).unwrap();

    let cfg = TnldConfig {
        listen: "127.0.0.1:0".into(),
        public_url: "http://test".into(),
        hostname_root: "t.example.com".into(),
        tokens_file: tmp.path().to_string_lossy().into_owned(),
        session_grace_sec: 30,
    };
    let handle = spawn_server(cfg).await.unwrap();
    let addr = handle.local_addr.to_string();
    let client = reqwest::Client::new();

    // alpha auth works.
    let r = client
        .get(format!("http://{addr}/whoami"))
        .bearer_auth("tnl_ALPHA")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // Rewrite the file: replace alpha with beta.
    let new_tf = TokensFile {
        tokens: vec![TokenEntry {
            name: "beta".into(),
            hash: hash_plaintext("tnl_BETA").unwrap(),
        }],
    };
    // Sleep 1.1s so mtime granularity (1s on many FS) reliably ticks.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    std::fs::write(tmp.path(), toml::to_string(&new_tf).unwrap()).unwrap();

    let r = client
        .get(format!("http://{addr}/whoami"))
        .bearer_auth("tnl_ALPHA")
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::UNAUTHORIZED,
        "alpha should be revoked"
    );

    let r = client
        .get(format!("http://{addr}/whoami"))
        .bearer_auth("tnl_BETA")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "beta should authenticate");
}
