use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use bytes::BytesMut;
use parking_lot::Mutex;
use tnl_protocol::Stream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::debug;

use crate::inspector::LogLine;

pub mod peek {
    /// Parse the first HTTP/1.x request line `METHOD PATH HTTP/x.y`.
    /// Returns `None` if any of the three tokens is missing (i.e. the line is
    /// incomplete or not a well-formed HTTP request line).
    pub fn parse_request_line(bytes: &[u8]) -> Option<(String, String)> {
        let s = std::str::from_utf8(bytes).ok()?;
        let first_line = s.lines().next()?;
        let mut parts = first_line.split_whitespace();
        let method = parts.next()?.to_string();
        let path = parts.next()?.to_string();
        parts.next()?; // require HTTP/x.y — signals a complete request line
        Some((method, path))
    }

    /// Parse the HTTP/1.x status line `HTTP/x.y CODE REASON`. Returns the
    /// numeric status code, or `None` if the line is not parseable.
    pub fn parse_response_status(bytes: &[u8]) -> Option<u16> {
        let s = std::str::from_utf8(bytes).ok()?;
        let first_line = s.lines().next()?;
        let mut parts = first_line.split_whitespace();
        let _http = parts.next()?;
        parts.next()?.parse().ok()
    }
}

/// Plain bidirectional copy; no inspection. Kept for the no-inspector path
/// (existing `accept_loop` calls).
pub async fn forward(mut stream: Pin<Box<dyn Stream>>, port: u16) -> anyhow::Result<()> {
    let mut tcp = TcpStream::connect(("127.0.0.1", port))
        .await
        .with_context(|| format!("connect 127.0.0.1:{port}"))?;
    tcp.set_nodelay(true)?;
    let (a, b) = tokio::io::copy_bidirectional(&mut *stream, &mut tcp).await?;
    debug!(sent_to_local = a, sent_from_local = b, "stream closed");
    Ok(())
}

/// Bidirectional copy with request/response peeking.
///
/// Tees up to 4 KiB of the request and 512 B of the response into peek
/// buffers, then emits a [`LogLine`] to `log_tx` when the stream closes.
/// Caller is responsible for spawning this per substream.
pub async fn forward_with_inspection(
    stream: Pin<Box<dyn Stream>>,
    port: u16,
    log_tx: mpsc::Sender<LogLine>,
) -> anyhow::Result<()> {
    let tcp = TcpStream::connect(("127.0.0.1", port))
        .await
        .with_context(|| format!("connect 127.0.0.1:{port}"))?;
    tcp.set_nodelay(true)?;

    let started = Instant::now();
    let (mut yr, mut yw) = tokio::io::split(stream);
    let (mut tr, mut tw) = tokio::io::split(tcp);

    let req_peek = Arc::new(Mutex::new(BytesMut::with_capacity(4096)));
    let resp_peek = Arc::new(Mutex::new(BytesMut::with_capacity(512)));
    let bytes_in = Arc::new(AtomicU64::new(0));
    let bytes_out = Arc::new(AtomicU64::new(0));

    let req_peek_a = req_peek.clone();
    let bin = bytes_in.clone();
    let fwd_req = async move {
        let mut buf = [0u8; 8192];
        loop {
            let n = yr.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            {
                let mut p = req_peek_a.lock();
                if p.len() < 4096 {
                    let take = (4096 - p.len()).min(n);
                    p.extend_from_slice(&buf[..take]);
                }
            }
            tw.write_all(&buf[..n]).await?;
            bin.fetch_add(u64::try_from(n).unwrap_or(u64::MAX), Ordering::Relaxed);
        }
        tw.shutdown().await?;
        Ok::<_, anyhow::Error>(())
    };

    let resp_peek_a = resp_peek.clone();
    let bout = bytes_out.clone();
    let fwd_resp = async move {
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

    tokio::try_join!(fwd_req, fwd_resp)?;

    let head = req_peek.lock().to_vec();
    let resp = resp_peek.lock().to_vec();
    let (method, path) =
        peek::parse_request_line(&head).unwrap_or_else(|| ("?".into(), "?".into()));
    let status = peek::parse_response_status(&resp);

    let _ = log_tx
        .send(LogLine {
            timestamp: std::time::SystemTime::now(),
            method,
            path,
            status,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            bytes_in: bytes_in.load(Ordering::Relaxed),
            bytes_out: bytes_out.load(Ordering::Relaxed),
            remote_ip: None,
        })
        .await;

    Ok(())
}
