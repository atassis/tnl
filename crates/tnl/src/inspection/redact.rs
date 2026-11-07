use super::model::Header;

pub(super) const MASK: &str = "[REDACTED]";
/// Controls are rendered visibly; slicing is always on a UTF-8 boundary.
pub(super) fn text(value: &str, limit: usize) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        let ch = if ch.is_control() { '\u{fffd}' } else { ch };
        if out.len() + ch.len_utf8() > limit {
            break;
        }
        out.push(ch);
    }
    out.into_boxed_str().into_string()
}
fn valid_percent(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit()
            {
                return false;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    true
}
pub(super) fn target(value: &str) -> String {
    // Only origin-form is displayed. Absolute/userinfo/invalid targets never
    // fall back to their unsanitized input.
    if !value.starts_with('/')
        || value.starts_with("//")
        || !valid_percent(value)
        || value.contains('#')
    {
        return "[unavailable]".into();
    }
    let (path, query) = value
        .split_once('?')
        .map_or((value, None), |(p, q)| (p, Some(q)));
    let mut out = text(path, 2048);
    if let Some(query) = query {
        if out.len() < 2048 {
            out.push('?');
        }
        for (i, pair) in query.split('&').enumerate() {
            let key = pair.split('=').next().unwrap_or("");
            let key = text(key, 128);
            let required = usize::from(i > 0) + key.len() + 1 + MASK.len();
            if out.len() + required > 2048 {
                break;
            }
            if i > 0 {
                out.push('&');
            }
            out.push_str(&key);
            out.push('=');
            out.push_str(MASK);
        }
    }
    out.into_boxed_str().into_string()
}
pub(super) fn headers(headers: &[httparse::Header<'_>]) -> Vec<Header> {
    let mut out = Vec::new();
    let mut budget = 8192usize;
    for header in headers.iter().take(64) {
        let name = text(header.name, 128).to_ascii_lowercase();
        let allowed = matches!(
            name.as_str(),
            "accept"
                | "accept-encoding"
                | "accept-language"
                | "content-type"
                | "content-length"
                | "transfer-encoding"
                | "connection"
                | "host"
                | "cache-control"
                | "pragma"
                | "expires"
                | "date"
                | "vary"
                | "allow"
                | "retry-after"
                | "x-tnl-component"
                | "x-tnl-origin-failure"
                | "x-tnl-request-id"
        );
        let value = if allowed {
            std::str::from_utf8(header.value)
                .map_or_else(|_| "[unavailable]".into(), |s| text(s, 1024))
        } else {
            MASK.into()
        };
        let size = name.len() + value.len();
        if size > budget {
            break;
        }
        budget -= size;
        out.push(Header {
            name,
            value,
            redacted: !allowed,
        });
    }
    out.into_boxed_slice().into_vec()
}
fn secret(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "password"
            | "passwd"
            | "secret"
            | "token"
            | "access_token"
            | "refresh_token"
            | "id_token"
            | "api_key"
            | "apikey"
            | "authorization"
            | "cookie"
            | "set-cookie"
    )
}
fn sanitize(value: &mut serde_json::Value, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if secret(key) {
                    *value = serde_json::Value::String(MASK.into());
                } else if !sanitize(value, depth + 1) {
                    return false;
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                if !sanitize(value, depth + 1) {
                    return false;
                }
            }
        }
        _ => {}
    }
    true
}
struct BoundedJson(Vec<u8>);
impl std::io::Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len() + bytes.len() > 4096 {
            return Err(std::io::ErrorKind::OutOfMemory.into());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn json(bytes: &[u8]) -> Option<String> {
    // Bound the parsed tree before serde allocates it. This deliberately omits
    // structurally dense JSON as well as deep JSON, even below the byte cap.
    let mut quoted = false;
    let mut escaped = false;
    let mut depth = 0usize;
    let mut structure = 0usize;
    for &byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'[' | b'{' => {
                    depth += 1;
                    structure += 1;
                }
                b']' | b'}' => depth = depth.checked_sub(1)?,
                b',' | b':' => structure += 1,
                _ => {}
            }
            if depth > 32 || structure > 64 {
                return None;
            }
        }
    }

    let mut value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if !sanitize(&mut value, 0) {
        return None;
    }
    let mut output = BoundedJson(Vec::with_capacity(4096));
    serde_json::to_writer(&mut output, &value).ok()?;
    String::from_utf8(output.0).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn controls_utf8_and_query_fail_closed() {
        assert_eq!(text("a\u{1b}éé", 6), "a�é");
        assert_eq!(
            target("/é?a=CANARY&a=&b"),
            "/é?a=[REDACTED]&a=[REDACTED]&b=[REDACTED]"
        );
        for target_ in ["/x?a=%GG", "http://user:secret@host/x", "//secret/x"] {
            assert_eq!(target(target_), "[unavailable]");
        }
        assert_eq!(
            json(br#"{"nested":[{"ToKeN":"CANARY"}],"ok":1}"#).unwrap(),
            r#"{"nested":[{"ToKeN":"[REDACTED]"}],"ok":1}"#
        );
        assert!(json(b"{\"token\":\"CANARY").is_none());
    }
}
