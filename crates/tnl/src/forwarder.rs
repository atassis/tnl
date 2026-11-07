//! Two-phase forwarder.
//!
//! Phase 1: pre-buffer the request head from the substream and attempt a
//! local connect. On any connect failure, synthesise a complete HTTP/1.1 502
//! onto the substream and close.
//!
//! Phase 2: write the buffered head + leftover body to the backend, then
//! concurrently upload the remaining request and probe the first response bytes.
//! If the backend sends a malformed prefix
//! or EOFs before any bytes, synthesise a 502. Otherwise pump bidirectionally
//! to completion (with response tee for the inspector).
//!
//! On every accepted substream the forwarder emits one `LogLine` to the
//! optional inspector channel.

use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use parking_lot::Mutex;
use tnl_protocol::error_page::{parse_accept, Accept};
use tnl_protocol::request_metadata::{
    read_request_metadata, DataStreamProtocol, RequestMetadataV1, VisitorIdentity,
    REQUEST_METADATA_TIMEOUT,
};
use tnl_protocol::Stream;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::connect::{connect_local, LOCAL_CONNECT_TIMEOUT};
use crate::inspector::LogLine;
use crate::peek::{
    peek_request_head, peek_response_first_line, RequestHeadProbe, ResponseProbe,
    REQUEST_HEAD_PROBE_MAX,
};
use crate::synth::{synth_response_bytes, SynthInput};
use crate::target::Target;

pub const LOCAL_FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(15);

/// Test-only override for the first-byte timeout, in milliseconds.
///
/// A value of `0` means "use the default `LOCAL_FIRST_BYTE_TIMEOUT`". Tests
/// set this to a small value (e.g. 200 ms) before exercising a no-response
/// path, then reset to 0 to leave shared process state unchanged.
///
/// This is a `pub` shared atomic because integration tests in
/// `crates/tnl/tests/` compile as external crates and cannot access
/// `pub(crate)` symbols, and `#[cfg(test)]` is not propagated to external
/// test crates.
#[doc(hidden)]
pub static TEST_FIRST_BYTE_TIMEOUT_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn first_byte_timeout() -> Duration {
    let ms = TEST_FIRST_BYTE_TIMEOUT_MS.load(std::sync::atomic::Ordering::Relaxed);
    if ms == 0 {
        LOCAL_FIRST_BYTE_TIMEOUT
    } else {
        Duration::from_millis(ms)
    }
}

/// Context carried through one forwarded substream.
#[derive(Clone)]
pub struct ForwardCtx {
    /// Opt-in process-scoped sanitized observation; absent means no scratch.
    pub inspection: Option<(
        crate::inspection::Inspection,
        crate::inspection::TunnelGeneration,
    )>,
    #[cfg(test)]
    pub(crate) inspection_ownership_check: Option<fn(bool)>,
    #[cfg(test)]
    pub(crate) connect_failure: Option<fn() -> crate::connect::ConnectError>,
    pub data_stream_protocol: DataStreamProtocol,
    pub tunnel: String,
    pub log_tx: Option<mpsc::Sender<LogLine>>,
    /// Crate version string used in error bodies (`env!("CARGO_PKG_VERSION")`).
    pub version: &'static str,
    /// What `Host` to present to the backend (dev-server allowlist smoothing).
    pub host_header: crate::host_header::HostHeader,
    /// Full public host (`<sub>.t.example.com`) shown in the block notice.
    pub host_public: String,
    /// Set once a host block is detected; makes `Auto` mode rewrite subsequent
    /// requests. Shared across every substream of a tunnel.
    pub rewrite_active: Arc<std::sync::atomic::AtomicBool>,
    /// Ensures the block notice prints at most once per tunnel.
    pub warned: Arc<std::sync::atomic::AtomicBool>,
}

impl ForwardCtx {
    /// Construct a context with default host-header handling (`Auto`) and fresh
    /// per-tunnel state. Prefer this in tests and simple call sites.
    #[must_use]
    pub fn new(
        tunnel: String,
        log_tx: Option<mpsc::Sender<LogLine>>,
        version: &'static str,
    ) -> Self {
        Self {
            inspection: None,
            #[cfg(test)]
            inspection_ownership_check: None,
            #[cfg(test)]
            connect_failure: None,
            data_stream_protocol: DataStreamProtocol::RawHttp1,
            tunnel,
            log_tx,
            version,
            host_header: crate::host_header::HostHeader::Auto,
            host_public: String::new(),
            rewrite_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            warned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

impl std::fmt::Debug for ForwardCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForwardCtx")
            .field("inspection", &self.inspection)
            .field("data_stream_protocol", &self.data_stream_protocol)
            .field("tunnel", &self.tunnel)
            .field("log_tx", &self.log_tx.as_ref().map(|_| "<sender>"))
            .field("version", &self.version)
            .field("host_header", &self.host_header)
            .field("host_public", &self.host_public)
            .field(
                "rewrite_active",
                &self
                    .rewrite_active
                    .load(std::sync::atomic::Ordering::Relaxed),
            )
            .field(
                "warned",
                &self.warned.load(std::sync::atomic::Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

/// Re-exported peek helpers (kept for backwards compatibility with tests that
/// imported `tnl::forwarder::peek::parse_request_line` / `parse_response_status`).
pub mod peek {
    pub use crate::peek::{peek_request_head, peek_response_first_line};

    /// Parse the first HTTP/1.x request line `METHOD PATH HTTP/x.y` plus
    /// optional `Accept` and `User-Agent` headers.
    #[allow(clippy::type_complexity)]
    pub fn parse_request_line(bytes: &[u8]) -> Option<(String, String, Option<(String, String)>)> {
        let s = std::str::from_utf8(bytes).ok()?;
        let mut lines = s.lines();
        let first = lines.next()?;
        let mut parts = first.split_whitespace();
        let method = parts.next()?.to_string();
        let path = parts.next()?.to_string();
        parts.next()?; // require HTTP/x.y to confirm a complete request line.
        let mut accept = None;
        let mut user_agent = None;
        for line in lines {
            if line.is_empty() {
                break;
            }
            if let Some(rest) = line
                .strip_prefix("Accept:")
                .or_else(|| line.strip_prefix("accept:"))
            {
                accept = Some(rest.trim().to_string());
            } else if let Some(rest) = line
                .strip_prefix("User-Agent:")
                .or_else(|| line.strip_prefix("user-agent:"))
            {
                user_agent = Some(rest.trim().to_string());
            }
        }
        let extras = match (accept, user_agent) {
            (Some(a), Some(u)) => Some((a, u)),
            (Some(a), None) => Some((a, String::new())),
            (None, Some(u)) => Some((String::new(), u)),
            (None, None) => None,
        };
        Some((method, path, extras))
    }

    /// Parse `HTTP/x.y CODE REASON` and return the numeric status code.
    pub fn parse_response_status(bytes: &[u8]) -> Option<u16> {
        let s = std::str::from_utf8(bytes).ok()?;
        let first_line = s.lines().next()?;
        let mut parts = first_line.split_whitespace();
        let _http = parts.next()?;
        parts.next()?.parse().ok()
    }
}

/// Authoritative context decoded before HTTP probing. Legacy sessions have no
/// metadata; request-ID propagation is intentionally owned by the next lane.
#[derive(Debug)]
struct RequestContext {
    metadata: Option<RequestMetadataV1>,
}

impl RequestContext {
    fn visitor(&self) -> Option<&VisitorIdentity> {
        self.metadata.as_ref().map(|metadata| &metadata.visitor)
    }
}

/// Single entry point for the forwarder. Returns `Ok(())` even when a synth
/// response was written for a local-side failure; returns `Err` only for
/// unrecoverable I/O on the substream itself.
#[allow(clippy::too_many_lines)]
pub async fn forward(
    substream: Pin<Box<dyn Stream>>,
    target: Target,
    ctx: ForwardCtx,
) -> anyhow::Result<()> {
    let req_id = ulid::Ulid::new();
    let started = Instant::now();
    let mut display = ForwardDisplay::new(&ctx, &target, started);
    #[cfg(test)]
    if let Some(check) = ctx.inspection_ownership_check {
        check(display.owns_allocated_state());
    }
    let mut substream = CountedIo {
        inner: substream,
        counter: display.counters.as_ref().map(|c| c.to_client.clone()),
    };
    let request = RequestContext {
        metadata: match ctx.data_stream_protocol {
            DataStreamProtocol::RawHttp1 => None,
            DataStreamProtocol::Http1MetadataV1 => Some(
                tokio::time::timeout(
                    REQUEST_METADATA_TIMEOUT,
                    read_request_metadata(&mut substream),
                )
                .await
                .inspect_err(|_| {
                    display.failure = Some("metadata-invalid");
                })?
                .inspect_err(|_| {
                    display.failure = Some("metadata-invalid");
                })?,
            ),
        },
    };

    display.metadata = request.metadata.clone();
    // Phase 1: peek request head.
    let probe = peek_request_head(
        &mut substream,
        REQUEST_HEAD_PROBE_MAX,
        Duration::from_secs(5),
    )
    .await
    .inspect_err(|_| {
        display.failure = Some("request-read");
    })?;
    let complete_head = matches!(probe, RequestHeadProbe::Complete { .. });
    let (head_bytes, leftover) = match probe {
        RequestHeadProbe::Complete { head, leftover } => (head, leftover),
        RequestHeadProbe::Capped(buf)
        | RequestHeadProbe::Eof(buf)
        | RequestHeadProbe::Timeout(buf) => (buf, Vec::new()),
    };
    let (method, path, accept) = parse_head_for_synth(&head_bytes);
    if ctx.inspection.is_some() {
        debug!(method = %crate::inspection::safe_method(&method), path = %crate::inspection::safe_target(&path), "request head parsed");
    } else {
        debug!(%method, %path, "request head parsed");
    }

    // Phase 1: connect.
    display.failure = Some("cancelled");
    #[cfg(not(test))]
    let connect_outcome = connect_local(&target, LOCAL_CONNECT_TIMEOUT).await;
    #[cfg(test)]
    let connect_outcome = if let Some(failure) = ctx.connect_failure {
        Err(failure())
    } else {
        connect_local(&target, LOCAL_CONNECT_TIMEOUT).await
    };
    let (tcp, resolved_addr) = match connect_outcome {
        Ok((s, a)) => (s, a),
        Err(e) => {
            if ctx.inspection.is_some() {
                warn!(kind = e.to_kind(), "local connect failed");
            } else {
                warn!(error = %e, display = %target.display(), "local connect failed");
            }
            display.failure = Some(e.to_kind());
            display.preconnect(Some(&head_bytes));
            substream.counter = display.counters.as_ref().map(|c| c.to_client.clone());
            let reason_str = e.to_string();
            let synth = synth_response_bytes(&SynthInput {
                kind: e.to_kind(),
                reason: &reason_str,
                hint: e.hint(),
                tunnel: &ctx.tunnel,
                display_target: &target.display(),
                accept,
                req_id,
                version: ctx.version,
            });
            display.synth(&synth);
            substream.write_all(&synth).await?;
            substream.shutdown().await.ok();
            emit_log(
                &ctx,
                LogLine {
                    req_id,
                    visitor_ip: request.visitor().map(|v| v.visitor_ip),
                    peer_ip: request.visitor().map(|v| v.peer_ip),
                    visitor_ip_source: request.visitor().map(|v| v.source),
                    timestamp: std::time::SystemTime::now(),
                    method,
                    path,
                    display_target: target.display(),
                    resolved_addr: None,
                    status: Some(502),
                    duration_ms: ms_since(started),
                    bytes_in: u64::try_from(head_bytes.len() + leftover.len()).unwrap_or(0),
                    bytes_out: u64::try_from(synth.len()).unwrap_or(0),
                    failure_kind: Some(e.to_kind().to_string()),
                },
            );
            return Ok(());
        }
    };

    display.resolved = Some(resolved_addr);
    let mut tcp = CountedIo {
        inner: tcp,
        counter: display.counters.as_ref().map(|c| c.to_backend.clone()),
    };
    // Decide what `Host` the backend sees (dev-server allowlist smoothing).
    let rewrite_to: Option<String> = match &ctx.host_header {
        crate::host_header::HostHeader::Preserve => None,
        crate::host_header::HostHeader::Fixed(v) => Some(v.clone()),
        crate::host_header::HostHeader::Rewrite => Some(resolved_addr.to_string()),
        crate::host_header::HostHeader::Auto => ctx
            .rewrite_active
            .load(std::sync::atomic::Ordering::Relaxed)
            .then(|| resolved_addr.to_string()),
    };
    let head_bytes = match &rewrite_to {
        Some(host) => crate::host_header::rewrite_host(&head_bytes, host),
        None => head_bytes,
    };

    // One backend-facing seam, after Host rewriting. Callback inputs are
    // intended writes, not successful-delivery counters. Errors never reject
    // forwarding; the lifetime guard records truncation on every early return.
    if complete_head {
        display.attempted = true;
        display.capture = ctx
            .inspection
            .as_ref()
            .and_then(|(inspection, generation)| {
                inspection.admit_backend(*generation, request.metadata.as_ref(), resolved_addr)
            })
            .map(|mut capture| {
                display.configure(&capture);
                capture.request(&head_bytes);
                capture.request(&leftover);
                Arc::new(Mutex::new(capture))
            });
    } else {
        display.preconnect(Some(&head_bytes));
    }
    substream.counter = display.counters.as_ref().map(|c| c.to_client.clone());
    tcp.counter = display.counters.as_ref().map(|c| c.to_backend.clone());
    #[cfg(test)]
    if let Some(check) = ctx.inspection_ownership_check {
        check(display.owns_allocated_state());
    }
    let observation = display.capture.clone();

    // Phase 2: write head + leftover body to backend.
    if let Err(e) = tcp.write_all(&head_bytes).await {
        return synth_phase2(
            &mut substream,
            &mut display,
            &ctx,
            &request,
            &target,
            resolved_addr,
            req_id,
            started,
            method,
            path,
            accept,
            "local-eof",
            "Backend closed connection while we wrote the request head.",
            "Verify the local server is listening on the right interface.",
            u64::try_from(head_bytes.len() + leftover.len()).unwrap_or(0),
            &format!("write to backend failed: {e}"),
        )
        .await;
    }
    if !leftover.is_empty() {
        if let Err(e) = tcp.write_all(&leftover).await {
            return synth_phase2(
                &mut substream,
                &mut display,
                &ctx,
                &request,
                &target,
                resolved_addr,
                req_id,
                started,
                method,
                path,
                accept,
                "local-eof",
                "Backend closed connection while we wrote the request body.",
                "Verify the local server is listening on the right interface.",
                u64::try_from(head_bytes.len() + leftover.len()).unwrap_or(0),
                &format!("write body to backend failed: {e}"),
            )
            .await;
        }
    }

    // Start uploading before probing: many backends wait for the entire body
    // before responding. Both futures stay in this task so cancellation drops
    // both directions without leaving a detached upload behind.
    let (mut yr, mut yw) = tokio::io::split(substream);
    let (mut tr, mut tw) = tokio::io::split(tcp);
    let bytes_in = Arc::new(AtomicU64::new(
        u64::try_from(head_bytes.len() + leftover.len()).unwrap_or(0),
    ));
    let bin = bytes_in.clone();
    let request_observation = observation.clone();
    let req_pump = async move {
        let mut buf = [0u8; 8192];
        loop {
            let n = yr.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            if let Some(capture) = &request_observation {
                capture.lock().request(&buf[..n]);
            }
            tw.write_all(&buf[..n]).await?;
            bin.fetch_add(u64::try_from(n).unwrap_or(u64::MAX), Ordering::Relaxed);
        }
        tw.shutdown().await?;
        Ok::<_, anyhow::Error>(())
    };
    tokio::pin!(req_pump);

    // Keep one probe future/deadline even if the upload finishes first. An
    // upload write failure must not discard an early backend response.
    let mut req_result = None;
    let probe_res = {
        display.failure = Some("cancelled");
        let probe = peek_response_first_line(&mut tr, first_byte_timeout());
        tokio::pin!(probe);
        tokio::select! {
            result = &mut probe => {display.failure=Some("response-probe"); result?},
            result = &mut req_pump => {
                req_result = Some(result);
                let response = probe.await;
                if response.is_err() {display.failure=Some("response-probe");}
                response?
            }
        }
    };
    let prefix_bytes = match probe_res {
        ResponseProbe::LooksHttp(prefix) => prefix,
        ResponseProbe::Eof => {
            return synth_phase2(
                &mut yw,
                &mut display,
                &ctx,
                &request,
                &target,
                resolved_addr,
                req_id,
                started,
                method,
                path,
                accept,
                "local-eof",
                "Backend closed connection without sending a response.",
                "Try restarting your dev server.",
                bytes_in.load(Ordering::Relaxed),
                "Backend closed connection without sending a response.",
            )
            .await;
        }
        ResponseProbe::NotHttp(_) => {
            return synth_phase2(
                &mut yw,
                &mut display,
                &ctx,
                &request,
                &target,
                resolved_addr,
                req_id,
                started,
                method,
                path,
                accept,
                "local-malformed",
                "Backend sent bytes that are not a valid HTTP response.",
                "Verify the local server is speaking HTTP/1.x.",
                bytes_in.load(Ordering::Relaxed),
                "Backend sent bytes that are not a valid HTTP response.",
            )
            .await;
        }
        ResponseProbe::Timeout => {
            return synth_phase2(
                &mut yw,
                &mut display,
                &ctx,
                &request,
                &target,
                resolved_addr,
                req_id,
                started,
                method,
                path,
                accept,
                "local-no-response",
                "Backend accepted the connection but did not send a response within the first-byte timeout.",
                "Local server is hung or slow on first response byte.",
                bytes_in.load(Ordering::Relaxed),
                "Backend accepted the connection but did not send a response within the first-byte timeout.",
            )
            .await;
        }
    };

    display.failure = Some("cancelled");
    display.source = crate::inspection::ResponseSource::Backend;
    if let Some(capture) = &observation {
        capture
            .lock()
            .update_record(|r| r.response_source = crate::inspection::ResponseSource::Backend);
        capture.lock().response(&prefix_bytes);
    }

    let resp_peek = Arc::new(Mutex::new({
        let mut b = BytesMut::with_capacity(1024);
        b.extend_from_slice(&prefix_bytes);
        b
    }));
    let bytes_out = Arc::new(AtomicU64::new(
        u64::try_from(prefix_bytes.len()).unwrap_or(0),
    ));

    let resp_peek_a = resp_peek.clone();
    let bout = bytes_out.clone();
    let response_observation = observation.clone();
    let resp_pump = async move {
        // Prefix replay can backpressure too; keep uploading while it writes.
        yw.write_all(&prefix_bytes).await?;
        let mut buf = [0u8; 8192];
        loop {
            let n = tr.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            {
                let mut p = resp_peek_a.lock();
                if p.len() < 1024 {
                    let take = (1024 - p.len()).min(n);
                    p.extend_from_slice(&buf[..take]);
                }
            }
            if let Some(capture) = &response_observation {
                capture.lock().response(&buf[..n]);
            }
            yw.write_all(&buf[..n]).await?;
            bout.fetch_add(u64::try_from(n).unwrap_or(u64::MAX), Ordering::Relaxed);
        }
        yw.shutdown().await?;
        Ok::<_, anyhow::Error>(())
    };

    if let Some(result) = req_result {
        let response = resp_pump.await;
        if response.is_err() {
            display.failure = Some("response-io");
        }
        response?;
        if result.is_err() {
            display.failure = Some("upload-io");
        }
        result?;
    } else {
        let request = async { req_pump.await.map_err(|e| ("upload-io", e)) };
        let response = async { resp_pump.await.map_err(|e| ("response-io", e)) };
        if let Err((kind, error)) = tokio::try_join!(request, response) {
            display.failure = Some(kind);
            return Err(error);
        }
    }
    display.failure = None;

    let resp_buf = resp_peek.lock().to_vec();
    // Live Host policy uses only its independent bounded prefix evidence.
    // A display observer may know a later final status, but cannot reinterpret
    // an informational prefix as evidence about that later response.
    let forwarding_status = final_response_status_prefix(&resp_buf);
    let status = observation.as_ref().map_or(forwarding_status, |capture| {
        let mut capture = capture.lock();
        capture.finish();
        capture.final_status().or(forwarding_status)
    });

    // Evidence-driven dev-server host smoothing: on a recognized host block in
    // `Auto` mode, flip on Host rewrite for subsequent requests and notify once.
    if matches!(ctx.host_header, crate::host_header::HostHeader::Auto)
        && !ctx.rewrite_active.load(Ordering::Relaxed)
    {
        if let Some(fw) = crate::block_detect::detect(forwarding_status, &resp_buf) {
            ctx.rewrite_active.store(true, Ordering::Relaxed);
            if !ctx.warned.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "! dev server blocked host '{}' (looks like {fw}).\n  \
                     auto-rewriting Host -> {} for this tunnel.\n  \
                     override: --host-header=preserve  (or =<host>)",
                    ctx.host_public, resolved_addr,
                );
            }
        }
    }

    emit_log(
        &ctx,
        LogLine {
            req_id,
            visitor_ip: request.visitor().map(|v| v.visitor_ip),
            peer_ip: request.visitor().map(|v| v.peer_ip),
            visitor_ip_source: request.visitor().map(|v| v.source),
            timestamp: std::time::SystemTime::now(),
            method,
            path,
            display_target: target.display(),
            resolved_addr: Some(resolved_addr),
            status,
            duration_ms: ms_since(started),
            bytes_in: bytes_in.load(Ordering::Relaxed),
            bytes_out: bytes_out.load(Ordering::Relaxed),
            failure_kind: None,
        },
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn synth_phase2<W: AsyncWrite + Unpin>(
    substream: &mut W,
    display: &mut ForwardDisplay<'_>,
    ctx: &ForwardCtx,
    request: &RequestContext,
    target: &Target,
    resolved_addr: std::net::SocketAddr,
    req_id: ulid::Ulid,
    started: Instant,
    method: String,
    path: String,
    accept: Accept,
    kind: &'static str,
    reason: &str,
    hint: &str,
    bytes_in: u64,
    log_reason_for_warn: &str,
) -> anyhow::Result<()> {
    if ctx.inspection.is_some() {
        warn!(%kind, "phase-2 failure → synth");
    } else {
        warn!(error = %log_reason_for_warn, %kind, "phase-2 failure → synth");
    }
    display.failure = Some(kind);
    let synth = synth_response_bytes(&SynthInput {
        kind,
        reason,
        hint,
        tunnel: &ctx.tunnel,
        display_target: &target.display(),
        accept,
        req_id,
        version: ctx.version,
    });
    display.synth(&synth);
    substream.write_all(&synth).await?;
    substream.shutdown().await.ok();
    emit_log(
        ctx,
        LogLine {
            req_id,
            visitor_ip: request.visitor().map(|v| v.visitor_ip),
            peer_ip: request.visitor().map(|v| v.peer_ip),
            visitor_ip_source: request.visitor().map(|v| v.source),
            timestamp: std::time::SystemTime::now(),
            method,
            path,
            display_target: target.display(),
            resolved_addr: Some(resolved_addr),
            status: Some(502),
            duration_ms: ms_since(started),
            bytes_in,
            bytes_out: u64::try_from(synth.len()).unwrap_or(0),
            failure_kind: Some(kind.to_string()),
        },
    );
    Ok(())
}

fn parse_head_for_synth(head: &[u8]) -> (String, String, Accept) {
    let parsed = self::peek::parse_request_line(head);
    let (method, path, headers) = parsed.unwrap_or_else(|| ("?".into(), "?".into(), None));
    let (accept_h, ua_h) = headers.map_or((None, None), |(a, u)| (Some(a), Some(u)));
    let accept = parse_accept(accept_h.as_deref(), ua_h.as_deref());
    (method, path, accept)
}

/// Independent status fallback for Host smoothing when observation rejects a
/// head. It inspects only bounded HTTP lines, never binary entity bytes, and
/// skips complete informational heads rather than reporting them as final.
fn final_response_status_prefix(mut prefix: &[u8]) -> Option<u16> {
    for informational in 0..=8 {
        let line_end = prefix.windows(2).position(|w| w == b"\r\n")?;
        let line = &prefix[..line_end];
        if line.len() < 12
            || !(line.starts_with(b"HTTP/1.1 ") || line.starts_with(b"HTTP/1.0 "))
            || !line[9..12].iter().all(u8::is_ascii_digit)
            || line.get(12).is_some_and(|byte| *byte != b' ')
        {
            return None;
        }
        let status = line[9..12]
            .iter()
            .fold(0u16, |n, byte| n * 10 + u16::from(byte - b'0'));
        if !(100..=599).contains(&status) {
            return None;
        }
        if !(100..200).contains(&status) || status == 101 {
            return Some(status);
        }
        if informational == 8 {
            return None;
        }
        let head_end = prefix.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
        prefix = &prefix[head_end..];
    }
    None
}

fn ms_since(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn emit_log(ctx: &ForwardCtx, mut line: LogLine) {
    if ctx.inspection.is_some() {
        line.path = crate::inspection::safe_target(&line.path);
        line.method = crate::inspection::safe_method(&line.method);
    }
    if let Some(tx) = &ctx.log_tx {
        if tx.try_send(line).is_err() {
            warn!("inspector channel full or closed; dropping log line");
        }
    }
}

// Counts only accepted write results, including partial writes before errors.
// It neither observes payloads nor claims receipt by the remote application.
struct CountedIo<T> {
    inner: T,
    counter: Option<Arc<AtomicU64>>,
}
impl<T: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for CountedIo<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for CountedIo<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_write(cx, bytes);
        if let std::task::Poll::Ready(Ok(n)) = &result {
            if let Some(counter) = &this.counter {
                let n = u64::try_from(*n).unwrap_or(u64::MAX);
                let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    Some(value.saturating_add(n))
                });
            }
        }
        result
    }
    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}
struct ForwardDisplay<'a> {
    ctx: &'a ForwardCtx,
    target: &'a Target,
    metadata: Option<RequestMetadataV1>,
    capture: Option<Arc<Mutex<crate::inspection::Capture>>>,
    attempted: bool,
    counters: Option<crate::inspection::TransportCounters>,
    started: Instant,
    wall: u64,
    resolved: Option<std::net::SocketAddr>,
    failure: Option<&'static str>,
    source: crate::inspection::ResponseSource,
}
impl<'a> ForwardDisplay<'a> {
    fn new(ctx: &'a ForwardCtx, target: &'a Target, started: Instant) -> Self {
        Self {
            ctx,
            target,
            metadata: None,
            capture: None,
            attempted: false,
            counters: None,
            started,
            wall: crate::inspection::wall_ms(),
            resolved: None,
            failure: Some("cancelled"),
            source: crate::inspection::ResponseSource::Unavailable,
        }
    }
    #[cfg(test)]
    const fn owns_allocated_state(&self) -> bool {
        self.counters.is_some() || self.capture.is_some()
    }
    fn configure(&mut self, capture: &crate::inspection::Capture) {
        // Capture owns the active admission permit before any counter or label
        // allocation. A declined/closed attempt retains only borrowed context.
        self.counters = Some(crate::inspection::TransportCounters {
            to_backend: Arc::new(AtomicU64::new(0)),
            to_client: Arc::new(AtomicU64::new(0)),
        });
        capture.update_record(|r| {
            r.started = self.started;
            r.started_at_ms = self.wall;
            r.tunnel = crate::inspection::safe_tunnel(&self.ctx.tunnel);
            r.display_target = self.target.display();
            r.resolved_addr = self.resolved;
            r.counters.clone_from(&self.counters);
        });
    }
    fn preconnect(&mut self, head: Option<&[u8]>) {
        if self.attempted {
            return;
        }
        self.attempted = true;
        if let Some((inspection, generation)) = &self.ctx.inspection {
            if let Some(mut capture) =
                inspection.admit_preconnect(*generation, self.metadata.as_ref())
            {
                self.configure(&capture);
                if let Some(head) = head {
                    capture.request(head);
                }
                self.capture = Some(Arc::new(Mutex::new(capture)));
            }
        }
    }
    fn synth(&mut self, bytes: &[u8]) {
        self.source = crate::inspection::ResponseSource::ClientSynth;
        if let Some(capture) = &self.capture {
            capture.lock().client_synth(bytes);
        }
    }
}
impl Drop for ForwardDisplay<'_> {
    fn drop(&mut self) {
        self.preconnect(None);
        if let Some(capture) = &self.capture {
            let mut capture = capture.lock();
            if let Some(kind) = self.failure {
                capture.fail(kind);
            }
            capture.update_record(|r| {
                r.response_source = self.source;
                if self.source == crate::inspection::ResponseSource::Unavailable
                    && r.response.headers.is_empty()
                {
                    r.response.headers_state = crate::inspection::HeadersState::Unavailable;
                }
                if r.request_boundary == crate::inspection::RequestBoundary::PreconnectUnavailable {
                    r.omit("preconnect-unavailable");
                    if r.request.headers.is_empty() {
                        r.request.headers_state = crate::inspection::HeadersState::Unavailable;
                    }
                }
                if let Some(counters) = &self.counters {
                    r.bytes_to_backend = counters.to_backend.load(Ordering::Relaxed);
                    r.bytes_to_client = counters.to_client.load(Ordering::Relaxed);
                    r.bytes_from_backend =
                        if self.source == crate::inspection::ResponseSource::Backend {
                            r.bytes_to_client
                        } else {
                            0
                        };
                }
                r.counters = None;
            });
        }
    }
}

#[cfg(test)]
mod transport_count_tests {
    use super::*;
    struct PartialWriter(bool);
    impl AsyncWrite for PartialWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            if self.0 {
                std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
            } else {
                self.0 = true;
                std::task::Poll::Ready(Ok(bytes.len().min(3)))
            }
        }
        fn poll_flush(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }
    #[tokio::test]
    async fn partial_write_then_error_counts_only_accepted_bytes_and_saturates() {
        for initial in [0, u64::MAX - 1] {
            let counter = Arc::new(AtomicU64::new(initial));
            let mut writer = CountedIo {
                inner: PartialWriter(false),
                counter: Some(counter.clone()),
            };
            assert!(writer.write_all(b"SECRET_PAYLOAD").await.is_err());
            assert_eq!(counter.load(Ordering::Relaxed), initial.saturating_add(3));
        }
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use crate::inspection::{Inspection, PreviewMode, TunnelGeneration};
    #[tokio::test]
    async fn declined_or_closed_long_lived_forward_retains_no_inspection_allocations() {
        for closed in [false, true] {
            let history = Inspection::new(PreviewMode::Json);
            let generation = TunnelGeneration::new();
            let held: Vec<_> = (0..64)
                .map(|_| history.admit(generation, None).unwrap())
                .collect();
            if closed {
                history.shutdown();
            }
            let mut ctx = ForwardCtx::new(
                "a-tunnel-label-that-must-not-be-copied".into(),
                None,
                env!("CARGO_PKG_VERSION"),
            );
            ctx.inspection = Some((history.clone(), generation));
            ctx.inspection_ownership_check = Some(|owned| {
                assert!(
                    !owned,
                    "declined inspection retained owned counters/strings"
                );
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let target = Target::Explicit(listener.local_addr().unwrap());
            let (mut visitor, stream) = tokio::io::duplex(4096);
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            let response = b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nLIVE";
            let backend = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                socket.read_to_end(&mut request).await.unwrap();
                assert_eq!(request, b"GET /live HTTP/1.1\r\nHost: local\r\n\r\n");
                socket.write_all(response).await.unwrap();
                release_rx.await.unwrap();
                socket.write_all(b"DONE").await.unwrap();
            };
            let client = async {
                visitor
                    .write_all(b"GET /live HTTP/1.1\r\nHost: local\r\n\r\n")
                    .await
                    .unwrap();
                visitor.shutdown().await.unwrap();
                let mut prefix = vec![0; response.len()];
                visitor.read_exact(&mut prefix).await.unwrap();
                assert_eq!(prefix, response);
                tokio::time::sleep(Duration::from_millis(20)).await;
                assert_eq!(history.search("").len(), if closed { 0 } else { 64 });
                release_tx.send(()).unwrap();
                let mut tail = Vec::new();
                visitor.read_to_end(&mut tail).await.unwrap();
                assert_eq!(tail, b"DONE");
            };
            let (result, (), ()) = tokio::time::timeout(
                Duration::from_secs(3),
                Box::pin(async {
                    tokio::join!(forward(Box::pin(stream), target, ctx), backend, client)
                }),
            )
            .await
            .unwrap();
            result.unwrap();
            drop(held);
            if !closed {
                let permits: Vec<_> = (0..64)
                    .map(|_| history.admit(generation, None).unwrap())
                    .collect();
                assert!(history.admit(generation, None).is_none());
                drop(permits);
            }
        }
    }
}

#[cfg(test)]
mod controlled_connect_tests {
    use super::*;
    use crate::inspection::{
        CaptureContext, ConsumerFactory, Inspection, ObservationConsumer, PreviewMode,
        TunnelGeneration,
    };
    struct NeverRaw;
    impl ConsumerFactory for NeverRaw {
        fn admit(&self, _: CaptureContext) -> Option<Box<dyn ObservationConsumer>> {
            panic!("preconnect must not invoke raw factory")
        }
    }
    #[tokio::test]
    async fn resolver_and_connect_deadline_failures_publish_sanitized_preconnect_records() {
        let failures: [fn() -> crate::connect::ConnectError; 2] = [
            || crate::connect::ConnectError::DnsFailed(std::io::Error::other("RESOLVER_CANARY")),
            || crate::connect::ConnectError::Timeout(LOCAL_CONNECT_TIMEOUT),
        ];
        for failure in failures {
            let expected = failure().to_kind();
            let history = Inspection::with_consumer(PreviewMode::Json, Some(Arc::new(NeverRaw)));
            let gen = TunnelGeneration::new();
            let mut ctx = ForwardCtx::new("fixture".into(), None, env!("CARGO_PKG_VERSION"));
            ctx.inspection = Some((history.clone(), gen));
            ctx.connect_failure = Some(failure);
            let (mut visitor, stream) = tokio::io::duplex(8192);
            let client = async {
                visitor.write_all(b"GET /?token=QUERY_CANARY HTTP/1.1\r\nHost: local\r\nAuthorization: HEADER_CANARY\r\n\r\n").await.unwrap();
                visitor.shutdown().await.unwrap();
                let mut response = Vec::new();
                visitor.read_to_end(&mut response).await.unwrap();
                assert!(response.starts_with(b"HTTP/1.1 502"));
            };
            let (result, ()) = tokio::join!(
                forward(Box::pin(stream), Target::LocalhostPort(1), ctx),
                client
            );
            result.unwrap();
            let records = history.search("");
            assert_eq!(records.len(), 1);
            let record = &records[0];
            assert_eq!(record.failure_kind, Some(expected));
            assert_eq!(
                record.request_boundary,
                crate::inspection::RequestBoundary::PreconnectUnavailable
            );
            assert!(record.resolved_addr.is_none());
            assert_eq!(record.bytes_to_backend, 0);
            assert!(!format!("{record:?}").contains("CANARY"));
            assert!(!serde_json::to_string(record).unwrap().contains("CANARY"));
        }
    }
}
