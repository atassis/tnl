use super::{redact, CaptureId, Inspection, PreviewMode, State, TunnelGeneration};
use std::sync::atomic::Ordering;

#[derive(Clone, Copy, Debug)]
pub struct CaptureContext {
    pub capture_id: CaptureId,
    pub generation: TunnelGeneration,
    pub request_id: Option<ulid::Ulid>,
    pub backend: Option<std::net::SocketAddr>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Request,
    Response,
}
/// Borrowed raw input: intentionally no Debug/Serialize.
///
/// Callbacks return
/// synchronously before buffers are reused. Bodies are transfer-decoded, never
/// content-decoded. A framed End is not proof of successful backend delivery.
#[allow(missing_debug_implementations)] // Raw borrowed inputs must not be debug-formatted.
pub enum Observation<'a> {
    RequestHead {
        method: &'a str,
        target: &'a str,
        headers: &'a [httparse::Header<'a>],
    },
    ResponseHead {
        status: u16,
        headers: &'a [httparse::Header<'a>],
    },
    Body(Direction, &'a [u8]),
    /// Provisional framing completion, not delivery or replay authorization.
    /// Later unexpected input can revoke it with Stopped for this direction.
    MessageEnd {
        direction: Direction,
        trailers: bool,
    },
    /// Sticky revocation for the direction. Cancellation may repeat a stop
    /// with a different reason; consumers must handle these idempotently.
    Stopped(Direction, &'static str),
    /// Emitted once per admitted consumer, including cancellation. A 101 head
    /// suppresses opaque inputs immediately, but Finished may arrive later.
    Finished,
}
/// Implementations must bound allocations/admission themselves, never block,
/// enqueue unbounded data, log raw inputs, or affect live forwarding on failure.
pub trait ObservationConsumer: Send {
    fn observe(&mut self, event: &Observation<'_>);
}
pub trait ConsumerFactory: Send + Sync {
    fn admit(&self, context: CaptureContext) -> Option<Box<dyn ObservationConsumer>>;
}

#[derive(Clone, Copy)]
enum Framing {
    Head,
    Fixed(u64),
    Close,
    ChunkSize,
    ChunkData(u64),
    ChunkCrlf(u8),
    Trailers,
    Done,
    Stopped,
}
struct Parser {
    direction: Direction,
    state: Framing,
    head: Vec<u8>,
    informational: usize,
    head_total: usize,
    trailer_total: usize,
    head_request: bool,
    final_status: Option<u16>,
    complete: bool,
}
impl Parser {
    fn new(direction: Direction) -> Self {
        Self {
            direction,
            state: Framing::Head,
            head: Vec::with_capacity(8192),
            informational: 0,
            head_total: 0,
            trailer_total: 0,
            head_request: false,
            final_status: None,
            complete: false,
        }
    }
    fn stop(&mut self, reason: &'static str, emit: &mut impl FnMut(Observation<'_>)) {
        self.state = Framing::Stopped;
        self.head = Vec::new();
        self.complete = false;
        emit(Observation::Stopped(self.direction, reason));
    }
    fn end(&mut self, trailers: bool, emit: &mut impl FnMut(Observation<'_>)) {
        self.state = Framing::Done;
        self.head = Vec::new();
        self.complete = true;
        emit(Observation::MessageEnd {
            direction: self.direction,
            trailers,
        });
    }
    #[allow(clippy::too_many_lines)] // One bounded, explicit framing state machine.
    fn feed(
        &mut self,
        mut bytes: &[u8],
        head_request: bool,
        emit: &mut impl FnMut(Observation<'_>),
    ) {
        while !bytes.is_empty() {
            match self.state {
                Framing::Stopped => return,
                Framing::Done => {
                    self.stop("unexpected-data", emit);
                    return;
                }
                Framing::Head => {
                    if self.head.capacity() == 0 {
                        self.head = Vec::with_capacity(8192);
                    }
                    if self.head.len() == 8192 {
                        self.stop("head-limit", emit);
                        return;
                    }
                    self.head.push(bytes[0]);
                    bytes = &bytes[1..];
                    if self.head.ends_with(b"\r\n\r\n") {
                        self.head_total += self.head.len();
                        if self.head_total > 32768 {
                            self.stop("head-limit", emit);
                            return;
                        }
                        let mut headers = [httparse::EMPTY_HEADER; 64];
                        let (framing, informational, upgrade) = if self.direction
                            == Direction::Request
                        {
                            let mut parsed = httparse::Request::new(&mut headers);
                            if !matches!(
                                parsed.parse(&self.head),
                                Ok(httparse::Status::Complete(_))
                            ) {
                                self.stop("malformed-head", emit);
                                return;
                            }
                            let (Some(method), Some(target)) = (parsed.method, parsed.path) else {
                                self.stop("malformed-head", emit);
                                return;
                            };
                            let Some(framing) = framing(parsed.headers, false) else {
                                self.stop("ambiguous-framing", emit);
                                return;
                            };
                            self.head_request = method == "HEAD";
                            emit(Observation::RequestHead {
                                method,
                                target,
                                headers: parsed.headers,
                            });
                            (framing, false, false)
                        } else {
                            let mut parsed = httparse::Response::new(&mut headers);
                            if !matches!(
                                parsed.parse(&self.head),
                                Ok(httparse::Status::Complete(_))
                            ) {
                                self.stop("malformed-head", emit);
                                return;
                            }
                            let Some(status) = parsed.code.filter(|s| (100..=599).contains(s))
                            else {
                                self.stop("malformed-head", emit);
                                return;
                            };
                            let informational = (100..200).contains(&status) && status != 101;
                            if informational && self.informational == 8 {
                                self.stop("informational-limit", emit);
                                return;
                            }
                            let Some(mut framing) = framing(parsed.headers, true) else {
                                self.stop("ambiguous-framing", emit);
                                return;
                            };
                            if head_request || informational || matches!(status, 101 | 204 | 304) {
                                framing = Framing::Fixed(0);
                            }
                            if !informational {
                                self.final_status = Some(status);
                            }
                            emit(Observation::ResponseHead {
                                status,
                                headers: parsed.headers,
                            });
                            (framing, informational, status == 101)
                        };
                        self.head = Vec::new();
                        if informational {
                            self.informational += 1;
                            self.state = Framing::Head;
                        } else if upgrade {
                            self.end(false, emit);
                            return; // discard opaque leftover
                        } else {
                            self.state = framing;
                            if matches!(self.state, Framing::Fixed(0)) {
                                self.end(false, emit);
                            }
                        }
                    }
                }
                Framing::Fixed(left) | Framing::ChunkData(left) => {
                    let n = usize::try_from(left).unwrap_or(usize::MAX).min(bytes.len());
                    emit(Observation::Body(self.direction, &bytes[..n]));
                    bytes = &bytes[n..];
                    let remaining = left - u64::try_from(n).unwrap_or(left);
                    if matches!(self.state, Framing::Fixed(_)) {
                        if remaining == 0 {
                            self.end(false, emit);
                        } else {
                            self.state = Framing::Fixed(remaining);
                        }
                    } else if remaining == 0 {
                        self.state = Framing::ChunkCrlf(0);
                    } else {
                        self.state = Framing::ChunkData(remaining);
                    }
                }
                Framing::Close => {
                    emit(Observation::Body(self.direction, bytes));
                    return;
                }
                Framing::ChunkCrlf(pos) => {
                    if bytes[0] != if pos == 0 { b'\r' } else { b'\n' } {
                        self.stop("invalid-chunk", emit);
                        return;
                    }
                    bytes = &bytes[1..];
                    self.state = if pos == 0 {
                        Framing::ChunkCrlf(1)
                    } else {
                        Framing::ChunkSize
                    };
                }
                Framing::ChunkSize | Framing::Trailers => {
                    if self.head.capacity() == 0 {
                        self.head = Vec::with_capacity(8192);
                    }
                    if self.head.len() >= 8192 {
                        self.stop("chunk-limit", emit);
                        return;
                    }
                    self.head.push(bytes[0]);
                    bytes = &bytes[1..];
                    if self.head.ends_with(b"\r\n") {
                        if matches!(self.state, Framing::Trailers) {
                            self.trailer_total += self.head.len();
                            if self.trailer_total > 8192 {
                                self.stop("trailer-limit", emit);
                                return;
                            }
                            if self.head.len() == 2 {
                                self.end(self.trailer_total > 2, emit);
                            } else if !valid_trailer(&self.head[..self.head.len() - 2]) {
                                self.stop("invalid-trailer", emit);
                                return;
                            }
                        } else {
                            let line = &self.head[..self.head.len() - 2];
                            // Extensions are unsupported, rather than guessed.
                            if line.is_empty()
                                || line.len() > 16
                                || !line.iter().all(u8::is_ascii_hexdigit)
                            {
                                self.stop("invalid-chunk", emit);
                                return;
                            }
                            let size = std::str::from_utf8(line)
                                .ok()
                                .and_then(|s| u64::from_str_radix(s, 16).ok());
                            let Some(size) = size else {
                                self.stop("invalid-chunk", emit);
                                return;
                            };
                            self.state = if size == 0 {
                                Framing::Trailers
                            } else {
                                Framing::ChunkData(size)
                            };
                        }
                        self.head.clear();
                    }
                }
            }
        }
    }
    fn eof(&mut self, emit: &mut impl FnMut(Observation<'_>)) {
        if matches!(self.state, Framing::Close) {
            self.end(false, emit);
        } else if !matches!(self.state, Framing::Done | Framing::Stopped) {
            self.stop("incomplete", emit);
        }
    }
}
// RFC 9110 field-name token and field-value octets; no obs-fold, bare
// newline, NUL, DEL or other controls. Validate borrowed bytes, retain none.
fn valid_trailer(line: &[u8]) -> bool {
    let Some(colon) = line.iter().position(|byte| *byte == b':') else {
        return false;
    };
    colon != 0
        && line[..colon].iter().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
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
        })
        && line[colon + 1..]
            .iter()
            .all(|byte| *byte == b'\t' || *byte >= b' ' && *byte != 0x7f)
}

fn framing(headers: &[httparse::Header<'_>], response: bool) -> Option<Framing> {
    let mut length = None;
    let mut transfer = false;
    for h in headers {
        if h.name.eq_ignore_ascii_case("content-length") {
            let value = std::str::from_utf8(h.value).ok()?;
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let n = value.parse::<u64>().ok()?;
            // Conservative: even duplicate identical lengths are ambiguous.
            if length.replace(n).is_some() {
                return None;
            }
        }
        if h.name.eq_ignore_ascii_case("transfer-encoding") {
            if transfer || !h.value.eq_ignore_ascii_case(b"chunked") {
                return None;
            }
            transfer = true;
        }
    }
    match (transfer, length) {
        (true, Some(_)) => None,
        (true, None) => Some(Framing::ChunkSize),
        (false, Some(n)) => Some(Framing::Fixed(n)),
        (false, None) => Some(if response {
            Framing::Close
        } else {
            Framing::Fixed(0)
        }),
    }
}
struct Preview {
    bytes: Vec<u8>,
    reason: Option<&'static str>,
    count: u64,
}
impl Preview {
    fn new(mode: PreviewMode) -> Self {
        Self {
            bytes: Vec::new(),
            reason: Some(if mode == PreviewMode::Off {
                "disabled"
            } else {
                "unsupported-media-type"
            }),
            count: 0,
        }
    }
    fn head(&mut self, headers: &[httparse::Header<'_>], mode: PreviewMode) {
        if mode == PreviewMode::Off {
            return;
        }
        let mut media = headers
            .iter()
            .filter(|h| h.name.eq_ignore_ascii_case("content-type"));
        let first = media.next();
        let json = media.next().is_none()
            && first
                .and_then(|h| std::str::from_utf8(h.value).ok())
                .is_some_and(|s| {
                    let s = s
                        .split(';')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_ascii_lowercase();
                    s == "application/json"
                        || (s.starts_with("application/") && s.ends_with("+json"))
                });
        let encoded = headers.iter().any(|h| {
            h.name.eq_ignore_ascii_case("content-encoding")
                && !h.value.eq_ignore_ascii_case(b"identity")
        });
        self.reason = if encoded {
            Some("encoded")
        } else if json {
            None
        } else {
            Some("unsupported-media-type")
        };
    }
    fn body(&mut self, bytes: &[u8]) {
        self.count = self
            .count
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if self.reason.is_some() {
            return;
        }
        if self.bytes.len() + bytes.len() > 4096 {
            self.reason = Some("too-large");
            self.bytes = Vec::new();
            return;
        }
        if self.bytes.capacity() == 0 {
            self.bytes = Vec::with_capacity(4096);
        }
        self.bytes.extend_from_slice(bytes);
    }
}
struct Display {
    inspection: Inspection,
    id: CaptureId,
    request: Preview,
    response: Preview,
    upgraded: bool,
}
impl Display {
    fn event(&mut self, event: &Observation<'_>) {
        let mode = self.inspection.inner.mode;
        match event {
            Observation::RequestHead {
                method,
                target,
                headers,
            } => {
                self.request.head(headers, mode);
                let method = redact::text(method, 32);
                let target = redact::target(target);
                let headers = redact::headers(headers);
                self.inspection.update(self.id, |r| {
                    r.method = method;
                    r.request_target = target;
                    r.request.headers = headers;
                });
            }
            Observation::ResponseHead { status, headers }
                if !(100..200).contains(status) || *status == 101 =>
            {
                self.response.head(headers, mode);
                let headers = redact::headers(headers);
                self.inspection.update(self.id, |r| {
                    r.status = Some(*status);
                    r.response.headers = headers;
                    if *status == 101 {
                        r.state = State::Upgraded;
                    }
                });
                if *status == 101 {
                    self.upgraded = true;
                    self.request.bytes = Vec::new();
                    self.response.bytes = Vec::new();
                }
            }
            Observation::Body(direction, bytes) => {
                let p = if *direction == Direction::Request {
                    &mut self.request
                } else {
                    &mut self.response
                };
                p.body(bytes);
            }
            Observation::MessageEnd { direction, .. } => {
                let p = if *direction == Direction::Request {
                    &mut self.request
                } else {
                    &mut self.response
                };
                let text = if p.reason.is_none() && !self.upgraded {
                    redact::json(&p.bytes)
                } else {
                    None
                };
                let reason = if self.upgraded {
                    Some("upgraded")
                } else {
                    p.reason.or_else(|| {
                        if text.is_none() {
                            Some("invalid-json")
                        } else {
                            None
                        }
                    })
                };
                let count = p.count;
                p.bytes = Vec::new();
                self.inspection.update(self.id, |r| {
                    let m = if *direction == Direction::Request {
                        &mut r.request
                    } else {
                        &mut r.response
                    };
                    m.body.text = text;
                    m.body.reason = reason;
                    m.body.observed_payload_bytes = count;
                });
            }
            Observation::Stopped(direction, reason) => {
                let p = if *direction == Direction::Request {
                    &mut self.request
                } else {
                    &mut self.response
                };
                p.bytes = Vec::new();
                p.reason = Some(reason);
                self.inspection.update(self.id, |r| {
                    r.observation_incomplete = true;
                    let m = if *direction == Direction::Request {
                        &mut r.request
                    } else {
                        &mut r.response
                    };
                    m.body.text = None;
                    m.body.reason = Some(reason);
                    m.body.observed_payload_bytes = p.count;
                });
            }
            _ => {}
        }
    }
}
/// Lifetime guard finalizes incomplete observations on I/O error or cancellation.
///
/// Scratch: 8 KiB head + at most 4 KiB entity per direction, admitted before
/// allocation (64 captures maximum). Opaque upgrade bytes are never retained.
pub struct Capture {
    context: CaptureContext,
    request: Parser,
    response: Parser,
    display: Display,
    consumer: Option<Box<dyn ObservationConsumer>>,
    finished: bool,
}
impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Capture")
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}
impl Capture {
    pub(crate) fn new(inspection: Inspection, context: CaptureContext) -> Self {
        let consumer = inspection
            .inner
            .consumer
            .as_ref()
            .and_then(|f| f.admit(context));
        let mode = inspection.inner.mode;
        Self {
            context,
            request: Parser::new(Direction::Request),
            response: Parser::new(Direction::Response),
            display: Display {
                inspection,
                id: context.capture_id,
                request: Preview::new(mode),
                response: Preview::new(mode),
                upgraded: false,
            },
            consumer,
            finished: false,
        }
    }
    pub const fn id(&self) -> CaptureId {
        self.context.capture_id
    }
    pub const fn final_status(&self) -> Option<u16> {
        self.response.final_status
    }
    pub fn request(&mut self, bytes: &[u8]) {
        if self.finished || self.display.upgraded {
            return;
        }
        let display = &mut self.display;
        let consumer = &mut self.consumer;
        self.request.feed(bytes, false, &mut |event| {
            display.event(&event);
            if let Some(c) = consumer {
                c.observe(&event);
            }
        });
    }
    pub fn response(&mut self, bytes: &[u8]) {
        if self.finished || self.display.upgraded {
            return;
        }
        let display = &mut self.display;
        let consumer = &mut self.consumer;
        self.response
            .feed(bytes, self.request.head_request, &mut |event| {
                display.event(&event);
                if let Some(c) = consumer {
                    c.observe(&event);
                }
            });
    }
    pub fn finish(&mut self) {
        self.terminate(true);
    }
    fn terminate(&mut self, graceful: bool) {
        if self.finished {
            return;
        }
        self.finished = true;
        let display = &mut self.display;
        let consumer = &mut self.consumer;
        if !display.upgraded {
            let mut emit = |event: Observation<'_>| {
                display.event(&event);
                if let Some(c) = consumer.as_mut() {
                    c.observe(&event);
                }
            };
            if graceful {
                self.request.eof(&mut emit);
                self.response.eof(&mut emit);
            } else {
                // Framed completion is provisional, including on cancellation
                // after both MessageEnd callbacks. Finished alone is not success.
                self.request.stop("cancelled", &mut emit);
                self.response.stop("cancelled", &mut emit);
            }
        }
        if let Some(c) = consumer {
            c.observe(&Observation::Finished);
        }
        display.inspection.update(self.context.capture_id, |r| {
            if r.state != State::Upgraded {
                r.state = if r.observation_incomplete {
                    State::Incomplete
                } else {
                    State::Complete
                };
            }
        });
        display
            .inspection
            .inner
            .active
            .fetch_sub(1, Ordering::AcqRel);
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.terminate(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scratch_capacities_remain_bounded_for_large_inputs() {
        let history = Inspection::new(PreviewMode::Json);
        let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
        capture.request(b"POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n100000\r\n");
        let bytes = [b'x'; 8192];
        for _ in 0..128 {
            capture.request(&bytes);
            assert!(capture.request.head.capacity() <= 8192);
            assert!(capture.response.head.capacity() <= 8192);
            assert!(capture.display.request.bytes.capacity() <= 4096);
            assert!(capture.display.response.bytes.capacity() <= 4096);
        }
        assert_eq!(capture.display.request.reason, Some("too-large"));
        assert!(capture.display.request.bytes.is_empty());
    }
    #[test]
    fn preview_retention_stops_before_allocating_the_4097th_byte() {
        let history = Inspection::new(PreviewMode::Json);
        let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
        capture.request(
            b"POST / HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 4097\r\n\r\n",
        );
        for _ in 0..4096 {
            capture.request(b"x");
        }
        assert_eq!(capture.display.request.bytes.len(), 4096);
        assert_eq!(capture.display.request.bytes.capacity(), 4096);
        capture.request(b"x");
        assert_eq!(capture.display.request.reason, Some("too-large"));
        assert_eq!(capture.display.request.bytes.capacity(), 0);
    }
}
