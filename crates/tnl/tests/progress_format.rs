//! Smoke-test the inspector's text rendering for the new columns introduced
//! by alpha.3 (`req_id` / `display_target` / `resolved_addr` / `failure_kind`).
//!
//! We don't pin exact whitespace — clippy/fmt nudges have been adjusting
//! `print_text` over the task chain — just verify the renderer drains and
//! does not panic on either of the two important shapes:
//! 1. success: `status=Some`, `resolved_addr=Some`, `failure_kind=None`.
//! 2. failure: `status=Some(502)`, `resolved_addr=None`, `failure_kind=Some`.

use std::time::SystemTime;

use tnl::inspector::{Format, Inspector, LogLine, Verbosity};

#[tokio::test]
async fn renders_success_and_failure_without_panic() {
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let insp = tokio::spawn(Inspector::new(rx, Verbosity::Default, Format::Text).run());

    let ok = LogLine {
        req_id: ulid::Ulid::nil(),
        timestamp: SystemTime::UNIX_EPOCH,
        method: "GET".into(),
        path: "/api".into(),
        display_target: "localhost:5173".into(),
        resolved_addr: Some("127.0.0.1:5173".parse().unwrap()),
        status: Some(200),
        duration_ms: 12,
        bytes_in: 320,
        bytes_out: 1024,
        failure_kind: None,
    };
    let err = LogLine {
        req_id: ulid::Ulid::nil(),
        timestamp: SystemTime::UNIX_EPOCH,
        method: "POST".into(),
        path: "/login".into(),
        display_target: "localhost:5173".into(),
        resolved_addr: None,
        status: Some(502),
        duration_ms: 3,
        bytes_in: 0,
        bytes_out: 0,
        failure_kind: Some("connect-refused".into()),
    };
    tx.send(ok).await.unwrap();
    tx.send(err).await.unwrap();
    drop(tx);
    insp.await.unwrap();
}
