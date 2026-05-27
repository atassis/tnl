use std::process::Command;

use tempfile::tempdir;

fn bin() -> String {
    env!("CARGO_BIN_EXE_tnld").to_string()
}

#[test]
fn init_writes_config_and_token_files() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    let tokens_path = dir.path().join("tokens.toml");

    let out = Command::new(bin())
        .args([
            "init",
            "--config",
            config_path.to_str().unwrap(),
            "--public-url",
            "https://tnl-api.example.com",
            "--hostname-root",
            "t.example.com",
            "--tokens-file",
            tokens_path.to_str().unwrap(),
            "--admin-token-name",
            "first",
            "-y",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // stdout has the plaintext token; stderr has the success line.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("tnl_"), "stdout: {stdout}");

    let cfg = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        cfg.contains("hostname_root    = \"t.example.com\""),
        "config: {cfg}"
    );
    assert!(
        cfg.contains("listen           = \"127.0.0.1:7777\""),
        "config: {cfg}"
    );

    let tokens = std::fs::read_to_string(&tokens_path).unwrap();
    assert!(tokens.contains("name = \"first\""), "tokens: {tokens}");
}

#[test]
fn init_refuses_to_overwrite_without_yes() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    std::fs::write(&config_path, "existing").unwrap();

    let out = Command::new(bin())
        .args([
            "init",
            "--config",
            config_path.to_str().unwrap(),
            "--public-url",
            "https://x.example.com",
            "--hostname-root",
            "x.example.com",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("already exists"), "stderr: {stderr}");
}

#[test]
fn init_in_non_tty_requires_public_url() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    let out = Command::new(bin())
        .args(["init", "--config", config_path.to_str().unwrap(), "-y"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("public-url is required"),
        "stderr: {stderr}"
    );
}
