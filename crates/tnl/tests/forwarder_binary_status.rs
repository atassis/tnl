//! Status observation must not depend on UTF-8 validity after the first line.
use tnl::forwarder::peek::parse_response_status;

#[test]
fn websocket_status_is_observable_before_binary_frame_bytes() {
    let bytes = b"HTTP/1.1 101 Switching Protocols\r\nConnection: upgrade\r\n\r\n\x81\x05hello";
    assert_eq!(parse_response_status(bytes), Some(101));
}

#[test]
fn binary_http_body_does_not_erase_response_status() {
    let bytes = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n\x00\xff\x80";
    assert_eq!(parse_response_status(bytes), Some(200));
}

#[test]
fn non_utf8_header_does_not_erase_response_status() {
    let bytes = b"HTTP/1.1 200 OK\r\nX-Bytes: \xff\r\n\r\n";
    assert_eq!(parse_response_status(bytes), Some(200));
}

#[test]
fn lf_terminated_status_is_observable_before_non_utf8_bytes() {
    assert_eq!(parse_response_status(b"HTTP/1.1 200 OK\n\xff"), Some(200));
}
