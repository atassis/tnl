//! Real-browser refresh/authentication and replay-safety regression.

#[test]
#[ignore = "requires Node, a Chromium browser, explicit TNL_TEST_CLIENT_BIN/TNL_TEST_DAEMON_BIN"]
fn inspector_refresh_preserves_auth_but_never_replay_confirmation() {
    let output = std::process::Command::new("node")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/inspector_refresh.mjs"
        ))
        .output()
        .expect("Node browser fixture must be available");
    assert!(
        output.status.success(),
        "browser fixture failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    for key in [
        "authenticated_F5",
        "F5_clears_confirmation_and_never_replays",
        "backend_public_capture_ID_equal",
        "secrets_absent_from_DOM",
        "HTML_canary_not_executed",
        "duplicate_click_and_polling_no_retry",
    ] {
        assert_eq!(report[key], true, "missing browser assertion: {key}");
    }
    assert_eq!(report["backend_requests"], 2);
    assert_eq!(report["JS_exceptions"], serde_json::json!([]));
}
