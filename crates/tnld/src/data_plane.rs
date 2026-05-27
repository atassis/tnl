use std::fmt::Write as _;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{HeaderName, HeaderValue};
use axum::http::{HeaderMap, Response, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, error, info, warn};

use crate::serve::AppState;

#[allow(clippy::too_many_lines)]
pub async fn handler(State(state): State<AppState>, req: Request) -> Response<Body> {
    use tnl_protocol::error_page::{parse_accept, Component, ErrorContext};
    use ulid::Ulid;

    let req_id = Ulid::new();
    let version = env!("CARGO_PKG_VERSION");

    // ---- Accept negotiation -------------------------------------------------
    let accept_h = req
        .headers()
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    let ua_h = req
        .headers()
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    let accept = parse_accept(accept_h.as_deref(), ua_h.as_deref());

    // ---- Host lookup --------------------------------------------------------
    let host = req
        .headers()
        .get("Host")
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    let Some(host) = host else {
        return error_response(
            StatusCode::BAD_REQUEST,
            Component::Registry,
            &ErrorContext {
                component: Component::Registry,
                kind: None,
                tunnel: None,
                target: None,
                reason: "Missing Host header",
                hint: "Caddy strips this; check the reverse proxy config.",
                req_id,
                version,
            },
            accept,
            None,
        );
    };
    let host_no_port = host.split(':').next().unwrap_or(&host).to_string();

    let Some(tunnel) = state.registry.find_by_hostname(&host_no_port) else {
        debug!(%host, "no tunnel registered for host");
        return error_response(
            StatusCode::NOT_FOUND,
            Component::Registry,
            &ErrorContext {
                component: Component::Registry,
                kind: None,
                tunnel: Some(&host_no_port),
                target: None,
                reason: "No tunnel is currently registered for this host.",
                hint: "Start the tunnel with `tnl http <TARGET> <SUBDOMAIN>`.",
                req_id,
                version,
            },
            accept,
            None,
        );
    };

    let Some(session_id) = state.registry.current_session_id(&tunnel.id) else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            Component::Daemon,
            &ErrorContext {
                component: Component::Daemon,
                kind: None,
                tunnel: Some(&tunnel.subdomain),
                target: None,
                reason: "Tunnel is in grace window awaiting reattach.",
                hint: "Try again in a moment; the client should reconnect automatically.",
                req_id,
                version,
            },
            accept,
            Some("1"),
        );
    };
    let Some(handle) = state.session_handles.get(&session_id).map(|h| h.clone()) else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            Component::Daemon,
            &ErrorContext {
                component: Component::Daemon,
                kind: None,
                tunnel: Some(&tunnel.subdomain),
                target: None,
                reason: "Tunnel session handle not yet ready.",
                hint: "Try again in a moment.",
                req_id,
                version,
            },
            accept,
            Some("1"),
        );
    };

    let mut session_guard = handle.lock().await;
    let mut substream = match session_guard.open_stream().await {
        Ok(s) => s,
        Err(e) => {
            error!(?e, "open_stream failed");
            warn!(
                target: "tnl::data_plane::server_failure",
                component = "transport",
                method = %req.method(),
                path = %req.uri().path(),
                tunnel = %tunnel.subdomain,
                req_id = %req_id,
                "served 502 from daemon: open_stream"
            );
            return error_response(
                StatusCode::BAD_GATEWAY,
                Component::Transport,
                &ErrorContext {
                    component: Component::Transport,
                    kind: Some("yamux-open-failed"),
                    tunnel: Some(&tunnel.subdomain),
                    target: None,
                    reason: &format!("Could not open a tunnel substream: {e}"),
                    hint: "Daemon transport is degraded; check `tnld` logs.",
                    req_id,
                    version,
                },
                accept,
                None,
            );
        }
    };
    drop(session_guard);

    let (parts, body) = req.into_parts();
    let head = serialize_http1_request_head(&parts);
    if let Err(e) = substream.write_all(head.as_bytes()).await {
        warn!(
            target: "tnl::data_plane::server_failure",
            component = "transport",
            req_id = %req_id,
            "served 502 from daemon: write head"
        );
        return error_response(
            StatusCode::BAD_GATEWAY,
            Component::Transport,
            &ErrorContext {
                component: Component::Transport,
                kind: Some("write-head"),
                tunnel: Some(&tunnel.subdomain),
                target: None,
                reason: &format!("Could not forward request head: {e}"),
                hint: "Daemon transport is degraded.",
                req_id,
                version,
            },
            accept,
            None,
        );
    }

    let body_bytes = match axum::body::to_bytes(body, 100 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            warn!(?e, "read upstream body");
            return error_response(
                StatusCode::BAD_GATEWAY,
                Component::Transport,
                &ErrorContext {
                    component: Component::Transport,
                    kind: Some("read-end-user-body"),
                    tunnel: Some(&tunnel.subdomain),
                    target: None,
                    reason: &format!("Could not read end-user body: {e}"),
                    hint: "End-user disconnected mid-upload.",
                    req_id,
                    version,
                },
                accept,
                None,
            );
        }
    };
    if !body_bytes.is_empty() {
        if let Err(e) = substream.write_all(&body_bytes).await {
            warn!(?e, "write body to substream");
            return error_response(
                StatusCode::BAD_GATEWAY,
                Component::Transport,
                &ErrorContext {
                    component: Component::Transport,
                    kind: Some("write-body"),
                    tunnel: Some(&tunnel.subdomain),
                    target: None,
                    reason: &format!("Could not forward request body: {e}"),
                    hint: "Daemon transport is degraded.",
                    req_id,
                    version,
                },
                accept,
                None,
            );
        }
    }

    let mut resp_buf = Vec::with_capacity(8 * 1024);
    let mut tmp = [0u8; 8 * 1024];
    loop {
        match substream.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => resp_buf.extend_from_slice(&tmp[..n]),
            Err(e) => {
                warn!(
                    target: "tnl::data_plane::server_failure",
                    component = "transport",
                    req_id = %req_id,
                    error = %e,
                    "served 502 from daemon: read resp",
                );
                return error_response(
                    StatusCode::BAD_GATEWAY,
                    Component::Transport,
                    &ErrorContext {
                        component: Component::Transport,
                        kind: Some("read-resp"),
                        tunnel: Some(&tunnel.subdomain),
                        target: None,
                        reason: &format!("Could not read tunnel response: {e}"),
                        hint: "Daemon transport is degraded.",
                        req_id,
                        version,
                    },
                    accept,
                    None,
                );
            }
        }
    }

    tunnel
        .stats
        .requests
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    tunnel.stats.bytes_in.fetch_add(
        body_bytes.len() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
    tunnel
        .stats
        .bytes_out
        .fetch_add(resp_buf.len() as u64, std::sync::atomic::Ordering::Relaxed);

    info!(
        host = %host_no_port,
        method = %parts.method,
        path = %parts.uri.path(),
        status = ?parsed_status_for_log(&resp_buf),
        bytes_resp = resp_buf.len(),
        req_id = %req_id,
        "data-plane request"
    );

    match build_response_from_raw(&resp_buf) {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "parse backend response");
            error_response(
                StatusCode::BAD_GATEWAY,
                Component::Client,
                &ErrorContext {
                    component: Component::Client,
                    kind: Some("client-malformed-response"),
                    tunnel: Some(&tunnel.subdomain),
                    target: None,
                    reason: "Client emitted a response that could not be parsed.",
                    hint: "Update to tnl >= 0.1.0-beta.1; older clients can produce this.",
                    req_id,
                    version,
                },
                accept,
                None,
            )
        }
    }
}

fn error_response(
    status: StatusCode,
    component: tnl_protocol::error_page::Component,
    ctx: &tnl_protocol::error_page::ErrorContext<'_>,
    accept: tnl_protocol::error_page::Accept,
    retry_after_secs: Option<&str>,
) -> Response<Body> {
    use tnl_protocol::error_page::render;
    let body = render(ctx, accept);
    let mut builder = Response::builder()
        .status(status)
        .header("Content-Type", accept.content_type())
        .header("Cache-Control", "no-store")
        .header("Connection", "close")
        .header("X-Tnl-Component", component.as_header())
        .header("X-Tnl-Request-Id", ctx.req_id.to_string());
    if let Some(s) = retry_after_secs {
        builder = builder.header("Retry-After", s);
    }
    builder.body(Body::from(body)).unwrap()
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
