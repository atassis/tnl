//! Synthesize a complete HTTP/1.1 502 response to write into the yamux
//! substream when local-side failure is detected before any backend bytes
//! have been forwarded.
//!
//! The daemon parses this identically to a real backend response.

use std::fmt::Write as _;

use tnl_protocol::error_page::{render, Accept, Component, ErrorContext};
use ulid::Ulid;

#[derive(Debug)]
pub struct SynthInput<'a> {
    /// Failure-kind string from `ConnectError::to_kind` or one of the
    /// phase-2 kinds ("local-eof", "local-malformed", "local-no-response").
    pub kind: &'a str,
    pub hint: &'a str,
    pub tunnel: &'a str,
    /// Target as the user typed it (or "localhost:<port>" for port-only).
    pub display_target: &'a str,
    pub accept: Accept,
    pub req_id: Ulid,
    pub version: &'static str,
}

/// Render the full HTTP/1.1 502 message (status line + headers + body) for
/// writing into the substream.
#[must_use]
pub fn synth_response_bytes(input: &SynthInput<'_>) -> Vec<u8> {
    let body = render(
        &ErrorContext {
            component: Component::Client,
            kind: Some(input.kind),
            tunnel: Some(input.tunnel),
            target: Some(input.display_target),
            reason: input.hint,
            hint: input.hint,
            req_id: input.req_id,
            version: input.version,
        },
        input.accept,
    );

    let mut head = String::with_capacity(384);
    let _ = writeln!(head, "HTTP/1.1 502 Bad Gateway\r");
    let _ = writeln!(head, "Content-Type: {}\r", input.accept.content_type());
    let _ = writeln!(head, "Cache-Control: no-store\r");
    let _ = writeln!(head, "Connection: close\r");
    let _ = writeln!(head, "X-Tnl-Component: client\r");
    let _ = writeln!(head, "X-Tnl-Origin-Failure: {}\r", input.kind);
    let _ = writeln!(head, "X-Tnl-Origin-Target: {}\r", input.display_target);
    let _ = writeln!(head, "X-Tnl-Tunnel: {}\r", input.tunnel);
    let _ = writeln!(head, "X-Tnl-Request-Id: {}\r", input.req_id);
    let _ = writeln!(head, "Content-Length: {}\r", body.len());
    head.push_str("\r\n");

    let mut out = Vec::with_capacity(head.len() + body.len());
    out.extend_from_slice(head.as_bytes());
    out.extend_from_slice(&body);
    out
}
