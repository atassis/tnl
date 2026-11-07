use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_util::compat::TokioAsyncReadCompatExt as _;
use yamux::{Config, Connection, Mode};

/// A multiplexed substream — must behave as a bidirectional byte channel.
pub trait Stream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + ?Sized> Stream for T {}

/// A logical session between a `tnl` client and the `tnld` daemon.
/// Provides substream multiplexing. Both ends can both open and accept.
#[async_trait]
pub trait Session: Send + Sync {
    /// Initiate a new outgoing substream toward the peer.
    async fn open_stream(&mut self) -> anyhow::Result<Pin<Box<dyn Stream>>>;

    /// Accept the next incoming substream initiated by the peer.
    /// Returns `None` when the session is closed.
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
    /// Sender side for opening new outbound streams: we send a oneshot back-channel
    /// and the driver task calls `poll_new_outbound` on our behalf.
    open_tx: mpsc::UnboundedSender<oneshot::Sender<yamux::Result<yamux::Stream>>>,
    /// Receiver side for inbound streams surfaced by the driver task.
    inbound_rx: mpsc::UnboundedReceiver<yamux::Stream>,
    /// Sender for close signal to driver (explicit graceful close).
    close_tx: Option<oneshot::Sender<()>>,
    /// Driver task handle.  Dropping `YamuxSession` closes all channel senders,
    /// which the driver detects and uses to perform a graceful yamux `poll_close`
    /// before exiting.  We keep the handle (rather than calling `abort()`) so
    /// that buffered writes can be flushed; the driver cannot deadlock after
    /// the C1 fix.
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
        // reach the underlying IO.  The driver is guaranteed to exit because C1 is
        // fixed (no deadlock path remains) and both channel senders are being dropped.
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

        let (open_tx, open_rx) =
            mpsc::unbounded_channel::<oneshot::Sender<yamux::Result<yamux::Stream>>>();
        // SAFETY: unbounded acceptable for v0.1.0-alpha — auth gates session, one substream per HTTP request, bounded rate.
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel::<yamux::Stream>();
        let (close_tx, close_rx) = oneshot::channel::<()>();

        let driver = tokio::spawn(driver_task(conn, open_rx, inbound_tx, close_rx));

        Self {
            open_tx,
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

/// Background task that owns the `yamux::Connection` and drives it.
///
/// It concurrently:
/// - Polls `poll_next_inbound` to drive the connection state machine and surface inbound streams.
/// - Handles open-outbound requests by calling `poll_new_outbound` on the same connection.
///
/// The task exits when the connection closes or the close signal is received.
async fn driver_task<T>(
    mut conn: Connection<tokio_util::compat::Compat<T>>,
    mut open_rx: mpsc::UnboundedReceiver<oneshot::Sender<yamux::Result<yamux::Stream>>>,
    inbound_tx: mpsc::UnboundedSender<yamux::Stream>,
    mut close_rx: oneshot::Receiver<()>,
) where
    T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    // VecDeque for O(1) push_front/pop_front used in the C1 deadlock fix.
    let mut pending_open: VecDeque<oneshot::Sender<yamux::Result<yamux::Stream>>> = VecDeque::new();

    loop {
        // Drain any new open requests into pending_open without blocking.
        while let Ok(req) = open_rx.try_recv() {
            pending_open.push_back(req);
        }

        // If there are pending open requests, race poll_new_outbound against
        // poll_next_inbound so that ACK frames continue to be ingested and the
        // yamux waker fires even when ack_backlog >= MAX_ACK_BACKLOG (256).
        // Without driving inbound concurrently the driver would deadlock: outbound
        // blocks waiting for an ACK, but the ACK can only arrive via inbound.
        //
        // We implement the race safely with a combined future that polls both
        // operations in a single poll call: try outbound first; if it's pending,
        // try inbound so ACK frames are processed and the outbound waker is stored.
        if let Some(requester) = pending_open.pop_front() {
            enum PendingAction {
                Outbound(yamux::Result<yamux::Stream>),
                Inbound(Option<yamux::Result<yamux::Stream>>),
            }

            tokio::select! {
                biased;

                // Explicit `close()` was called → graceful poll_close, exit.
                _ = &mut close_rx => {
                    std::future::poll_fn(|cx: &mut Context<'_>| {
                        conn.poll_close(cx).map_err(|_| ())
                    }).await.ok();
                    return;
                }

                // Combined future: tries outbound; if pending, drives inbound.
                // This is safe: both polls happen sequentially within a single
                // future::poll invocation — only one exclusive borrow at a time.
                action = std::future::poll_fn(|cx: &mut Context<'_>| {
                    match conn.poll_new_outbound(cx) {
                        Poll::Ready(result) => Poll::Ready(PendingAction::Outbound(result)),
                        Poll::Pending => match conn.poll_next_inbound(cx) {
                            Poll::Ready(inbound) => Poll::Ready(PendingAction::Inbound(inbound)),
                            Poll::Pending => Poll::Pending,
                        },
                    }
                }) => {
                    match action {
                        PendingAction::Outbound(result) => {
                            let _ = requester.send(result);
                        }
                        PendingAction::Inbound(inbound) => {
                            // Re-queue the requester so the next iteration still serves it.
                            pending_open.push_front(requester);
                            match inbound {
                                Some(Ok(stream)) => {
                                    let _ = inbound_tx.send(stream);
                                }
                                Some(Err(e)) => {
                                    tracing::debug!(error = %e, "yamux connection error while awaiting outbound, terminating driver");
                                    return;
                                }
                                None => return,
                            }
                        }
                    }
                }
            }
            continue;
        }

        // Drive the connection: poll for inbound streams and connection progress.
        // We select between:
        // 1. poll_next_inbound  (drives the connection)
        // 2. a new open request arriving
        // 3. close signal (explicit graceful close via close())
        // 4. open_rx closed (session dropped — forceful shutdown)
        tokio::select! {
            biased;

            // Explicit `close()` was called → graceful poll_close, exit.
            _ = &mut close_rx => {
                std::future::poll_fn(|cx: &mut Context<'_>| {
                    conn.poll_close(cx).map_err(|_| ())
                }).await.ok();
                return;
            }

            // New open request arrives while we are waiting on inbound.
            // None means open_tx was dropped (session dropped) → also exit.
            maybe_req = open_rx.recv() => {
                if let Some(r) = maybe_req {
                    pending_open.push_back(r);
                } else {
                    // Session was dropped or open_tx closed → forceful shutdown.
                    std::future::poll_fn(|cx: &mut Context<'_>| {
                        conn.poll_close(cx).map_err(|_| ())
                    }).await.ok();
                    return;
                }
            }

            // Drive the connection by polling for inbound streams.
            maybe = std::future::poll_fn(|cx: &mut Context<'_>| conn.poll_next_inbound(cx)) => {
                match maybe {
                    Some(Ok(stream)) => {
                        // Surface to session's accept_stream().
                        // If the receiver is gone, just drop the stream.
                        let _ = inbound_tx.send(stream);
                    }
                    Some(Err(e)) => {
                        tracing::warn!(error = %e, "yamux connection error, terminating driver");
                        return;
                    }
                    None => {
                        // Connection closed.
                        return;
                    }
                }
            }
        }
    }
}

#[async_trait]
impl Session for YamuxSession {
    async fn open_stream(&mut self) -> anyhow::Result<Pin<Box<dyn Stream>>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.open_tx
            .send(reply_tx)
            .map_err(|_| anyhow::anyhow!("driver task is gone"))?;
        let stream = reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("driver task dropped reply channel"))??;
        Ok(Box::pin(YamuxStreamCompat::new(stream)))
    }

    async fn accept_stream(&mut self) -> anyhow::Result<Option<Pin<Box<dyn Stream>>>> {
        Ok(self
            .inbound_rx
            .recv()
            .await
            .map(|s| -> Pin<Box<dyn Stream>> { Box::pin(YamuxStreamCompat::new(s)) }))
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
