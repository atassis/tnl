use std::process::Command;

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;

fn bin() -> String {
    env!("CARGO_BIN_EXE_tnld").to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pair_add_against_running_daemon_prints_invite() {
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
        public_url: "https://tnl-api.example.com".into(),
        hostname_root: "t.example.com".into(),
        tokens_file: tmp.path().to_string_lossy().into_owned(),
    };
    let h = spawn_server(cfg).await.unwrap();
    let addr = h.local_addr.to_string();

    let out = Command::new(bin())
        .args([
            "pair",
            "add",
            "laptop",
            "--endpoint",
            &format!("http://{addr}"),
            "--admin-token",
            "tnl_ADMIN",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("invite"));
    assert!(s.contains("https://tnl-api.example.com/invite/"));
}
