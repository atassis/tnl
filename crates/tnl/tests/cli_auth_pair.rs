use std::process::Command;

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;

fn bin() -> String {
    env!("CARGO_BIN_EXE_tnl").to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tnl_auth_pair_redeems_and_saves_config() {
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
        public_url: String::new(), // filled in after bind
        hostname_root: "t.x".into(),
        tokens_file: tmp.path().to_string_lossy().into_owned(),
    };
    let h = spawn_server(cfg).await.unwrap();
    let addr = h.local_addr;
    let port = addr.port();

    // Mint an invite via the REST API.
    let c = reqwest::Client::new();
    let r = c
        .post(format!("http://127.0.0.1:{port}/pair"))
        .bearer_auth("tnl_ADMIN")
        .json(&serde_json::json!({"name": "laptop", "expires_in_sec": 60}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::OK, "pair create failed");
    let v: serde_json::Value = r.json().await.unwrap();
    // The invite_url from the server uses its public_url (empty string in our
    // test config), so we construct a valid invite URL manually from the code.
    let code = v["code"].as_str().unwrap();
    let invite_url = format!("http://127.0.0.1:{port}/invite/{code}");

    // Redeem via tnl auth pair.
    let cfg_dir = tempfile::tempdir().unwrap();
    let cfg_path = cfg_dir.path().join("config.toml");
    let out = Command::new(bin())
        .args(["auth", "pair", &invite_url])
        .env("TNL_CONFIG", cfg_path.to_str().unwrap())
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
