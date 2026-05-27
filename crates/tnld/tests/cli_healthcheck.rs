use std::process::Command;

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;

fn bin() -> String {
    env!("CARGO_BIN_EXE_tnld").to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn healthcheck_exits_zero_when_daemon_up() {
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
    };
    let handle = spawn_server(cfg).await.unwrap();
    let addr = handle.local_addr.to_string();

    let cfg_path = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        cfg_path.path(),
        format!(
            r#"listen        = "{addr}"
public_url    = "http://x"
hostname_root = "t.x"
tokens_file   = "{}"
"#,
            tmp.path().display()
        ),
    )
    .unwrap();

    let out = Command::new(bin())
        .args(["healthcheck", "--config", cfg_path.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout: {}, stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn healthcheck_exits_nonzero_when_daemon_down() {
    let cfg = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        cfg.path(),
        r#"listen        = "127.0.0.1:1"
public_url    = "http://x"
hostname_root = "t.x"
tokens_file   = "/dev/null"
"#,
    )
    .unwrap();
    let out = Command::new(bin())
        .args(["healthcheck", "--config", cfg.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success(), "expected nonzero exit");
}
