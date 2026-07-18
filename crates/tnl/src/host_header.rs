//! `Host` header handling for forwarded requests (dev-server allowlist smoothing).

/// What `Host` the backend should see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostHeader {
    /// Forward the real host; rewrite to the connected address only after a
    /// detected dev-server host block (default).
    Auto,
    /// Never rewrite.
    Preserve,
    /// Always rewrite to the connected `resolved_addr` from the first request.
    Rewrite,
    /// Always rewrite to an explicit value.
    Fixed(String),
}

impl HostHeader {
    /// Parse the `--host-header` flag value (`None` = default `Auto`).
    #[must_use]
    pub fn parse(s: Option<&str>) -> Self {
        match s {
            None => Self::Auto,
            Some("preserve") => Self::Preserve,
            Some("rewrite") => Self::Rewrite,
            Some(v) => Self::Fixed(v.to_string()),
        }
    }
}

/// Replace the `Host:` header value in a raw HTTP/1.x request head.
///
/// Preserves line order and CRLF framing; case-insensitive on the header name.
/// If no `Host` line exists, one is inserted immediately after the request line.
/// Non-UTF-8 input is returned unchanged.
#[must_use]
pub fn rewrite_host(head: &[u8], new_host: &str) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(head) else {
        return head.to_vec();
    };
    let mut out = String::with_capacity(text.len() + new_host.len() + 8);
    let mut replaced = false;
    let mut rest = text;
    let mut is_first = true;
    while let Some(pos) = rest.find("\r\n") {
        let line = &rest[..pos];
        rest = &rest[pos + 2..];
        if !is_first && !replaced && line.len() >= 5 && line[..5].eq_ignore_ascii_case("host:") {
            out.push_str("Host: ");
            out.push_str(new_host);
            replaced = true;
        } else {
            out.push_str(line);
        }
        out.push_str("\r\n");
        is_first = false;
        if line.is_empty() {
            out.push_str(rest);
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    if !replaced {
        if let Some(pos) = out.find("\r\n") {
            let (a, b) = out.split_at(pos + 2);
            return format!("{a}Host: {new_host}\r\n{b}").into_bytes();
        }
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_modes() {
        assert_eq!(HostHeader::parse(None), HostHeader::Auto);
        assert_eq!(HostHeader::parse(Some("preserve")), HostHeader::Preserve);
        assert_eq!(HostHeader::parse(Some("rewrite")), HostHeader::Rewrite);
        assert_eq!(
            HostHeader::parse(Some("myapp.local")),
            HostHeader::Fixed("myapp.local".into())
        );
    }

    #[test]
    fn rewrite_replaces_host_value_preserving_rest() {
        let head = b"GET /x HTTP/1.1\r\nHost: sub.t.example.com\r\nAccept: */*\r\n\r\n";
        let out = rewrite_host(head, "127.0.0.1:4321");
        assert_eq!(
            out,
            b"GET /x HTTP/1.1\r\nHost: 127.0.0.1:4321\r\nAccept: */*\r\n\r\n".to_vec()
        );
    }

    #[test]
    fn rewrite_is_case_insensitive_on_name() {
        let head = b"GET / HTTP/1.1\r\nhost: old\r\n\r\n";
        let out = rewrite_host(head, "[::1]:8080");
        assert_eq!(out, b"GET / HTTP/1.1\r\nHost: [::1]:8080\r\n\r\n".to_vec());
    }

    #[test]
    fn rewrite_inserts_host_when_absent() {
        let head = b"GET / HTTP/1.1\r\nAccept: */*\r\n\r\n";
        let out = rewrite_host(head, "127.0.0.1:9");
        assert_eq!(
            out,
            b"GET / HTTP/1.1\r\nHost: 127.0.0.1:9\r\nAccept: */*\r\n\r\n".to_vec()
        );
    }
}
