use std::io::Write;
use std::process::Command;

use tempfile::NamedTempFile;

fn bin() -> String {
    env!("CARGO_BIN_EXE_tnld").to_string()
}

#[test]
fn token_list_emits_table_with_known_entries() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(
        f,
        r#"[[tokens]]
name = "alpha"
hash = "$argon2id$v=19$m=19456,t=2,p=1$AAAA$BBBB"

[[tokens]]
name = "beta"
hash = "$argon2id$v=19$m=19456,t=2,p=1$CCCC$DDDD"
"#
    )
    .unwrap();
    let path = f.path().to_str().unwrap();

    let out = Command::new(bin())
        .args(["token", "list", "--tokens-file", path])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("alpha"), "missing 'alpha' in: {s}");
    assert!(s.contains("beta"), "missing 'beta' in: {s}");
    // Hash prefix shown, not full hash.
    assert!(s.contains("$argon2id$v=19"), "missing hash prefix in: {s}");
}

#[test]
fn token_list_json_mode() {
    let f = NamedTempFile::new().unwrap();
    std::fs::write(
        f.path(),
        r#"[[tokens]]
name = "only"
hash = "$argon2id$v=19$m=19456,t=2,p=1$AAAA$BBBB"
"#,
    )
    .unwrap();
    let out = Command::new(bin())
        .args([
            "token",
            "list",
            "--tokens-file",
            f.path().to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&s).expect("json output");
    let arr = v.as_array().expect("expected array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "only");
}
