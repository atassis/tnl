//! Test-only physical shutdown barrier on the real WS-backed transport.
use std::{
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
};
use tnl_protocol::Session as _;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[derive(Default)]
pub struct CloseGate {
    sessions: AtomicUsize,
    pub(crate) entered: tokio_util::sync::CancellationToken,
    released: AtomicBool,
    waker: futures::task::AtomicWaker,
    pub(crate) termination: Mutex<Option<tnl_protocol::SessionTermination>>,
}
impl CloseGate {
    pub(crate) fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.waker.wake();
    }
}
tokio::task_local! {
    pub(crate) static TEST_CLOSE_GATE: Arc<CloseGate>;
}

pub(super) fn session_from_ws<S>(
    ws: async_tungstenite::WebSocketStream<S>,
) -> tnl_protocol::transport::YamuxSession
where
    S: futures::io::AsyncRead + futures::io::AsyncWrite + Send + Unpin + 'static,
{
    let gate = TEST_CLOSE_GATE
        .try_with(|gate| (gate.sessions.fetch_add(1, Ordering::SeqCst) == 1).then(|| gate.clone()))
        .ok()
        .flatten();
    if let Some(gate) = gate {
        let session = tnl_protocol::transport::YamuxSession::new_server(GatedShutdown {
            inner: ws_stream_tungstenite::WsStream::new(ws),
            gate: gate.clone(),
        });
        *gate.termination.lock().unwrap() = Some(session.termination());
        session
    } else {
        tnl_protocol::transport::client_session_from_ws_generic(ws)
    }
}

struct GatedShutdown<T> {
    inner: T,
    gate: Arc<CloseGate>,
}
impl<T: AsyncRead + Unpin> AsyncRead for GatedShutdown<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for GatedShutdown<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.gate.entered.cancel();
        self.gate.waker.register(cx.waker());
        if !self.gate.released.load(Ordering::SeqCst) {
            return Poll::Pending;
        }
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
