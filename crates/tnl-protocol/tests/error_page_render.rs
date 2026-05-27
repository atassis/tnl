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

#[test]
fn parse_accept_picks_json_first() {
    use tnl_protocol::error_page::{parse_accept, Accept};
    assert_eq!(parse_accept(Some("application/json"), None), Accept::Json);
    assert_eq!(
        parse_accept(Some("application/json, text/html;q=0.9"), None),
        Accept::Json
    );
    assert_eq!(
        parse_accept(Some("application/vnd.x+json"), None),
        Accept::Json
    );
}

#[test]
fn parse_accept_picks_html_when_no_json() {
    use tnl_protocol::error_page::{parse_accept, Accept};
    assert_eq!(
        parse_accept(Some("text/html, */*;q=0.8"), None),
        Accept::Html
    );
}

#[test]
fn parse_accept_browser_wildcard_promotes_to_html() {
    use tnl_protocol::error_page::{parse_accept, Accept};
    assert_eq!(parse_accept(Some("*/*"), Some("Mozilla/5.0")), Accept::Html);
    assert_eq!(parse_accept(Some("*/*"), Some("curl/8.0")), Accept::Text);
    assert_eq!(parse_accept(Some("*/*"), None), Accept::Text);
}

#[test]
fn parse_accept_defaults_text_when_missing() {
    use tnl_protocol::error_page::{parse_accept, Accept};
    assert_eq!(parse_accept(None, None), Accept::Text);
    assert_eq!(parse_accept(Some(""), None), Accept::Text);
}

#[test]
fn parse_accept_empty_with_mozilla_ua_is_html() {
    // Edge case from review I1: empty Accept + Mozilla UA falls through the
    // wildcard branch (intentional — browsers omitting Accept get HTML).
    use tnl_protocol::error_page::{parse_accept, Accept};
    assert_eq!(
        parse_accept(Some(""), Some("Mozilla/5.0 (X11)")),
        Accept::Html
    );
    assert_eq!(parse_accept(None, Some("Mozilla/5.0 (X11)")), Accept::Html);
}

#[test]
fn daemon_component_json_omits_target_kind_as_null() {
    use tnl_protocol::error_page::{render, Accept, Component, ErrorContext};
    let ctx = ErrorContext {
        component: Component::Daemon,
        kind: None,
        tunnel: Some("demo"),
        target: None,
        reason: "tunnel disconnected",
        hint: "try again",
        req_id: ulid::Ulid::nil(),
        version: "0.1.0-test",
    };
    let body = render(&ctx, Accept::Json);
    let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert!(
        v["kind"].is_null(),
        "kind should be null, got {:?}",
        v["kind"]
    );
    assert!(
        v["target"].is_null(),
        "target should be null, got {:?}",
        v["target"]
    );
    assert_eq!(v["error"], "tunnel_disconnected");
    assert_eq!(v["component"], "daemon");
}
