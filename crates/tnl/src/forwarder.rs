//! Two-phase forwarder.
//!
//! Phase 1: pre-buffer the request head from the substream and attempt a
//! local connect. On any connect failure, synthesise a complete HTTP/1.1 502
//! onto the substream and close.
//!
//! Phase 2: write the buffered head + leftover body to the backend, then
//! probe the first response bytes. If the backend sends a malformed prefix
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
use tnl_protocol::Stream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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

/// Context carried through one forwarded substream.
#[derive(Clone)]
pub struct ForwardCtx {
    pub tunnel: String,
    pub log_tx: Option<mpsc::Sender<LogLine>>,
    /// Crate version string used in error bodies (`env!("CARGO_PKG_VERSION")`).
    pub version: &'static str,
}

impl std::fmt::Debug for ForwardCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForwardCtx")
            .field("tunnel", &self.tunnel)
            .field("log_tx", &self.log_tx.as_ref().map(|_| "<sender>"))
            .field("version", &self.version)
            .finish()
    }
}

/// Re-exported peek helpers (kept for backwards compatibility with tests that
/// imported `tnl::forwarder::peek::parse_request_line` / `parse_response_status`).
pub mod peek {
    pub use crate::peek::{peek_request_head, peek_response_first_line};

    /// Parse the first HTTP/1.x request line `METHOD PATH HTTP/x.y` plus
    /// optional `Accept` and `User-Agent` headers.
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

/// Single entry point for the forwarder. Returns `Ok(())` even when a synth
/// response was written for a local-side failure; returns `Err` only for
/// unrecoverable I/O on the substream itself.
#[allow(clippy::too_many_lines)]
pub async fn forward(
    mut substream: Pin<Box<dyn Stream>>,
    target: Target,
    ctx: ForwardCtx,
) -> anyhow::Result<()> {
    let req_id = ulid::Ulid::new();
    let started = Instant::now();

    // Phase 1: peek request head.
    let probe = peek_request_head(
        &mut substream,
        REQUEST_HEAD_PROBE_MAX,
        Duration::from_secs(5),
    )
    .await?;
    let (head_bytes, leftover) = match probe {
        RequestHeadProbe::Complete { head, leftover } => (head, leftover),
        RequestHeadProbe::Capped(buf)
        | RequestHeadProbe::Eof(buf)
        | RequestHeadProbe::Timeout(buf) => (buf, Vec::new()),
    };
    let (method, path, accept) = parse_head_for_synth(&head_bytes);
    debug!(%method, %path, "request head parsed");

    // Phase 1: connect.
    let connect_outcome = connect_local(&target, LOCAL_CONNECT_TIMEOUT).await;
    let (mut tcp, resolved_addr) = match connect_outcome {
        Ok((s, a)) => (s, a),
        Err(e) => {
            warn!(error = %e, display = %target.display(), "local connect failed");
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
            substream.write_all(&synth).await?;
            substream.shutdown().await.ok();
            emit_log(
                &ctx,
                LogLine {
                    req_id,
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
            )
            .await;
            return Ok(());
        }
    };

    // Phase 2: write head + leftover body to backend.
    if let Err(e) = tcp.write_all(&head_bytes).await {
        return synth_phase2(
            &mut substream,
            &ctx,
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
            head_bytes.len() + leftover.len(),
            &format!("write to backend failed: {e}"),
        )
        .await;
    }
    if !leftover.is_empty() {
        if let Err(e) = tcp.write_all(&leftover).await {
            return synth_phase2(
                &mut substream,
                &ctx,
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
                head_bytes.len() + leftover.len(),
                &format!("write body to backend failed: {e}"),
            )
            .await;
        }
    }

    // Phase 2: probe response first line.
    let probe_res = peek_response_first_line(&mut tcp, LOCAL_FIRST_BYTE_TIMEOUT).await?;
    let prefix_bytes = match probe_res {
        ResponseProbe::LooksHttp(prefix) => prefix,
        ResponseProbe::Eof => {
            return synth_phase2(
                &mut substream,
                &ctx,
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
                head_bytes.len() + leftover.len(),
                "Backend closed connection without sending a response.",
            )
            .await;
        }
        ResponseProbe::NotHttp(_) => {
            return synth_phase2(
                &mut substream,
                &ctx,
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
                head_bytes.len() + leftover.len(),
                "Backend sent bytes that are not a valid HTTP response.",
            )
            .await;
        }
        ResponseProbe::Timeout => {
            return synth_phase2(
                &mut substream,
                &ctx,
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
                head_bytes.len() + leftover.len(),
                "Backend accepted the connection but did not send a response within the first-byte timeout.",
            )
            .await;
        }
    };

    // Success path: bidirectional pump.
    let (mut yr, mut yw) = tokio::io::split(substream);
    let (mut tr, mut tw) = tokio::io::split(tcp);

    let resp_peek = Arc::new(Mutex::new({
        let mut b = BytesMut::with_capacity(512);
        b.extend_from_slice(&prefix_bytes);
        b
    }));
    let bytes_in = Arc::new(AtomicU64::new(
        u64::try_from(head_bytes.len() + leftover.len()).unwrap_or(0),
    ));
    let bytes_out = Arc::new(AtomicU64::new(
        u64::try_from(prefix_bytes.len()).unwrap_or(0),
    ));

    // Replay the prefix bytes already consumed from the backend into the substream.
    yw.write_all(&prefix_bytes).await?;

    let bin = bytes_in.clone();
    let req_pump = async move {
        let mut buf = [0u8; 8192];
        loop {
            let n = yr.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            tw.write_all(&buf[..n]).await?;
            bin.fetch_add(u64::try_from(n).unwrap_or(u64::MAX), Ordering::Relaxed);
        }
        tw.shutdown().await?;
        Ok::<_, anyhow::Error>(())
    };

    let resp_peek_a = resp_peek.clone();
    let bout = bytes_out.clone();
    let resp_pump = async move {
        let mut buf = [0u8; 8192];
        loop {
            let n = tr.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            {
                let mut p = resp_peek_a.lock();
                if p.len() < 512 {
                    let take = (512 - p.len()).min(n);
                    p.extend_from_slice(&buf[..take]);
                }
            }
            yw.write_all(&buf[..n]).await?;
            bout.fetch_add(u64::try_from(n).unwrap_or(u64::MAX), Ordering::Relaxed);
        }
        yw.shutdown().await?;
        Ok::<_, anyhow::Error>(())
    };

    tokio::try_join!(req_pump, resp_pump)?;

    let resp_buf = resp_peek.lock().to_vec();
    let status = self::peek::parse_response_status(&resp_buf);
    emit_log(
        &ctx,
        LogLine {
            req_id,
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
    )
    .await;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn synth_phase2(
    substream: &mut Pin<Box<dyn Stream>>,
    ctx: &ForwardCtx,
    target: &Target,
    resolved_addr: std::net::SocketAddr,
    req_id: ulid::Ulid,
    started: Instant,
    method: String,
    path: String,
    accept: Accept,
    kind: &str,
    reason: &str,
    hint: &str,
    bytes_in: usize,
    log_reason_for_warn: &str,
) -> anyhow::Result<()> {
    warn!(error = %log_reason_for_warn, %kind, "phase-2 failure → synth");
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
    substream.write_all(&synth).await?;
    substream.shutdown().await.ok();
    emit_log(
        ctx,
        LogLine {
            req_id,
            timestamp: std::time::SystemTime::now(),
            method,
            path,
            display_target: target.display(),
            resolved_addr: Some(resolved_addr),
            status: Some(502),
            duration_ms: ms_since(started),
            bytes_in: u64::try_from(bytes_in).unwrap_or(0),
            bytes_out: u64::try_from(synth.len()).unwrap_or(0),
            failure_kind: Some(kind.to_string()),
        },
    )
    .await;
    Ok(())
}

fn parse_head_for_synth(head: &[u8]) -> (String, String, Accept) {
    let parsed = self::peek::parse_request_line(head);
    let (method, path, headers) = parsed.unwrap_or_else(|| ("?".into(), "?".into(), None));
    let (accept_h, ua_h) = headers
        .map(|(a, u)| (Some(a), Some(u)))
        .unwrap_or((None, None));
    let accept = parse_accept(accept_h.as_deref(), ua_h.as_deref());
    (method, path, accept)
}

fn ms_since(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX)
}

async fn emit_log(ctx: &ForwardCtx, line: LogLine) {
    if let Some(tx) = &ctx.log_tx {
        if let Err(e) = tx.try_send(line) {
            warn!(error = ?e, "inspector channel full or closed; dropping log line");
        }
    }
}
