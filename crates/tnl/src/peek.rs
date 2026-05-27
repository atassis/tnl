//! Pre-buffered peek helpers used by the forwarder's two-phase pipeline.
//!
//! Unlike the old tee-style peek (kept for the response side via the old
//! `forwarder::peek` module), these helpers read into a buffer **before** any
//! action is taken on the backend side, so the forwarder can synthesise a
//! response if local connect fails.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::time::timeout;

pub const REQUEST_HEAD_PROBE_MAX: usize = 8 * 1024;
pub const RESPONSE_PROBE_BYTES: usize = 9;

#[derive(Debug)]
pub enum RequestHeadProbe {
    /// Found `\r\n\r\n`. `head` ends with the blank line; `leftover` holds
    /// post-blank-line bytes already consumed.
    Complete { head: Vec<u8>, leftover: Vec<u8> },
    /// Buffer filled to `max` before any blank line; treat as malformed.
    Capped(Vec<u8>),
    /// EOF before blank line; partial head buffered.
    Eof(Vec<u8>),
    /// Wall-clock timeout elapsed before blank line.
    Timeout(Vec<u8>),
}

pub async fn peek_request_head<R>(
    src: &mut R,
    max: usize,
    deadline: Duration,
) -> std::io::Result<RequestHeadProbe>
where
    R: AsyncRead + Unpin,
{
    // `buf` is moved into the async block; on timeout we cannot reclaim it, so
    // `Timeout` carries an empty vec — callers treat it as fatal.
    let mut buf: Vec<u8> = Vec::with_capacity(1024.min(max));
    let mut tmp = [0u8; 1024];
    let inner = async move {
        loop {
            let remaining_cap = max.saturating_sub(buf.len());
            if remaining_cap == 0 {
                return Ok::<RequestHeadProbe, std::io::Error>(RequestHeadProbe::Capped(buf));
            }
            let to_read = tmp.len().min(remaining_cap);
            let n = src.read(&mut tmp[..to_read]).await?;
            if n == 0 {
                return Ok(RequestHeadProbe::Eof(buf));
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(i) = find_blank_line(&buf) {
                let head_end = i + 4; // include "\r\n\r\n"
                let head = buf[..head_end].to_vec();
                let leftover = buf[head_end..].to_vec();
                return Ok(RequestHeadProbe::Complete { head, leftover });
            }
        }
    };
    // buf was moved into `inner` and cannot be recovered after a timeout;
    // yield an empty partial-bytes vec — callers treat Timeout as fatal.
    timeout(deadline, inner)
        .await
        .unwrap_or(Ok(RequestHeadProbe::Timeout(Vec::new())))
}

#[derive(Debug)]
pub enum ResponseProbe {
    /// First bytes look like an HTTP status line; forward through unchanged.
    /// Carries the prefix already consumed.
    LooksHttp(Vec<u8>),
    /// Peer closed without sending anything.
    Eof,
    /// Read returned bytes but they do not start with `HTTP/1.` or `HTTP/2`.
    /// Carries the prefix for hex-preview / diagnostics.
    NotHttp(Vec<u8>),
    /// Timeout elapsed without any bytes.
    Timeout,
}

pub async fn peek_response_first_line<R>(
    src: &mut R,
    timeout_dur: Duration,
) -> std::io::Result<ResponseProbe>
where
    R: AsyncRead + Unpin,
{
    let mut buf = [0u8; RESPONSE_PROBE_BYTES];
    let mut filled = 0usize;

    let inner = async {
        while filled < RESPONSE_PROBE_BYTES {
            let n = src.read(&mut buf[filled..]).await?;
            if n == 0 {
                return Ok::<ResponseProbe, std::io::Error>(if filled == 0 {
                    ResponseProbe::Eof
                } else if looks_http(&buf[..filled]) {
                    ResponseProbe::LooksHttp(buf[..filled].to_vec())
                } else {
                    ResponseProbe::NotHttp(buf[..filled].to_vec())
                });
            }
            filled += n;
        }
        Ok(if looks_http(&buf[..filled]) {
            ResponseProbe::LooksHttp(buf[..filled].to_vec())
        } else {
            ResponseProbe::NotHttp(buf[..filled].to_vec())
        })
    };
    timeout(timeout_dur, inner)
        .await
        .map_or(Ok(ResponseProbe::Timeout), |r| r)
}

fn find_blank_line(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn looks_http(buf: &[u8]) -> bool {
    buf.starts_with(b"HTTP/1.") || buf.starts_with(b"HTTP/2")
}
