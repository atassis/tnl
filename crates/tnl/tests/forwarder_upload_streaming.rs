//! Real backend sockets and bounded substreams exercise upload/response ordering.

use std::time::Duration;

use tnl::forwarder::{forward, ForwardCtx};
use tnl::host_header::HostHeader;
use tnl::peek::{peek_request_head, RequestHeadProbe, REQUEST_HEAD_PROBE_MAX};
use tnl::target::Target;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const DEADLINE: Duration = Duration::from_secs(3);
const RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK";

async fn read_head(socket: &mut TcpStream) -> (Vec<u8>, Vec<u8>) {
    match peek_request_head(socket, REQUEST_HEAD_PROBE_MAX, DEADLINE)
        .await
        .unwrap()
    {
        RequestHeadProbe::Complete { head, leftover } => (head, leftover),
        other => panic!("incomplete backend request head: {other:?}"),
    }
}

async fn upload_waiting_for_full_body(chunked: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let body = vec![b'x'; REQUEST_HEAD_PROBE_MAX * 8];
    let (head, wire_body) = if chunked {
        let mut wire = Vec::new();
        for chunk in body.chunks(701) {
            wire.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            wire.extend_from_slice(chunk);
            wire.extend_from_slice(b"\r\n");
        }
        wire.extend_from_slice(b"0\r\nX-Trailer: done\r\n\r\n");
        (
            "POST /upload HTTP/1.1\r\nHost: public.example\r\nTransfer-Encoding: chunked\r\n\r\n"
                .to_string(),
            wire,
        )
    } else {
        (
            format!(
                "POST /upload HTTP/1.1\r\nHost: public.example\r\nContent-Length: {}\r\n\r\n",
                body.len()
            ),
            body,
        )
    };
    let (visitor, substream) = tokio::io::duplex(1024);
    let (log_tx, mut log_rx) = tokio::sync::mpsc::channel(2);
    let mut ctx = ForwardCtx::new("upload".into(), Some(log_tx), env!("CARGO_PKG_VERSION"));
    ctx.host_header = HostHeader::Fixed("backend.example".into());

    let backend = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (backend_head, mut received) = read_head(&mut socket).await;
        assert!(String::from_utf8_lossy(&backend_head).contains("Host: backend.example\r\n"));
        // Require the entire body AND propagated request half-close before replying.
        socket.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, wire_body);
        socket.write_all(RESPONSE).await.unwrap();
        backend_head.len()
    };
    let visitor = async {
        let (mut reader, mut writer) = tokio::io::split(visitor);
        let send = async {
            writer.write_all(head.as_bytes()).await.unwrap();
            for chunk in wire_body.chunks(97) {
                writer.write_all(chunk).await.unwrap();
                tokio::task::yield_now().await;
            }
            writer.shutdown().await.unwrap();
        };
        let receive = async {
            let mut response = Vec::new();
            reader.read_to_end(&mut response).await.unwrap();
            assert_eq!(response, RESPONSE);
        };
        tokio::join!(send, receive);
    };
    let exchange = async {
        let (result, backend_head_len, ()) = tokio::join!(
            Box::pin(forward(Box::pin(substream), Target::Explicit(addr), ctx)),
            backend,
            visitor,
        );
        result.unwrap();
        let log = log_rx.recv().await.unwrap();
        assert_eq!(log.status, Some(200));
        assert_eq!(log.failure_kind, None);
        assert_eq!(log.bytes_in, (backend_head_len + wire_body.len()) as u64);
        assert_eq!(log.bytes_out, RESPONSE.len() as u64);
        assert!(
            log_rx.recv().await.is_none(),
            "exactly one log per exchange"
        );
    };
    tokio::time::timeout(DEADLINE, exchange)
        .await
        .expect("upload stalled while backend waited for the full request body");
}

#[tokio::test]
async fn body_larger_than_probe_reaches_backend_before_response() {
    upload_waiting_for_full_body(false).await;
}

#[tokio::test]
async fn fragmented_chunked_upload_reaches_backend_before_response() {
    upload_waiting_for_full_body(true).await;
}

#[tokio::test]
async fn early_response_is_relayed_before_upload_finishes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (mut visitor, substream) = tokio::io::duplex(1024);
    let (log_tx, mut log_rx) = tokio::sync::mpsc::channel(1);
    let backend = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (_, mut body) = read_head(&mut socket).await;
        // Like 100 Continue: the visitor cannot send the body until this arrives.
        socket
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .await
            .unwrap();
        socket.read_to_end(&mut body).await.unwrap();
        assert_eq!(body, b"body");
        socket.write_all(RESPONSE).await.unwrap();
    };
    let visitor = async {
        visitor
            .write_all(
                b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 4\r\nExpect: 100-continue\r\n\r\n",
            )
            .await
            .unwrap();
        let mut interim = vec![0; b"HTTP/1.1 100 Continue\r\n\r\n".len()];
        visitor.read_exact(&mut interim).await.unwrap();
        assert_eq!(interim, b"HTTP/1.1 100 Continue\r\n\r\n");
        visitor.write_all(b"body").await.unwrap();
        visitor.shutdown().await.unwrap();
        let mut response = Vec::new();
        visitor.read_to_end(&mut response).await.unwrap();
        assert_eq!(
            response, RESPONSE,
            "no synthetic response after response starts"
        );
    };
    tokio::time::timeout(DEADLINE, async {
        let (result, (), ()) = tokio::join!(
            Box::pin(forward(
                Box::pin(substream),
                Target::Explicit(addr),
                ForwardCtx::new("early".into(), Some(log_tx), env!("CARGO_PKG_VERSION"))
            )),
            backend,
            visitor,
        );
        result.unwrap();
        let log = log_rx.recv().await.unwrap();
        // T4's independent status fallback now skips the provisional 100;
        // upload/response ordering still belongs to this P0 regression.
        assert_eq!(log.status, Some(200));
        assert_eq!(log.failure_kind, None);
    })
    .await
    .expect("early response/upload exchange stalled");
}

#[tokio::test]
async fn early_final_response_half_close_does_not_stop_upload() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (mut visitor, substream) = tokio::io::duplex(1024);
    let response = b"HTTP/1.1 413 Content Too Large\r\nContent-Length: 0\r\n\r\n";
    let body = vec![b'x'; REQUEST_HEAD_PROBE_MAX * 8];
    let backend = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (_, mut received) = read_head(&mut socket).await;
        socket.write_all(response).await.unwrap();
        socket.shutdown().await.unwrap();
        socket.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, body);
    };
    let visitor = async {
        visitor
            .write_all(
                format!(
                    "POST / HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut received = Vec::new();
        // Response EOF must propagate independently of the still-open upload.
        visitor.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, response);
        visitor.write_all(&body).await.unwrap();
        visitor.shutdown().await.unwrap();
    };
    tokio::time::timeout(DEADLINE, async {
        let (result, (), ()) = tokio::join!(
            Box::pin(forward(
                Box::pin(substream),
                Target::Explicit(addr),
                ForwardCtx::new("early-final".into(), None, env!("CARGO_PKG_VERSION"))
            )),
            backend,
            visitor,
        );
        result.unwrap();
    })
    .await
    .expect("response half-close blocked the remaining upload");
}

async fn cancellation_releases_both_directions(response_started: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (mut visitor, substream) = tokio::io::duplex(1024);
    let (log_tx, mut log_rx) = tokio::sync::mpsc::channel(1);
    let mut driver = Box::pin(forward(
        Box::pin(substream),
        Target::Explicit(addr),
        ForwardCtx::new("cancel".into(), Some(log_tx), env!("CARGO_PKG_VERSION")),
    ));
    let setup = async {
        visitor
            .write_all(b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 65536\r\n\r\n")
            .await
            .unwrap();
        let (mut socket, _) = listener.accept().await.unwrap();
        let (_, leftover) = read_head(&mut socket).await;
        assert!(leftover.is_empty());
        // Prove the remaining upload pump is running before cancelling it.
        let body = vec![b'x'; REQUEST_HEAD_PROBE_MAX * 2];
        let send = visitor.write_all(&body);
        let receive = async {
            let mut received = vec![0; body.len()];
            socket.read_exact(&mut received).await.unwrap();
            assert_eq!(received, body);
        };
        let (write_result, ()) = tokio::join!(send, receive);
        write_result.unwrap();
        if response_started {
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 65536\r\n\r\nx")
                .await
                .unwrap();
            let mut prefix = vec![0; b"HTTP/1.1 200 OK\r\nContent-Length: 65536\r\n\r\nx".len()];
            visitor.read_exact(&mut prefix).await.unwrap();
            assert_eq!(prefix, b"HTTP/1.1 200 OK\r\nContent-Length: 65536\r\n\r\nx");
        }
        socket
    };
    let mut socket = tokio::time::timeout(DEADLINE, async {
        tokio::select! {
            result = &mut driver => panic!("forwarder exited before cancellation: {result:?}"),
            socket = setup => socket,
        }
    })
    .await
    .expect("forwarder did not upload before cancellation");
    // Drop the forwarding future itself: no spawned pump may retain either socket.
    drop(driver);
    tokio::time::timeout(DEADLINE, async {
        let mut remaining = Vec::new();
        socket.read_to_end(&mut remaining).await.unwrap();
        assert!(remaining.is_empty());
        visitor.read_to_end(&mut remaining).await.unwrap();
        assert!(
            remaining.is_empty(),
            "cancellation must not synthesize a second response"
        );
        assert!(log_rx.recv().await.is_none());
    })
    .await
    .expect("cancelled pump retained a socket or log sender");
}

#[tokio::test]
async fn cancellation_during_response_probe_releases_both_directions() {
    cancellation_releases_both_directions(false).await;
}

#[tokio::test]
async fn cancellation_after_response_starts_releases_both_directions() {
    cancellation_releases_both_directions(true).await;
}

async fn synth_during_open_upload(response: &[u8], expected_kind: &str) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (mut visitor, substream) = tokio::io::duplex(1024);
    let backend = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_head(&mut socket).await;
        if !response.is_empty() {
            socket.write_all(response).await.unwrap();
        }
        socket.shutdown().await.unwrap();
        let mut remaining = Vec::new();
        socket.read_to_end(&mut remaining).await.unwrap();
        assert!(remaining.is_empty());
    };
    let visitor = async {
        visitor
            .write_all(b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 65536\r\n\r\n")
            .await
            .unwrap();
        // Keep the upload open: a synthetic failure must cancel, not join it.
        let mut received = Vec::new();
        visitor.read_to_end(&mut received).await.unwrap();
        let text = String::from_utf8(received).unwrap();
        assert!(text.starts_with("HTTP/1.1 502 Bad Gateway\r\n"), "{text}");
        assert!(
            text.contains(&format!("X-Tnl-Origin-Failure: {expected_kind}\r\n")),
            "{text}"
        );
        assert_eq!(text.matches("HTTP/1.1 502 Bad Gateway\r\n").count(), 1);
    };
    tokio::time::timeout(DEADLINE, async {
        let (result, (), ()) = tokio::join!(
            Box::pin(forward(
                Box::pin(substream),
                Target::Explicit(addr),
                ForwardCtx::new("synth".into(), None, env!("CARGO_PKG_VERSION"))
            )),
            backend,
            visitor,
        );
        result.unwrap();
    })
    .await
    .expect("synthetic response waited for unfinished upload");
}

#[tokio::test]
async fn eof_before_response_synthesizes_without_waiting_for_upload() {
    synth_during_open_upload(b"", "local-eof").await;
}

#[tokio::test]
async fn malformed_response_synthesizes_without_waiting_for_upload() {
    synth_during_open_upload(b"not HTTP!", "local-malformed").await;
}

#[tokio::test]
async fn upload_larger_than_yamux_window_and_drop_based_shutdown() {
    use tnl_protocol::transport::YamuxSession;
    use tnl_protocol::Session as _;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (daemon_io, client_io) = tokio::io::duplex(16 * 1024);
    let mut daemon = YamuxSession::new_client(daemon_io);
    let mut client = YamuxSession::new_server(client_io);
    let body = vec![b'x'; 1024 * 1024];
    let backend = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (_, mut received) = read_head(&mut socket).await;
        socket.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, body);
        socket.write_all(RESPONSE).await.unwrap();
    };
    let visitor = async {
        let mut stream = daemon.open_stream().await.unwrap();
        stream
            .write_all(
                format!(
                    "POST / HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        stream.write_all(&body).await.unwrap();
        stream.shutdown().await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        assert_eq!(
            response, RESPONSE,
            "session drop must flush buffered response"
        );
        drop(daemon);
    };
    let forwarder = async {
        let stream = client.accept_stream().await.unwrap().unwrap();
        Box::pin(forward(
            stream,
            Target::Explicit(addr),
            ForwardCtx::new("yamux".into(), None, env!("CARGO_PKG_VERSION")),
        ))
        .await
        .unwrap();
        // Exercise production's graceful channel-drop path, never abort the driver.
        drop(client);
    };
    tokio::time::timeout(DEADLINE, async {
        tokio::join!(forwarder, backend, visitor);
    })
    .await
    .expect("upload stalled across yamux flow control");
}

#[tokio::test]
async fn backpressured_response_prefix_does_not_block_upload() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // Smaller than the nine-byte response probe, forcing prefix replay to yield.
    let (mut visitor, substream) = tokio::io::duplex(1);
    let body = vec![b'x'; REQUEST_HEAD_PROBE_MAX * 2];
    let backend = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (_, mut received) = read_head(&mut socket).await;
        socket.write_all(RESPONSE).await.unwrap();
        socket.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, body);
    };
    let visitor = async {
        visitor
            .write_all(
                format!(
                    "POST / HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        // Intentionally don't read the early response until upload completes.
        visitor.write_all(&body).await.unwrap();
        visitor.shutdown().await.unwrap();
        let mut response = Vec::new();
        visitor.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, RESPONSE);
    };
    tokio::time::timeout(DEADLINE, async {
        let (result, (), ()) = tokio::join!(
            Box::pin(forward(
                Box::pin(substream),
                Target::Explicit(addr),
                ForwardCtx::new("backpressure".into(), None, env!("CARGO_PKG_VERSION"))
            )),
            backend,
            visitor,
        );
        result.unwrap();
    })
    .await
    .expect("response prefix backpressure blocked request upload");
}
