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

    /// Send a ping and await pong; returns the round-trip time.
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
    /// Sender for close signal to driver.
    close_tx: Option<oneshot::Sender<()>>,
    /// Driver task handle (kept to ensure the task outlives the session).
    _driver: JoinHandle<()>,
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

        let (open_tx, open_rx) = mpsc::unbounded_channel::<oneshot::Sender<yamux::Result<yamux::Stream>>>();
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel::<yamux::Stream>();
        let (close_tx, close_rx) = oneshot::channel::<()>();

        let driver = tokio::spawn(driver_task(conn, open_rx, inbound_tx, close_rx));

        Self {
            open_tx,
            inbound_rx,
            close_tx: Some(close_tx),
            _driver: driver,
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
    // Pending open requests that are waiting for the connection to be ready.
    let mut pending_open: Vec<oneshot::Sender<yamux::Result<yamux::Stream>>> = Vec::new();

    loop {
        // Drain any new open requests into pending_open without blocking.
        while let Ok(req) = open_rx.try_recv() {
            pending_open.push(req);
        }

        // If there are pending open requests, try to fulfill them one per loop
        // iteration to avoid starving inbound polling.
        if !pending_open.is_empty() {
            let result =
                std::future::poll_fn(|cx: &mut Context<'_>| conn.poll_new_outbound(cx)).await;
            let requester = pending_open.remove(0);
            // If the requester dropped their receiver, just ignore the error.
            let _ = requester.send(result);
            continue;
        }

        // Drive the connection: poll for inbound streams and connection progress.
        // We select between:
        // 1. poll_next_inbound  (drives the connection)
        // 2. a new open request arriving
        // 3. close signal
        tokio::select! {
            biased;

            // Close signal takes priority.
            _ = &mut close_rx => {
                std::future::poll_fn(|cx: &mut Context<'_>| {
                    conn.poll_close(cx).map_err(|_| ())
                }).await.ok();
                return;
            }

            // New open request arrives while we are waiting on inbound.
            req = open_rx.recv() => {
                if let Some(r) = req {
                    pending_open.push(r);
                } else {
                    // Session handle dropped; shut down.
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
                    Some(Err(_e)) => {
                        // Connection error; shut down driver.
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
        // yamux 0.13 does not expose application-level ping; we open a substream
        // with empty payload and close it immediately as a liveness check.
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

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
}
