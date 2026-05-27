use std::process::Command;

fn bin() -> String {
    env!("CARGO_BIN_EXE_tnl").to_string()
}

#[test]
fn tnl_completion_zsh_starts_with_zsh_header() {
    let out = Command::new(bin())
        .args(["completion", "zsh"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("#compdef tnl"), "got: {s}");
}

#[test]
fn tnl_completion_bash_emits_complete_command() {
    let out = Command::new(bin())
        .args(["completion", "bash"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("_tnl"),
        "expected `_tnl` function in bash output"
    );
    assert!(s.contains("complete"), "expected `complete` directive");
}

#[test]
fn tnl_completion_rejects_unknown_shell() {
    let out = Command::new(bin())
        .args(["completion", "tcsh"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "tcsh should not be a supported shell"
    );
}
