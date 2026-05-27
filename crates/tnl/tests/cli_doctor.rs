use std::process::Command;

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;

fn bin() -> String {
    env!("CARGO_BIN_EXE_tnl").to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_all_green_against_live_daemon() {
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
        format!("endpoint = \"http://{addr}\"\ntoken = \"tnl_U\"\n"),
    )
    .unwrap();

    let out = Command::new(bin())
        .env("TNL_CONFIG", cfg_path.to_str().unwrap())
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(s.trim()).expect("valid JSON");
    let arr = v.as_array().expect("array");
    // No "fail" status anywhere.
    for check in arr {
        let status = check["status"].as_str().unwrap();
        assert_ne!(status, "fail", "unexpected fail: {check}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doctor_reports_unauth_with_hint() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let tf = TokensFile {
        tokens: vec![TokenEntry {
            name: "u".into(),
            hash: hash_plaintext("tnl_RIGHT").unwrap(),
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
    // Wrong token.
    std::fs::write(
        &cfg_path,
        format!("endpoint = \"http://{addr}\"\ntoken = \"tnl_WRONG\"\n"),
    )
    .unwrap();

    let out = Command::new(bin())
        .env("TNL_CONFIG", cfg_path.to_str().unwrap())
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    // Exits nonzero because whoami fails.
    assert!(!out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(s.trim()).expect("valid JSON");
    let arr = v.as_array().expect("array");
    let whoami = arr
        .iter()
        .find(|c| c["name"] == "whoami")
        .expect("whoami check present");
    assert_eq!(whoami["status"], "fail");
    assert!(whoami["hint"].as_str().unwrap().contains("tnl init"));
}
