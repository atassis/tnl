use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use futures::task::AtomicWaker;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::compat::TokioAsyncReadCompatExt as _;
use yamux::{Config, Connection, Mode};

/// Typed physical yamux outcome shared by outbound opening, exhausted acceptance,
/// and session cleanup. The first outcome is retained across all three observers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OpenStreamError {
    #[error("yamux transport closed before opening a stream")]
    ConnectionClosed,
    #[error("yamux transport I/O failed before opening a stream")]
    Io,
    #[error("yamux transport protocol failed before opening a stream")]
    Protocol,
    /// Native yamux TooManyStreams/NoMoreStreamIds: yamux has already entered
    /// fatal Cleanup/Closed. Local admission backpressure never produces this.
    #[error("yamux transport cannot open another stream")]
    Capacity,
    #[error("yamux inbound delivery capacity exhausted")]
    InboundOverflow,
}

impl OpenStreamError {
    const fn from_yamux(error: &yamux::ConnectionError) -> Self {
        match error {
            yamux::ConnectionError::Io(_)
            | yamux::ConnectionError::Decode(yamux::FrameDecodeError::Io(_)) => Self::Io,
            yamux::ConnectionError::Closed => Self::ConnectionClosed,
            yamux::ConnectionError::Decode(_) | yamux::ConnectionError::InvalidWindowUpdate => {
                Self::Protocol
            }
            yamux::ConnectionError::NoMoreStreamIds | yamux::ConnectionError::TooManyStreams => {
                Self::Capacity
            }
            _ => Self::Protocol,
        }
    }

    /// Whether this is an ordinary physical transport loss that can be retried.
    pub const fn is_retryable_transport(self) -> bool {
        matches!(self, Self::ConnectionClosed | Self::Io)
    }
}

struct DriverExit {
    sender: watch::Sender<Option<OpenStreamError>>,
    cause: Option<OpenStreamError>,
}

impl DriverExit {
    const fn new(sender: watch::Sender<Option<OpenStreamError>>) -> Self {
        Self {
            sender,
            cause: None,
        }
    }

    fn set(&mut self, cause: OpenStreamError) -> OpenStreamError {
        if let Some(first) = self.cause {
            return first;
        }
        self.cause = Some(cause);
        // Publish synchronously before sending or dropping any affected reply.
        // Another worker can resume open_stream at that exact wake, before this
        // driver's locals drop. Later Closed/None results are not a new cause.
        self.sender.send_replace(Some(cause));
        cause
    }
}

impl Drop for DriverExit {
    fn drop(&mut self) {
        self.set(OpenStreamError::ConnectionClosed);
    }
}

// Default yamux allows 512 active streams and stalls opening at 256 un-ACK'd
// streams. Keep at most 64 retained open requests TOTAL (channel + driver),
// split as channel32 + pending32, independent of that ACK stall. A selected
// action replaces one pending entry; no draining occurs until it is delivered.
// Caller futures waiting on channel reservation are not admitted requests.
// Inbound delivery holds at most one ACK backlog
// (256 streams). Callers normally serialize opening behind the Session mutex.
const OPEN_CHANNEL_CAPACITY: usize = 32;
const MAX_PENDING_OPEN: usize = 32;
const INBOUND_CAPACITY: usize = 256;
// Explicit yielding bounds ready work even with non-cooperative IO/channels.
const DRIVER_TURN_BUDGET: usize = 32;

type OpenReply = oneshot::Sender<Result<yamux::Stream, OpenStreamError>>;

#[derive(Debug)]
struct OpenRequest {
    reply: OpenReply,
}

// Closing before waking prevents a driver on another worker from observing a
// still-live receiver, returning Pending and losing the cancellation signal.
struct OpenReceiver {
    reply: oneshot::Receiver<Result<yamux::Stream, OpenStreamError>>,
    driver_waker: Arc<AtomicWaker>,
}

impl Drop for OpenReceiver {
    fn drop(&mut self) {
        self.reply.close();
        self.driver_waker.wake();
    }
}

/// A multiplexed substream — must behave as a bidirectional byte channel.
pub trait Stream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + ?Sized> Stream for T {}

/// Independent control of a physical session, usable while its Session is locked
/// or owned by an accept loop. Retaining this handle does not prevent normal drop.
#[derive(Clone, Debug)]
pub struct SessionTermination {
    graceful: tokio_util::sync::CancellationToken,
    force: tokio_util::sync::CancellationToken,
    completed: tokio_util::sync::CancellationToken,
    cause: watch::Receiver<Option<OpenStreamError>>,
}

/// Held by the physical connection owner until all its IO has been dropped.
#[derive(Debug)]
pub struct SessionCompletion {
    termination: SessionTermination,
    cause_tx: watch::Sender<Option<OpenStreamError>>,
}

impl Drop for SessionCompletion {
    fn drop(&mut self) {
        self.cause_tx.send_if_modified(|cause| {
            if cause.is_none() {
                *cause = Some(OpenStreamError::ConnectionClosed);
                true
            } else {
                false
            }
        });
        self.termination.completed.cancel();
    }
}

impl SessionTermination {
    /// Transport implementers must retain the completion guard in the IO owner,
    /// not in a public session handle that can outlive that owner.
    pub fn pair() -> (Self, SessionCompletion) {
        let (cause_tx, cause) = watch::channel(None);
        let handle = Self {
            graceful: tokio_util::sync::CancellationToken::new(),
            force: tokio_util::sync::CancellationToken::new(),
            completed: tokio_util::sync::CancellationToken::new(),
            cause,
        };
        (
            handle.clone(),
            SessionCompletion {
                termination: handle,
                cause_tx,
            },
        )
    }

    pub fn request_close(&self) {
        self.graceful.cancel();
    }

    /// Only for a conclusively failed physical connection, never normal EOF or
    /// a business/policy rejection. Healthy closure must keep flushing writes.
    pub fn force(&self) {
        self.force.cancel();
    }

    pub async fn close_requested(&self) {
        self.graceful.cancelled().await;
    }

    pub async fn forced(&self) {
        self.force.cancelled().await;
    }

    async fn close_or_drop_requested(&self, close_rx: &mut oneshot::Receiver<()>) {
        tokio::select! {
            _ = close_rx => {},
            () = self.close_requested() => {},
        }
    }

    /// Observes IO ownership release, not merely a close request or exit cause.
    pub async fn completed(&self) {
        self.completed.cancelled().await;
    }

    /// Gracefully request closure and await the first typed physical outcome.
    /// Stream EOF can wake before the driver publishes its cause; this must be
    /// awaited, not replaced by a snapshot. Healthy blocked close is unbounded.
    pub async fn close_and_observe_cause(&self) -> OpenStreamError {
        self.request_close();
        let mut cause = self.cause.clone();
        loop {
            let observed = *cause.borrow_and_update();
            if let Some(cause) = observed {
                return cause;
            }
            if cause.changed().await.is_err() {
                return OpenStreamError::ConnectionClosed;
            }
        }
    }

    pub async fn terminate_failed(&self) {
        self.request_close();
        if tokio::time::timeout(Duration::from_secs(2), self.completed())
            .await
            .is_err()
        {
            self.force();
            self.completed().await;
        }
    }
}

/// A logical session between a `tnl` client and the `tnld` daemon.
/// Provides substream multiplexing. Both ends can both open and accept.
#[async_trait]
pub trait Session: Send + Sync {
    /// Physical shutdown/completion control independent of exclusive session access.
    fn termination(&self) -> SessionTermination;

    /// Initiate a new outgoing substream toward the peer.
    async fn open_stream(&mut self) -> anyhow::Result<Pin<Box<dyn Stream>>>;

    /// Accept the next incoming substream initiated by the peer.
    /// Returns `None` on normal closure; physical failures remain typed errors.
    async fn accept_stream(&mut self) -> anyhow::Result<Option<Pin<Box<dyn Stream>>>>;

    /// Best-effort liveness check: opens a substream and shuts it down,
    /// returning approximate local elapsed time. Not a true round-trip RTT.
    async fn ping(&mut self) -> anyhow::Result<Duration>;

    /// Gracefully close the session.
    async fn close(&mut self) -> anyhow::Result<()>;
}

/// Yamux-backed multiplexed session over any byte-stream.
///
/// Role mapping: in the tnl/tnld system the **daemon side** is yamux `Mode::Client`
/// (it opens substreams when end-user requests arrive) and the **CLI side** is
/// yamux `Mode::Server` (it accepts substreams). This is intentional and opposite
/// to whoever dialed the underlying connection.
pub struct YamuxSession {
    termination: SessionTermination,
    /// Sender side for opening new outbound streams: we send a oneshot back-channel
    /// and the driver task calls `poll_new_outbound` on our behalf.
    open_tx: mpsc::Sender<OpenRequest>,
    open_waker: Arc<AtomicWaker>,
    /// The first typed terminal cause observed by the background driver.
    driver_exit: watch::Receiver<Option<OpenStreamError>>,
    /// Receiver side for inbound streams surfaced by the driver task.
    inbound_rx: mpsc::Receiver<yamux::Stream>,
    /// Sender for close signal to driver (explicit graceful close).
    close_tx: Option<oneshot::Sender<()>>,
    /// Driver task handle.  Dropping `YamuxSession` closes all channel senders,
    /// which the driver detects and uses to perform a graceful yamux `poll_close`
    /// before exiting.  We keep the handle (rather than calling `abort()`) so
    /// that buffered writes can be flushed. A blocked physical write may keep
    /// healthy close pending indefinitely; only explicit failure permits force.
    #[allow(dead_code)]
    driver: JoinHandle<()>,
}

impl Drop for YamuxSession {
    fn drop(&mut self) {
        // Dropping `close_tx` and `open_tx` (via field drop after this impl runs)
        // signals the driver to call poll_close and exit gracefully.  We do NOT
        // call self.driver.abort() here because that would race with the yamux
        // write-buffer flush: bytes written to a stream and shutdown()-ed would be
        // silently discarded if the driver task is cancelled before the frames
        // reach the underlying IO. A healthy close keeps driving poll_close;
        // blocked OS writes are not a reason to force it without proven failure.
    }
}

impl std::fmt::Debug for YamuxSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YamuxSession").finish_non_exhaustive()
    }
}

impl YamuxSession {
    fn start<T>(io: T, mode: Mode) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        // yamux::Connection requires `futures::io::{AsyncRead, AsyncWrite}`;
        // wrap the tokio IO resource with the tokio_util compat adapter.
        let conn = Connection::new(io.compat(), Config::default(), mode);

        let (open_tx, open_rx) = mpsc::channel(OPEN_CHANNEL_CAPACITY);
        let open_waker = Arc::new(AtomicWaker::new());
        let driver_waker = open_waker.clone();
        let (inbound_tx, inbound_rx) = mpsc::channel(INBOUND_CAPACITY);
        let (close_tx, close_rx) = oneshot::channel::<()>();
        let (termination, completion) = SessionTermination::pair();
        let driver_exit_tx = completion.cause_tx.clone();
        let driver_exit = termination.cause.clone();
        let driver_control = termination.clone();
        let driver = tokio::spawn(async move {
            let _completion = completion;
            // The select surrounds the whole driver, including any poll_close
            // await. Force must remain observable inside a blocked flush.
            let force_exit = driver_exit_tx.clone();
            let driver = driver_task(
                conn,
                open_rx,
                inbound_tx,
                close_rx,
                driver_exit_tx,
                &driver_control,
                &driver_waker,
            );
            tokio::pin!(driver);
            tokio::select! {
                biased;
                () = driver_control.forced() => {
                    // Publish before dropping pending replies, preserving R1's
                    // first-cause-before-wake rule even on explicit force.
                    force_exit.send_if_modified(|cause| {
                        if cause.is_none() {
                            *cause = Some(OpenStreamError::ConnectionClosed);
                            true
                        } else { false }
                    });
                }
                () = &mut driver => {},
            }
        });

        Self {
            termination,
            open_tx,
            open_waker,
            driver_exit,
            inbound_rx,
            close_tx: Some(close_tx),
            driver,
        }
    }

    pub fn new_client<T>(io: T) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        Self::start(io, Mode::Client)
    }

    pub fn new_server<T>(io: T) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        Self::start(io, Mode::Server)
    }
}

enum DriverAction {
    Progress,
    Outbound(OpenRequest, yamux::Result<yamux::Stream>),
    Inbound(Option<yamux::Result<yamux::Stream>>),
    Dropped,
}

fn poll_driver_action<T>(
    conn: &mut Connection<tokio_util::compat::Compat<T>>,
    open_rx: &mut mpsc::Receiver<OpenRequest>,
    pending_open: &mut VecDeque<OpenRequest>,
    outbound_first: bool,
    cx: &mut Context<'_>,
    open_waker: &AtomicWaker,
) -> Poll<DriverAction>
where
    T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    // Register before checking either stage: unread channel receiver drops
    // wake this even when pending is full and poll_recv cannot be called.
    open_waker.register(cx.waker());
    pending_open.retain_mut(|request| request.reply.poll_closed(cx).is_pending());
    let mut drained = 0;
    while drained < DRIVER_TURN_BUDGET && pending_open.len() < MAX_PENDING_OPEN {
        match open_rx.poll_recv(cx) {
            Poll::Ready(Some(request)) => {
                pending_open.push_back(request);
                drained += 1;
            }
            Poll::Ready(None) => return Poll::Ready(DriverAction::Dropped),
            Poll::Pending => break,
        }
    }
    // poll_closed registers the wake even when yamux is waiting on
    // ACKs. Checking is_closed alone would strand cancelled requests.
    if drained != 0 {
        pending_open.retain_mut(|request| request.reply.poll_closed(cx).is_pending());
    }

    // Alternate preference when both directions remain ready;
    // when outbound is ACK-blocked we still poll inbound in the
    // SAME invocation. Neither direction can monopolize the driver.
    for outbound in [outbound_first, !outbound_first] {
        if outbound {
            if let Some(request) = pending_open.front_mut() {
                if request.reply.poll_closed(cx).is_ready() {
                    pending_open.pop_front();
                    return Poll::Ready(DriverAction::Progress);
                }
                if let Poll::Ready(result) = conn.poll_new_outbound(cx) {
                    if let Some(request) = pending_open.pop_front() {
                        return Poll::Ready(DriverAction::Outbound(request, result));
                    }
                }
            }
        } else if let Poll::Ready(inbound) = conn.poll_next_inbound(cx) {
            return Poll::Ready(DriverAction::Inbound(inbound));
        }
    }
    // A batch of cancelled entries may leave space without polling recv to
    // Pending (and registering its wake). Reschedule once if admission made
    // progress and left space. Never self-wake merely because pending is full.
    if drained != 0 && pending_open.len() < MAX_PENDING_OPEN {
        cx.waker().wake_by_ref();
    }
    Poll::Pending
}

/// Background task that owns the `yamux::Connection` and drives it.
///
/// It concurrently:
/// - Polls `poll_next_inbound` to drive the connection state machine and surface inbound streams.
/// - Handles open-outbound requests by calling `poll_new_outbound` on the same connection.
///
/// The task exits when the connection closes or the close signal is received.
async fn driver_task<T>(
    mut conn: Connection<tokio_util::compat::Compat<T>>,
    mut open_rx: mpsc::Receiver<OpenRequest>,
    inbound_tx: mpsc::Sender<yamux::Stream>,
    mut close_rx: oneshot::Receiver<()>,
    driver_exit_tx: watch::Sender<Option<OpenStreamError>>,
    termination: &SessionTermination,
    open_waker: &AtomicWaker,
) where
    T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let mut exit = DriverExit::new(driver_exit_tx);
    let mut pending_open: VecDeque<OpenRequest> = VecDeque::new();
    let mut turns = 0;
    let mut outbound_first = false;
    loop {
        // Each ready iteration admits <=32 requests, examines <=64 cancellations,
        // and polls each yamux direction once. After <=32 such iterations we
        // explicitly yield; no reliance on Tokio channel/IO cooperative budgets.
        if turns == DRIVER_TURN_BUDGET {
            tokio::task::yield_now().await;
            turns = 0;
        }
        turns += 1;
        let next_action = std::future::poll_fn(|cx| {
            poll_driver_action(
                &mut conn,
                &mut open_rx,
                &mut pending_open,
                outbound_first,
                cx,
                open_waker,
            )
        });
        let action = tokio::select! {
            biased;
            () = termination.close_or_drop_requested(&mut close_rx) => {
                close_connection(&mut conn, &mut exit).await;
                return;
            }
            action = next_action => action,
        };
        outbound_first = !outbound_first;
        match action {
            DriverAction::Progress => {}
            DriverAction::Dropped => {
                close_connection(&mut conn, &mut exit).await;
                return;
            }
            DriverAction::Outbound(request, result) => {
                let result = result.map_err(|error| exit.set(OpenStreamError::from_yamux(&error)));
                // Ready(Ok) is the physical-open linearization point. If the
                // receiver races that point, dropping the undelivered stream
                // uses yamux's normal disposal; no application bytes are sent.
                // We cannot promise zero SYNs after an already committed open.
                let _ = request.reply.send(result);
            }
            DriverAction::Inbound(Some(Ok(stream))) => {
                match inbound_tx.try_send(stream) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        close_connection(&mut conn, &mut exit).await;
                        return;
                    }
                    Err(mpsc::error::TrySendError::Full(stream)) => {
                        // An overflowing physical session is conclusively failed,
                        // unlike ordinary outbound admission pressure. Publish
                        // before waking openers and independently of acceptance.
                        exit.set(OpenStreamError::InboundOverflow);
                        open_rx.close();
                        drop(pending_open);
                        while let Ok(request) = open_rx.try_recv() {
                            drop(request);
                        }
                        drop(stream);
                        // Only this explicit failed-session path has a deadline.
                        // Keep healthy queued writes draining for the existing
                        // two-second cleanup budget, then expose force to bridge
                        // owners as well as dropping this physical connection.
                        if tokio::time::timeout(
                            Duration::from_secs(2),
                            close_connection(&mut conn, &mut exit),
                        )
                        .await
                        .is_err()
                        {
                            termination.force();
                        }
                        return;
                    }
                }
            }
            DriverAction::Inbound(Some(Err(error))) => {
                let cause = exit.set(OpenStreamError::from_yamux(&error));
                if let Some(request) = pending_open.pop_front() {
                    let _ = request.reply.send(Err(cause));
                }
                return;
            }
            DriverAction::Inbound(None) => {
                let cause = exit.set(OpenStreamError::ConnectionClosed);
                // Preserve the first outbound cause for queued requests too.
                for request in pending_open {
                    let _ = request.reply.send(Err(cause));
                }
                return;
            }
        }
    }
}

async fn close_connection<T>(
    conn: &mut Connection<tokio_util::compat::Compat<T>>,
    exit: &mut DriverExit,
) where
    T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    // Current vendored Cleanup completes within the poll that closed/woke
    // substreams. Its error is handled before the driver can select close.
    // Never infer the cause from a substream EOF or discard poll_close IO errors.
    let cause = match std::future::poll_fn(|cx| conn.poll_close(cx)).await {
        Ok(()) => OpenStreamError::ConnectionClosed,
        Err(error) => OpenStreamError::from_yamux(&error),
    };
    exit.set(cause);
}

impl YamuxSession {
    fn driver_exit_cause(&self) -> OpenStreamError {
        (*self.driver_exit.borrow()).unwrap_or(OpenStreamError::ConnectionClosed)
    }
}

#[async_trait]
impl Session for YamuxSession {
    fn termination(&self) -> SessionTermination {
        self.termination.clone()
    }

    async fn open_stream(&mut self) -> anyhow::Result<Pin<Box<dyn Stream>>> {
        // reserve() is cancellation-safe: no request exists while capacity is
        // unavailable. Admission linearizes at send(), without an intervening
        // await; a dropped capacity waiter cannot enqueue later.
        let admission = self
            .open_tx
            .reserve()
            .await
            .map_err(|_| anyhow::Error::new(self.driver_exit_cause()))?;
        let (reply_tx, reply_rx) = oneshot::channel();
        let mut receiver = OpenReceiver {
            reply: reply_rx,
            driver_waker: self.open_waker.clone(),
        };
        admission.send(OpenRequest { reply: reply_tx });
        let stream = (&mut receiver.reply)
            .await
            .map_err(|_| anyhow::Error::new(self.driver_exit_cause()))??;
        Ok(Box::pin(YamuxStreamCompat::new(stream)))
    }

    async fn accept_stream(&mut self) -> anyhow::Result<Option<Pin<Box<dyn Stream>>>> {
        if let Some(stream) = self.inbound_rx.recv().await {
            return Ok(Some(Box::pin(YamuxStreamCompat::new(stream))));
        }
        // DriverExit publishes before inbound_tx is dropped. A closed channel
        // is not proof of healthy EOF: preserve the physical first cause.
        match self.driver_exit_cause() {
            OpenStreamError::ConnectionClosed => Ok(None),
            cause => Err(cause.into()),
        }
    }

    async fn ping(&mut self) -> anyhow::Result<Duration> {
        use tokio::io::AsyncWriteExt as _;
        // yamux 0.13 has no app-level ping; we approximate via open+shutdown.
        let start = std::time::Instant::now();
        let mut s = self.open_stream().await?;
        s.shutdown().await?;
        Ok(start.elapsed())
    }

    async fn close(&mut self) -> anyhow::Result<()> {
        if let Some(tx) = self.close_tx.take() {
            let _ = tx.send(());
        }
        Ok(())
    }
}

// ─── WebSocket helpers ────────────────────────────────────────────────────────
//
// `ws_stream_tungstenite::WsStream` adapts an `async_tungstenite::WebSocketStream`
// (message-level Sink+Stream) to a byte-level `AsyncRead+AsyncWrite` suitable for
// yamux.  We use `async-tungstenite` (with the `tokio-runtime` feature) rather than
// `tokio-tungstenite` because `WsStream` was written against `async_tungstenite`'s
// `WebSocketStream` type.  The two crates share the same underlying `tungstenite`
// library but expose incompatible wrapper types.
//
// Role mapping (repeated here for visibility):
//   • tnld daemon  → yamux `Mode::Client` (opens substreams) → `new_client`
//   • tnl CLI      → yamux `Mode::Server` (accepts substreams) → `new_server`

use async_tungstenite::{tokio::TokioAdapter, WebSocketStream};
use tokio::net::TcpStream;
use ws_stream_tungstenite::WsStream;

/// Build a yamux session in **daemon role** (opens substreams) from a server-accepted
/// WebSocket connection on a plain TCP stream.
///
/// The caller is expected to have performed the WebSocket handshake already
/// (e.g. via `async_tungstenite::tokio::accept_async`).
pub fn server_session_from_ws(ws: WebSocketStream<TokioAdapter<TcpStream>>) -> YamuxSession {
    YamuxSession::new_client(WsStream::new(ws))
}

/// Build a yamux session in **CLI role** (accepts substreams) from a client-dialed
/// WebSocket connection.
///
/// The caller is expected to have performed the WebSocket handshake already
/// (e.g. via `async_tungstenite::tokio::connect_async`).
pub fn client_session_from_ws(ws: WebSocketStream<TokioAdapter<TcpStream>>) -> YamuxSession {
    YamuxSession::new_server(WsStream::new(ws))
}

/// Build a yamux session in **daemon role** from any `WebSocketStream`.
///
/// Used by `tnld` when the WebSocket was obtained via axum's extractor
/// (which gives a `TokioAdapter<DuplexStream>` after the bridge).
pub fn server_session_from_ws_generic<S>(ws: WebSocketStream<S>) -> YamuxSession
where
    S: futures::io::AsyncRead + futures::io::AsyncWrite + Send + Unpin + 'static,
{
    YamuxSession::new_client(WsStream::new(ws))
}

/// Build a yamux session in **CLI role** from any `WebSocketStream`.
///
/// Mirror of [`client_session_from_ws`] for non-`TcpStream` inner IO.
pub fn client_session_from_ws_generic<S>(ws: WebSocketStream<S>) -> YamuxSession
where
    S: futures::io::AsyncRead + futures::io::AsyncWrite + Send + Unpin + 'static,
{
    YamuxSession::new_server(WsStream::new(ws))
}

// ─────────────────────────────────────────────────────────────────────────────

/// Adapter wrapping `yamux::Stream` to expose tokio's `AsyncRead+AsyncWrite`.
struct YamuxStreamCompat {
    inner: tokio_util::compat::Compat<yamux::Stream>,
}

impl std::fmt::Debug for YamuxStreamCompat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YamuxStreamCompat").finish_non_exhaustive()
    }
}

impl YamuxStreamCompat {
    fn new(s: yamux::Stream) -> Self {
        use tokio_util::compat::FuturesAsyncReadCompatExt as _;
        Self { inner: s.compat() }
    }
}

impl AsyncRead for YamuxStreamCompat {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for YamuxStreamCompat {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time assertion that the trait is object-safe.
    #[allow(dead_code)]
    fn _is_object_safe(_: Box<dyn Session>) {}
}

#[cfg(test)]
mod yamux_tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    use futures::task::AtomicWaker;
    use futures::FutureExt as _;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, ReadBuf};
    use tokio::sync::Notify;

    use super::*;

    #[derive(Default)]
    struct IoProbe {
        block_writes: AtomicBool,
        blocked_polls: AtomicUsize,
        write_blocked: Notify,
        write_waker: AtomicWaker,
        read_error: AtomicBool,
        read_waker: AtomicWaker,
    }

    impl IoProbe {
        fn unblock_writes(&self) {
            self.block_writes.store(false, Ordering::SeqCst);
            self.write_waker.wake();
        }

        fn fail_reads(&self) {
            self.read_error.store(true, Ordering::SeqCst);
            self.read_waker.wake();
        }
    }

    struct ObservedIo {
        inner: DuplexStream,
        probe: Arc<IoProbe>,
        released: Option<oneshot::Sender<()>>,
    }

    impl Drop for ObservedIo {
        fn drop(&mut self) {
            let _ = self.released.take().unwrap().send(());
        }
    }

    impl AsyncRead for ObservedIo {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            self.probe.read_waker.register(cx.waker());
            if self.probe.read_error.load(Ordering::SeqCst) {
                return Poll::Ready(Err(std::io::ErrorKind::ConnectionReset.into()));
            }
            Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for ObservedIo {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.probe.write_waker.register(cx.waker());
            if self.probe.block_writes.load(Ordering::SeqCst) {
                self.probe.blocked_polls.fetch_add(1, Ordering::SeqCst);
                self.probe.write_blocked.notify_one();
                return Poll::Pending;
            }
            Pin::new(&mut self.inner).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    struct ObservedPair {
        local: YamuxSession,
        peer: YamuxSession,
        probe: Arc<IoProbe>,
        released: oneshot::Receiver<()>,
    }

    impl ObservedPair {
        fn new(mode: Mode) -> Self {
            let (a, b) = tokio::io::duplex(64 * 1024);
            let probe = Arc::new(IoProbe::default());
            let (released_tx, released) = oneshot::channel();
            let local = YamuxSession::start(
                ObservedIo {
                    inner: a,
                    probe: probe.clone(),
                    released: Some(released_tx),
                },
                mode,
            );
            let peer = YamuxSession::start(
                b,
                match mode {
                    Mode::Client => Mode::Server,
                    Mode::Server => Mode::Client,
                },
            );
            Self {
                local,
                peer,
                probe,
                released,
            }
        }

        async fn streams(
            &mut self,
            local_opens: bool,
        ) -> (Pin<Box<dyn Stream>>, Pin<Box<dyn Stream>>) {
            let (opener, accepter) = if local_opens {
                (&mut self.local, &mut self.peer)
            } else {
                (&mut self.peer, &mut self.local)
            };
            let mut outgoing = opener.open_stream().await.unwrap();
            outgoing.write_all(b"x").await.unwrap();
            let mut incoming = accepter.accept_stream().await.unwrap().unwrap();
            incoming.read_exact(&mut [0; 1]).await.unwrap();
            if local_opens {
                (outgoing, incoming)
            } else {
                (incoming, outgoing)
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn yamux_substream_roundtrip_1mb() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        // Daemon role: Client; CLI role: Server (see role mapping note above).
        let mut daemon = YamuxSession::new_client(a);
        let mut cli = YamuxSession::new_server(b);

        let cli_handle = tokio::spawn(async move {
            let mut s = cli.accept_stream().await.unwrap().unwrap();
            let mut buf = vec![0u8; 1024 * 1024];
            AsyncReadExt::read_exact(&mut s, &mut buf).await.unwrap();
            assert!(buf.iter().all(|&b| b == 0x42));
            s.write_all(b"ack").await.unwrap();
            s.shutdown().await.unwrap();
        });

        let mut s = daemon.open_stream().await.unwrap();
        let payload = vec![0x42u8; 1024 * 1024];
        s.write_all(&payload).await.unwrap();
        let mut ack = [0u8; 3];
        AsyncReadExt::read_exact(&mut s, &mut ack).await.unwrap();
        assert_eq!(&ack, b"ack");

        cli_handle.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_then_accept_returns_none() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let mut daemon = YamuxSession::new_client(a);
        let mut cli = YamuxSession::new_server(b);

        // Daemon closes gracefully.
        daemon.close().await.unwrap();

        // CLI's next accept should now resolve to None.
        let next = tokio::time::timeout(std::time::Duration::from_secs(2), cli.accept_stream())
            .await
            .expect("accept did not resolve");
        assert!(
            matches!(next, Ok(None)),
            "expected Ok(None) after graceful close"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn yamux_over_wss_localhost_roundtrip() {
        use async_tungstenite::tokio::{accept_async, connect_async};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("ws://{addr}/control");

        // Oneshot: client signals server it has read the payload, server then drops its session.
        // This keeps the underlying WebSocket connection alive until the client is done,
        // preventing a race where the server closes TCP before the client can respond to
        // the yamux RTT Ping (which would cause a Broken-pipe write error in the client driver).
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

        let server_task = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let ws = accept_async(sock).await.unwrap();
            let mut session = crate::transport::server_session_from_ws(ws); // daemon role
            let mut s = session.open_stream().await.unwrap();
            s.write_all(b"hello from daemon").await.unwrap();
            s.shutdown().await.unwrap();
            // Wait until the client confirms it finished reading before the session drops.
            let _ = done_rx.await;
        });

        let (ws, _) = connect_async(url).await.unwrap();
        // `ws` is `WebSocketStream<TokioAdapter<TcpStream>>` (plain ws://, no TLS).
        // Build a CLI-side yamux session.
        let mut session = crate::transport::client_session_from_ws_generic(ws); // cli role
        let mut s = session.accept_stream().await.unwrap().unwrap();
        let mut buf = vec![0u8; b"hello from daemon".len()];
        AsyncReadExt::read_exact(&mut s, &mut buf).await.unwrap();
        assert_eq!(buf, b"hello from daemon");
        // Signal the server that the client is done; the server session can now drop.
        let _ = done_tx.send(());

        server_task.await.expect("server task panicked");
    }

    async fn local_shutdown_wakes_pending_reader(
        mode: Mode,
        explicit_close: bool,
        local_opens: bool,
    ) {
        let mut pair = ObservedPair::new(mode);
        let (mut local_stream, _peer_stream) = pair.streams(local_opens).await;
        let ObservedPair {
            mut local,
            peer: _peer,
            released,
            ..
        } = pair;

        let (pending_tx, pending_rx) = oneshot::channel();
        let reader = tokio::spawn(async move {
            let mut byte = [0; 1];
            let mut read = Box::pin(local_stream.read(&mut byte));
            assert!(futures::poll!(&mut read).is_pending());
            pending_tx.send(()).unwrap();
            read.await
        });
        pending_rx.await.unwrap();
        if explicit_close {
            local.close().await.unwrap();
        } else {
            drop(local);
        }
        // Time out the task handle, not the read: a timeout's final re-poll of
        // the read itself can hide a missing wakeup by producing WriteZero.
        let result = tokio::time::timeout(Duration::from_secs(2), reader)
            .await
            .expect("local stream reader hung after session shutdown")
            .unwrap();
        assert!(matches!(result, Ok(0) | Err(_)));
        tokio::time::timeout(Duration::from_secs(2), released)
            .await
            .expect("driver retained IO after shutdown")
            .unwrap();
    }

    #[tokio::test]
    async fn client_close_wakes_pending_local_reader() {
        local_shutdown_wakes_pending_reader(Mode::Client, true, true).await;
    }

    #[tokio::test]
    async fn server_close_wakes_pending_local_reader() {
        local_shutdown_wakes_pending_reader(Mode::Server, true, true).await;
    }

    #[tokio::test]
    async fn client_drop_wakes_pending_local_reader() {
        local_shutdown_wakes_pending_reader(Mode::Client, false, true).await;
    }

    #[tokio::test]
    async fn server_drop_wakes_pending_local_reader() {
        local_shutdown_wakes_pending_reader(Mode::Server, false, true).await;
    }

    #[tokio::test]
    async fn local_shutdown_wakes_pending_accepted_reader() {
        for mode in [Mode::Client, Mode::Server] {
            for explicit_close in [true, false] {
                local_shutdown_wakes_pending_reader(mode, explicit_close, false).await;
            }
        }
    }

    async fn buffered_reads_after_shutdown(mode: Mode, explicit_close: bool) {
        for local_opens in [true, false] {
            let mut pair = ObservedPair::new(mode);
            let (mut local_stream, mut peer_stream) = pair.streams(local_opens).await;
            peer_stream.write_all(b"buffered").await.unwrap();
            // A single yamux data frame has arrived; leave its remaining bytes buffered.
            let mut first = [0; 1];
            local_stream.read_exact(&mut first).await.unwrap();
            assert_eq!(&first, b"b");
            if explicit_close {
                pair.local.close().await.unwrap();
            } else {
                drop(pair.local);
            }
            tokio::time::timeout(Duration::from_secs(2), pair.released)
                .await
                .expect("driver did not release IO")
                .unwrap();
            let mut rest = Vec::new();
            tokio::time::timeout(Duration::from_secs(2), local_stream.read_to_end(&mut rest))
                .await
                .expect("buffered stream reader hung after session shutdown")
                .unwrap();
            assert_eq!(rest, b"uffered");
        }
    }

    #[tokio::test]
    async fn local_close_preserves_buffered_reads() {
        buffered_reads_after_shutdown(Mode::Client, true).await;
    }

    #[tokio::test]
    async fn server_close_preserves_buffered_reads() {
        buffered_reads_after_shutdown(Mode::Server, true).await;
    }

    #[tokio::test]
    async fn client_drop_preserves_buffered_reads() {
        buffered_reads_after_shutdown(Mode::Client, false).await;
    }

    #[tokio::test]
    async fn server_drop_preserves_buffered_reads() {
        buffered_reads_after_shutdown(Mode::Server, false).await;
    }

    #[tokio::test]
    async fn local_shutdown_wakes_flow_control_blocked_writer() {
        for mode in [Mode::Client, Mode::Server] {
            for explicit_close in [true, false] {
                let mut pair = ObservedPair::new(mode);
                let (mut local_stream, _peer_stream) = pair.streams(true).await;
                // One byte of initial credit was consumed establishing the stream.
                local_stream
                    .write_all(&vec![0x42; yamux::DEFAULT_CREDIT as usize - 1])
                    .await
                    .unwrap();
                let (pending_tx, pending_rx) = oneshot::channel();
                let writer = tokio::spawn(async move {
                    let mut write = Box::pin(local_stream.write(b"x"));
                    assert!(futures::poll!(&mut write).is_pending());
                    pending_tx.send(()).unwrap();
                    write.await
                });
                pending_rx.await.unwrap();
                if explicit_close {
                    pair.local.close().await.unwrap();
                } else {
                    drop(pair.local);
                }
                let result = tokio::time::timeout(Duration::from_secs(2), writer)
                    .await
                    .expect("flow-control-blocked writer was not woken")
                    .unwrap();
                assert!(result.is_err());
                tokio::time::timeout(Duration::from_secs(2), pair.released)
                    .await
                    .expect("driver retained IO after writer shutdown")
                    .unwrap();
            }
        }
    }

    #[tokio::test]
    async fn local_shutdown_flushes_queued_writes_without_spinning_or_cancellation() {
        for mode in [Mode::Client, Mode::Server] {
            // Explicit close, drop, and dropping the session during an in-flight close.
            for shutdown in 0..3 {
                let mut pair = ObservedPair::new(mode);
                let (mut local_stream, mut peer_stream) = pair.streams(true).await;
                pair.probe.block_writes.store(true, Ordering::SeqCst);
                let payload = vec![0x42; 8192];
                local_stream.write_all(&payload).await.unwrap();
                local_stream.shutdown().await.unwrap();
                if shutdown != 1 {
                    pair.local.close().await.unwrap();
                }
                if shutdown != 0 {
                    drop(pair.local);
                }
                tokio::time::timeout(Duration::from_secs(2), pair.probe.write_blocked.notified())
                    .await
                    .expect("close did not encounter the write gate");
                assert!(matches!(
                    pair.released.try_recv(),
                    Err(oneshot::error::TryRecvError::Empty)
                ));
                // A genuinely blocked close must wait for IO, not return false-ready
                // and repeatedly poll the socket. Allow initial close-transition wakes.
                tokio::time::sleep(Duration::from_millis(20)).await;
                let polls = pair.probe.blocked_polls.load(Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                assert_eq!(pair.probe.blocked_polls.load(Ordering::SeqCst), polls);
                pair.probe.unblock_writes();
                let mut received = Vec::new();
                tokio::time::timeout(
                    Duration::from_secs(2),
                    peer_stream.read_to_end(&mut received),
                )
                .await
                .expect("peer did not receive queued writes and EOF")
                .unwrap();
                assert_eq!(received, payload);
                tokio::time::timeout(Duration::from_secs(2), pair.released)
                    .await
                    .expect("driver did not release IO after flushing")
                    .unwrap();
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn failed_termination_forces_blocked_close_after_cleanup_budget() {
        for mode in [Mode::Client, Mode::Server] {
            let mut pair = ObservedPair::new(mode);
            let (mut stream, _peer_stream) = pair.streams(true).await;
            let termination = pair.local.termination();
            pair.probe.block_writes.store(true, Ordering::SeqCst);
            stream.write_all(b"queued").await.unwrap();
            stream.shutdown().await.unwrap();
            // A retained lifecycle handle must not inhibit normal drop closure.
            drop(pair.local);
            pair.probe.write_blocked.notified().await;
            assert!(futures::poll!(termination.completed().boxed()).is_pending());
            tokio::time::advance(Duration::from_secs(60)).await;
            assert!(
                pair.released.try_recv().is_err(),
                "healthy close was forced"
            );
            let mut cleanup = Box::pin(termination.terminate_failed());
            assert!(futures::poll!(&mut cleanup).is_pending());
            tokio::time::advance(Duration::from_millis(1999)).await;
            assert!(futures::poll!(&mut cleanup).is_pending());
            assert!(pair.released.try_recv().is_err());
            tokio::time::advance(Duration::from_millis(1)).await;
            tokio::time::timeout(Duration::from_secs(1), cleanup)
                .await
                .expect("force not observed inside blocked poll_close");
            pair.released
                .try_recv()
                .expect("completion preceded IO release");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn failed_termination_is_independent_of_locked_session_accept_owner() {
        let mut pair = ObservedPair::new(Mode::Server);
        let (mut stream, _peer_stream) = pair.streams(false).await;
        pair.probe.block_writes.store(true, Ordering::SeqCst);
        stream.write_all(b"queued").await.unwrap();
        stream.shutdown().await.unwrap();
        let session = tokio::sync::Mutex::new(Box::new(pair.local) as Box<dyn Session>);
        let mut owner = session.lock().await;
        let termination = owner.termination();
        let mut accept = Box::pin(owner.accept_stream());
        assert!(futures::poll!(&mut accept).is_pending());
        termination.request_close();
        pair.probe.write_blocked.notified().await;
        tokio::time::timeout(Duration::from_secs(3), termination.terminate_failed())
            .await
            .expect("failed cleanup required the session mutex");
        pair.released
            .try_recv()
            .expect("completion preceded IO release");
        assert!(
            session.try_lock().is_err(),
            "fixture lost exclusive accept ownership"
        );
        drop(accept);
        drop(owner);
    }

    #[tokio::test]
    async fn cancelled_stream_read_does_not_prevent_shutdown() {
        for mode in [Mode::Client, Mode::Server] {
            for explicit_close in [true, false] {
                let mut pair = ObservedPair::new(mode);
                let (mut local_stream, _peer_stream) = pair.streams(true).await;
                let (pending_tx, pending_rx) = oneshot::channel();
                let (cancel_tx, cancel_rx) = oneshot::channel();
                let reader = tokio::spawn(async move {
                    {
                        let mut byte = [0; 1];
                        let mut read = Box::pin(local_stream.read(&mut byte));
                        assert!(futures::poll!(&mut read).is_pending());
                        pending_tx.send(()).unwrap();
                        tokio::select! {
                            _ = read => panic!("read ended before cancellation"),
                            _ = cancel_rx => {}
                        }
                    }
                    local_stream
                });
                pending_rx.await.unwrap();
                cancel_tx.send(()).unwrap();
                let mut stream = reader.await.unwrap();
                if explicit_close {
                    pair.local.close().await.unwrap();
                } else {
                    drop(pair.local);
                }
                tokio::time::timeout(Duration::from_secs(2), pair.released)
                    .await
                    .expect("cancelled read retained driver IO")
                    .unwrap();
                assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 0);
            }
        }
    }

    #[tokio::test]
    async fn remote_shutdown_wakes_pending_reader_and_preserves_buffered_reads() {
        for mode in [Mode::Client, Mode::Server] {
            for explicit_close in [true, false] {
                let mut pair = ObservedPair::new(mode);
                let (mut pending_stream, _peer_pending_stream) = pair.streams(true).await;
                let (mut buffered_stream, mut peer_buffered_stream) = pair.streams(false).await;
                peer_buffered_stream.write_all(b"buffered").await.unwrap();
                buffered_stream.read_exact(&mut [0; 1]).await.unwrap();
                let (pending_tx, pending_rx) = oneshot::channel();
                let reader = tokio::spawn(async move {
                    let mut byte = [0; 1];
                    let mut read = Box::pin(pending_stream.read(&mut byte));
                    assert!(futures::poll!(&mut read).is_pending());
                    pending_tx.send(()).unwrap();
                    read.await
                });
                pending_rx.await.unwrap();
                if explicit_close {
                    pair.peer.close().await.unwrap();
                } else {
                    drop(pair.peer);
                }
                let result = tokio::time::timeout(Duration::from_secs(2), reader)
                    .await
                    .expect("reader hung after remote shutdown")
                    .unwrap();
                assert_eq!(result.unwrap(), 0);
                tokio::time::timeout(Duration::from_secs(2), pair.released)
                    .await
                    .expect("driver retained IO after remote shutdown")
                    .unwrap();
                let mut buffered = Vec::new();
                buffered_stream.read_to_end(&mut buffered).await.unwrap();
                assert_eq!(buffered, b"uffered");
            }
        }
    }

    #[tokio::test]
    async fn transport_error_wakes_pending_reader_and_preserves_buffered_reads() {
        for mode in [Mode::Client, Mode::Server] {
            let mut pair = ObservedPair::new(mode);
            let (mut pending_stream, _peer_pending_stream) = pair.streams(true).await;
            let (mut buffered_stream, mut peer_buffered_stream) = pair.streams(false).await;
            peer_buffered_stream.write_all(b"buffered").await.unwrap();
            buffered_stream.read_exact(&mut [0; 1]).await.unwrap();
            let (pending_tx, pending_rx) = oneshot::channel();
            let reader = tokio::spawn(async move {
                let mut byte = [0; 1];
                let mut read = Box::pin(pending_stream.read(&mut byte));
                assert!(futures::poll!(&mut read).is_pending());
                pending_tx.send(()).unwrap();
                read.await
            });
            pending_rx.await.unwrap();
            pair.probe.fail_reads();
            let result = tokio::time::timeout(Duration::from_secs(2), reader)
                .await
                .expect("reader hung after transport error")
                .unwrap();
            assert_eq!(result.unwrap(), 0);
            tokio::time::timeout(Duration::from_secs(2), pair.released)
                .await
                .expect("driver retained IO after transport error")
                .unwrap();
            let mut buffered = Vec::new();
            buffered_stream.read_to_end(&mut buffered).await.unwrap();
            assert_eq!(buffered, b"uffered");
        }
    }

    #[test]
    fn yamux_open_failures_keep_transport_and_protocol_categories_distinct() {
        assert_eq!(
            OpenStreamError::from_yamux(&yamux::ConnectionError::Closed),
            OpenStreamError::ConnectionClosed
        );
        assert_eq!(
            OpenStreamError::from_yamux(&yamux::ConnectionError::Io(std::io::Error::other("io"))),
            OpenStreamError::Io
        );
        assert_eq!(
            OpenStreamError::from_yamux(&yamux::ConnectionError::Decode(
                yamux::FrameDecodeError::Io(std::io::Error::other("io")),
            )),
            OpenStreamError::Io
        );
        assert_eq!(
            OpenStreamError::from_yamux(&yamux::ConnectionError::InvalidWindowUpdate),
            OpenStreamError::Protocol
        );
        assert_eq!(
            OpenStreamError::from_yamux(&yamux::ConnectionError::TooManyStreams),
            OpenStreamError::Capacity
        );
    }

    #[test]
    fn driver_exit_publishes_protocol_before_request_channels_can_drop() {
        let (tx, rx) = watch::channel(None);
        let mut exit = DriverExit::new(tx);
        exit.set(OpenStreamError::Protocol);
        assert_eq!(*rx.borrow(), Some(OpenStreamError::Protocol));
    }

    #[test]
    fn driver_exit_keeps_first_root_cause_after_ordinary_closure() {
        for cause in [OpenStreamError::Protocol, OpenStreamError::Capacity] {
            let (tx, rx) = watch::channel(None);
            let mut exit = DriverExit::new(tx);
            exit.set(cause);
            exit.set(OpenStreamError::ConnectionClosed);
            drop(exit);
            assert_eq!(*rx.borrow(), Some(cause));
        }
    }

    struct ReadyInboundIo {
        incoming: Arc<std::sync::Mutex<std::io::Cursor<Vec<u8>>>>,
        written: Arc<std::sync::Mutex<Vec<u8>>>,
        block_writes: Arc<AtomicBool>,
        fail_writes: Arc<AtomicBool>,
        fail_reads: bool,
    }

    impl AsyncRead for ReadyInboundIo {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.fail_reads {
                return Poll::Ready(Err(std::io::ErrorKind::ConnectionReset.into()));
            }
            let n = std::io::Read::read(
                &mut *self.incoming.lock().unwrap(),
                buf.initialize_unfilled(),
            )
            .unwrap();
            if n == 0 {
                return Poll::Pending;
            }
            buf.advance(n);
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncWrite for ReadyInboundIo {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            if self.fail_writes.load(Ordering::SeqCst) {
                return Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()));
            }
            if self.block_writes.load(Ordering::SeqCst) {
                return Poll::Pending;
            }
            self.written.lock().unwrap().extend_from_slice(bytes);
            Poll::Ready(Ok(bytes.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    const GOAWAY: [u8; 12] = [0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

    fn ready_inbound_io(bytes: Vec<u8>, fail_writes: bool, fail_reads: bool) -> ReadyInboundIo {
        ReadyInboundIo {
            incoming: Arc::new(std::sync::Mutex::new(std::io::Cursor::new(bytes))),
            written: Arc::default(),
            block_writes: Arc::default(),
            fail_writes: Arc::new(AtomicBool::new(fail_writes)),
            fail_reads,
        }
    }

    #[test]
    fn ready_peer_goaway_qualifies_concurrent_write_error_as_normal_close() {
        for fail_at_ready in [false, true] {
            let io = ready_inbound_io(
                if fail_at_ready {
                    vec![]
                } else {
                    GOAWAY.to_vec()
                },
                !fail_at_ready,
                false,
            );
            let incoming = io.incoming.clone();
            let block_writes = io.block_writes.clone();
            let fail_writes = io.fail_writes.clone();
            block_writes.store(fail_at_ready, Ordering::SeqCst);
            let mut connection = Connection::new(io.compat(), Config::default(), Mode::Client);
            let waker = futures::task::noop_waker();
            let mut cx = Context::from_waker(&waker);
            if fail_at_ready {
                // Leave the frame sink waiting on its pending ping header, so
                // the next poll fails at poll_ready rather than poll_flush.
                assert!(connection.poll_next_inbound(&mut cx).is_pending());
                incoming
                    .lock()
                    .unwrap()
                    .get_mut()
                    .extend_from_slice(&GOAWAY);
                fail_writes.store(true, Ordering::SeqCst);
            }
            assert!(
                matches!(connection.poll_next_inbound(&mut cx), Poll::Ready(None)),
                "ready graceful GoAway was replaced by a write error"
            );
        }
    }

    #[test]
    fn non_normal_goaway_preserves_write_error_at_readiness_and_flush() {
        let mut suppressed = Vec::new();
        for (flags, stream_id, reason) in [
            (0u16, 0u32, 1u32),
            (0, 0, 2),
            (0, 0, u32::MAX),
            (1, 0, 0),
            (2, 0, 0),
            (4, 0, 0),
            (8, 0, 0),
            (0x8000, 0, 0),
            (0, 1, 0),
            (0, 2, 0),
        ] {
            let mut header = GOAWAY;
            header[2..4].copy_from_slice(&flags.to_be_bytes());
            header[4..8].copy_from_slice(&stream_id.to_be_bytes());
            header[8..12].copy_from_slice(&reason.to_be_bytes());
            for fail_at_ready in [false, true] {
                let io = ready_inbound_io(
                    if fail_at_ready {
                        vec![]
                    } else {
                        header.to_vec()
                    },
                    !fail_at_ready,
                    false,
                );
                let incoming = io.incoming.clone();
                let fail_writes = io.fail_writes.clone();
                io.block_writes.store(fail_at_ready, Ordering::SeqCst);
                let mut connection = Connection::new(io.compat(), Config::default(), Mode::Client);
                let waker = futures::task::noop_waker();
                let mut cx = Context::from_waker(&waker);
                if fail_at_ready {
                    assert!(connection.poll_next_inbound(&mut cx).is_pending());
                    incoming
                        .lock()
                        .unwrap()
                        .get_mut()
                        .extend_from_slice(&header);
                    fail_writes.store(true, Ordering::SeqCst);
                }
                match connection.poll_next_inbound(&mut cx) {
                    Poll::Ready(Some(Err(yamux::ConnectionError::Io(error)))) => {
                        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
                    }
                    _ => suppressed.push((flags, stream_id, reason, fail_at_ready)),
                }
            }
        }
        assert!(
            suppressed.is_empty(),
            "non-normal GoAway replaced write IO: {suppressed:?}"
        );
    }

    #[test]
    fn physical_errors_are_not_hidden_by_a_later_goaway() {
        let syn = [0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0];
        for (first, fail_write, fail_read, expected) in [
            (vec![], true, false, OpenStreamError::Io),
            (GOAWAY.to_vec(), false, true, OpenStreamError::Io),
            (
                [vec![1; 12], GOAWAY.to_vec()].concat(),
                false,
                false,
                OpenStreamError::Protocol,
            ),
            (
                [vec![1; 12], GOAWAY.to_vec()].concat(),
                true,
                false,
                OpenStreamError::Io,
            ),
            (
                [syn.to_vec(), GOAWAY.to_vec()].concat(),
                true,
                false,
                OpenStreamError::Io,
            ),
        ] {
            let mut connection = Connection::new(
                ready_inbound_io(first, fail_write, fail_read).compat(),
                Config::default(),
                Mode::Client,
            );
            let waker = futures::task::noop_waker();
            let Poll::Ready(Some(Err(error))) =
                connection.poll_next_inbound(&mut Context::from_waker(&waker))
            else {
                panic!("physical failure became graceful GoAway or pending");
            };
            assert_eq!(OpenStreamError::from_yamux(&error), expected);
        }
    }

    #[tokio::test]
    async fn ready_inbound_streams_preserve_write_progress_and_backpressure() {
        for blocked in [false, true] {
            let mut bytes = Vec::new();
            for id in (2u32..=64).step_by(2) {
                bytes.extend_from_slice(&[0, 0, 0, 1]);
                bytes.extend_from_slice(&id.to_be_bytes());
                bytes.extend_from_slice(&[0; 4]);
            }
            let io = ready_inbound_io(bytes, false, false);
            let written = io.written.clone();
            let gate = io.block_writes.clone();
            gate.store(blocked, Ordering::SeqCst);
            let mut connection = Connection::new(io.compat(), Config::default(), Mode::Client);
            let waker = futures::task::noop_waker();
            let mut cx = Context::from_waker(&waker);
            let Poll::Ready(Ok(stream)) = connection.poll_new_outbound(&mut cx) else {
                panic!("fixture could not open outgoing stream");
            };
            let mut stream = YamuxStreamCompat::new(stream);
            stream.write_all(b"queued outbound").await.unwrap();
            let mut incoming = Vec::new();
            for _ in 0..32 {
                let Poll::Ready(Some(Ok(stream))) = connection.poll_next_inbound(&mut cx) else {
                    panic!("ready reads starved behind write backpressure");
                };
                incoming.push(stream);
                if !blocked {
                    assert!(
                        written
                            .lock()
                            .unwrap()
                            .windows(15)
                            .any(|bytes| bytes == b"queued outbound"),
                        "ready inbound starved queued writes"
                    );
                }
            }
            if blocked {
                assert!(written.lock().unwrap().is_empty());
            }
            gate.store(false, Ordering::SeqCst);
            assert!(connection.poll_next_inbound(&mut cx).is_pending());
            assert!(written
                .lock()
                .unwrap()
                .windows(15)
                .any(|bytes| bytes == b"queued outbound"));
            drop(incoming);
        }
    }

    struct CloseDuringCleanup {
        termination: SessionTermination,
        observed: std::sync::Mutex<Vec<(Option<OpenStreamError>, bool)>>,
    }

    impl futures::task::ArcWake for CloseDuringCleanup {
        fn wake_by_ref(this: &Arc<Self>) {
            let before = *this.termination.cause.borrow();
            let mut close = Box::pin(this.termination.close_and_observe_cause());
            let waker = futures::task::noop_waker();
            let pending =
                std::future::Future::poll(close.as_mut(), &mut Context::from_waker(&waker))
                    .is_pending();
            this.observed.lock().unwrap().push((before, pending));
        }
    }

    #[tokio::test]
    async fn control_eof_wake_before_publication_cannot_replace_physical_root_with_close() {
        for cause in [OpenStreamError::Io, OpenStreamError::Protocol] {
            let (io, mut peer) = tokio::io::duplex(64 * 1024);
            let probe = Arc::new(IoProbe::default());
            let (released, _release) = oneshot::channel();
            let mut conn = Connection::new(
                ObservedIo {
                    inner: io,
                    probe: probe.clone(),
                    released: Some(released),
                }
                .compat(),
                Config::default(),
                Mode::Client,
            );
            // Actual even-ID CLI control stream, SYN plus one byte of data.
            peer.write_all(&[0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, b'x'])
                .await
                .unwrap();
            let stream = std::future::poll_fn(|cx| conn.poll_next_inbound(cx))
                .await
                .unwrap()
                .unwrap();
            let mut stream = YamuxStreamCompat::new(stream);
            stream.read_exact(&mut [0; 1]).await.unwrap();
            let (termination, completion) = SessionTermination::pair();
            let observer = Arc::new(CloseDuringCleanup {
                termination: termination.clone(),
                observed: std::sync::Mutex::new(Vec::new()),
            });
            let waker = futures::task::waker(observer.clone());
            assert!(Pin::new(&mut stream)
                .poll_read(
                    &mut Context::from_waker(&waker),
                    &mut ReadBuf::new(&mut [0; 1])
                )
                .is_pending());
            let (_open_tx, open_rx) = mpsc::channel(MAX_PENDING_OPEN);
            let (inbound_tx, _inbound_rx) = mpsc::channel(MAX_PENDING_OPEN);
            let (_close_tx, close_rx) = oneshot::channel();
            let open_waker = AtomicWaker::new();
            let mut driver = Box::pin(driver_task(
                conn,
                open_rx,
                inbound_tx,
                close_rx,
                completion.cause_tx.clone(),
                &termination,
                &open_waker,
            ));
            assert!(futures::poll!(&mut driver).is_pending());
            if cause == OpenStreamError::Io {
                probe.fail_reads();
            } else {
                peer.write_all(&[1; 12]).await.unwrap();
            }
            assert!(futures::poll!(&mut driver).is_ready());
            // This wake ran reentrantly INSIDE vendor cleanup, before its poll
            // returned to our driver. Close must wait, not infer normal EOF.
            assert_eq!(*observer.observed.lock().unwrap(), vec![(None, true)]);
            assert_eq!(termination.close_and_observe_cause().await, cause);
            drop(driver);
            drop(completion);
            assert_eq!(*termination.cause.borrow(), Some(cause));
        }
    }

    #[tokio::test]
    async fn accept_exhaustion_preserves_physical_errors_but_normal_close_is_none() {
        for mode in [Mode::Client, Mode::Server] {
            for cause in [OpenStreamError::Io, OpenStreamError::Protocol] {
                let (io, mut peer) = tokio::io::duplex(64 * 1024);
                let probe = Arc::new(IoProbe::default());
                let (released, _release) = oneshot::channel();
                let mut session = YamuxSession::start(
                    ObservedIo {
                        inner: io,
                        probe: probe.clone(),
                        released: Some(released),
                    },
                    mode,
                );
                if cause == OpenStreamError::Io {
                    probe.fail_reads();
                } else {
                    peer.write_all(&[1; 12]).await.unwrap();
                }
                let error = session
                    .accept_stream()
                    .await
                    .err()
                    .expect("physical failure became clean None");
                assert_eq!(error.downcast_ref::<OpenStreamError>(), Some(&cause));
                assert_eq!(session.termination().close_and_observe_cause().await, cause);
                session.close().await.unwrap();
                assert_eq!(
                    session
                        .accept_stream()
                        .await
                        .err()
                        .unwrap()
                        .downcast_ref::<OpenStreamError>(),
                    Some(&cause)
                );
            }
            let mut pair = ObservedPair::new(mode);
            pair.local.close().await.unwrap();
            assert!(pair.local.accept_stream().await.unwrap().is_none());
            assert_eq!(
                pair.local.termination().close_and_observe_cause().await,
                OpenStreamError::ConnectionClosed
            );
        }
    }

    #[tokio::test]
    async fn graceful_close_io_failure_is_observed_instead_of_normal_closure() {
        let (io, peer) = tokio::io::duplex(64 * 1024);
        drop(peer);
        let mut session = YamuxSession::new_client(io);
        // No yield before close: the driver's first branch is poll_close.
        session.close().await.unwrap();
        assert_eq!(
            session.termination().close_and_observe_cause().await,
            OpenStreamError::Io
        );
    }

    fn inbound_syns(count: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        for id in (2u32..).step_by(2).take(count) {
            bytes.extend_from_slice(&[0, 0, 0, 1]);
            bytes.extend_from_slice(&id.to_be_bytes());
            bytes.extend_from_slice(&[0; 4]);
        }
        bytes
    }

    #[tokio::test]
    async fn outbound_total_bound_includes_drained_requests_and_cancellation_reclaims_it() {
        let (io, mut peer) = tokio::io::duplex(64 * 1024);
        let mut conn = Connection::new(io.compat(), Config::default(), Mode::Client);
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        let held: Vec<_> = (0..256)
            .map(|_| {
                let Poll::Ready(Ok(stream)) = conn.poll_new_outbound(&mut cx) else {
                    panic!("could not fill ACK backlog");
                };
                stream
            })
            .collect();
        let (open_tx, open_rx) = mpsc::channel(OPEN_CHANNEL_CAPACITY);
        let (inbound_tx, _inbound_rx) = mpsc::channel(INBOUND_CAPACITY);
        let (_close_tx, close_rx) = oneshot::channel();
        let (termination, completion) = SessionTermination::pair();
        let open_waker = Arc::new(AtomicWaker::new());
        let mut pending_replies = Vec::new();
        for _ in 0..MAX_PENDING_OPEN {
            let (reply, receive) = oneshot::channel();
            open_tx.try_send(open_request(reply)).unwrap();
            pending_replies.push(receive);
        }
        let mut driver = Box::pin(driver_task(
            conn,
            open_rx,
            inbound_tx,
            close_rx,
            completion.cause_tx.clone(),
            &termination,
            &open_waker,
        ));
        let counter = Arc::new(CountWake::default());
        let waker = futures::task::waker(counter.clone());
        let mut driver_cx = Context::from_waker(&waker);
        assert!(std::future::Future::poll(driver.as_mut(), &mut driver_cx).is_pending());
        assert_eq!(open_tx.capacity(), OPEN_CHANNEL_CAPACITY);
        let mut queued_replies = Vec::new();
        for _ in 0..OPEN_CHANNEL_CAPACITY {
            let (reply, receive) = oneshot::channel();
            open_tx.try_send(open_request(reply)).unwrap();
            queued_replies.push(OpenReceiver {
                reply: receive,
                driver_waker: open_waker.clone(),
            });
        }
        assert_eq!(pending_replies.len() + queued_replies.len(), 64);
        assert!(
            open_tx.try_reserve().is_err(),
            "65th request must wait outside driver queues"
        );
        assert!(std::future::Future::poll(driver.as_mut(), &mut driver_cx).is_pending());
        assert_eq!(
            open_tx.capacity(),
            0,
            "full pending must not drain into an unbounded deque"
        );
        let before = counter.0.load(Ordering::SeqCst);
        drop(queued_replies);
        assert!(
            counter.0.load(Ordering::SeqCst) > before,
            "unread cancellation lost its wake"
        );
        let before = counter.0.load(Ordering::SeqCst);
        assert!(std::future::Future::poll(driver.as_mut(), &mut driver_cx).is_pending());
        assert_eq!(
            counter.0.load(Ordering::SeqCst),
            before,
            "full live pending must not self-wake/hot-loop"
        );
        assert_eq!(
            open_tx.capacity(),
            0,
            "canceled FIFO entries stay charged until pending frees"
        );
        assert_eq!(*termination.cause.borrow(), None);
        assert!(!termination.force.is_cancelled());
        drop(pending_replies);
        assert!(std::future::Future::poll(driver.as_mut(), &mut driver_cx).is_pending());
        assert_eq!(
            open_tx.capacity(),
            OPEN_CHANNEL_CAPACITY,
            "canceled entries were not reclaimed after pending freed"
        );
        let (reply, receive) = oneshot::channel();
        open_tx.try_send(open_request(reply)).unwrap();
        // A single ACK slot proves none of the 64 cancelled entries opened late.
        peer.write_all(&[0, 1, 0, 2, 0, 0, 0, 1, 0, 0, 0, 0])
            .await
            .unwrap();
        assert!(std::future::Future::poll(driver.as_mut(), &mut driver_cx).is_pending());
        assert!(receive.now_or_never().unwrap().unwrap().is_ok());
        assert_eq!(*termination.cause.borrow(), None);
        assert!(!termination.force.is_cancelled());
        drop(held);
    }

    #[tokio::test]
    async fn full_live_pending_still_drives_inbound_ack() {
        let io = ready_inbound_io(vec![], false, false);
        let incoming = io.incoming.clone();
        let mut conn = Connection::new(io.compat(), Config::default(), Mode::Client);
        let waker = futures::task::noop_waker();
        let held: Vec<_> = (0..256)
            .map(|_| {
                let Poll::Ready(Ok(stream)) =
                    conn.poll_new_outbound(&mut Context::from_waker(&waker))
                else {
                    panic!("could not fill ACK backlog");
                };
                stream
            })
            .collect();
        let (open_tx, open_rx) = mpsc::channel(OPEN_CHANNEL_CAPACITY);
        let (inbound_tx, _inbound_rx) = mpsc::channel(INBOUND_CAPACITY);
        let (_close_tx, close_rx) = oneshot::channel();
        let (termination, completion) = SessionTermination::pair();
        let mut live = Vec::new();
        for _ in 0..MAX_PENDING_OPEN {
            let (reply, receive) = oneshot::channel();
            open_tx.try_send(open_request(reply)).unwrap();
            live.push(receive);
        }
        let open_waker = AtomicWaker::new();
        let mut driver = Box::pin(driver_task(
            conn,
            open_rx,
            inbound_tx,
            close_rx,
            completion.cause_tx.clone(),
            &termination,
            &open_waker,
        ));
        assert!(futures::poll!(&mut driver).is_pending());
        assert_eq!(open_tx.capacity(), OPEN_CHANNEL_CAPACITY);
        for receive in &mut live {
            assert!(receive.try_recv().is_err());
        }
        // All 32 pending requests are live; no cancellation frees a slot.
        incoming
            .lock()
            .unwrap()
            .get_mut()
            .extend_from_slice(&[0, 1, 0, 2, 0, 0, 0, 1, 0, 0, 0, 0]);
        assert!(futures::poll!(&mut driver).is_pending());
        let progressed = live[0]
            .try_recv()
            .expect("full live pending stopped inbound ACK progress")
            .unwrap();
        assert!(
            live[1].try_recv().is_err(),
            "one ACK must open exactly one stream"
        );
        assert_eq!(*termination.cause.borrow(), None);
        drop(progressed);
        drop(held);
    }

    #[tokio::test]
    async fn cancelling_public_open_before_admission_never_enqueues() {
        let (io, _peer) = tokio::io::duplex(64 * 1024);
        let mut session = YamuxSession::new_client(io);
        // No await: the spawned driver cannot drain until this current-thread
        // test gives it control. Fill real public admission to capacity first.
        let mut replies = Vec::new();
        for _ in 0..OPEN_CHANNEL_CAPACITY {
            let (reply, receive) = oneshot::channel();
            session.open_tx.try_send(open_request(reply)).unwrap();
            replies.push(receive);
        }
        let mut opening = Box::pin(session.open_stream());
        assert!(futures::poll!(&mut opening).is_pending());
        drop(opening);
        assert_eq!(session.open_tx.capacity(), 0);
        drop(replies);
        let _stream = session.open_stream().await.unwrap();
        assert_eq!(*session.driver_exit.borrow(), None);
        session.close().await.unwrap();
        session.termination.completed().await;
    }

    #[tokio::test]
    async fn ready_driver_work_yields_and_services_both_directions() {
        let io = ready_inbound_io(inbound_syns(64), false, false);
        let conn = Connection::new(io.compat(), Config::default(), Mode::Client);
        let (open_tx, open_rx) = mpsc::channel(OPEN_CHANNEL_CAPACITY);
        let (inbound_tx, mut inbound_rx) = mpsc::channel(INBOUND_CAPACITY);
        let (_close_tx, close_rx) = oneshot::channel();
        let (termination, completion) = SessionTermination::pair();
        let mut replies = Vec::new();
        for _ in 0..MAX_PENDING_OPEN {
            let (reply, receive) = oneshot::channel();
            open_tx.try_send(open_request(reply)).unwrap();
            replies.push(receive);
        }
        let open_waker = AtomicWaker::new();
        let mut driver = Box::pin(driver_task(
            conn,
            open_rx,
            inbound_tx,
            close_rx,
            completion.cause_tx.clone(),
            &termination,
            &open_waker,
        ));
        let counter = Arc::new(CountWake::default());
        let waker = futures::task::waker(counter.clone());
        assert!(
            std::future::Future::poll(driver.as_mut(), &mut Context::from_waker(&waker))
                .is_pending()
        );
        let mut opened = Vec::new();
        for reply in &mut replies {
            if let Ok(stream) = reply.try_recv() {
                opened.push(stream.unwrap());
            }
        }
        assert_eq!(opened.len(), DRIVER_TURN_BUDGET / 2);
        assert_eq!(inbound_rx.len(), DRIVER_TURN_BUDGET / 2);
        assert!(
            counter.0.load(Ordering::SeqCst) > 0,
            "budget exhaustion must reschedule"
        );
        // Close is checked before the next batch, even under permanently ready IO.
        termination.request_close();
        assert!(futures::poll!(&mut driver).is_ready());
        assert_eq!(
            *termination.cause.borrow(),
            Some(OpenStreamError::ConnectionClosed)
        );
        while let Ok(stream) = inbound_rx.try_recv() {
            drop(stream);
        }
    }

    #[tokio::test]
    async fn full_inbound_delivery_still_drives_ack_and_outbound_open() {
        let io = ready_inbound_io(inbound_syns(INBOUND_CAPACITY), false, false);
        let incoming = io.incoming.clone();
        let mut conn = Connection::new(io.compat(), Config::default(), Mode::Client);
        let waker = futures::task::noop_waker();
        let Poll::Ready(Ok(held)) = conn.poll_new_outbound(&mut Context::from_waker(&waker)) else {
            panic!("fixture could not open stream");
        };
        let (open_tx, open_rx) = mpsc::channel(OPEN_CHANNEL_CAPACITY);
        let (inbound_tx, inbound_rx) = mpsc::channel(INBOUND_CAPACITY);
        let (_close_tx, close_rx) = oneshot::channel();
        let (termination, completion) = SessionTermination::pair();
        let open_waker = AtomicWaker::new();
        let mut driver = Box::pin(driver_task(
            conn,
            open_rx,
            inbound_tx,
            close_rx,
            completion.cause_tx.clone(),
            &termination,
            &open_waker,
        ));
        for _ in 0..=INBOUND_CAPACITY / DRIVER_TURN_BUDGET {
            assert!(futures::poll!(&mut driver).is_pending());
        }
        assert_eq!(inbound_rx.len(), INBOUND_CAPACITY);
        incoming
            .lock()
            .unwrap()
            .get_mut()
            .extend_from_slice(&[0, 1, 0, 2, 0, 0, 0, 1, 0, 0, 0, 0]);
        let (reply, receive) = oneshot::channel();
        open_tx.try_send(open_request(reply)).unwrap();
        assert!(futures::poll!(&mut driver).is_pending());
        assert!(receive.now_or_never().unwrap().unwrap().is_ok());
        assert_eq!(inbound_rx.len(), INBOUND_CAPACITY);
        assert_eq!(*termination.cause.borrow(), None);
        termination.request_close();
        assert!(
            futures::poll!(&mut driver).is_ready(),
            "full inbound delivery blocked close"
        );
        drop(held);
    }

    #[tokio::test(start_paused = true)]
    async fn inbound_overflow_publishes_before_open_wake_and_bounds_blocked_cleanup() {
        let waker = futures::task::noop_waker();
        // Keep opening blocked on ACKs, leaving replies pending at overflow.
        // Use a larger test-only stream limit: 256 outgoing + 257 incoming.
        // Default-limit validation is covered by the full-inbound test above.
        let mut config = Config::default();
        config.set_max_num_streams(1024);
        let io = ready_inbound_io(inbound_syns(INBOUND_CAPACITY + 1), false, false);
        let gate = io.block_writes.clone();
        let mut conn = Connection::new(io.compat(), config, Mode::Client);
        let held: Vec<_> = (0..256)
            .map(|_| {
                let Poll::Ready(Ok(stream)) =
                    conn.poll_new_outbound(&mut Context::from_waker(&waker))
                else {
                    panic!("could not fill ACK backlog");
                };
                stream
            })
            .collect();
        let (open_tx, open_rx) = mpsc::channel(OPEN_CHANNEL_CAPACITY);
        let (inbound_tx, inbound_rx) = mpsc::channel(INBOUND_CAPACITY);
        let (_close_tx, close_rx) = oneshot::channel();
        let (termination, completion) = SessionTermination::pair();
        let (reply, mut receive) = oneshot::channel();
        open_tx.try_send(open_request(reply)).unwrap();
        let observer = Arc::new(OpenReplyWake {
            cause: termination.cause.clone(),
            observed: std::sync::Mutex::new(Vec::new()),
            unread: std::sync::Mutex::new(None),
        });
        let reply_waker = futures::task::waker(observer.clone());
        assert!(std::future::Future::poll(
            Pin::new(&mut receive),
            &mut Context::from_waker(&reply_waker)
        )
        .is_pending());
        let open_waker = AtomicWaker::new();
        let mut driver = Box::pin(driver_task(
            conn,
            open_rx,
            inbound_tx,
            close_rx,
            completion.cause_tx.clone(),
            &termination,
            &open_waker,
        ));
        for _ in 0..INBOUND_CAPACITY / DRIVER_TURN_BUDGET {
            assert!(futures::poll!(&mut driver).is_pending());
        }
        assert_eq!(inbound_rx.len(), INBOUND_CAPACITY);
        gate.store(true, Ordering::SeqCst);
        assert!(futures::poll!(&mut driver).is_pending());
        assert_eq!(
            *termination.cause.borrow(),
            Some(OpenStreamError::InboundOverflow)
        );
        assert_eq!(
            *observer.observed.lock().unwrap(),
            vec![Some(OpenStreamError::InboundOverflow)]
        );
        assert!(receive.await.is_err());
        tokio::time::advance(Duration::from_millis(1999)).await;
        assert!(futures::poll!(&mut driver).is_pending());
        assert!(futures::poll!(Box::pin(termination.forced())).is_pending());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(futures::poll!(&mut driver).is_ready());
        assert!(futures::poll!(Box::pin(termination.forced())).is_ready());
        assert_eq!(
            *termination.cause.borrow(),
            Some(OpenStreamError::InboundOverflow)
        );
        drop(driver);
        drop(completion);
        assert!(futures::poll!(Box::pin(termination.completed())).is_ready());
        drop(held);
    }

    #[tokio::test(start_paused = true)]
    async fn default_limit_overflow_releases_actual_io_without_accept_consumer() {
        let (io, mut peer) = tokio::io::duplex(64 * 1024);
        let probe = Arc::new(IoProbe::default());
        probe.block_writes.store(true, Ordering::SeqCst);
        let (released, mut release) = oneshot::channel();
        let mut session = YamuxSession::new_client(ObservedIo {
            inner: io,
            probe: probe.clone(),
            released: Some(released),
        });
        let termination = session.termination();
        let mut cause = termination.cause.clone();
        peer.write_all(&inbound_syns(INBOUND_CAPACITY + 1))
            .await
            .unwrap();
        while cause.borrow_and_update().is_none() {
            cause.changed().await.unwrap();
        }
        assert_eq!(*cause.borrow(), Some(OpenStreamError::InboundOverflow));
        assert_eq!(session.inbound_rx.len(), INBOUND_CAPACITY);
        let error = session.open_stream().await.err().unwrap();
        assert_eq!(
            error.downcast_ref::<OpenStreamError>(),
            Some(&OpenStreamError::InboundOverflow)
        );
        probe.write_blocked.notified().await;
        assert!(release.try_recv().is_err());
        tokio::time::advance(Duration::from_millis(1999)).await;
        assert!(release.try_recv().is_err());
        tokio::time::advance(Duration::from_millis(1)).await;
        termination.completed().await;
        release
            .try_recv()
            .expect("completion preceded physical IO release");
        assert_eq!(*cause.borrow(), Some(OpenStreamError::InboundOverflow));
        assert!(termination.force.is_cancelled());
    }

    struct ObserveReceiverClosed {
        sender: std::sync::Mutex<OpenReply>,
        closed_at_wake: AtomicBool,
    }

    impl futures::task::ArcWake for ObserveReceiverClosed {
        fn wake_by_ref(this: &Arc<Self>) {
            this.closed_at_wake
                .store(this.sender.lock().unwrap().is_closed(), Ordering::SeqCst);
        }
    }

    #[test]
    fn unread_open_cancellation_closes_receiver_before_driver_wake() {
        let (reply, receive) = oneshot::channel();
        let observer = Arc::new(ObserveReceiverClosed {
            sender: std::sync::Mutex::new(reply),
            closed_at_wake: AtomicBool::new(false),
        });
        let driver_waker = Arc::new(AtomicWaker::new());
        driver_waker.register(&futures::task::waker(observer.clone()));
        drop(OpenReceiver {
            reply: receive,
            driver_waker,
        });
        assert!(observer.closed_at_wake.load(Ordering::SeqCst));
    }

    struct CancelCommittedReply {
        receiver:
            std::sync::Mutex<Option<oneshot::Receiver<Result<yamux::Stream, OpenStreamError>>>>,
        wakes: AtomicUsize,
    }

    impl futures::task::ArcWake for CancelCommittedReply {
        fn wake_by_ref(this: &Arc<Self>) {
            this.wakes.fetch_add(1, Ordering::SeqCst);
            // oneshot send has committed the physical stream before waking.
            // Cancel the receiver reentrantly, before send returns to driver.
            drop(this.receiver.lock().unwrap().take());
        }
    }

    #[tokio::test]
    async fn cancelled_committed_open_disposes_stream_without_application_bytes() {
        let io = ready_inbound_io(vec![], false, false);
        let written = io.written.clone();
        let conn = Connection::new(io.compat(), Config::default(), Mode::Client);
        let (open_tx, open_rx) = mpsc::channel(OPEN_CHANNEL_CAPACITY);
        let (inbound_tx, _inbound_rx) = mpsc::channel(INBOUND_CAPACITY);
        let (_close_tx, close_rx) = oneshot::channel();
        let (termination, completion) = SessionTermination::pair();
        let (reply, receive) = oneshot::channel();
        let cancel = Arc::new(CancelCommittedReply {
            receiver: std::sync::Mutex::new(Some(receive)),
            wakes: AtomicUsize::new(0),
        });
        let waker = futures::task::waker(cancel.clone());
        assert!(std::future::Future::poll(
            Pin::new(cancel.receiver.lock().unwrap().as_mut().unwrap()),
            &mut Context::from_waker(&waker),
        )
        .is_pending());
        open_tx.try_send(open_request(reply)).unwrap();
        let (next, receive_next) = oneshot::channel();
        open_tx.try_send(open_request(next)).unwrap();
        let open_waker = AtomicWaker::new();
        let mut driver = Box::pin(driver_task(
            conn,
            open_rx,
            inbound_tx,
            close_rx,
            completion.cause_tx.clone(),
            &termination,
            &open_waker,
        ));
        assert!(futures::poll!(&mut driver).is_pending());
        assert_eq!(cancel.wakes.load(Ordering::SeqCst), 1);
        assert!(cancel.receiver.lock().unwrap().is_none());
        let next_stream = receive_next.now_or_never().unwrap().unwrap().unwrap();
        // The reset identifies disposal of the committed first physical stream;
        // no SYN/zero-SYN assumption or application-level IO is involved.
        let bytes = written.lock().unwrap().clone();
        assert!(bytes
            .windows(12)
            .any(|header| header == [0, 0, 0, 8, 0, 0, 0, 1, 0, 0, 0, 0]));
        assert_eq!(bytes.len() % 12, 0);
        for header in bytes.chunks_exact(12).filter(|header| header[1] == 0) {
            assert_eq!(
                &header[8..12],
                &[0; 4],
                "cancelled open sent application bytes"
            );
        }
        assert_eq!(*termination.cause.borrow(), None);
        assert!(!termination.force.is_cancelled());
        drop(next_stream);
    }

    #[derive(Default)]
    struct CountWake(AtomicUsize);

    impl futures::task::ArcWake for CountWake {
        fn wake_by_ref(this: &Arc<Self>) {
            this.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn cancelled_ack_blocked_open_wakes_driver_and_never_opens_late() {
        let (io, mut peer) = tokio::io::duplex(64 * 1024);
        let mut conn = Connection::new(io.compat(), Config::default(), Mode::Client);
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        let held: Vec<_> = (0..256)
            .map(|_| {
                let Poll::Ready(Ok(stream)) = conn.poll_new_outbound(&mut cx) else {
                    panic!("could not fill real ACK backlog");
                };
                stream
            })
            .collect();
        assert!(conn.poll_new_outbound(&mut cx).is_pending());
        let (open_tx, open_rx) = mpsc::channel(OPEN_CHANNEL_CAPACITY);
        let (inbound_tx, _inbound_rx) = mpsc::channel(MAX_PENDING_OPEN);
        let (_close_tx, close_rx) = oneshot::channel();
        let (termination, completion) = SessionTermination::pair();
        let (selected_tx, selected_rx) = oneshot::channel();
        let (queued_tx, queued_rx) = oneshot::channel();
        open_tx.try_send(open_request(selected_tx)).unwrap();
        open_tx.try_send(open_request(queued_tx)).unwrap();
        let open_waker = AtomicWaker::new();
        let mut driver = Box::pin(driver_task(
            conn,
            open_rx,
            inbound_tx,
            close_rx,
            completion.cause_tx.clone(),
            &termination,
            &open_waker,
        ));
        let counter = Arc::new(CountWake::default());
        let wake = futures::task::waker(counter.clone());
        let mut driver_cx = Context::from_waker(&wake);
        assert!(std::future::Future::poll(driver.as_mut(), &mut driver_cx).is_pending());
        let before = counter.0.load(Ordering::SeqCst);
        drop(queued_rx);
        assert!(
            counter.0.load(Ordering::SeqCst) > before,
            "queued cancellation while outbound was Pending did not wake driver"
        );
        assert!(std::future::Future::poll(driver.as_mut(), &mut driver_cx).is_pending());
        let before = counter.0.load(Ordering::SeqCst);
        drop(selected_rx);
        assert!(
            counter.0.load(Ordering::SeqCst) > before,
            "selected cancellation while outbound was Pending did not wake driver"
        );
        assert!(std::future::Future::poll(driver.as_mut(), &mut driver_cx).is_pending());
        let (next_tx, next_rx) = oneshot::channel();
        open_tx.try_send(open_request(next_tx)).unwrap();
        // ACK exactly one existing stream. Any ghost consumes this single slot
        // and leaves the subsequent legitimate opener stuck at the ACK limit.
        peer.write_all(&[0, 1, 0, 2, 0, 0, 0, 1, 0, 0, 0, 0])
            .await
            .unwrap();
        assert!(std::future::Future::poll(driver.as_mut(), &mut driver_cx).is_pending());
        let next = next_rx
            .now_or_never()
            .expect("cancelled opener consumed the ACK slot");
        assert!(next.unwrap().is_ok());
        assert_eq!(*termination.cause.borrow(), None);
        drop(held);
    }

    fn open_request(reply: OpenReply) -> OpenRequest {
        OpenRequest { reply }
    }

    /// Checks the cause synchronously inside oneshot's wake, before the driver
    /// can proceed with dropping any other locals. The first wake also enqueues
    /// a request after the driver's drain, covering unread `open_rx` entries.
    struct OpenReplyWake {
        cause: watch::Receiver<Option<OpenStreamError>>,
        observed: std::sync::Mutex<Vec<Option<OpenStreamError>>>,
        unread: std::sync::Mutex<Option<(mpsc::Sender<OpenRequest>, OpenRequest)>>,
    }

    impl futures::task::ArcWake for OpenReplyWake {
        fn wake_by_ref(this: &Arc<Self>) {
            this.observed.lock().unwrap().push(*this.cause.borrow());
            let unread = this.unread.lock().unwrap().take();
            if let Some((tx, request)) = unread {
                tx.try_send(request).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn protocol_exit_publishes_before_selected_queued_and_unread_openers_wake() {
        let (local, mut peer) = tokio::io::duplex(64 * 1024);
        let mut conn = Connection::new(local.compat(), Config::default(), Mode::Client);
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        // Fill the actual vendor ACK backlog so the selected outbound request
        // must drive inbound (and encounter the malformed frame) instead.
        let streams: Vec<_> = (0..256)
            .map(|_| match conn.poll_new_outbound(&mut cx) {
                Poll::Ready(Ok(stream)) => stream,
                other => panic!("could not fill ACK backlog: {other:?}"),
            })
            .collect();
        assert!(conn.poll_new_outbound(&mut cx).is_pending());

        let (open_tx, open_rx) = mpsc::channel(OPEN_CHANNEL_CAPACITY);
        let (inbound_tx, _inbound_rx) = mpsc::channel(MAX_PENDING_OPEN);
        let (_close_tx, close_rx) = oneshot::channel();
        let (exit_tx, exit_rx) = watch::channel(None);
        let (selected_tx, mut selected_rx) = oneshot::channel();
        let (queued_tx, mut queued_rx) = oneshot::channel();
        let (unread_tx, mut unread_rx) = oneshot::channel();
        open_tx.try_send(open_request(selected_tx)).unwrap();
        open_tx.try_send(open_request(queued_tx)).unwrap();
        let observer = Arc::new(OpenReplyWake {
            cause: exit_rx.clone(),
            observed: std::sync::Mutex::new(Vec::new()),
            unread: std::sync::Mutex::new(Some((open_tx, open_request(unread_tx)))),
        });
        let reply_waker = futures::task::waker(observer.clone());
        let mut reply_cx = Context::from_waker(&reply_waker);
        for reply in [&mut selected_rx, &mut queued_rx, &mut unread_rx] {
            assert!(std::future::Future::poll(Pin::new(reply), &mut reply_cx).is_pending());
        }

        let (termination, _completion) = SessionTermination::pair();
        let open_waker = AtomicWaker::new();
        let mut driver = Box::pin(driver_task(
            conn,
            open_rx,
            inbound_tx,
            close_rx,
            exit_tx,
            &termination,
            &open_waker,
        ));
        assert!(futures::poll!(&mut driver).is_pending());
        // Invalid yamux version in a complete frame header => Protocol.
        peer.write_all(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        assert!(futures::poll!(&mut driver).is_ready());
        // No driver join or task scheduling barrier: these snapshots were
        // taken at the exact wakes that let open_stream resume on any worker.
        assert_eq!(
            *observer.observed.lock().unwrap(),
            vec![Some(OpenStreamError::Protocol); 3]
        );
        assert!(matches!(
            selected_rx.await,
            Ok(Err(OpenStreamError::Protocol))
        ));
        for reply in [queued_rx, unread_rx] {
            assert!(reply.await.is_err());
            assert_eq!(*exit_rx.borrow(), Some(OpenStreamError::Protocol));
        }
        drop(streams);
    }

    #[tokio::test]
    async fn outbound_capacity_failure_is_preserved_for_first_and_late_openers() {
        let (local, _peer) = tokio::io::duplex(64 * 1024);
        let mut config = Config::default();
        config.set_max_num_streams(0);
        let conn = Connection::new(local.compat(), config, Mode::Client);
        let (open_tx, open_rx) = mpsc::channel(OPEN_CHANNEL_CAPACITY);
        let (inbound_tx, inbound_rx) = mpsc::channel(MAX_PENDING_OPEN);
        let (close_tx, close_rx) = oneshot::channel();
        let (termination, completion) = SessionTermination::pair();
        let exit_tx = completion.cause_tx.clone();
        let driver_exit = termination.cause.clone();
        let driver_control = termination.clone();
        let open_waker = Arc::new(AtomicWaker::new());
        let driver_waker = open_waker.clone();
        let driver = tokio::spawn(async move {
            let _completion = completion;
            driver_task(
                conn,
                open_rx,
                inbound_tx,
                close_rx,
                exit_tx,
                &driver_control,
                &driver_waker,
            )
            .await;
        });
        let mut session = YamuxSession {
            termination,
            open_tx,
            open_waker,
            driver_exit,
            inbound_rx,
            close_tx: Some(close_tx),
            driver,
        };
        let open_tx = session.open_tx.clone();
        let (queued_tx, queued_rx) = oneshot::channel();
        let first_result = {
            let mut first_open = Box::pin(session.open_stream());
            assert!(futures::poll!(&mut first_open).is_pending());
            // Both requests reach the driver before it is scheduled. The
            // second poll_new_outbound yields Closed, not the original error.
            open_tx.try_send(open_request(queued_tx)).unwrap();
            first_open.await
        };
        let Err(first) = first_result else {
            panic!("zero-capacity connection opened a stream");
        };
        assert!(matches!(
            queued_rx.await,
            Ok(Err(OpenStreamError::Capacity))
        ));
        assert_eq!(
            first.downcast_ref::<OpenStreamError>(),
            Some(&OpenStreamError::Capacity)
        );
        // Vendor poll_next_inbound now returns None. That ordinary closure
        // must not replace the outbound error that caused it.
        (&mut session.driver).await.unwrap();
        let Err(late) = session.open_stream().await else {
            panic!("closed zero-capacity connection opened a stream");
        };
        assert_eq!(
            late.downcast_ref::<OpenStreamError>(),
            Some(&OpenStreamError::Capacity)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stopped_driver_open_is_a_typed_io_transport_error_through_context() {
        use anyhow::Context as _;

        let (client_io, peer_io) = tokio::io::duplex(64 * 1024);
        let mut session = YamuxSession::new_server(client_io);
        drop(peer_io);

        // Awaiting the actual driver is the completion barrier: no timing sleep
        // or blackhole is involved before the bootstrap open is attempted.
        (&mut session.driver).await.unwrap();
        let Err(error) = session
            .open_stream()
            .await
            .context("open control substream")
        else {
            panic!("opening after a stopped driver unexpectedly succeeded");
        };
        assert!(
            matches!(
                error.downcast_ref::<OpenStreamError>(),
                Some(OpenStreamError::Io)
            ),
            "unexpected bootstrap error: {error:#?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn explicitly_closed_driver_open_is_a_typed_closed_transport_error() {
        let (client_io, _peer_io) = tokio::io::duplex(64 * 1024);
        let mut session = YamuxSession::new_server(client_io);
        session.close().await.unwrap();
        (&mut session.driver).await.unwrap();

        let Err(error) = session.open_stream().await else {
            panic!("opening after an explicitly closed driver unexpectedly succeeded");
        };
        assert!(matches!(
            error.downcast_ref::<OpenStreamError>(),
            Some(OpenStreamError::ConnectionClosed)
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn driver_io_failure_is_a_typed_transport_error() {
        let (client_io, _peer_io) = tokio::io::duplex(64 * 1024);
        let probe = Arc::new(IoProbe::default());
        let (released_tx, _released_rx) = oneshot::channel();
        let mut session = YamuxSession::start(
            ObservedIo {
                inner: client_io,
                probe: probe.clone(),
                released: Some(released_tx),
            },
            Mode::Server,
        );
        probe.fail_reads();
        (&mut session.driver).await.unwrap();

        let Err(error) = session.open_stream().await else {
            panic!("opening after an I/O-stopped driver unexpectedly succeeded");
        };
        assert!(
            matches!(
                error.downcast_ref::<OpenStreamError>(),
                Some(OpenStreamError::Io)
            ),
            "unexpected bootstrap error: {error:#?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drop_terminates_driver_promptly() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let daemon = YamuxSession::new_client(a);
        let mut cli = YamuxSession::new_server(b);
        // Drop the daemon side without an explicit close.
        drop(daemon);
        // CLI's accept should resolve (Err or Ok(None)) within reasonable time;
        // do not assert which — implementation detail.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), cli.accept_stream())
            .await
            .expect("accept did not resolve after peer drop");
    }
}
