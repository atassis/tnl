#[path = "support/timing_io.rs"]
mod timing_io;
use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tnl::{
    forwarder::{forward, ForwardCtx},
    inspection::{Inspection, PreviewMode, TunnelGeneration},
    target::Target,
};
use tnl_inspector_api::timing::{Outcome, Snapshot};
use tnl_protocol::Stream;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
fn context(history: &Inspection) -> ForwardCtx {
    let mut ctx = ForwardCtx::new("timing-fixture".into(), None, env!("CARGO_PKG_VERSION"));
    ctx.inspection = Some((history.clone(), TunnelGeneration::new()));
    ctx
}
fn snapshot(history: &Inspection) -> Snapshot {
    let rows = history.search("");
    assert_eq!(rows.len(), 1);
    let record = history.get(rows[0].capture_id).unwrap();
    assert!(record.timing.snapshot().is_some(), "{:?}", record.timing);
    record.timing.snapshot().unwrap()
}
#[tokio::test]
async fn informational_head_is_not_final_and_upload_overlaps_early_response() {
    tokio::time::timeout(
        Duration::from_secs(5),
        Box::pin(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let target = Target::Explicit(listener.local_addr().unwrap());
            let history = Inspection::new(PreviewMode::Off);
            let (mut visitor, stream) = tokio::io::duplex(8192);
            let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
            let info = b"HTTP/1.1 103 Early Hints\r\nLink: </fixture>\r\n\r\n";
            let final_head = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
            let backend = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                timing_io::read_head(&mut socket).await;
                let mut prefix = [0; 2];
                socket.read_exact(&mut prefix).await.unwrap();
                assert_eq!(&prefix, b"ab");
                socket.write_all(info).await.unwrap();
                finish_rx.await.unwrap();
                socket.write_all(final_head).await.unwrap();
                let mut tail = Vec::new();
                socket.read_to_end(&mut tail).await.unwrap();
                assert_eq!(tail, b"cdef");
            };
            let client = async {
                visitor
                    .write_all(b"POST / HTTP/1.1\r\nHost: local\r\nContent-Length: 6\r\n\r\nab")
                    .await
                    .unwrap();
                let mut received = vec![0; info.len()];
                visitor.read_exact(&mut received).await.unwrap();
                assert_eq!(received, info);
                let s = snapshot(&history);
                assert!(s.backend_first_byte_offset_us.is_some());
                assert_eq!(s.backend_final_headers_offset_us, None);
                assert_eq!(s.intervals.request_write.outcome, Outcome::Active);
                finish_tx.send(()).unwrap();
                let mut received = vec![0; final_head.len()];
                visitor.read_exact(&mut received).await.unwrap();
                assert_eq!(received, final_head);
                let s = snapshot(&history);
                assert!(
                    s.backend_final_headers_offset_us.is_some(),
                    "final backend head was fully observed"
                );
                assert_eq!(s.intervals.request_write.outcome, Outcome::Active);
                visitor.write_all(b"cdef").await.unwrap();
                visitor.shutdown().await.unwrap();
                assert!(visitor.read_to_end(&mut Vec::new()).await.unwrap() == 0);
            };
            let (result, (), ()) = tokio::join!(
                forward(Box::pin(stream), target, context(&history)),
                backend,
                client
            );
            result.unwrap();
            let s = snapshot(&history);
            assert!(s.terminal);
            assert_eq!(s.intervals.request_write.outcome, Outcome::Completed);
            assert_eq!(s.intervals.response_write.outcome, Outcome::Completed);
            assert!(s.request_framing_valid && s.response_framing_valid);
            assert!(
                s.backend_first_byte_offset_us.unwrap()
                    <= s.backend_final_headers_offset_us.unwrap()
            );
        }),
    )
    .await
    .unwrap();
}
#[tokio::test]
async fn first_backend_byte_is_visible_in_admitted_history_while_prefix_is_held() {
    tokio::time::timeout(
        Duration::from_secs(5),
        Box::pin(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let target = Target::Explicit(listener.local_addr().unwrap());
            let history = Inspection::new(PreviewMode::Off);
            let (mut visitor, stream) = tokio::io::duplex(8192);
            let (first_tx, first_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            let backend = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                timing_io::read_head(&mut socket).await;
                socket.write_all(b"H").await.unwrap();
                first_tx.send(()).unwrap();
                release_rx.await.unwrap();
                socket
                    .write_all(b"TTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                    .await
                    .unwrap();
            };
            let client = async {
                visitor
                    .write_all(b"GET / HTTP/1.1\r\nHost: local\r\n\r\n")
                    .await
                    .unwrap();
                visitor.shutdown().await.unwrap();
                first_rx.await.unwrap();
                let live = loop {
                    let live = snapshot(&history);
                    if live.backend_first_byte_offset_us.is_some() {
                        break live;
                    }
                    tokio::task::yield_now().await;
                };
                assert!(!live.terminal);
                assert_eq!(live.backend_final_headers_offset_us, None);
                assert_eq!(live.intervals.response_read.outcome, Outcome::Active);
                assert_eq!(live.intervals.response_write.outcome, Outcome::NotStarted);
                release_tx.send(()).unwrap();
                let mut response = Vec::new();
                visitor.read_to_end(&mut response).await.unwrap();
                assert_eq!(response, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            };
            let (result, (), ()) = tokio::join!(
                forward(Box::pin(stream), target, context(&history)),
                backend,
                client,
            );
            result.unwrap();
        }),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn close_delimited_sse_is_live_until_eof_and_then_freezes() {
    tokio::time::timeout(
        Duration::from_secs(5),
        Box::pin(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let target = Target::Explicit(listener.local_addr().unwrap());
            let history = Inspection::new(PreviewMode::Off);
            let (mut visitor, stream) = tokio::io::duplex(8192);
            let (close_tx, close_rx) = tokio::sync::oneshot::channel();
            let response =
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: fixture\n\n";
            let backend = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                timing_io::read_head(&mut socket).await;
                socket.write_all(response).await.unwrap();
                close_rx.await.unwrap();
            };
            let client = async {
                visitor
                    .write_all(b"GET / HTTP/1.1\r\nHost: local\r\n\r\n")
                    .await
                    .unwrap();
                visitor.shutdown().await.unwrap();
                let mut received = vec![0; response.len()];
                visitor.read_exact(&mut received).await.unwrap();
                assert_eq!(received, response);
                let a = snapshot(&history);
                let b = snapshot(&history);
                assert!(!a.terminal);
                assert!(b.as_of_offset_us >= a.as_of_offset_us);
                assert_eq!(a.intervals.response_read.outcome, Outcome::Active);
                assert!(a.backend_final_headers_offset_us.is_some());
                assert_eq!(a.response_framing_end_offset_us, None);
                close_tx.send(()).unwrap();
                visitor.read_to_end(&mut Vec::new()).await.unwrap();
            };
            let (result, (), ()) = tokio::join!(
                forward(Box::pin(stream), target, context(&history)),
                backend,
                client
            );
            result.unwrap();
            let a = snapshot(&history);
            assert!(a.terminal);
            assert!(a.response_framing_valid);
            assert_eq!(a.intervals.response_write.outcome, Outcome::Completed);
            assert_eq!(a, snapshot(&history));
        }),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn upgraded_lifetime_never_becomes_an_http_body_transfer() {
    tokio::time::timeout(Duration::from_secs(5), Box::pin(async {
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target=Target::Explicit(listener.local_addr().unwrap());let history=Inspection::new(PreviewMode::Json);
        let (mut visitor,stream)=tokio::io::duplex(8192);let (close_tx,close_rx)=tokio::sync::oneshot::channel();
        let head=b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n";
        let frame=b"\x82\x04\x00\xff\x01\xfe";
        let backend=async {
            let (mut socket,_)=listener.accept().await.unwrap();timing_io::read_head(&mut socket).await;
            socket.write_all(&[head.as_slice(),frame.as_slice()].concat()).await.unwrap();
            let mut input=Vec::new();socket.read_to_end(&mut input).await.unwrap();assert_eq!(input,frame);
            close_rx.await.unwrap();
        };
        let client=async {
            visitor.write_all(b"GET /socket HTTP/1.1\r\nHost: local\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n").await.unwrap();
            let mut response=vec![0;head.len()+frame.len()];visitor.read_exact(&mut response).await.unwrap();
            assert_eq!(response,[head.as_slice(),frame.as_slice()].concat());
            let s=snapshot(&history);
            assert_eq!(s.intervals.upgraded.outcome,Outcome::Active);
            assert_eq!(s.intervals.response_read.outcome,Outcome::Completed);
            assert_eq!(s.intervals.response_write.outcome,Outcome::Completed);
            let record=&history.search("")[0];
            assert_eq!(record.request.body.observed_payload_bytes,0);
            assert_eq!(record.response.body.observed_payload_bytes,0);
            assert!(record.request.body.text.is_none()&&record.response.body.text.is_none());
            visitor.write_all(frame).await.unwrap();visitor.shutdown().await.unwrap();
            close_tx.send(()).unwrap();visitor.read_to_end(&mut Vec::new()).await.unwrap();
        };
        let(result,(),())=tokio::join!(forward(Box::pin(stream),target,context(&history)),backend,client);result.unwrap();
        let s=snapshot(&history);assert!(s.terminal);assert_eq!(s.intervals.upgraded.outcome,Outcome::Completed);
        assert!(s.intervals.response_read.end_offset_us<=s.intervals.upgraded.end_offset_us);
    })).await.unwrap();
}

struct PartialResponseStream {
    inner: tokio::io::DuplexStream,
    failed: bool,
}
impl AsyncRead for PartialResponseStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}
impl AsyncWrite for PartialResponseStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if this.failed {
            return Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()));
        }
        match Pin::new(&mut this.inner).poll_write(cx, &bytes[..bytes.len().min(3)]) {
            Poll::Ready(Ok(written)) => {
                this.failed = true;
                Poll::Ready(Ok(written))
            }
            other => other,
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[tokio::test]
async fn partial_tunnel_response_write_preserves_accepted_counters_without_completion_claim() {
    tokio::time::timeout(
        Duration::from_secs(5),
        Box::pin(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let target = Target::Explicit(listener.local_addr().unwrap());
            let history = Inspection::new(PreviewMode::Off);
            let (mut visitor, inner) = tokio::io::duplex(8192);
            let stream: Pin<Box<dyn Stream>> = Box::pin(PartialResponseStream {
                inner,
                failed: false,
            });
            let response = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
            let backend = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                timing_io::read_head(&mut socket).await;
                socket.write_all(response).await.unwrap();
            };
            let client = async {
                visitor
                    .write_all(b"GET / HTTP/1.1\r\nHost: local\r\n\r\n")
                    .await
                    .unwrap();
                visitor.shutdown().await.unwrap();
                let mut received = Vec::new();
                visitor.read_to_end(&mut received).await.unwrap();
                assert_eq!(received, b"HTT");
            };
            let (result, (), ()) =
                tokio::join!(forward(stream, target, context(&history)), backend, client,);
            assert!(result.is_err());
            let record = history.get(history.search("")[0].capture_id).unwrap();
            let timing = record.timing.snapshot().unwrap();
            assert_eq!(record.bytes_to_client, 3);
            assert_eq!(record.bytes_from_backend, 3);
            assert_eq!(timing.intervals.response_write.outcome, Outcome::Failed);
            assert_ne!(timing.intervals.response_write.outcome, Outcome::Completed);
            assert_eq!(timing.response_framing_end_offset_us, None);
            assert!(!timing.response_framing_valid);
        }),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn unexpected_backend_bytes_revoke_framing_without_moving_its_original_end() {
    tokio::time::timeout(
        Duration::from_secs(5),
        Box::pin(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let target = Target::Explicit(listener.local_addr().unwrap());
            let history = Inspection::new(PreviewMode::Off);
            let (mut visitor, stream) = tokio::io::duplex(8192);
            let valid_response = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            let backend = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                timing_io::read_head(&mut socket).await;
                socket.write_all(valid_response).await.unwrap();
                release_rx.await.unwrap();
                socket.write_all(b"X").await.unwrap();
            };
            let client = async {
                visitor
                    .write_all(b"GET / HTTP/1.1\r\nHost: local\r\n\r\n")
                    .await
                    .unwrap();
                visitor.shutdown().await.unwrap();
                let mut received = vec![0; valid_response.len()];
                visitor.read_exact(&mut received).await.unwrap();
                assert_eq!(received, valid_response);
                let completed_end = snapshot(&history)
                    .intervals
                    .response_write
                    .end_offset_us
                    .unwrap();
                assert_eq!(
                    snapshot(&history).intervals.response_write.outcome,
                    Outcome::Completed
                );
                release_tx.send(()).unwrap();
                let mut tail = Vec::new();
                visitor.read_to_end(&mut tail).await.unwrap();
                assert_eq!(tail, b"X");
                completed_end
            };
            let (result, (), completed_end) = tokio::join!(
                forward(Box::pin(stream), target, context(&history)),
                backend,
                client,
            );
            result.unwrap();
            let record = history.get(history.search("")[0].capture_id).unwrap();
            let timing = record.timing.snapshot().unwrap();
            assert_eq!(record.state.as_str(), "incomplete");
            assert!(timing.response_framing_end_offset_us.is_some());
            assert!(!timing.response_framing_valid);
            assert_eq!(timing.intervals.response_write.outcome, Outcome::Failed);
            assert_eq!(
                timing.intervals.response_write.end_offset_us,
                Some(completed_end)
            );
        }),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn cancellation_of_held_response_prefix_cancels_only_active_timing() {
    tokio::time::timeout(
        Duration::from_secs(5),
        Box::pin(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let target = Target::Explicit(listener.local_addr().unwrap());
            let history = Inspection::new(PreviewMode::Off);
            let (mut visitor, stream) = tokio::io::duplex(8192);
            let (first_tx, first_rx) = tokio::sync::oneshot::channel();
            let backend = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                timing_io::read_head(&mut socket).await;
                socket.write_all(b"H").await.unwrap();
                first_tx.send(()).unwrap();
                let mut discarded = Vec::new();
                socket.read_to_end(&mut discarded).await.unwrap();
            });
            visitor
                .write_all(b"GET / HTTP/1.1\r\nHost: local\r\n\r\n")
                .await
                .unwrap();
            visitor.shutdown().await.unwrap();
            let task = tokio::spawn(forward(Box::pin(stream), target, context(&history)));
            first_rx.await.unwrap();
            let live = snapshot(&history);
            assert_eq!(live.intervals.response_read.outcome, Outcome::Active);
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            backend.await.unwrap();
            let terminal = snapshot(&history);
            assert!(terminal.terminal);
            assert_eq!(terminal.intervals.metadata.outcome, Outcome::Skipped);
            assert_eq!(terminal.intervals.request_head.outcome, Outcome::Completed);
            assert_eq!(terminal.intervals.resolve.outcome, Outcome::Skipped);
            assert_eq!(terminal.intervals.connect.outcome, Outcome::Completed);
            assert_eq!(terminal.intervals.request_write.outcome, Outcome::Completed);
            assert_eq!(terminal.intervals.response_read.outcome, Outcome::Cancelled);
        }),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn cancellation_during_unadmitted_metadata_or_head_finalizes_only_started_phases() {
    use tnl_protocol::request_metadata::DataStreamProtocol;
    for metadata in [false, true] {
        let history = Inspection::new(PreviewMode::Off);
        let mut ctx = context(&history);
        ctx.data_stream_protocol = if metadata {
            DataStreamProtocol::Http1MetadataV1
        } else {
            DataStreamProtocol::RawHttp1
        };
        let (_visitor, stream) = tokio::io::duplex(8192);
        {
            let mut future = Box::pin(forward(
                Box::pin(stream),
                Target::Explicit("127.0.0.1:9".parse().unwrap()),
                ctx,
            ));
            tokio::select! {biased; _=&mut future=>panic!("input is deliberately held"), ()=std::future::ready(())=>{}}
            assert!(
                history.search("").is_empty(),
                "early probes do not allocate a capture"
            );
        }
        let s = snapshot(&history);
        assert!(s.terminal);
        assert_eq!(
            s.intervals.metadata.outcome,
            if metadata {
                Outcome::Cancelled
            } else {
                Outcome::Skipped
            }
        );
        assert_eq!(
            s.intervals.request_head.outcome,
            if metadata {
                Outcome::NotStarted
            } else {
                Outcome::Cancelled
            }
        );
        assert_eq!(s.intervals.resolve.outcome, Outcome::NotStarted);
        assert_eq!(s.intervals.connect.outcome, Outcome::NotStarted);
    }
}
