use tnl_protocol::error_page::{render, Accept, Component, ErrorContext};
use ulid::Ulid;

fn ctx() -> ErrorContext<'static> {
    ErrorContext {
        component: Component::Client,
        kind: Some("connect-refused"),
        tunnel: Some("demo"),
        target: Some("127.0.0.1:5173"),
        reason: "Connection refused on all resolved addresses",
        hint: "Is your dev server running?",
        req_id: Ulid::from_string("01J0000000000000000000000").unwrap_or_else(|_| Ulid::nil()),
        version: "0.1.0-test",
    }
}

#[test]
fn html_is_html_under_4kb_and_includes_placeholders() {
    let body = render(&ctx(), Accept::Html);
    let s = std::str::from_utf8(&body).expect("utf-8");
    assert!(s.starts_with("<!doctype html>"), "{s}");
    assert!(s.contains("demo"), "missing tunnel");
    assert!(s.contains("127.0.0.1:5173"), "missing target");
    assert!(s.contains("connect-refused"), "missing kind");
    assert!(s.contains("Is your dev server running?"), "missing hint");
    assert!(body.len() <= 4096, "len = {}", body.len());
}

#[test]
fn json_round_trips_keys() {
    let body = render(&ctx(), Accept::Json);
    let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(v["error"], "local_backend_failure");
    assert_eq!(v["kind"], "connect-refused");
    assert_eq!(v["target"], "127.0.0.1:5173");
    assert_eq!(v["tunnel"], "demo");
    assert_eq!(v["hint"], "Is your dev server running?");
    assert!(v["request_id"].is_string());
}

#[test]
fn plain_text_is_short() {
    let body = render(&ctx(), Accept::Text);
    let s = std::str::from_utf8(&body).expect("utf-8");
    assert!(s.starts_with("tnl:"), "{s}");
    assert!(s.contains("demo"));
    assert!(body.len() <= 1024);
}

#[test]
fn server_side_component_omits_local_only_fields_safely() {
    let mut c = ctx();
    c.component = Component::Daemon;
    c.target = None;
    c.kind = None;
    let body = render(&c, Accept::Html);
    let s = std::str::from_utf8(&body).expect("utf-8");
    assert!(s.contains("demo"), "{s}");
    assert!(!s.contains("None"));
}
