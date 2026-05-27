use std::process::Command;

fn bin() -> String {
    env!("CARGO_BIN_EXE_tnl").to_string()
}

#[test]
fn version_json_emits_schema() {
    let out = Command::new(bin())
        .args(["version", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let s = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(s.trim()).expect("valid JSON");
    assert_eq!(v["name"], "tnl");
    assert!(v["version"].as_str().unwrap().contains('.'));
}

#[test]
fn version_text_default() {
    let out = Command::new(bin()).args(["version"]).output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.starts_with("tnl "), "expected `tnl <ver>`, got: {s}");
}

#[test]
fn config_show_json_with_existing_config() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("config.toml");
    std::fs::write(
        &cfg_path,
        r#"endpoint = "https://x.example.com"
token = "tnl_ABCDEFGHIJKLMNOPQRST"
"#,
    )
    .unwrap();

    let out = Command::new(bin())
        .env("TNL_CONFIG", cfg_path.to_str().unwrap())
        .args(["config", "show", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(s.trim()).expect("valid JSON");
    assert_eq!(v["endpoint"], "https://x.example.com");
    // Token is masked — should NOT contain the full plaintext.
    let masked = v["token_masked"].as_str().unwrap();
    assert!(masked.contains('*') || masked.contains("..."));
    assert!(!masked.contains("KLMNOPQRST"), "raw token leaked: {masked}");
}
