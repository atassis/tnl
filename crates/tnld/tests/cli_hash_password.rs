use std::process::Command;

#[test]
fn hash_password_emits_argon2_hash() {
    let bin = env!("CARGO_BIN_EXE_tnld");
    let out = Command::new(bin)
        .args(["hash-password", "tnl_DEMOSECRET"])
        .output()
        .expect("run tnld hash-password");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("$argon2id$"),
        "expected argon2id hash in stdout, got: {stdout}"
    );
}
