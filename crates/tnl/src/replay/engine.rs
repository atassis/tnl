use super::{
    model::{AttemptState, Delivery, EngineError, ReplayOutcome},
    retention::{safe_backend, RawRequest},
};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

const CONNECT: Duration = Duration::from_secs(3);
const FIRST_RESPONSE: Duration = Duration::from_secs(15);
const OVERALL: Duration = Duration::from_secs(30);
const RESPONSE_LIMIT: u64 = 1024 * 1024;

pub(super) async fn execute(
    raw: Arc<RawRequest>,
    backend: Option<SocketAddr>,
    inspector_port: u16,
    cancel: CancellationToken,
    mut outcome: ReplayOutcome,
) -> ReplayOutcome {
    let started = Instant::now();
    outcome.delivery = Delivery::NotSent;
    let operation = async {
        let address = backend
            .filter(|a| safe_backend(*a, inspector_port))
            .ok_or(EngineError::ConnectFailed)?;
        // Serialization validation precedes even the one allowed connect.
        let request = request(&raw).ok_or(EngineError::WriteFailed)?;
        let socket = tokio::time::timeout(CONNECT, tokio::net::TcpStream::connect(address))
            .await
            .map_err(|_| EngineError::ConnectFailed)?
            .map_err(|_| EngineError::ConnectFailed)?;
        let mut builder = hyper::client::conn::http1::Builder::new();
        builder.max_buf_size(32 * 1024).writev(true);
        let (mut sender, connection) = builder
            .handshake(TokioIo::new(BoundedResponse::new(socket)))
            .await
            .map_err(|_| EngineError::WriteFailed)?;
        // From here the sender may write; no failure can promise not-sent.
        outcome.delivery = Delivery::PossiblySent;
        let exchange = async {
            let mut response = tokio::time::timeout(FIRST_RESPONSE, sender.send_request(request))
                .await
                .map_err(|_| EngineError::ResponseTimeout)?
                .map_err(|_| EngineError::ResponseInvalid)?;
            let status = response.status().as_u16();
            outcome.status = Some(status);
            if status == 101 {
                return Err(EngineError::ResponseInvalid);
            }
            while let Some(frame) = response.body_mut().frame().await {
                let frame = frame.map_err(|_| EngineError::ResponseInvalid)?;
                if let Some(bytes) = frame.data_ref() {
                    outcome.response_body_bytes = outcome
                        .response_body_bytes
                        .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                    if outcome.response_body_bytes > RESPONSE_LIMIT {
                        return Err(EngineError::ResponseTooLarge);
                    }
                }
            }
            outcome.response_complete = true;
            outcome.delivery = Delivery::ResponseReceived;
            Ok(())
        };
        tokio::pin!(exchange);
        tokio::pin!(connection);
        tokio::select! {
            biased;
            result = &mut exchange => result,
            // EOF may coincide with the last buffered body frame. Drive the
            // exchange after a clean close; the outer total deadline still wins.
            result = &mut connection => {
                if result.is_err() { Err(EngineError::ResponseInvalid) } else { exchange.await }
            }
        }
    };
    let result = tokio::select! {
        biased;
        () = cancel.cancelled() => Err(EngineError::Cancelled),
        result = tokio::time::timeout(OVERALL, operation) => result.unwrap_or(Err(EngineError::ResponseTimeout)),
    };
    outcome.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match result {
        Ok(()) => outcome.state = AttemptState::Complete,
        Err(error) => {
            outcome.state = if error == EngineError::Cancelled {
                AttemptState::Cancelled
            } else {
                AttemptState::Failed
            };
            outcome.error_code = Some(error);
        }
    }
    outcome
}
fn request(raw: &RawRequest) -> Option<http::Request<Full<bytes::Bytes>>> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut parsed = httparse::Request::new(&mut headers);
    if !matches!(
        parsed.parse(&raw.head[..raw.head_len]),
        Ok(httparse::Status::Complete(_))
    ) {
        return None;
    }
    let mut request = http::Request::builder()
        .method(parsed.method?)
        .uri(parsed.path?)
        .version(http::Version::HTTP_11);
    for header in parsed.headers.iter() {
        let nominated = parsed
            .headers
            .iter()
            .filter(|h| h.name.eq_ignore_ascii_case("connection"))
            .flat_map(|h| h.value.split(|b| *b == b','))
            .any(|name| {
                name.trim_ascii()
                    .eq_ignore_ascii_case(header.name.as_bytes())
            });
        if nominated
            || [
                "connection",
                "keep-alive",
                "proxy-connection",
                "proxy-authenticate",
                "proxy-authorization",
                "te",
                "trailer",
                "transfer-encoding",
                "upgrade",
                "expect",
                "content-length",
            ]
            .iter()
            .any(|h| header.name.eq_ignore_ascii_case(h))
        {
            continue;
        }
        request = request.header(header.name, header.value);
    }
    request
        .header("connection", "close")
        .header("content-length", raw.body.len())
        .body(Full::new(raw.body.clone()))
        .ok()
}

// Hyper bounds each individual parse buffer. This non-retaining wire guard
// additionally caps cumulative informational/final head bytes and read scratch.
// It does not interpret entity framing; Hyper remains the HTTP response parser.
struct BoundedResponse {
    socket: tokio::net::TcpStream,
    total: usize,
    prefix: [u8; 13],
    prefix_len: usize,
    tail: [u8; 4],
    informational: usize,
    final_head: bool,
    scratch: [u8; 8192],
}
impl BoundedResponse {
    const fn new(socket: tokio::net::TcpStream) -> Self {
        Self {
            socket,
            total: 0,
            prefix: [0; 13],
            prefix_len: 0,
            tail: [0; 4],
            informational: 0,
            final_head: false,
            scratch: [0; 8192],
        }
    }
    fn inspect(&mut self, n: usize) -> std::io::Result<()> {
        for index in 0..n {
            if self.final_head {
                break;
            }
            let byte = self.scratch[index];
            self.total += 1;
            if self.total > 32 * 1024 {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            // Hyper tolerates leading blanks and bare LF. This guard accepts
            // only canonical status prefixes and CRLF head lines, so unknown
            // input can never disable accounting as a supposedly final head.
            if (byte == b'\n' && self.tail[3] != b'\r') || (self.tail[3] == b'\r' && byte != b'\n')
            {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            if self.prefix_len < self.prefix.len() {
                self.prefix[self.prefix_len] = byte;
                self.prefix_len += 1;
                if self.prefix_len == self.prefix.len()
                    && (!matches!(&self.prefix[..9], b"HTTP/1.0 " | b"HTTP/1.1 ")
                        || !(b'1'..=b'5').contains(&self.prefix[9])
                        || !self.prefix[10..12].iter().all(u8::is_ascii_digit)
                        || self.prefix[12] != b' ')
                {
                    return Err(std::io::ErrorKind::InvalidData.into());
                }
            }
            self.tail.rotate_left(1);
            self.tail[3] = byte;
            if self.tail == *b"\r\n\r\n" {
                if self.prefix_len != self.prefix.len() {
                    return Err(std::io::ErrorKind::InvalidData.into());
                }
                if self.prefix[9] == b'1' && &self.prefix[9..12] != b"101" {
                    self.informational += 1;
                    if self.informational > 8 {
                        return Err(std::io::ErrorKind::InvalidData.into());
                    }
                    self.prefix_len = 0;
                    self.tail = [0; 4];
                } else {
                    self.final_head = true;
                }
            }
        }
        Ok(())
    }
}
impl tokio::io::AsyncRead for BoundedResponse {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let length = buf.remaining().min(this.scratch.len());
        let mut read = tokio::io::ReadBuf::new(&mut this.scratch[..length]);
        match std::pin::Pin::new(&mut this.socket).poll_read(cx, &mut read) {
            std::task::Poll::Ready(Ok(())) => {
                let n = read.filled().len();
                this.inspect(n)?;
                buf.put_slice(&this.scratch[..n]);
                std::task::Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}
impl tokio::io::AsyncWrite for BoundedResponse {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().socket).poll_write(cx, bytes)
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().socket).poll_flush(cx)
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().socket).poll_shutdown(cx)
    }
    fn is_write_vectored(&self) -> bool {
        true
    }
    fn poll_write_vectored(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buffers: &[std::io::IoSlice<'_>],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().socket).poll_write_vectored(cx, buffers)
    }
}
