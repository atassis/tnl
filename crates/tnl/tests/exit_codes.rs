use std::process::Command;

fn bin() -> String {
    env!("CARGO_BIN_EXE_tnl").to_string()
}

#[test]
fn ex_usage_on_bogus_flag() {
    let out = Command::new(bin()).args(["--bogus"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn ex_not_auth_when_no_config() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("missing.toml");
    let out = Command::new(bin())
        .env("TNL_CONFIG", cfg_path.to_str().unwrap())
        .args(["status"])
        .output()
        .unwrap();
    let code = out.status.code().unwrap();
    assert!(
        code == 64,
        "expected EX_NOT_AUTH (64) for missing config, got {code}; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
