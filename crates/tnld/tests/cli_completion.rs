use std::process::Command;

fn bin() -> String {
    env!("CARGO_BIN_EXE_tnld").to_string()
}

#[test]
fn tnld_completion_zsh_starts_with_zsh_header() {
    let out = Command::new(bin())
        .args(["completion", "zsh"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("#compdef tnld"), "got: {s}");
}
