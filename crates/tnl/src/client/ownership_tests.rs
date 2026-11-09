use super::*;
use tnl_protocol::transport::{SessionCompletion, SessionTermination};
use tokio::sync::{oneshot, Notify};

/// One already accepted stream, independent of the fake session's accept side.
/// Unlike a physical yamux connection, ending acceptance cannot close this IO.
struct AcceptedSession {
    stream: Option<tokio::io::DuplexStream>,
    accepting: std::sync::Arc<Notify>,
    termination: SessionTermination,
    _completion: SessionCompletion,
}

#[async_trait::async_trait]
impl tnl_protocol::Session for AcceptedSession {
    fn termination(&self) -> SessionTermination {
        self.termination.clone()
    }

    async fn open_stream(&mut self) -> anyhow::Result<Pin<Box<dyn tnl_protocol::Stream>>> {
        anyhow::bail!("accept-only fixture")
    }

    async fn accept_stream(
        &mut self,
    ) -> anyhow::Result<Option<Pin<Box<dyn tnl_protocol::Stream>>>> {
        if let Some(stream) = self.stream.take() {
            return Ok(Some(Box::pin(stream)));
        }
        self.accepting.notify_one();
        tokio::select! {
            () = self.termination.close_requested() => {},
            () = self.termination.forced() => {},
        }
        Ok(None)
    }

    async fn ping(&mut self) -> anyhow::Result<std::time::Duration> {
        anyhow::bail!("accept-only fixture")
    }

    async fn close(&mut self) -> anyhow::Result<()> {
        self.stream.take();
        self.termination.request_close();
        Ok(())
    }
}

async fn forwarding_generation(
    tasks: &ForwardTasks,
) -> (
    AcceptGeneration,
    tokio::io::DuplexStream,
    oneshot::Receiver<()>,
) {
    let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = backend.local_addr().unwrap().port();
    let (connected, connection) = oneshot::channel();
    let (released, release) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = backend.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(socket.read_u8().await.unwrap());
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx\r\n")
            .await
            .unwrap();
        connected.send(()).unwrap();
        let mut remaining = Vec::new();
        socket.read_to_end(&mut remaining).await.unwrap();
        released.send(()).unwrap();
    });
    let (mut peer, stream) = tokio::io::duplex(4096);
    let accepting = std::sync::Arc::new(Notify::new());
    let (termination, completion) = SessionTermination::pair();
    let session = AcceptedSession {
        stream: Some(stream),
        accepting: accepting.clone(),
        termination,
        _completion: completion,
    };
    let generation = tasks.accept(
        Box::new(session),
        crate::target::Target::LocalhostPort(port),
        crate::forwarder::ForwardCtx::new("ownership".into(), None, "test"),
    );
    peer.write_all(b"GET / HTTP/1.1\r\nHost: ownership.t.x\r\n\r\n")
        .await
        .unwrap();
    connection.await.unwrap();
    accepting.notified().await;
    (generation, peer, release)
}

#[tokio::test]
async fn generation_accept_is_joined_without_cancelling_run_owned_forwards() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let tasks = ForwardTasks::default();
        let (mut first, _first_peer, mut first_released) = forwarding_generation(&tasks).await;
        first.stop_and_join().await;
        assert!(first.joined);
        assert!(first_released.try_recv().is_err());
        let (mut second, _second_peer, second_released) = forwarding_generation(&tasks).await;
        second.stop_and_join().await;
        // An explicit physical failure cancels only the failed generation.
        second.terminate_failed().await;
        second_released.await.unwrap();
        assert!(
            first_released.try_recv().is_err(),
            "old accepted forward cancelled by new generation"
        );
        // Explicit run stop still cancels and joins every remaining forward.
        tasks.shutdown().await;
        first_released.await.unwrap();
        assert!(tasks.tracker.is_empty());
    })
    .await
    .expect("accept owner or backend IO was not released");
}
