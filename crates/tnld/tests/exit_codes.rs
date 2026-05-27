use std::process::Command;

fn bin() -> String {
    env!("CARGO_BIN_EXE_tnld").to_string()
}

#[test]
fn ex_usage_on_bogus_flag() {
    let out = Command::new(bin()).args(["--bogus"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn ex_generic_on_missing_config_file() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("missing.toml");
    let out = Command::new(bin())
        .args(["serve", "--config", cfg.to_str().unwrap()])
        .output()
        .unwrap();
    let code = out.status.code().unwrap();
    // serve loads config eagerly; missing file → error chain → classify returns EX_GENERIC or EX_NOT_AUTH.
    assert!(
        code == 1 || code == 64,
        "expected 1 or 64, got {code}; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
