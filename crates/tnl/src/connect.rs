//! Low-level local connect helper.
//!
//! Dual-stack `lookup_host("localhost", port)` for the port-only target,
//! single-address connect for explicit targets, with `LOCAL_CONNECT_TIMEOUT`
//! ceiling and structured `ConnectError`.

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::{lookup_host, TcpStream};
use tokio::time::{timeout, Instant};

use crate::target::Target;

pub const LOCAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(thiserror::Error, Debug)]
pub enum ConnectError {
    #[error("connection refused on all resolved addresses")]
    Refused,
    #[error("connect timed out after {0:?}")]
    Timeout(Duration),
    #[error("network unreachable")]
    Unreachable,
    #[error("DNS lookup failed: {0}")]
    DnsFailed(#[source] io::Error),
    #[error("DNS returned no addresses")]
    DnsEmpty,
}

impl ConnectError {
    pub const fn to_kind(&self) -> &'static str {
        match self {
            Self::Refused => "connect-refused",
            Self::Timeout(_) => "connect-timeout",
            Self::Unreachable => "connect-unreachable",
            Self::DnsFailed(_) | Self::DnsEmpty => "dns-failed",
        }
    }

    pub const fn hint(&self) -> &'static str {
        match self {
            Self::Refused => "Is your dev server running? For Vite/uvicorn try --host 127.0.0.1.",
            Self::Timeout(_) => "Server might be firewalled or unresponsive on the target address.",
            Self::Unreachable => "Network stack unreachable; check the target is correct.",
            Self::DnsFailed(_) | Self::DnsEmpty => {
                "DNS resolution failed; if target is custom, verify hostname."
            }
        }
    }
}

/// Resolve the target into one or more candidate `SocketAddr`s.
///
/// For `LocalhostPort` we use `lookup_host(("localhost", port))` so that
/// `/etc/hosts` dictates v4/v6 order; for `Explicit` we return a single
/// address.
pub async fn resolve_target(target: &Target) -> Result<Vec<SocketAddr>, ConnectError> {
    match target {
        Target::Explicit(addr) => Ok(vec![*addr]),
        Target::LocalhostPort(port) => {
            let addrs: Vec<SocketAddr> = lookup_host(("localhost", *port))
                .await
                .map_err(ConnectError::DnsFailed)?
                .collect();
            if addrs.is_empty() {
                Err(ConnectError::DnsEmpty)
            } else {
                Ok(addrs)
            }
        }
    }
}

/// Try to connect to each candidate in turn; return the first success or
/// classify the last error. Returns the connected TCP stream plus the address
/// that won.
pub async fn connect_local(
    target: &Target,
    deadline: Duration,
) -> Result<(TcpStream, SocketAddr), ConnectError> {
    let addrs = resolve_target(target).await?;
    let start = Instant::now();
    let mut last_err: Option<ConnectError> = None;

    for addr in addrs {
        let remaining = deadline
            .checked_sub(start.elapsed())
            .unwrap_or_else(|| Duration::from_millis(0));
        if remaining.is_zero() {
            return Err(ConnectError::Timeout(deadline));
        }
        match timeout(remaining, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => {
                let _ = stream.set_nodelay(true);
                return Ok((stream, addr));
            }
            Ok(Err(e)) => {
                last_err = Some(classify_io(&e));
            }
            Err(_elapsed) => {
                last_err = Some(ConnectError::Timeout(deadline));
            }
        }
    }
    Err(last_err.unwrap_or(ConnectError::Refused))
}

fn classify_io(e: &io::Error) -> ConnectError {
    use io::ErrorKind as K;
    match e.kind() {
        K::ConnectionRefused => ConnectError::Refused,
        K::TimedOut => ConnectError::Timeout(LOCAL_CONNECT_TIMEOUT),
        K::NetworkUnreachable | K::HostUnreachable => ConnectError::Unreachable,
        _ => {
            // AF not supported (e.g. [::1] when v6 disabled) shows as InvalidInput on Linux.
            // Treat as unreachable; the next candidate may succeed.
            ConnectError::Unreachable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_strings_stable() {
        let cases: [(ConnectError, &str); 5] = [
            (ConnectError::Refused, "connect-refused"),
            (
                ConnectError::Timeout(LOCAL_CONNECT_TIMEOUT),
                "connect-timeout",
            ),
            (ConnectError::Unreachable, "connect-unreachable"),
            (ConnectError::DnsEmpty, "dns-failed"),
            (ConnectError::DnsFailed(io::Error::other("x")), "dns-failed"),
        ];
        for (e, want) in cases {
            assert_eq!(e.to_kind(), want);
        }
    }

    #[tokio::test]
    async fn connect_refused_when_no_listener() {
        // OS-assigned ephemeral port that we immediately drop.
        let tmp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = tmp.local_addr().unwrap().port();
        drop(tmp);
        let t = Target::LocalhostPort(port);
        // Small deadline; with no listener Linux returns ECONNREFUSED instantly.
        let err = connect_local(&t, Duration::from_millis(500))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ConnectError::Refused | ConnectError::Unreachable),
            "got {err:?}"
        );
    }
}
