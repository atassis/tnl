use std::process::Command;

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;

async fn boot() -> (
    String, /* endpoint */
    String, /* token */
    tempfile::NamedTempFile,
) {
    let hash = hash_plaintext("tnl_TESTSECRET").unwrap();
    let tokens = TokensFile {
        tokens: vec![TokenEntry {
            name: "smoke".into(),
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
    let handle = spawn_server(cfg).await.unwrap();
    (
        format!("http://{}", handle.local_addr),
        "tnl_TESTSECRET".into(),
        tmp_tokens,
    )
}

#[tokio::test]
async fn login_writes_config() {
    let (endpoint, token, _tokens) = boot().await;
    let cfg_path = tempfile::NamedTempFile::new().unwrap().into_temp_path();

    let bin = env!("CARGO_BIN_EXE_tnl");
    let out = tokio::task::spawn_blocking(move || {
        Command::new(bin)
            .args(["auth", "login", "--endpoint", &endpoint, "--token", &token])
            .env("TNL_CONFIG", cfg_path.to_str().unwrap())
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // config file should exist with the right content
}
