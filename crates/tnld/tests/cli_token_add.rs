use std::process::Command;

use tempfile::NamedTempFile;

fn bin() -> String {
    env!("CARGO_BIN_EXE_tnld").to_string()
}

#[test]
fn token_add_creates_new_entry_and_prints_plaintext_once() {
    let f = NamedTempFile::new().unwrap();
    std::fs::write(f.path(), "").unwrap();
    let out = Command::new(bin())
        .args([
            "token",
            "add",
            "laptop",
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
    let s = String::from_utf8_lossy(&out.stdout);
    // Plaintext token printed exactly once, with the documented prefix.
    let count = s.matches("tnl_").count();
    assert_eq!(
        count, 1,
        "expected one tnl_ token printout, got {count}: {s}"
    );

    // File now contains a hash entry named "laptop".
    let written = std::fs::read_to_string(f.path()).unwrap();
    assert!(written.contains("name = \"laptop\""), "file: {written}");
    assert!(written.contains("$argon2id$"), "file: {written}");
}

#[test]
fn token_add_rejects_duplicate_name() {
    let f = NamedTempFile::new().unwrap();
    std::fs::write(
        f.path(),
        r#"[[tokens]]
name = "laptop"
hash = "$argon2id$v=19$m=19456,t=2,p=1$AAAA$BBBB"
"#,
    )
    .unwrap();
    let out = Command::new(bin())
        .args([
            "token",
            "add",
            "laptop",
            "--tokens-file",
            f.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected failure, stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let s = String::from_utf8_lossy(&out.stderr);
    assert!(s.contains("already exists"), "stderr: {s}");
}

#[test]
fn token_add_with_replace_overwrites() {
    let f = NamedTempFile::new().unwrap();
    std::fs::write(
        f.path(),
        r#"[[tokens]]
name = "laptop"
hash = "$argon2id$v=19$m=19456,t=2,p=1$AAAA$BBBB"
"#,
    )
    .unwrap();
    let out = Command::new(bin())
        .args([
            "token",
            "add",
            "laptop",
            "--replace",
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
    // Old salt 'AAAA' should be gone.
    assert!(
        !written.contains("AAAA"),
        "old hash not replaced: {written}"
    );
}
