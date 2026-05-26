use std::pin::Pin;

use anyhow::Context;
use tnl_protocol::Stream;
use tokio::net::TcpStream;
use tracing::debug;

/// Bidirectionally copy bytes between a yamux substream and a TCP socket to
/// `127.0.0.1:port`. Closes both sides on either EOF.
pub async fn forward(mut stream: Pin<Box<dyn Stream>>, port: u16) -> anyhow::Result<()> {
    let mut tcp = TcpStream::connect(("127.0.0.1", port))
        .await
        .with_context(|| format!("connect 127.0.0.1:{port}"))?;
    tcp.set_nodelay(true)?;
    let (a, b) = tokio::io::copy_bidirectional(&mut *stream, &mut tcp).await?;
    debug!(sent_to_local = a, sent_from_local = b, "stream closed");
    Ok(())
}
