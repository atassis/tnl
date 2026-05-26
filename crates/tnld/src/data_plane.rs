use std::fmt::Write as _;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::Response;
use axum::http::StatusCode;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, error, warn};

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

    let session_id = &tunnel.session_id;
    let Some(handle) = state.session_handles.get(session_id).map(|h| h.clone()) else {
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

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/octet-stream")
        .body(Body::from(resp_buf))
        .unwrap()
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
