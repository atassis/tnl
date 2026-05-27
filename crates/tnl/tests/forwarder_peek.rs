use tnl::forwarder::peek::{parse_request_line, parse_response_status};

#[test]
fn parses_request_line() {
    let bytes = b"GET /api/foo HTTP/1.1\r\nHost: x\r\n\r\nbody";
    let (m, p, _host_req) = parse_request_line(bytes).unwrap();
    assert_eq!(m, "GET");
    assert_eq!(p, "/api/foo");
}

#[test]
fn parses_response_status() {
    let bytes = b"HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\n\r\nx";
    assert_eq!(parse_response_status(bytes), Some(404));
}

#[test]
fn parser_returns_none_on_partial_request() {
    assert!(parse_request_line(b"GET").is_none()); // no path
    assert!(parse_request_line(b"GET /api/foo").is_none()); // no HTTP version
}

#[test]
fn parser_returns_none_on_partial_status() {
    // No status token at all
    assert_eq!(parse_response_status(b"HTTP/1.1"), None);
    // Non-numeric status token
    assert_eq!(parse_response_status(b"HTTP/1.1 abc Not Found"), None);
    // Completely non-HTTP garbage
    assert_eq!(parse_response_status(b"\x00\x01\x02"), None);
}
