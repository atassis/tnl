use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::HeaderName;
use axum::http::{HeaderMap, HeaderValue, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tnl_protocol::request_metadata::{
    write_request_metadata, DataStreamProtocol, RequestMetadataV1, VisitorIdentity,
    REQUEST_METADATA_TIMEOUT,
};
use tracing::Instrument as _;
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

// Reframe an end-to-end trailer declaration after removing hop headers. Hyper
// will otherwise discard trailer frames even though the leased body retains them.
fn response_trailer_declaration(headers: &HeaderMap) -> Option<HeaderValue> {
    let connection_fields = headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|name| name.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut names = headers
        .get_all("trailer")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .filter(|name| {
            !HOP_BY_HOP.contains(&name.as_str())
                && !connection_fields.iter().any(|field| field == name.as_str())
                && ![
                    "authorization",
                    "set-cookie",
                    "host",
                    "cache-control",
                    "content-encoding",
                    "content-range",
                    "content-type",
                    "max-forwards",
                ]
                .contains(&name.as_str())
        })
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    if names.is_empty() {
        None
    } else {
        HeaderValue::from_str(&names.join(", ")).ok()
    }
}

const WEBSOCKET_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const WEBSOCKET_RENDEZVOUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

struct WebSocketHandshake {
    expected_accept: String,
    offered_protocols: Vec<String>,
}

fn header_tokens(headers: &HeaderMap, name: &str) -> Result<Vec<String>, &'static str> {
    let mut tokens = Vec::new();
    for value in headers.get_all(name) {
        let value = value
            .to_str()
            .map_err(|_| "Invalid handshake header encoding.")?;
        for token in value.split(',') {
            let token = token.trim_matches([' ', '\t']);
            if token.is_empty() || !token.bytes().all(is_token_byte) {
                return Err("Invalid handshake token list.");
            }
            tokens.push(token.to_string());
        }
    }
    Ok(tokens)
}

const fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn singleton_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, &'static str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next().ok_or("Missing WebSocket handshake header.")?;
    if values.next().is_some() {
        return Err("Duplicate WebSocket handshake header.");
    }
    let value = value
        .to_str()
        .map_err(|_| "Invalid handshake header encoding.")?
        .trim_matches([' ', '\t']);
    if value.is_empty() {
        return Err("Empty WebSocket handshake header.");
    }
    Ok(value)
}

fn validate_upgrade_headers(headers: &HeaderMap, response: bool) -> Result<(), &'static str> {
    let connection = header_tokens(headers, "connection")?;
    if !connection.iter().any(|s| s.eq_ignore_ascii_case("upgrade"))
        || connection.iter().any(|s| s.eq_ignore_ascii_case("close"))
    {
        return Err("WebSocket requires Connection upgrade without close.");
    }
    for field in &connection {
        let field = field.to_ascii_lowercase();
        if matches!(
            field.as_str(),
            "host" | "origin" | "authorization" | "cookie"
        ) || field.starts_with("sec-websocket-")
            || (response && field == "set-cookie")
        {
            return Err("Connection nominates a protected handshake header.");
        }
    }
    if !singleton_header(headers, "upgrade")?.eq_ignore_ascii_case("websocket") {
        return Err("Only classic WebSocket upgrades are supported.");
    }
    Ok(())
}

fn classify_websocket_upgrade(req: &Request) -> Result<Option<WebSocketHandshake>, &'static str> {
    let headers = req.headers();
    // Do not tighten token handling on ordinary HTTP requests. Only Upgrade or
    // a Connection upgrade token opts into this protocol-specific validation.
    let attempted = headers.contains_key("upgrade")
        || headers
            .get_all("connection")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(|v| v.trim().eq_ignore_ascii_case("upgrade"));
    if !attempted {
        return Ok(None);
    }
    if req.method() != axum::http::Method::GET || req.version() != axum::http::Version::HTTP_11 {
        return Err("WebSocket requires HTTP/1.1 GET.");
    }
    validate_upgrade_headers(headers, false)?;
    let key = singleton_header(headers, "sec-websocket-key")?;
    if singleton_header(headers, "sec-websocket-version")? != "13" {
        return Err("WebSocket version must be 13.");
    }
    if headers.contains_key("transfer-encoding") {
        return Err("WebSocket handshake cannot have a request body.");
    }
    // Hyper normalizes identical wire Content-Length values before Axum. We
    // validate the normalized envelope, not original wire-field cardinality.
    if headers.contains_key("content-length") {
        let length = singleton_header(headers, "content-length")?;
        if !length.bytes().all(|b| b.is_ascii_digit()) || length.parse::<u64>() != Ok(0) {
            return Err("WebSocket handshake cannot have a request body.");
        }
    }
    let offered_protocols = header_tokens(headers, "sec-websocket-protocol")?;
    if req
        .extensions()
        .get::<hyper::upgrade::OnUpgrade>()
        .is_none()
    {
        return Err("The visitor connection cannot be upgraded.");
    }
    Ok(Some(WebSocketHandshake {
        expected_accept: async_tungstenite::tungstenite::handshake::derive_accept_key(
            key.as_bytes(),
        ),
        offered_protocols,
    }))
}

fn validate_websocket_response<B>(
    resp: &Response<B>,
    handshake: &WebSocketHandshake,
) -> Result<(), &'static str> {
    if resp.version() != axum::http::Version::HTTP_11 {
        return Err("Backend upgrade requires HTTP/1.1.");
    }
    let headers = resp.headers();
    validate_upgrade_headers(headers, true)?;
    if singleton_header(headers, "sec-websocket-accept")? != handshake.expected_accept {
        return Err("Backend WebSocket accept does not match.");
    }
    if headers.contains_key("content-length") || headers.contains_key("transfer-encoding") {
        return Err("Backend upgrade cannot contain HTTP body framing.");
    }
    if headers.contains_key("sec-websocket-protocol") {
        let selected = singleton_header(headers, "sec-websocket-protocol")?;
        if !selected.bytes().all(is_token_byte)
            || !handshake.offered_protocols.iter().any(|p| p == selected)
        {
            return Err("Backend selected an unoffered WebSocket protocol.");
        }
    }
    Ok(())
}

fn set_websocket_upgrade_headers(headers: &mut HeaderMap) {
    headers.insert("connection", HeaderValue::from_static("upgrade"));
    headers.insert("upgrade", HeaderValue::from_static("websocket"));
}

/// Cancellation while negotiating must release this request's tunnel IO.
/// Ordinary streamed responses explicitly detach; upgrades transfer IO to the
/// relay only after the Hyper driver has completed its handoff.
struct TunnelConnectionDriver(Option<tokio::task::JoinHandle<()>>);

impl TunnelConnectionDriver {
    async fn stop(mut self) {
        if let Some(driver) = self.0.take() {
            driver.abort();
            let _ = driver.await;
        }
    }

    fn detach(mut self) {
        self.0.take();
    }
}

impl Drop for TunnelConnectionDriver {
    fn drop(&mut self) {
        if let Some(driver) = &self.0 {
            driver.abort();
        }
    }
}

/// This future owns both upgraded IOs and its concurrency lease through
/// rendezvous and the entire relay, independently of the empty 101 body.
async fn relay_websocket(
    downstream: hyper::upgrade::OnUpgrade,
    upstream: hyper::upgrade::Upgraded,
    req_id: ulid::Ulid,
    lease: crate::access::AdmissionLease,
) {
    let _lease = lease;
    let Ok(Ok(downstream)) = tokio::time::timeout(WEBSOCKET_RENDEZVOUS_TIMEOUT, downstream).await
    else {
        warn!(%req_id, "WebSocket visitor upgrade rendezvous failed");
        return;
    };
    // TokioIo retains Hyper's read-ahead buffers on BOTH connections. The raw
    // copy uses two fixed-size buffers with half-close propagation; no frames
    // are decoded, queued, recorded, or rewritten here.
    if let Err(error) =
        tokio::io::copy_bidirectional(&mut TokioIo::new(downstream), &mut TokioIo::new(upstream))
            .await
    {
        debug!(%req_id, %error, "WebSocket relay ended");
    }
}

pub async fn handler(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Response<Body> {
    let visitor = state.trusted_proxies.resolve(peer.ip(), req.headers());
    let span = tracing::info_span!("data_plane_request", visitor_ip = %visitor.visitor_ip, peer_ip = %visitor.peer_ip, visitor_ip_source = visitor.source.as_str());
    handle_request(state, req, visitor).instrument(span).await
}

// The admission lease deliberately spans driver/body/relay ownership transfers.
#[allow(clippy::too_many_lines, clippy::significant_drop_tightening)]
async fn handle_request(
    state: AppState,
    mut req: Request,
    visitor: VisitorIdentity,
) -> Response<Body> {
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

    let lease = if let Some(access) = &tunnel.access {
        match access
            .admit(
                crate::access::visitor_ip_for_admission(&visitor),
                req.headers_mut(),
            )
            .await
        {
            Ok(lease) => lease,
            Err(denial) => {
                use crate::access::AccessDenial;
                let (status, kind, retry) = match denial {
                    AccessDenial::Basic => (StatusCode::UNAUTHORIZED, "basic-auth-required", None),
                    AccessDenial::Ip => (StatusCode::FORBIDDEN, "ip-not-allowed", None),
                    AccessDenial::Rate { retry_after } => (
                        StatusCode::TOO_MANY_REQUESTS,
                        "rate-limited",
                        Some(retry_after.to_string()),
                    ),
                    AccessDenial::Concurrency => (
                        StatusCode::TOO_MANY_REQUESTS,
                        "concurrency-limited",
                        Some("1".into()),
                    ),
                    AccessDenial::Unavailable => (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "access-unavailable",
                        Some("1".into()),
                    ),
                };
                info!(%req_id, tunnel = %tunnel.subdomain, method = %req.method(), path = %req.uri().path(), %status, %kind, "visitor access denied");
                let mut response = error_response(
                    status,
                    Component::Access,
                    &ErrorContext {
                        component: Component::Access,
                        kind: Some(kind),
                        tunnel: Some(&tunnel.subdomain),
                        target: None,
                        reason: "Visitor access denied.",
                        hint: "Check the tunnel's visitor access requirements.",
                        req_id,
                        version,
                    },
                    accept,
                    retry.as_deref(),
                );
                if matches!(denial, AccessDenial::Basic) {
                    response.headers_mut().insert(
                        "www-authenticate",
                        HeaderValue::from_static("Basic realm=\"tnl\", charset=\"UTF-8\""),
                    );
                }
                if req.method() == axum::http::Method::HEAD {
                    *response.body_mut() = Body::empty();
                }
                return response;
            }
        }
    } else {
        crate::access::AdmissionLease::default()
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

    // Validate before opening a substream. The downstream socket capability must
    // stay here, never in the upstream request's extensions.
    let upgrade_error = |status: StatusCode, component: Component, kind, reason| {
        if status.is_server_error() {
            warn!(target: "tnl::data_plane::server_failure",
                component = component.as_header(), %kind, %req_id,
                tunnel = %tunnel.subdomain, "served WebSocket handshake error");
        }
        error_response(
            status,
            component,
            &ErrorContext {
                component,
                kind: Some(kind),
                tunnel: Some(&tunnel.subdomain),
                target: None,
                reason,
                hint: "Check the application WebSocket handshake.",
                req_id,
                version,
            },
            accept,
            None,
        )
    };
    let websocket = match classify_websocket_upgrade(&req) {
        Ok(ws) => ws,
        Err(reason) => {
            return upgrade_error(
                StatusCode::BAD_REQUEST,
                Component::Registry,
                "invalid-websocket-request",
                reason,
            );
        }
    };
    let downstream_upgrade = websocket.as_ref().map(|_| hyper::upgrade::on(&mut req));

    // ---- Open a fresh substream to the client session -----------------------
    let mut session_guard = handle.session.lock().await;
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

    if handle.data_stream_protocol == DataStreamProtocol::Http1MetadataV1 {
        let metadata = RequestMetadataV1 {
            request_id: req_id,
            visitor,
        };
        let result = tokio::time::timeout(
            REQUEST_METADATA_TIMEOUT,
            write_request_metadata(&mut substream, &metadata),
        )
        .await;
        if !matches!(result, Ok(Ok(()))) {
            warn!(component = "transport", kind = "request-metadata-write", %req_id, error = ?result, "served 502 from daemon: request metadata write");
            return error_response(
                StatusCode::BAD_GATEWAY,
                Component::Transport,
                &ErrorContext {
                    component: Component::Transport,
                    kind: Some("request-metadata-write"),
                    tunnel: Some(&tunnel.subdomain),
                    target: None,
                    reason: "Could not write tunnel request metadata.",
                    hint: "Daemon transport is degraded; check tnld logs.",
                    req_id,
                    version,
                },
                accept,
                None,
            );
        }
    }

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
    let driver_lease = lease.clone();
    let driver = TunnelConnectionDriver(Some(tokio::spawn(
        async move {
            let _lease = driver_lease;
            if let Err(e) = conn.with_upgrades().await {
                debug!(error = %e, "tunnel client connection ended");
            }
        }
        .in_current_span(),
    )));

    // ---- Forward the request ------------------------------------------------
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let (mut parts, body) = req.into_parts();
    strip_hop_by_hop(&mut parts.headers);
    if websocket.is_some() {
        set_websocket_upgrade_headers(&mut parts.headers);
    } else {
        parts.headers.insert(
            axum::http::header::CONNECTION,
            HeaderValue::from_static("close"),
        );
    }
    // hyper's HTTP/1 client speaks 1.1; the loopback hop from Caddy is 1.1.
    parts.version = axum::http::Version::HTTP_11;
    let upstream_req = Request::from_parts(parts, body);

    let deadline = tokio::time::Instant::now() + WEBSOCKET_HANDSHAKE_TIMEOUT;
    let response = if websocket.is_some() {
        let Ok(result) = tokio::time::timeout_at(deadline, sender.send_request(upstream_req)).await
        else {
            driver.stop().await;
            return upgrade_error(
                StatusCode::BAD_GATEWAY,
                Component::Upstream,
                "websocket-handshake-timeout",
                "The backend did not complete the WebSocket response in time.",
            );
        };
        result
    } else {
        sender.send_request(upstream_req).await
    };
    let mut resp = match response {
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

    if resp.status() == StatusCode::SWITCHING_PROTOCOLS {
        let validation = websocket
            .as_ref()
            .ok_or("Unsolicited backend upgrade.")
            .and_then(|handshake| validate_websocket_response(&resp, handshake));
        if let Err(reason) = validation {
            driver.stop().await;
            return upgrade_error(
                StatusCode::BAD_GATEWAY,
                Component::Upstream,
                "invalid-websocket-response",
                reason,
            );
        }
        let Ok(Ok(upstream)) =
            tokio::time::timeout_at(deadline, hyper::upgrade::on(&mut resp)).await
        else {
            driver.stop().await;
            return upgrade_error(
                StatusCode::BAD_GATEWAY,
                Component::Transport,
                "websocket-upgrade-failed",
                "Could not acquire the upgraded tunnel connection.",
            );
        };
        // The HTTP driver has handed off the IO. Join it before transferring
        // ownership to the one rendezvous/relay task. Cancellation before this
        // point aborts only this per-request Hyper driver, never yamux's driver.
        driver.stop().await;
        let Some(downstream) = downstream_upgrade else {
            return upgrade_error(
                StatusCode::BAD_GATEWAY,
                Component::Transport,
                "websocket-upgrade-failed",
                "The visitor connection cannot be upgraded.",
            );
        };
        tokio::spawn(relay_websocket(downstream, upstream, req_id, lease).in_current_span());
        let (mut parts, _) = resp.into_parts();
        strip_hop_by_hop(&mut parts.headers);
        set_websocket_upgrade_headers(&mut parts.headers);
        info!(host = %host_no_port, method = %method, path = %path,
            status = 101, req_id = %req_id, "data-plane request");
        return Response::from_parts(parts, Body::empty());
    }

    // A non-101 (including an application auth/Origin refusal) is ordinary
    // streamed HTTP, not a retry. Its driver must outlive the response body.
    driver.detach();
    let (mut resp_parts, incoming) = resp.into_parts();
    let status = resp_parts.status;
    let trailer = response_trailer_declaration(&resp_parts.headers);
    strip_hop_by_hop(&mut resp_parts.headers);
    if let Some(trailer) = trailer {
        resp_parts.headers.insert("trailer", trailer);
    }

    info!(
        host = %host_no_port,
        method = %method,
        path = %path,
        status = %status.as_u16(),
        req_id = %req_id,
        "data-plane request"
    );

    Response::from_parts(
        resp_parts,
        Body::new(crate::access::LeasedBody::new(incoming, lease)),
    )
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
