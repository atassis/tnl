//! Validates the dynamic-completion helpers introduced for `tnl stop <TAB>`.
//!
//! These tests exercise the contract without a live daemon: the completer must
//! return an empty vec (not panic) when the daemon is unreachable.

use std::ffi::OsStr;

#[test]
fn complete_returns_empty_when_daemon_unreachable() {
    // Point TNL_CONFIG at a temp config with a known-unreachable endpoint.
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("config.toml");
    std::fs::write(
        &cfg_path,
        r#"endpoint = "http://127.0.0.1:1"
token = "tnl_unused"
"#,
    )
    .unwrap();

    // Mutates the process environment — acceptable for a single-threaded test.
    std::env::set_var("TNL_CONFIG", cfg_path.to_str().unwrap());

    let candidates = tnl::completion::complete_live_subdomains(OsStr::new(""));
    assert!(
        candidates.is_empty(),
        "expected empty list when daemon is unreachable, got {} candidates",
        candidates.len()
    );

    // Clean up so other tests in the same process aren't affected.
    std::env::remove_var("TNL_CONFIG");
}
