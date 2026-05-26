use std::pin::Pin;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time assertion that the trait is object-safe.
    #[allow(dead_code)]
    fn _is_object_safe(_: Box<dyn Session>) {}
}
