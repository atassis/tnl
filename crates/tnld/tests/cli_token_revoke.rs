use std::process::Command;

use tempfile::NamedTempFile;

fn bin() -> String {
    env!("CARGO_BIN_EXE_tnld").to_string()
}

#[test]
fn token_revoke_removes_entry() {
    let f = NamedTempFile::new().unwrap();
    std::fs::write(
        f.path(),
        r#"[[tokens]]
name = "alpha"
hash = "$argon2id$v=19$m=19456,t=2,p=1$AAAA$BBBB"

[[tokens]]
name = "beta"
hash = "$argon2id$v=19$m=19456,t=2,p=1$CCCC$DDDD"
"#,
    )
    .unwrap();
    let out = Command::new(bin())
        .args([
            "token",
            "revoke",
            "alpha",
            "--yes",
            "--tokens-file",
            f.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let written = std::fs::read_to_string(f.path()).unwrap();
    assert!(!written.contains("alpha"), "alpha not removed: {written}");
    assert!(
        written.contains("beta"),
        "beta accidentally removed: {written}"
    );
}

#[test]
fn token_revoke_missing_name_fails() {
    let f = NamedTempFile::new().unwrap();
    std::fs::write(f.path(), "").unwrap();
    let out = Command::new(bin())
        .args([
            "token",
            "revoke",
            "ghost",
            "--yes",
            "--tokens-file",
            f.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let s = String::from_utf8_lossy(&out.stderr);
    assert!(s.contains("no such token"), "stderr: {s}");
}
