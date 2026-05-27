use tnl::connect::ConnectError;
use tnl::synth::{synth_response_bytes, SynthInput};
use tnl_protocol::error_page::Accept;
use ulid::Ulid;

#[test]
fn synth_html_is_well_formed() {
    let req_id = Ulid::nil();
    let bytes = synth_response_bytes(&SynthInput {
        kind: ConnectError::Refused.to_kind(),
        hint: ConnectError::Refused.hint(),
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
    let (head, body) = s.split_once("\r\n\r\n").expect("blank line");
    let cl_line = head
        .lines()
        .find(|l| l.starts_with("Content-Length: "))
        .expect("content-length");
    let cl: usize = cl_line["Content-Length: ".len()..].parse().expect("num");
    assert_eq!(cl, body.len(), "Content-Length must match body bytes");
}

#[test]
fn synth_json_parses() {
    let bytes = synth_response_bytes(&SynthInput {
        kind: ConnectError::Timeout(std::time::Duration::from_secs(3)).to_kind(),
        hint: ConnectError::Timeout(std::time::Duration::from_secs(3)).hint(),
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
}
