//! One bounded authenticated HTTP/1 loopback listener. No raw-content endpoints.
use super::{CaptureId, Inspection, InspectionRecord};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Semaphore};
use tokio::task::{JoinHandle, JoinSet};

const HEAD_LIMIT: usize = 32768;
const RESPONSE_LIMIT: usize = 65536;
const JSON_LIMIT: usize = 1024;
const HEX: &[u8; 16] = b"0123456789abcdef";
const DEADLINE: Duration = Duration::from_secs(3);
const UI: &str = include_str!("ui.html");
const SCRIPT: &str = include_str!("ui.js");

/// Owns listener shutdown; Debug deliberately excludes the capability.
pub struct InspectorService {
    addr: SocketAddr,
    token: Arc<[u8; 64]>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    history: Inspection,
}
impl std::fmt::Debug for InspectorService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InspectorService")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}
impl InspectorService {
    pub async fn start(history: Inspection, port: u16) -> anyhow::Result<Self> {
        Ok(BoundInspector::bind(port).await?.start(history))
    }
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }
    /// Sensitive startup link: print once to stderr, never log or serialize.
    pub fn bootstrap_url(&self) -> String {
        format!(
            "http://{}/#token={}",
            self.addr,
            String::from_utf8_lossy(self.token.as_ref())
        )
    }
    pub async fn shutdown(mut self) {
        self.history.shutdown();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
impl Drop for InspectorService {
    fn drop(&mut self) {
        self.history.shutdown();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
/// Bound but not serving: supplies the actual nonzero forbidden inspector port
/// to a future replay controller before that controller and router are built.
pub struct BoundInspector {
    listener: TcpListener,
    addr: SocketAddr,
    token: Arc<[u8; 64]>,
}
impl std::fmt::Debug for BoundInspector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundInspector")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}
impl BoundInspector {
    pub async fn bind(port: u16) -> anyhow::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .await
            .context("bind loopback inspector")?;
        let addr = listener.local_addr().context("inspector local address")?;
        let mut random = [0u8; 32];
        getrandom::fill(&mut random)
            .map_err(|_| anyhow::anyhow!("inspector capability randomness unavailable"))?;
        let mut token = [0u8; 64];
        for (i, byte) in random.iter().enumerate() {
            token[2 * i] = HEX[usize::from(byte >> 4)];
            token[2 * i + 1] = HEX[usize::from(byte & 15)];
        }
        Ok(Self {
            listener,
            addr,
            token: Arc::new(token),
        })
    }
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }
    pub fn start(self, history: Inspection) -> InspectorService {
        let (stop, rx) = oneshot::channel();
        let state = ServerState {
            history: history.clone(),
            token: self.token.clone(),
            authority: self.addr.to_string(),
            origin: format!("http://{}", self.addr),
            handlers: Semaphore::new(4),
        };
        let task = tokio::spawn(serve(self.listener, Arc::new(state), rx));
        InspectorService {
            addr: self.addr,
            token: self.token,
            stop: Some(stop),
            task: Some(task),
            history,
        }
    }
}

struct ServerState {
    history: Inspection,
    token: Arc<[u8; 64]>,
    authority: String,
    origin: String,
    handlers: Semaphore,
}
struct ClearOnExit(Inspection);
impl Drop for ClearOnExit {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

async fn serve(listener: TcpListener, state: Arc<ServerState>, mut stop: oneshot::Receiver<()>) {
    let _clear = ClearOnExit(state.history.clone());
    let mut connections = JoinSet::new();
    let mut sweep = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = sweep.tick() => state.history.sweep(),
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            accepted=listener.accept() => {
                let Ok((socket,peer))=accepted else { tracing::warn!("local inspector listener stopped; forwarding continues"); break; };
                // Admission before parser buffers/task creation; no queued sockets.
                if !peer.ip().is_loopback() || connections.len()>=8 { drop(socket); continue; }
                let state=state.clone(); connections.spawn(async move { let _=connection(socket,&state).await; });
            }
        }
    }
    state.history.shutdown();
    connections.shutdown().await;
}
async fn connection(mut socket: TcpStream, state: &ServerState) -> std::io::Result<()> {
    // Four handlers reserve 256 KiB each: one <=64-KiB record clone, <=64-KiB
    // summaries, <=64-KiB serialized output, 32-KiB head and bounded search
    // scratch. No snapshot/serialization waits behind clients or history locks.
    let Ok(_permit) = state.handlers.try_acquire() else {
        return Ok(());
    };
    let mut input = Vec::with_capacity(HEAD_LIMIT + JSON_LIMIT);
    let read = tokio::time::timeout(DEADLINE, async {
        let mut chunk = [0u8; 1024];
        let end = loop {
            if input.len() == HEAD_LIMIT {
                return Err(0);
            }
            let available = (HEAD_LIMIT - input.len()).min(chunk.len());
            let n = socket.read(&mut chunk[..available]).await.map_err(|_| 0)?;
            if n == 0 {
                return Err(0);
            }
            input.extend_from_slice(&chunk[..n]);
            if let Some(end) = input.windows(4).position(|w| w == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let length = post_body_length(&input[..end]).map_err(|()| 400)?;
        if input.len() > end + length {
            return Err(400);
        }
        while input.len() < end + length {
            let available = (end + length - input.len()).min(chunk.len());
            let n = socket.read(&mut chunk[..available]).await.map_err(|_| 0)?;
            if n == 0 {
                return Err(0);
            }
            input.extend_from_slice(&chunk[..n]);
        }
        Ok(end)
    })
    .await;
    let reply = match read {
        Ok(Ok(end)) => route(&input[..end], &input[end..], state),
        Ok(Err(400)) => error(400),
        _ => return Ok(()),
    };
    let body = reply.body;
    let headers=format!("HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nPragma: no-cache\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nContent-Security-Policy: default-src 'none'; script-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'\r\n\r\n",reply.status,reason(reply.status),reply.mime,body.len());
    let _ = tokio::time::timeout(DEADLINE, async {
        socket.write_all(headers.as_bytes()).await?;
        socket.write_all(&body).await?;
        socket.shutdown().await
    })
    .await;
    Ok(())
}
// Fixed admission and read deadline also cover JSON; transfer coding is never
// accepted. All other methods retain the no-body policy checked by route().
fn post_body_length(head: &[u8]) -> Result<usize, ()> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers);
    if !matches!(req.parse(head), Ok(httparse::Status::Complete(_))) {
        return Err(());
    }
    if req.method != Some("POST") {
        return Ok(0);
    }
    if one(req.headers, "transfer-encoding") != Ok(None) || one(req.headers, "upgrade") != Ok(None)
    {
        return Err(());
    }
    let Some(value) = one(req.headers, "content-length")? else {
        return Ok(0);
    };
    if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
        return Err(());
    }
    let length: usize = std::str::from_utf8(value)
        .map_err(|_| ())?
        .parse()
        .map_err(|_| ())?;
    if length > JSON_LIMIT {
        return Err(());
    }
    Ok(length)
}
struct Reply {
    status: u16,
    mime: &'static str,
    body: Vec<u8>,
}
fn error(status: u16) -> Reply {
    Reply {
        status,
        mime: "application/json",
        body: format!("{{\"error\":{{\"code\":\"{}\"}}}}", reason(status)).into_bytes(),
    }
}
const fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        _ => "Service Unavailable",
    }
}
fn json(value: &impl Serialize) -> Reply {
    struct Output(Vec<u8>);
    impl std::io::Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len() + bytes.len() > RESPONSE_LIMIT {
                return Err(std::io::ErrorKind::OutOfMemory.into());
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Output(Vec::with_capacity(RESPONSE_LIMIT));
    if serde_json::to_writer(&mut output, value).is_err() {
        return error(503);
    }
    Reply {
        status: 200,
        mime: "application/json",
        body: output.0,
    }
}
#[derive(Serialize)]
struct StatsReply {
    schema_version: u8,
    records: usize,
    tunnel: super::TunnelStatus,
}
#[derive(Serialize)]
struct ListReply<'a> {
    schema_version: u8,
    items: &'a [super::Summary],
    next_before: Option<u64>,
}

fn one<'a>(headers: &'a [httparse::Header<'a>], name: &str) -> Result<Option<&'a [u8]>, ()> {
    let mut values = headers.iter().filter(|h| h.name.eq_ignore_ascii_case(name));
    let value = values.next().map(|h| h.value);
    if values.next().is_some() {
        return Err(());
    }
    Ok(value)
}
#[allow(clippy::too_many_lines)] // Ordered transport/auth checks precede route disclosure.
fn route(head: &[u8], body: &[u8], state: &ServerState) -> Reply {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers);
    if !matches!(req.parse(head), Ok(httparse::Status::Complete(_))) || req.version != Some(1) {
        return error(400);
    }
    if one(req.headers, "host") != Ok(Some(state.authority.as_bytes())) {
        return error(403);
    }
    match one(req.headers, "origin") {
        Ok(None) => {}
        Ok(Some(origin)) if origin == state.origin.as_bytes() => {}
        _ => return error(403),
    }
    match one(req.headers, "sec-fetch-site") {
        Ok(None | Some(b"same-origin" | b"none")) => {}
        // A user-activated cross-site navigation may load only the static root
        // document. It grants no capability/data access; Host/Origin above and
        // framing below remain mandatory. Scripts and APIs get no exception.
        Ok(Some(b"cross-site"))
            if req.method == Some("GET")
                && req.path == Some("/")
                && one(req.headers, "sec-fetch-mode") == Ok(Some(b"navigate"))
                && one(req.headers, "sec-fetch-dest") == Ok(Some(b"document"))
                && one(req.headers, "sec-fetch-user") == Ok(Some(b"?1")) => {}
        _ => return error(403),
    }
    // Only bounded, length-framed JSON POSTs can reach replay mutation routes.
    // Transfer coding, upgrades and non-POST bodies remain forbidden.
    if one(req.headers, "transfer-encoding") != Ok(None)
        || one(req.headers, "upgrade") != Ok(None)
        || (req.method != Some("POST")
            && !matches!(one(req.headers, "content-length"), Ok(None | Some(b"0"))))
    {
        return error(400);
    }
    let Some(path) = req
        .path
        .filter(|p| p.len() <= 2048 && p.starts_with('/') && !p.starts_with("//"))
    else {
        return error(400);
    };
    let api = path.starts_with("/api/");
    if api {
        let Ok(Some(auth)) = one(req.headers, "authorization") else {
            return error(401);
        };
        let Some(candidate) = auth.strip_prefix(b"Bearer ") else {
            return error(401);
        };
        if candidate.len() != 64 || !bool::from(candidate.ct_eq(state.token.as_ref())) {
            return error(401);
        }
    }
    if req.method != Some("GET") {
        if api
            && (one(req.headers, "origin") != Ok(Some(state.origin.as_bytes()))
                || one(req.headers, "sec-fetch-site") != Ok(Some(b"same-origin"))
                || one(req.headers, "sec-fetch-dest") != Ok(Some(b"empty"))
                || !matches!(
                    one(req.headers, "sec-fetch-mode"),
                    Ok(Some(b"cors" | b"same-origin"))
                ))
        {
            return error(403);
        }
        if req.method == Some("POST") {
            if replay_capture(path, "/replay/prepare").is_none()
                && replay_capture(path, "/replay/confirm").is_none()
            {
                return error(405);
            }
            if one(req.headers, "content-encoding") != Ok(None)
                || one(req.headers, "content-type") != Ok(Some(b"application/json"))
            {
                return error(400);
            }
            return replay_mutation(path, body, state);
        }
        return error(405);
    }
    match path {
        "/" => Reply {
            status: 200,
            mime: "text/html; charset=utf-8",
            body: UI.as_bytes().to_vec(),
        },
        "/inspector.js" => Reply {
            status: 200,
            mime: "text/javascript; charset=utf-8",
            body: SCRIPT.as_bytes().to_vec(),
        },
        "/api/v1/stats" => match state.history.api_status() {
            Ok((records, tunnel)) => json(&StatsReply {
                schema_version: 1,
                records,
                tunnel,
            }),
            Err(()) => error(503),
        },
        _ if path == "/api/v1/requests" || path.starts_with("/api/v1/requests?") => {
            let Ok(filter) = Filter::parse(path.split_once('?').map_or("", |(_, q)| q)) else {
                return error(400);
            };
            match state.history.api_list(&filter) {
                Ok(items) => {
                    let next = items.last().map(|s| s.sequence);
                    json(&ListReply {
                        schema_version: 1,
                        items: &items,
                        next_before: next,
                    })
                }
                Err(()) => error(503),
            }
        }
        _ if path.ends_with("/replay/prepare") || path.ends_with("/replay/confirm") => error(405),
        _ if path.starts_with("/api/v1/replays/") => {
            let Some(id) = path
                .strip_prefix("/api/v1/replays/")
                .and_then(|id| id.parse().ok())
            else {
                return error(404);
            };
            match state.history.replay_outcome(id) {
                Ok(outcome) => json(&OutcomeReply {
                    schema_version: 1,
                    outcome,
                }),
                Err(reason) => replay_error(reason),
            }
        }
        _ if path.ends_with("/replay") => {
            let Some(id) = replay_capture(path, "/replay") else {
                return error(404);
            };
            match state
                .history
                .with_replay_row(id, |replay, _| Ok(replay.availability(id)))
            {
                Ok(availability) => json(&AvailabilityReply {
                    schema_version: 1,
                    availability,
                }),
                Err(reason) => replay_error(reason),
            }
        }
        _ if path.starts_with("/api/v1/requests/") => {
            let Some(id) = path
                .strip_prefix("/api/v1/requests/")
                .and_then(|id| id.parse::<ulid::Ulid>().ok())
            else {
                return error(404);
            };
            match state.history.api_detail(CaptureId(id)) {
                Ok(Some(record)) => json(&record),
                Ok(None) => error(404),
                Err(()) => error(503),
            }
        }
        _ => error(404),
    }
}
#[derive(Serialize)]
struct AvailabilityReply {
    schema_version: u8,
    availability: crate::replay::ReplayAvailability,
}
#[derive(Serialize)]
struct PreviewReply {
    schema_version: u8,
    method: String,
    request_target: String,
    preview: crate::replay::ReplayPreview,
}
#[derive(Serialize)]
struct OutcomeReply {
    schema_version: u8,
    outcome: Option<crate::replay::ReplayOutcome>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareRequest {
    schema_version: u8,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmRequest {
    schema_version: u8,
    replay_id: ulid::Ulid,
    confirmation_token: String,
    confirm_side_effects: bool,
}
fn replay_capture(path: &str, suffix: &str) -> Option<CaptureId> {
    path.strip_prefix("/api/v1/requests/")?
        .strip_suffix(suffix)?
        .parse()
        .ok()
        .map(CaptureId)
}
fn replay_error(reason: crate::replay::ReplayError) -> Reply {
    use crate::replay::ReplayError;
    #[derive(Serialize)]
    struct Failure {
        schema_version: u8,
        error: crate::replay::ReplayError,
    }
    let mut reply = json(&Failure {
        schema_version: 1,
        error: reason,
    });
    reply.status = match reason {
        ReplayError::NotRetained => 404,
        ReplayError::InvalidRequest
        | ReplayError::InvalidConfirmation
        | ReplayError::ConfirmationRequired => 400,
        ReplayError::Busy | ReplayError::Overloaded | ReplayError::RuntimeUnavailable => 503,
        _ => 409,
    };
    reply
}
fn replay_mutation(path: &str, body: &[u8], state: &ServerState) -> Reply {
    use crate::replay::ReplayError;
    if let Some(id) = replay_capture(path, "/replay/prepare") {
        let Ok(request) = serde_json::from_slice::<PrepareRequest>(body) else {
            return replay_error(ReplayError::InvalidRequest);
        };
        if request.schema_version != 1 {
            return replay_error(ReplayError::InvalidRequest);
        }
        match state.history.with_replay_row(id, |replay, row| {
            Ok(PreviewReply {
                schema_version: 1,
                method: row.method.clone(),
                request_target: row.request_target.clone(),
                preview: replay.prepare(id)?,
            })
        }) {
            Ok(preview) => json(&preview),
            Err(reason) => replay_error(reason),
        }
    } else if let Some(id) = replay_capture(path, "/replay/confirm") {
        let Ok(request) = serde_json::from_slice::<ConfirmRequest>(body) else {
            return replay_error(ReplayError::InvalidRequest);
        };
        if request.schema_version != 1 {
            return replay_error(ReplayError::InvalidRequest);
        }
        match state.history.confirm_replay(
            id,
            request.replay_id,
            &request.confirmation_token,
            request.confirm_side_effects,
        ) {
            Ok(outcome) => json(&OutcomeReply {
                schema_version: 1,
                outcome: Some(outcome),
            }),
            Err(reason) => replay_error(reason),
        }
    } else {
        error(405)
    }
}
pub(super) struct Filter {
    pub before: Option<u64>,
    pub limit: usize,
    query: String,
    method: Option<String>,
    status: Option<u16>,
    state: Option<String>,
}
impl Filter {
    fn parse(query: &str) -> Result<Self, ()> {
        // Reject malformed escapes/UTF-8 before a bounded decoding allocation.
        let bytes = query.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' {
                if i + 2 >= bytes.len() || !bytes[i + 1..i + 3].iter().all(u8::is_ascii_hexdigit) {
                    return Err(());
                }
                i += 3;
            } else {
                i += 1;
            }
        }
        let mut out = Self {
            before: None,
            limit: 50,
            query: String::new(),
            method: None,
            status: None,
            state: None,
        };
        let mut seen = Vec::with_capacity(6);
        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            if value.contains('\u{fffd}') || seen.contains(&key) {
                return Err(());
            }
            seen.push(key.clone());
            match key.as_ref() {
                "q" if value.len() <= 256 => out.query = value.to_lowercase(),
                "before" => out.before = Some(value.parse().map_err(|_| ())?),
                "limit" => {
                    out.limit = value.parse().map_err(|_| ())?;
                    if !(1..=100).contains(&out.limit) {
                        return Err(());
                    }
                }
                "method"
                    if !value.is_empty()
                        && value.len() <= 32
                        && value.bytes().all(|b| b.is_ascii_alphabetic()) =>
                {
                    out.method = Some(value.into_owned());
                }
                "status" => {
                    let status = value.parse().map_err(|_| ())?;
                    if !(100..=599).contains(&status) {
                        return Err(());
                    }
                    out.status = Some(status);
                }
                "state"
                    if matches!(
                        value.as_ref(),
                        "active" | "complete" | "incomplete" | "upgraded"
                    ) =>
                {
                    out.state = Some(value.into_owned());
                }
                _ => return Err(()),
            }
        }
        Ok(out)
    }
    pub fn matches(&self, r: &InspectionRecord) -> bool {
        if self.method.as_ref().is_some_and(|m| *m != r.method)
            || self.status.is_some_and(|s| r.status != Some(s))
            || self.state.as_ref().is_some_and(|s| *s != r.state.as_str())
        {
            return false;
        }
        let q = &self.query;
        r.method.to_lowercase().contains(q)
            || r.request_target.to_lowercase().contains(q)
            || r.capture_id.0.to_string().to_lowercase().contains(q)
            || [&r.request, &r.response].iter().any(|m| {
                m.headers.iter().any(|h| {
                    h.name.to_lowercase().contains(q) || h.value.to_lowercase().contains(q)
                }) || m
                    .body
                    .text
                    .as_ref()
                    .is_some_and(|s| s.to_lowercase().contains(q))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn handler_admission_precedes_buffers_and_shutdown_cancels_idle_clients() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let history = Inspection::new(super::super::PreviewMode::Off);
        let state = Arc::new(ServerState {
            history: history.clone(),
            token: Arc::new([b'a'; 64]),
            authority: addr.to_string(),
            origin: format!("http://{addr}"),
            handlers: Semaphore::new(4),
        });
        let (stop, rx) = oneshot::channel();
        let task = tokio::spawn(serve(listener, state.clone(), rx));
        let mut sockets = Vec::new();
        for _ in 0..4 {
            let mut socket = TcpStream::connect(addr).await.unwrap();
            socket.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
            sockets.push(socket);
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            while state.handlers.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut excess = TcpStream::connect(addr).await.unwrap();
        let mut bytes = [0u8; 1];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), excess.read(&mut bytes))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        // Service pressure never owns a forwarding permit or a history lock.
        let capture = history
            .admit(super::super::TunnelGeneration::new(), None)
            .unwrap();
        drop(capture);
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.handlers.available_permits(), 4);
        assert!(history.search("").is_empty());
        for mut socket in sockets {
            let _ = socket.read(&mut bytes).await;
        }
        assert!(TcpStream::connect(addr).await.is_err());
    }
    #[test]
    fn serialized_output_never_exceeds_64kib() {
        assert_eq!(json(&"x".repeat(65534)).body.len(), 65536);
        let capped = json(&"x".repeat(65535));
        assert_eq!(capped.status, 503);
        assert!(capped.body.len() < 1024);
    }
}
