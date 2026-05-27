use std::fmt::Write as _;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{HeaderName, HeaderValue};
use axum::http::{HeaderMap, Response, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, error, info, warn};

use crate::serve::AppState;

pub async fn handler(State(state): State<AppState>, req: Request) -> Response<Body> {
    let host = req
        .headers()
        .get("Host")
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    let Some(host) = host else {
        return text(StatusCode::BAD_REQUEST, "missing Host header");
    };
    let host_no_port = host.split(':').next().unwrap_or(&host).to_string();

    let Some(tunnel) = state.registry.find_by_hostname(&host_no_port) else {
        debug!(%host, "no tunnel registered for host");
        return text(StatusCode::BAD_GATEWAY, "no such tunnel\n");
    };

    let Some(session_id) = state.registry.current_session_id(&tunnel.id) else {
        return text(
            StatusCode::SERVICE_UNAVAILABLE,
            "tunnel disconnected, awaiting reattach\n",
        );
    };
    let Some(handle) = state.session_handles.get(&session_id).map(|h| h.clone()) else {
        return text(
            StatusCode::SERVICE_UNAVAILABLE,
            "client session not ready\n",
        );
    };

    let mut session_guard = handle.lock().await;
    let mut substream = match session_guard.open_stream().await {
        Ok(s) => s,
        Err(e) => {
            error!(?e, "open_stream failed");
            return text(
                StatusCode::SERVICE_UNAVAILABLE,
                "could not open substream\n",
            );
        }
    };
    drop(session_guard);

    let (parts, body) = req.into_parts();
    let head = serialize_http1_request_head(&parts);
    if let Err(e) = substream.write_all(head.as_bytes()).await {
        warn!(?e, "write head to substream");
        return text(StatusCode::BAD_GATEWAY, "write upstream failed\n");
    }

    let body_bytes = match axum::body::to_bytes(body, 100 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            warn!(?e, "read upstream body");
            return text(StatusCode::BAD_GATEWAY, "read req body failed\n");
        }
    };
    if !body_bytes.is_empty() {
        if let Err(e) = substream.write_all(&body_bytes).await {
            warn!(?e, "write body to substream");
            return text(StatusCode::BAD_GATEWAY, "write body failed\n");
        }
    }

    let mut resp_buf = Vec::with_capacity(8 * 1024);
    let mut tmp = [0u8; 8 * 1024];
    loop {
        match substream.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => resp_buf.extend_from_slice(&tmp[..n]),
            Err(e) => {
                warn!(?e, "read substream resp");
                return text(StatusCode::BAD_GATEWAY, "read resp failed\n");
            }
        }
    }

    info!(
        host = %host_no_port,
        method = %parts.method,
        path = %parts.uri.path(),
        status = ?parsed_status_for_log(&resp_buf),
        bytes_resp = resp_buf.len(),
        "data-plane request"
    );

    match build_response_from_raw(&resp_buf) {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "parse backend response");
            text(StatusCode::BAD_GATEWAY, "bad backend response\n")
        }
    }
}

fn serialize_http1_request_head(parts: &axum::http::request::Parts) -> String {
    let mut s = String::with_capacity(256);
    let path = parts.uri.path_and_query().map_or("/", |p| p.as_str());
    let _ = write!(s, "{} {} {:?}\r\n", parts.method, path, parts.version);
    s.push_str("Connection: close\r\n");
    // Inject Host header (use original host header) so the downstream server sees it.
    if let Some(h) = parts.headers.get("Host").and_then(|v| v.to_str().ok()) {
        let _ = write!(s, "Host: {h}\r\n");
    }
    for (k, v) in &parts.headers {
        let name = k.as_str();
        if name.eq_ignore_ascii_case("host") || name.eq_ignore_ascii_case("connection") {
            continue;
        }
        if let Ok(vs) = v.to_str() {
            let _ = write!(s, "{name}: {vs}\r\n");
        }
    }
    s.push_str("\r\n");
    s
}

fn text(code: StatusCode, msg: &str) -> Response<Body> {
    Response::builder()
        .status(code)
        .body(Body::from(msg.to_owned()))
        .unwrap()
}

fn build_response_from_raw(buf: &[u8]) -> Result<Response<Body>, String> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut parsed = httparse::Response::new(&mut headers);
    let header_end = match parsed.parse(buf).map_err(|e| e.to_string())? {
        httparse::Status::Complete(n) => n,
        httparse::Status::Partial => return Err("partial response from backend".into()),
    };
    let code = parsed.code.ok_or("missing status code")?;
    let status = StatusCode::from_u16(code).map_err(|e| e.to_string())?;

    let mut chunked = false;
    let mut out_headers = HeaderMap::new();
    for h in parsed.headers.iter() {
        let name_lc = h.name.to_ascii_lowercase();
        // Hop-by-hop and framing headers we must not forward verbatim; axum/hyper
        // sets Content-Length itself based on the body bytes we hand it.
        match name_lc.as_str() {
            "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "upgrade"
            | "content-length" => continue,
            "transfer-encoding" => {
                if h.value.eq_ignore_ascii_case(b"chunked") {
                    chunked = true;
                }
                continue;
            }
            _ => {}
        }
        let name = HeaderName::from_bytes(h.name.as_bytes()).map_err(|e| e.to_string())?;
        let value = HeaderValue::from_bytes(h.value).map_err(|e| e.to_string())?;
        out_headers.append(name, value);
    }

    let raw_body = &buf[header_end..];
    let body_bytes = if chunked {
        decode_chunked(raw_body)?
    } else {
        raw_body.to_vec()
    };

    let mut resp = Response::builder().status(status);
    if let Some(hs) = resp.headers_mut() {
        *hs = out_headers;
    }
    resp.body(Body::from(body_bytes)).map_err(|e| e.to_string())
}

fn decode_chunked(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    loop {
        let crlf = input
            .get(i..)
            .and_then(|s| s.windows(2).position(|w| w == b"\r\n"))
            .ok_or_else(|| "missing CRLF in chunk size line".to_string())?;
        let size_line = std::str::from_utf8(&input[i..i + crlf]).map_err(|e| e.to_string())?;
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|e| format!("bad chunk size {size_hex:?}: {e}"))?;
        i += crlf + 2;
        if size == 0 {
            break;
        }
        if i + size > input.len() {
            return Err("truncated chunk body".into());
        }
        out.extend_from_slice(&input[i..i + size]);
        i += size;
        if input.get(i..i + 2) != Some(b"\r\n") {
            return Err("missing CRLF after chunk body".into());
        }
        i += 2;
    }
    Ok(out)
}

fn parsed_status_for_log(buf: &[u8]) -> Option<u16> {
    let mut headers = [httparse::EMPTY_HEADER; 8];
    let mut parsed = httparse::Response::new(&mut headers);
    match parsed.parse(buf).ok()? {
        httparse::Status::Complete(_) => parsed.code,
        httparse::Status::Partial => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_http10_response() {
        let raw = b"HTTP/1.0 200 OK\r\nContent-Type: text/html\r\nContent-Length: 5\r\n\r\nhello";
        let resp = build_response_from_raw(raw).expect("parse ok");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .map(axum::http::HeaderValue::as_bytes),
            Some(b"text/html".as_ref())
        );
        // Content-Length must NOT be forwarded; axum sets it.
        assert!(resp.headers().get("content-length").is_none());
    }

    #[test]
    fn decodes_chunked_body() {
        let raw =
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let resp = build_response_from_raw(raw).expect("parse ok");
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get("transfer-encoding").is_none());
    }

    #[test]
    fn propagates_non_200_status() {
        let raw = b"HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\n\r\nnope";
        let resp = build_response_from_raw(raw).expect("parse ok");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn rejects_partial() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type:";
        assert!(build_response_from_raw(raw).is_err());
    }

    #[test]
    fn chunked_decode_matches() {
        let body = b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let decoded = decode_chunked(body).expect("decode ok");
        assert_eq!(decoded, b"hello world");
    }
}
