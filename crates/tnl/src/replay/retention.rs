use super::model::ReplayError;
use bytes::Bytes;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

pub(super) const TTL: Duration = Duration::from_secs(600);
pub(super) const MAX_ENTRIES: usize = 20;
pub(super) const MAX_BYTES: usize = 16 * 1024 * 1024;
pub(super) const BODY_LIMIT: usize = 1024 * 1024;
pub(super) const HEAD_LIMIT: usize = 8192;
// Covers the fixed head, entry/Arc/vector bookkeeping and bounded temporary
// validation allocations. Body capacity is reserved separately before copying.
pub(super) const HEAD_CHARGE: usize = 32 * 1024;
// Hyper's 32 KiB read buffer, bounded HeaderMap/URI serialization and driver
// scratch. The Full body shares the already charged Bytes allocation.
pub(super) const ENGINE_CHARGE: usize = 256 * 1024;

pub(super) struct Charge {
    budget: Arc<AtomicUsize>,
    bytes: usize,
}
impl Charge {
    pub(super) fn reserve(budget: &Arc<AtomicUsize>, bytes: usize) -> Option<Self> {
        budget
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|n| *n <= MAX_BYTES)
            })
            .ok()?;
        Some(Self {
            budget: budget.clone(),
            bytes,
        })
    }
}
impl Drop for Charge {
    fn drop(&mut self) {
        self.budget.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
// No Debug or Serialize: even a malformed head may contain credentials.
pub(super) struct RawRequest {
    pub(super) head: Box<[u8; HEAD_LIMIT]>,
    pub(super) head_len: usize,
    pub(super) body: Bytes,
    pub(super) building: Vec<u8>,
    pub(super) expected_length: Option<usize>,
    pub(super) head_seen: bool,
    pub(super) ended: bool,
    pub(super) finished: bool,
    pub(super) charges: Vec<Charge>,
    slots: Arc<AtomicUsize>,
}
impl RawRequest {
    pub(super) fn new(charge: Charge, slots: Arc<AtomicUsize>) -> Self {
        Self {
            head: Box::new([0; HEAD_LIMIT]),
            head_len: 0,
            body: Bytes::new(),
            building: Vec::new(),
            expected_length: None,
            head_seen: false,
            ended: false,
            finished: false,
            charges: vec![charge],
            slots,
        }
    }
    pub(super) const fn body_len(&self) -> usize {
        self.body.len() + self.building.len()
    }
    pub(super) const fn complete(&self) -> bool {
        self.head_seen && self.ended && self.finished
    }
    pub(super) fn finish(&mut self) {
        self.body = Bytes::from(std::mem::take(&mut self.building));
        self.finished = true;
    }
    pub(super) fn head(
        &mut self,
        method: &str,
        target: &str,
        headers: &[httparse::Header<'_>],
    ) -> Result<(), ReplayError> {
        if self.head_seen || !valid_head(method, target, headers) {
            return Err(ReplayError::InvalidRequest);
        }
        self.head_seen = true;
        if !headers
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case("transfer-encoding"))
        {
            self.expected_length = Some(0);
        }
        // Write directly into the precharged fixed allocation, with no raw
        // intermediate formatting String or second full-head copy.
        for part in [method.as_bytes(), b" ", target.as_bytes(), b" HTTP/1.1\r\n"] {
            self.append(part)?;
        }
        for h in headers {
            for part in [h.name.as_bytes(), b": ", h.value, b"\r\n"] {
                self.append(part)?;
            }
            if h.name.eq_ignore_ascii_case("content-length") {
                self.expected_length = std::str::from_utf8(h.value)
                    .ok()
                    .and_then(|s| s.parse().ok());
            }
        }
        self.append(b"\r\n")?;
        if self.expected_length.is_some_and(|n| n > BODY_LIMIT) {
            return Err(ReplayError::InvalidRequest);
        }
        Ok(())
    }
    fn append(&mut self, bytes: &[u8]) -> Result<(), ReplayError> {
        let end = self
            .head_len
            .checked_add(bytes.len())
            .filter(|n| *n <= HEAD_LIMIT)
            .ok_or(ReplayError::InvalidRequest)?;
        self.head[self.head_len..end].copy_from_slice(bytes);
        self.head_len = end;
        Ok(())
    }
}
fn token(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(b))
}
fn valid_head(method: &str, target: &str, headers: &[httparse::Header<'_>]) -> bool {
    let size = headers.iter().try_fold(
        method.len().saturating_add(target.len()).saturating_add(16),
        |n, h| {
            n.checked_add(h.name.len())?
                .checked_add(h.value.len())?
                .checked_add(4)
        },
    );
    if size.is_none_or(|n| n > HEAD_LIMIT) {
        return false;
    }
    if !matches!(
        method,
        "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"
    ) || !target.starts_with('/')
        || target.contains('#')
        || target.parse::<http::uri::PathAndQuery>().is_err()
        || !target.bytes().all(|b| (0x21..=0x7e).contains(&b))
        || headers.len() > 64
    {
        return false;
    }
    let mut hosts = 0;
    let mut length = None;
    let mut transfer = false;
    for h in headers {
        if h.name.len() > 128
            || !token(h.name.as_bytes())
            || !h
                .value
                .iter()
                .all(|b| *b == b'\t' || *b >= 0x20 && *b != 0x7f)
        {
            return false;
        }
        if h.name.eq_ignore_ascii_case("host") {
            hosts += 1;
            let Ok(value) = std::str::from_utf8(h.value) else {
                return false;
            };
            if !valid_host(value) {
                return false;
            }
        }
        if h.name.eq_ignore_ascii_case("upgrade") || h.name.eq_ignore_ascii_case("trailer") {
            return false;
        }
        if h.name.eq_ignore_ascii_case("content-length") {
            if length.is_some() || h.value.is_empty() || !h.value.iter().all(u8::is_ascii_digit) {
                return false;
            }
            length = std::str::from_utf8(h.value)
                .ok()
                .and_then(|s| s.parse::<usize>().ok());
            if length.is_none() {
                return false;
            }
        }
        if h.name.eq_ignore_ascii_case("transfer-encoding") {
            if transfer || !h.value.eq_ignore_ascii_case(b"chunked") {
                return false;
            }
            transfer = true;
        }
        if h.name.eq_ignore_ascii_case("connection") {
            for nominated in h.value.split(|b| *b == b',') {
                let nominated = nominated.trim_ascii();
                if !token(nominated)
                    || nominated.eq_ignore_ascii_case(b"host")
                    || nominated.eq_ignore_ascii_case(b"upgrade")
                {
                    return false;
                }
            }
        }
    }
    hosts == 1 && !(length.is_some() && transfer)
}

// Host is URI host[:port], not a DNS-label policy or a replay destination.
// Authority supplies the existing lexical checks but does not validate ports
// or the contents of IP-literal brackets.
fn valid_host(value: &str) -> bool {
    if value.contains('@') {
        return false;
    }
    let Ok(authority) = value.parse::<http::uri::Authority>() else {
        return false;
    };
    let host = authority.host();
    if host.is_empty() {
        return false;
    }
    let suffix = &value[host.len()..];
    if !suffix.is_empty()
        && !suffix.strip_prefix(':').is_some_and(|port| {
            !port.is_empty()
                && port.bytes().all(|b| b.is_ascii_digit())
                && port.parse::<u16>().is_ok()
        })
    {
        return false;
    }
    host.strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .map_or_else(
            || host.bytes().all(|b| unreserved(b) || sub_delimiter(b)),
            valid_ip_literal,
        )
}
fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~".contains(&byte)
}
fn sub_delimiter(byte: u8) -> bool {
    b"!$&'()*+,;=".contains(&byte)
}
fn valid_ip_literal(literal: &str) -> bool {
    if literal.parse::<std::net::Ipv6Addr>().is_ok() {
        return true;
    }
    // Preserve RFC 3986 IPvFuture and RFC 6874 scoped IPv6 forms already
    // accepted by Authority; neither is resolved or used as a socket target.
    if let Some((version, address)) = literal
        .strip_prefix(['v', 'V'])
        .and_then(|s| s.split_once('.'))
    {
        return !version.is_empty()
            && version.bytes().all(|b| b.is_ascii_hexdigit())
            && !address.is_empty()
            && address
                .bytes()
                .all(|b| unreserved(b) || sub_delimiter(b) || b == b':');
    }
    let Some((address, zone)) = literal.split_once("%25") else {
        return false;
    };
    if address.parse::<std::net::Ipv6Addr>().is_err() || zone.is_empty() {
        return false;
    }
    let mut bytes = zone.as_bytes();
    while let Some((&byte, rest)) = bytes.split_first() {
        if unreserved(byte) {
            bytes = rest;
        } else if byte == b'%' && rest.len() >= 2 && rest[..2].iter().all(u8::is_ascii_hexdigit) {
            bytes = &rest[2..];
        } else {
            return false;
        }
    }
    true
}

pub(super) fn safe_backend(address: std::net::SocketAddr, inspector_port: u16) -> bool {
    let loopback = match address.ip() {
        std::net::IpAddr::V4(ip) => ip.is_loopback(),
        std::net::IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map_or_else(|| ip.is_loopback(), |v4| v4.is_loopback()),
    };
    loopback && address.port() != 0 && address.port() != inspector_port
}

impl Drop for RawRequest {
    fn drop(&mut self) {
        self.slots.fetch_sub(1, Ordering::AcqRel);
    }
}
