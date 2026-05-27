use std::process::Command;

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;

fn bin() -> String {
    env!("CARGO_BIN_EXE_tnl").to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_with_invite_url_saves_config() {
    // Daemon.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let tf = TokensFile {
        tokens: vec![TokenEntry {
            name: "admin".into(),
            hash: hash_plaintext("tnl_ADMIN").unwrap(),
        }],
    };
    std::fs::write(tmp.path(), toml::to_string(&tf).unwrap()).unwrap();
    let cfg = TnldConfig {
        listen: "127.0.0.1:0".into(),
        public_url: String::new(),
        hostname_root: "t.x".into(),
        tokens_file: tmp.path().to_string_lossy().into_owned(),
        session_grace_sec: 30,
    };
    let h = spawn_server(cfg).await.unwrap();
    let port = h.local_addr.port();

    // Mint an invite via REST.
    let c = reqwest::Client::new();
    let r = c
        .post(format!("http://127.0.0.1:{port}/pair"))
        .bearer_auth("tnl_ADMIN")
        .json(&serde_json::json!({"name": "laptop", "expires_in_sec": 60}))
        .send()
        .await
        .unwrap();
    let code = r.json::<serde_json::Value>().await.unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    let invite_url = format!("http://127.0.0.1:{port}/invite/{code}");

    let cfg_dir = tempfile::tempdir().unwrap();
    let cfg_path = cfg_dir.path().join("config.toml");

    let out = Command::new(bin())
        .env("TNL_CONFIG", cfg_path.to_str().unwrap())
        .args([
            "init",
            "--invite",
            &invite_url,
            "--no-shell-completion",
            "-y",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let written = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(
        written.contains("token = \"tnl_"),
        "config should contain a tnl_ token: {written}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_with_endpoint_token_saves_config() {
    // Daemon with a known token "tnl_X".
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let tf = TokensFile {
        tokens: vec![TokenEntry {
            name: "x".into(),
            hash: hash_plaintext("tnl_X").unwrap(),
        }],
    };
    std::fs::write(tmp.path(), toml::to_string(&tf).unwrap()).unwrap();
    let cfg = TnldConfig {
        listen: "127.0.0.1:0".into(),
        public_url: "http://x".into(),
        hostname_root: "t.x".into(),
        tokens_file: tmp.path().to_string_lossy().into_owned(),
        session_grace_sec: 30,
    };
    let h = spawn_server(cfg).await.unwrap();
    let endpoint = format!("http://{}", h.local_addr);

    let cfg_dir = tempfile::tempdir().unwrap();
    let cfg_path = cfg_dir.path().join("config.toml");

    let out = Command::new(bin())
        .env("TNL_CONFIG", cfg_path.to_str().unwrap())
        .args([
            "init",
            "--endpoint",
            &endpoint,
            "--token",
            "tnl_X",
            "--no-shell-completion",
            "-y",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let written = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(written.contains("token = \"tnl_X\""));
}

#[test]
fn init_without_flags_in_non_tty_errors() {
    // No TTY for cargo test child processes — should fail with a clear message.
    let cfg_dir = tempfile::tempdir().unwrap();
    let cfg_path = cfg_dir.path().join("config.toml");
    let out = Command::new(bin())
        .env("TNL_CONFIG", cfg_path.to_str().unwrap())
        .args(["init", "--no-shell-completion"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let s = String::from_utf8_lossy(&out.stderr);
    assert!(
        s.contains("non-interactive") || s.contains("TTY"),
        "expected helpful error, got: {s}"
    );
}
