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
    connect_local_observed(target, deadline, |_| {}).await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectEvent {
    ResolveStarted,
    ResolveSkipped,
    ResolveCompleted,
    ResolveFailed,
    ConnectStarted,
    ConnectCompleted,
    ConnectFailed,
}

pub(crate) async fn connect_local_observed(
    target: &Target,
    deadline: Duration,
    observe: impl FnMut(ConnectEvent) + Send,
) -> Result<(TcpStream, SocketAddr), ConnectError> {
    connect_using(
        resolve_target(target),
        deadline,
        TcpStream::connect,
        matches!(target, Target::LocalhostPort(_)),
        observe,
    )
    .await
}

// The production resolver and dialer are unchanged; scoped test futures can
// drive failures/deadlines without hostname support or network blackholes.
async fn connect_using<R, D, F>(
    resolve: R,
    deadline: Duration,
    dial: D,
    resolve_required: bool,
    mut observe: impl FnMut(ConnectEvent) + Send,
) -> Result<(TcpStream, SocketAddr), ConnectError>
where
    R: std::future::Future<Output = Result<Vec<SocketAddr>, ConnectError>> + Send,
    D: Fn(SocketAddr) -> F + Send,
    F: std::future::Future<Output = io::Result<TcpStream>> + Send,
{
    observe(if resolve_required {
        ConnectEvent::ResolveStarted
    } else {
        ConnectEvent::ResolveSkipped
    });
    let addrs = resolve
        .await
        .inspect_err(|_| observe(ConnectEvent::ResolveFailed))?;
    if resolve_required {
        observe(ConnectEvent::ResolveCompleted);
    }
    observe(ConnectEvent::ConnectStarted);
    let result = async move {
        let start = Instant::now();
        let mut last_err: Option<ConnectError> = None;

        for addr in addrs {
            let remaining = deadline
                .checked_sub(start.elapsed())
                .unwrap_or_else(|| Duration::from_millis(0));
            if remaining.is_zero() {
                return Err(ConnectError::Timeout(deadline));
            }
            match timeout(remaining, dial(addr)).await {
                Ok(Ok(stream)) => {
                    let _ = stream.set_nodelay(true);
                    return Ok((stream, addr));
                }
                Ok(Err(e)) => {
                    last_err = Some(classify_io(&e, deadline));
                }
                Err(_elapsed) => {
                    last_err = Some(ConnectError::Timeout(deadline));
                }
            }
        }
        Err(last_err.unwrap_or(ConnectError::Refused))
    }
    .await;
    observe(if result.is_ok() {
        ConnectEvent::ConnectCompleted
    } else {
        ConnectEvent::ConnectFailed
    });
    result
}

fn classify_io(e: &io::Error, active_deadline: Duration) -> ConnectError {
    use io::ErrorKind as K;
    match e.kind() {
        K::ConnectionRefused => ConnectError::Refused,
        K::TimedOut => ConnectError::Timeout(active_deadline),
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

#[cfg(test)]
mod controlled_failures {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn resolver_error_never_dials_and_pending_connect_obeys_deadline() {
        let error = connect_using(
            async { Err(ConnectError::DnsFailed(io::Error::other("RESOLVER_CANARY"))) },
            LOCAL_CONNECT_TIMEOUT,
            |_| async { panic!("resolver failure must not dial") },
            true,
            |_| {},
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_kind(), "dns-failed");
        let started = Instant::now();
        let error = connect_using(
            async { Ok(vec!["127.0.0.1:1".parse().unwrap()]) },
            LOCAL_CONNECT_TIMEOUT,
            |_| std::future::pending::<io::Result<TcpStream>>(),
            false,
            |_| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            ConnectError::Timeout(LOCAL_CONNECT_TIMEOUT)
        ));
        assert_eq!(started.elapsed(), LOCAL_CONNECT_TIMEOUT);
    }
}

#[cfg(test)]
mod timing_tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn connect_timing_resolver_failure_never_starts_a_dial() {
        let mut events = Vec::new();
        let error = connect_using(
            async { Err(ConnectError::DnsEmpty) },
            LOCAL_CONNECT_TIMEOUT,
            |_| async { panic!("must not dial") },
            true,
            |event| events.push(event),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ConnectError::DnsEmpty));
        assert_eq!(
            events,
            [ConnectEvent::ResolveStarted, ConnectEvent::ResolveFailed]
        );
    }
    #[tokio::test(start_paused = true)]
    async fn connect_timing_resolution_stays_outside_aggregate_dial_deadline() {
        let mut events = Vec::new();
        let started = Instant::now();
        let error = connect_using(
            async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok(vec!["127.0.0.1:1".parse().unwrap()])
            },
            LOCAL_CONNECT_TIMEOUT,
            |_| std::future::pending::<io::Result<TcpStream>>(),
            true,
            |event| events.push(event),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            ConnectError::Timeout(LOCAL_CONNECT_TIMEOUT)
        ));
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(10) + LOCAL_CONNECT_TIMEOUT
        );
        assert_eq!(
            events,
            [
                ConnectEvent::ResolveStarted,
                ConnectEvent::ResolveCompleted,
                ConnectEvent::ConnectStarted,
                ConnectEvent::ConnectFailed
            ]
        );
    }
    #[tokio::test]
    async fn connect_timing_numeric_address_skips_resolution_and_fallback_is_aggregate() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (client, server) = tokio::join!(TcpStream::connect(address), listener.accept());
        let stream = parking_lot::Mutex::new(Some(client.unwrap()));
        let _server = server.unwrap();
        let first = "127.0.0.1:1".parse().unwrap();
        let second = "127.0.0.1:2".parse().unwrap();
        let attempted = parking_lot::Mutex::new(Vec::new());
        let mut events = Vec::new();
        let (_, winning) = connect_using(
            async { Ok(vec![first, second]) },
            LOCAL_CONNECT_TIMEOUT,
            |addr| {
                attempted.lock().push(addr);
                std::future::ready(if addr == first {
                    Err(io::ErrorKind::ConnectionRefused.into())
                } else {
                    Ok(stream.lock().take().unwrap())
                })
            },
            false,
            |event| events.push(event),
        )
        .await
        .unwrap();
        assert_eq!(winning, second);
        assert_eq!(*attempted.lock(), [first, second]);
        assert_eq!(
            events,
            [
                ConnectEvent::ResolveSkipped,
                ConnectEvent::ConnectStarted,
                ConnectEvent::ConnectCompleted
            ]
        );
    }
    #[tokio::test]
    async fn connect_timing_cancellation_leaves_only_the_started_phase() {
        let mut events = Vec::new();
        {
            let connect = connect_using(
                std::future::pending::<Result<Vec<SocketAddr>, ConnectError>>(),
                LOCAL_CONNECT_TIMEOUT,
                |_| async { panic!("must not dial") },
                true,
                |event| events.push(event),
            );
            tokio::pin!(connect);
            tokio::select! {biased; _=&mut connect=>panic!("resolver cannot finish"), ()=std::future::ready(())=>{}}
        }
        assert_eq!(events, [ConnectEvent::ResolveStarted]);
    }
}
