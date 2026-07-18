//! Detects dev-server "host not allowed" responses so the forwarder can flip on
//! Host rewrite. Advice-only: never modifies the response.

/// Returns a short framework label if `(status, body_prefix)` matches a known
/// host-block. `body_prefix` may include the status line + headers + body start.
#[must_use]
pub fn detect(status: Option<u16>, body_prefix: &[u8]) -> Option<&'static str> {
    let status = status?;
    let text = String::from_utf8_lossy(body_prefix);
    const SIGS: &[(u16, &str, &str)] = &[
        (403, "Blocked request", "Vite"),
        (403, "Invalid Host header", "webpack-dev-server"),
        (400, "DisallowedHost", "Django"),
        (400, "Invalid HTTP_HOST", "Django"),
        (403, "Blocked host", "Rails"),
    ];
    SIGS.iter()
        .find(|(code, needle, _)| status == *code && text.contains(needle))
        .map(|(_, _, label)| *label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_vite_block() {
        let body = b"HTTP/1.1 403 Forbidden\r\n\r\nBlocked request. This host is not allowed.";
        assert_eq!(detect(Some(403), body), Some("Vite"));
    }

    #[test]
    fn detects_django_and_webpack_and_rails() {
        assert_eq!(
            detect(Some(400), b"...DisallowedHost at /..."),
            Some("Django")
        );
        assert_eq!(
            detect(Some(403), b"...Invalid Host header..."),
            Some("webpack-dev-server")
        );
        assert_eq!(detect(Some(403), b"...Blocked host: x..."), Some("Rails"));
    }

    #[test]
    fn ignores_normal_403_and_missing_status() {
        assert_eq!(
            detect(Some(403), b"HTTP/1.1 403 Forbidden\r\n\r\nnope"),
            None
        );
        assert_eq!(detect(None, b"Blocked request"), None);
    }
}
