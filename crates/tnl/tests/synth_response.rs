use tnl::connect::ConnectError;
use tnl::synth::{synth_response_bytes, SynthInput};
use tnl_protocol::error_page::Accept;
use ulid::Ulid;

#[test]
fn synth_html_is_well_formed() {
    let req_id = Ulid::nil();
    let err = ConnectError::Refused;
    let reason = err.to_string();
    let bytes = synth_response_bytes(&SynthInput {
        kind: err.to_kind(),
        reason: &reason,
        hint: err.hint(),
        tunnel: "demo",
        display_target: "localhost:5173",
        accept: Accept::Html,
        req_id,
        version: "0.0.0-test",
    });
    let s = std::str::from_utf8(&bytes).expect("utf-8");
    assert!(s.starts_with("HTTP/1.1 502 Bad Gateway\r\n"), "{s}");
    assert!(s.contains("X-Tnl-Component: client\r\n"));
    assert!(s.contains("X-Tnl-Origin-Failure: connect-refused\r\n"));
    assert!(s.contains("X-Tnl-Origin-Target: localhost:5173\r\n"));
    assert!(s.contains("X-Tnl-Tunnel: demo\r\n"));
    assert!(s.contains("Content-Type: text/html; charset=utf-8\r\n"));
    assert!(s.contains("Connection: close\r\n"));
    // Body must show reason and hint as distinct strings, not duplicates.
    let (head, body) = s.split_once("\r\n\r\n").expect("blank line");
    assert!(body.contains(&reason), "missing reason in body: {body}");
    assert!(body.contains(err.hint()), "missing hint in body: {body}");
    assert_ne!(
        reason,
        err.hint(),
        "test fixture: reason and hint must differ"
    );
    let cl_line = head
        .lines()
        .find(|l| l.starts_with("Content-Length: "))
        .expect("content-length");
    // body.len() is byte count, matching Content-Length semantics.
    let cl: usize = cl_line["Content-Length: ".len()..].parse().expect("num");
    assert_eq!(cl, body.len(), "Content-Length must match body bytes");
}

#[test]
fn synth_json_parses() {
    let err = ConnectError::Timeout(std::time::Duration::from_secs(3));
    let reason = err.to_string();
    let bytes = synth_response_bytes(&SynthInput {
        kind: err.to_kind(),
        reason: &reason,
        hint: err.hint(),
        tunnel: "demo",
        display_target: "[::1]:5173",
        accept: Accept::Json,
        req_id: Ulid::nil(),
        version: "0.0.0-test",
    });
    let (_head, body) = std::str::from_utf8(&bytes)
        .unwrap()
        .split_once("\r\n\r\n")
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["kind"], "connect-timeout");
    assert_eq!(v["component"], "client");
    assert_eq!(v["reason"], reason);
    assert_eq!(v["hint"], err.hint());
}

#[test]
fn synth_text_has_expected_shape() {
    let err = ConnectError::Refused;
    let reason = err.to_string();
    let bytes = synth_response_bytes(&SynthInput {
        kind: err.to_kind(),
        reason: &reason,
        hint: err.hint(),
        tunnel: "demo",
        display_target: "localhost:5173",
        accept: Accept::Text,
        req_id: Ulid::nil(),
        version: "0.0.0-test",
    });
    let s = std::str::from_utf8(&bytes).expect("utf-8");
    assert!(s.contains("Content-Type: text/plain; charset=utf-8\r\n"));
    let (head, body) = s.split_once("\r\n\r\n").expect("blank line");
    assert!(body.starts_with("tnl:"), "body: {body}");
    assert!(body.contains("demo"));
    let cl: usize = head
        .lines()
        .find(|l| l.starts_with("Content-Length: "))
        .expect("content-length")["Content-Length: ".len()..]
        .parse()
        .expect("num");
    assert_eq!(cl, body.len());
}
