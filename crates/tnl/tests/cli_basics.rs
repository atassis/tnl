use std::process::Command;

#[test]
fn version_prints_semver() {
    let bin = env!("CARGO_BIN_EXE_tnl");
    let out = Command::new(bin).arg("version").output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(s.contains("0.1.0-alpha"), "stdout was: {s}");
}

#[test]
fn config_show_with_env_token() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        tmp.path(),
        r#"endpoint = "http://localhost:7777"
token = "tnl_K7H3MQ9R2VTBNX5WPYZF8DJCEA"
"#,
    )
    .unwrap();
    let bin = env!("CARGO_BIN_EXE_tnl");
    let out = Command::new(bin)
        .arg("config")
        .arg("show")
        .env("TNL_CONFIG", tmp.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(s.contains("http://localhost:7777"), "{s}");
    assert!(s.contains("tnl_...JCEA"), "{s}");
}
