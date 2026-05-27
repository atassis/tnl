//! `tnl http <TARGET>` positional argument: either a bare port (forwards to
//! localhost with dual-stack) or an explicit `host:port` (IP only in
//! alpha.3 — hostname targets are deferred to beta).

use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Port-only input: forwarder uses `tokio::net::lookup_host(("localhost", port))`
    /// to obtain both `127.0.0.1:port` and `[::1]:port` (order per `/etc/hosts`).
    LocalhostPort(u16),
    /// Explicit `IP:port`: single address, no DNS, no fallback.
    Explicit(SocketAddr),
}

impl Target {
    /// Display string suitable for log lines (e.g. column "→ <target>").
    pub fn display(&self) -> String {
        match self {
            Self::LocalhostPort(p) => format!("localhost:{p}"),
            Self::Explicit(a) => a.to_string(),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TargetParseError {
    #[error("empty target")]
    Empty,
    #[error("port must be 1..=65535")]
    InvalidPort,
    #[error(
        "invalid target {input:?}: expected PORT, IPv4:PORT, or [IPv6]:PORT \
         (hostnames are not accepted in alpha.3 — file an issue if you need them)"
    )]
    Invalid { input: String },
}

impl FromStr for Target {
    type Err = TargetParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(TargetParseError::Empty);
        }
        // Port-only path.
        if let Ok(p) = s.parse::<u16>() {
            if p == 0 {
                return Err(TargetParseError::InvalidPort);
            }
            return Ok(Self::LocalhostPort(p));
        }
        // Explicit SocketAddr — accepts both v4 and `[v6]:port`.
        if let Ok(addr) = s.parse::<SocketAddr>() {
            if addr.port() == 0 {
                return Err(TargetParseError::InvalidPort);
            }
            return Ok(Self::Explicit(addr));
        }
        Err(TargetParseError::Invalid {
            input: s.to_owned(),
        })
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LocalhostPort(p) => write!(f, "{p}"),
            Self::Explicit(a) => write!(f, "{a}"),
        }
    }
}
