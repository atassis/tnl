use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::HeaderName;
use axum::http::{HeaderMap, HeaderValue, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tracing::{debug, error, info, warn};

use crate::serve::AppState;

/// Hop-by-hop headers that MUST NOT be forwarded across a proxy (RFC 9110
/// §7.6.1). Framing headers (`content-length`, `transfer-encoding`) are also
/// stripped: hyper reframes both the forwarded request body and the relayed
/// response body itself.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "content-length",
];

/// Strip hop-by-hop headers, including any field named in the `Connection`
/// header value (RFC 9110 §7.6.1), then the framing/connection headers above.
fn strip_hop_by_hop(headers: &mut HeaderMap) {
    // Collect connection-named fields first (before we remove `Connection`).
    let mut named: Vec<HeaderName> = Vec::new();
    for v in headers.get_all("connection") {
        if let Ok(s) = v.to_str() {
            for tok in s.split(',') {
                if let Ok(h) = HeaderName::from_bytes(tok.trim().as_bytes()) {
                    named.push(h);
                }
            }
        }
    }
    for h in named {
        headers.remove(&h);
    }
    for name in HOP_BY_HOP {
        // `remove` drops the whole entry (every value) for the name.
        headers.remove(*name);
    }
}

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

    // ---- Open a fresh substream to the client session -----------------------
    let mut session_guard = handle.lock().await;
    let substream = match session_guard.open_stream().await {
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

    // ---- Drive the substream as an HTTP/1 client connection -----------------
    //
    // hyper owns all response parsing: chunked de-framing, Content-Length and
    // close-delimited bodies, leading 1xx responses, generous header limits,
    // and streaming. We never hand-parse bytes the client relayed, so we never
    // misattribute a parse failure to the client.
    let io = TokioIo::new(substream);
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(io).await {
        Ok(pair) => pair,
        Err(e) => {
            warn!(
                target: "tnl::data_plane::server_failure",
                component = "transport",
                req_id = %req_id,
                error = %e,
                "served 502 from daemon: tunnel handshake"
            );
            return error_response(
                StatusCode::BAD_GATEWAY,
                Component::Transport,
                &ErrorContext {
                    component: Component::Transport,
                    kind: Some("tunnel-handshake"),
                    tunnel: Some(&tunnel.subdomain),
                    target: None,
                    reason: &format!("Could not start the tunnel connection: {e}"),
                    hint: "Daemon transport is degraded.",
                    req_id,
                    version,
                },
                accept,
                None,
            );
        }
    };
    // The connection future drives IO; it completes once the response body has
    // been consumed and the substream closes. It must run concurrently with the
    // response read below, so spawn it.
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            debug!(error = %e, "tunnel client connection ended");
        }
    });

    // ---- Forward the request ------------------------------------------------
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let (mut parts, body) = req.into_parts();
    strip_hop_by_hop(&mut parts.headers);
    parts.headers.insert(
        axum::http::header::CONNECTION,
        HeaderValue::from_static("close"),
    );
    // hyper's HTTP/1 client speaks 1.1; the loopback hop from Caddy is 1.1.
    parts.version = axum::http::Version::HTTP_11;
    let upstream_req = Request::from_parts(parts, body);

    let resp = match sender.send_request(upstream_req).await {
        Ok(r) => r,
        Err(e) => {
            let (kind, reason): (&str, String) = if e.is_incomplete_message() {
                (
                    "incomplete-response",
                    "The backend closed the connection before sending a complete response."
                        .to_string(),
                )
            } else if e.is_parse() || e.is_parse_status() || e.is_parse_too_large() {
                (
                    "unparseable-response",
                    format!("The response received over the tunnel was not valid HTTP: {e}"),
                )
            } else {
                ("tunnel-io", format!("Tunnel I/O error: {e}"))
            };
            let is_io = kind == "tunnel-io";
            let component = if is_io {
                Component::Transport
            } else {
                Component::Upstream
            };
            warn!(
                target: "tnl::data_plane::server_failure",
                component = component.as_header(),
                %kind,
                tunnel = %tunnel.subdomain,
                host = %host_no_port,
                method = %method,
                path = %path,
                req_id = %req_id,
                error = %e,
                error_debug = ?e,
                "served 502 from daemon: send_request"
            );
            let hint = if is_io {
                "Daemon transport is degraded."
            } else {
                "The local backend spoke something tnld could not relay as HTTP/1.1 \
                 (e.g. HTTP/2, a non-HTTP protocol, or it closed mid-response). \
                 Check the local server."
            };
            return error_response(
                StatusCode::BAD_GATEWAY,
                component,
                &ErrorContext {
                    component,
                    kind: Some(kind),
                    tunnel: Some(&tunnel.subdomain),
                    target: None,
                    reason: &reason,
                    hint,
                    req_id,
                    version,
                },
                accept,
                None,
            );
        }
    };

    tunnel
        .stats
        .requests
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let (mut resp_parts, incoming) = resp.into_parts();
    let status = resp_parts.status;
    strip_hop_by_hop(&mut resp_parts.headers);

    info!(
        host = %host_no_port,
        method = %method,
        path = %path,
        status = %status.as_u16(),
        req_id = %req_id,
        "data-plane request"
    );

    Response::from_parts(resp_parts, Body::new(incoming))
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
