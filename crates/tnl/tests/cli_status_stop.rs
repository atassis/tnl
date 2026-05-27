use std::process::Command;

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;

fn bin() -> String {
    env!("CARGO_BIN_EXE_tnl").to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tnl_status_returns_empty_when_no_tunnels() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let tf = TokensFile {
        tokens: vec![TokenEntry {
            name: "u".into(),
            hash: hash_plaintext("tnl_U").unwrap(),
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
    let addr = h.local_addr.to_string();

    let cfg_dir = tempfile::tempdir().unwrap();
    let cfg_path = cfg_dir.path().join("config.toml");
    std::fs::write(
        &cfg_path,
        format!(
            r#"endpoint = "http://{addr}"
token = "tnl_U"
"#
        ),
    )
    .unwrap();

    let out = Command::new(bin())
        .env("TNL_CONFIG", cfg_path.to_str().unwrap())
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tnl_stop_unknown_subdomain_errors() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let tf = TokensFile {
        tokens: vec![TokenEntry {
            name: "u".into(),
            hash: hash_plaintext("tnl_U").unwrap(),
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
    let addr = h.local_addr.to_string();

    let cfg_dir = tempfile::tempdir().unwrap();
    let cfg_path = cfg_dir.path().join("config.toml");
    std::fs::write(
        &cfg_path,
        format!(
            r#"endpoint = "http://{addr}"
token = "tnl_U"
"#
        ),
    )
    .unwrap();

    let out = Command::new(bin())
        .env("TNL_CONFIG", cfg_path.to_str().unwrap())
        .args(["stop", "ghost"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected nonzero exit for unknown subdomain"
    );
    let s = String::from_utf8_lossy(&out.stderr);
    assert!(s.contains("no such tunnel"), "stderr: {s}");
}
