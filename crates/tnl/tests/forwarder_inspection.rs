use std::time::Duration;
use tnl::forwarder::{forward, ForwardCtx};
use tnl::host_header::HostHeader;
use tnl::inspection::{Inspection, PreviewMode, TunnelGeneration};
use tnl::target::Target;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn observer_never_changes_concurrent_upload_or_binary_response() {
    for mode in [None, Some(PreviewMode::Off), Some(PreviewMode::Json)] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (visitor, substream) = tokio::io::duplex(97);
        let history = mode.map(Inspection::new);
        let (tx, mut rx) = tokio::sync::mpsc::channel(2);
        let mut ctx = ForwardCtx::new("fixture".into(), Some(tx), env!("CARGO_PKG_VERSION"));
        ctx.inspection = history.clone().map(|h| (h, TunnelGeneration::new()));
        ctx.host_header = HostHeader::Fixed("effective.local".into());
        let body = vec![b'x'; 65536];
        let response=b"HTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n\xff\x00\xfe";
        let backend = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            socket.read_to_end(&mut request).await.unwrap();
            let split = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
            assert!(std::str::from_utf8(&request[..split])
                .unwrap()
                .contains("Host: effective.local\r\n"));
            assert_eq!(&request[split..], body.as_slice());
            for fragment in response.chunks(7) {
                socket.write_all(fragment).await.unwrap();
                tokio::task::yield_now().await;
            }
        };
        let visitor = async {
            let (mut reader, mut writer) = tokio::io::split(visitor);
            let send = async {
                writer.write_all(format!("POST /upload?token=QUERY_CANARY HTTP/1.1\r\nHost: public.local\r\nAuthorization: HEADER_CANARY\r\nContent-Length: {}\r\n\r\n",body.len()).as_bytes()).await.unwrap();
                for chunk in body.chunks(103) {
                    writer.write_all(chunk).await.unwrap();
                }
                writer.shutdown().await.unwrap();
            };
            let receive = async {
                let mut received = Vec::new();
                reader.read_to_end(&mut received).await.unwrap();
                assert_eq!(received, response);
            };
            tokio::join!(send, receive);
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            let (result, (), ()) = tokio::join!(
                Box::pin(forward(Box::pin(substream), Target::Explicit(addr), ctx)),
                backend,
                visitor
            );
            result.unwrap();
        })
        .await
        .unwrap();
        let log = rx.recv().await.unwrap();
        if let Some(history) = history {
            let records = history.search("");
            assert_eq!(records.len(), 1);
            let record = &records[0];
            assert_eq!(record.status, Some(200));
            assert!(!record.observation_incomplete, "{record:?}");
            assert_eq!(record.request.body.observed_payload_bytes, 65536);
            assert!(record
                .request
                .headers
                .iter()
                .any(|h| h.name == "host" && h.value == "effective.local"));
            assert!(!format!("{records:?}").contains("CANARY"));
            assert_eq!(log.status, Some(200));
            assert!(!log.path.contains("CANARY"));
            assert!(!format!("{log:?}").contains("CANARY"));
            assert!(!serde_json::to_string(&log).unwrap().contains("CANARY"));
        }
    }
}

#[tokio::test]
async fn malformed_observation_preserves_live_forwarded_bytes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let history = Inspection::new(PreviewMode::Json);
    let mut ctx = ForwardCtx::new("ambiguous".into(), None, env!("CARGO_PKG_VERSION"));
    ctx.inspection = Some((history.clone(), TunnelGeneration::new()));
    ctx.host_header = HostHeader::Preserve;
    let request = b"POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4\r\nTransfer-Encoding: chunked\r\n\r\nwire";
    let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
    let (mut visitor, substream) = tokio::io::duplex(97);
    let backend = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut received = Vec::new();
        socket.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, request);
        socket.write_all(response).await.unwrap();
    };
    let visitor = async {
        visitor.write_all(request).await.unwrap();
        visitor.shutdown().await.unwrap();
        let mut received = Vec::new();
        visitor.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, response);
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        let (result, (), ()) = tokio::join!(
            Box::pin(forward(Box::pin(substream), Target::Explicit(addr), ctx)),
            backend,
            visitor,
        );
        result.unwrap();
    })
    .await
    .unwrap();
    let records = history.search("");
    assert_eq!(records.len(), 1);
    assert!(records[0].observation_incomplete);
}

async fn host_auto_exchange(response: &[u8], mode: u8) -> (Option<u16>, bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let history = Inspection::new(PreviewMode::Off);
    let _held: Vec<_> = if mode == 2 {
        (0..64)
            .map(|_| history.admit(TunnelGeneration::new(), None).unwrap())
            .collect()
    } else {
        Vec::new()
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let mut ctx = ForwardCtx::new("host-auto".into(), Some(tx), env!("CARGO_PKG_VERSION"));
    if mode != 0 {
        ctx.inspection = Some((history, TunnelGeneration::new()));
    }
    let rewrite = ctx.rewrite_active.clone();
    let request = b"GET / HTTP/1.1\r\nHost: public.local\r\n\r\n";
    let (mut visitor, substream) = tokio::io::duplex(1024);
    let backend = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut received = Vec::new();
        socket.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, request);
        socket.write_all(response).await.unwrap();
    };
    let visitor = async {
        visitor.write_all(request).await.unwrap();
        visitor.shutdown().await.unwrap();
        let mut received = Vec::new();
        visitor.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, response);
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        let (result, (), ()) = tokio::join!(
            Box::pin(forward(Box::pin(substream), Target::Explicit(addr), ctx)),
            backend,
            visitor
        );
        result.unwrap();
    })
    .await
    .unwrap();
    (
        rx.recv().await.unwrap().status,
        rewrite.load(std::sync::atomic::Ordering::Relaxed),
    )
}

#[tokio::test]
async fn rejected_observer_head_cannot_change_host_auto_status_or_rewrite() {
    for interim in ["", "HTTP/1.1 103 Early Hints\r\n\r\n"] {
        for (status, expected_rewrite) in [(403, true), (200, false)] {
            let response = format!(
                "{interim}HTTP/1.1 {status} Final\r\n{}Content-Length: 15\r\n\r\nBlocked request",
                "X:a\r\n".repeat(65)
            );
            assert!(response.len() < 1024);
            for mode in 0..3 {
                assert_eq!(
                    host_auto_exchange(response.as_bytes(), mode).await,
                    (Some(status), expected_rewrite),
                    "inspection mode {mode}"
                );
            }
        }
    }
    for mode in 0..3 {
        assert_eq!(
            host_auto_exchange(
                b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\n\r\n",
                mode
            )
            .await,
            (None, false)
        );
    }
}

#[path = "support/inspection_consumer.rs"]
mod inspection_consumer;

fn chunked_wire(entity: &[u8]) -> Vec<u8> {
    let mut wire = Vec::new();
    for chunk in entity.chunks(257) {
        wire.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        wire.extend_from_slice(chunk);
        wire.extend_from_slice(b"\r\n");
    }
    wire.extend_from_slice(b"0\r\n\r\n");
    wire
}

#[tokio::test]
async fn actual_forwarder_consumer_records_exact_decoded_leftover_prefix_and_pumps() {
    use std::sync::{Arc, Mutex};
    let seen = Arc::new(Mutex::new(inspection_consumer::Recorded::default()));
    let history = Inspection::with_consumer(
        PreviewMode::Off,
        Some(Arc::new(inspection_consumer::Recorder(seen.clone()))),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut ctx = ForwardCtx::new("recording".into(), None, env!("CARGO_PKG_VERSION"));
    ctx.inspection = Some((history.clone(), TunnelGeneration::new()));
    ctx.host_header = HostHeader::Fixed("effective.local".into());
    let entity: Vec<_> = (0u8..=250).cycle().take(16384).collect();
    let response_entity: Vec<_> = (0u8..=255).rev().cycle().take(2049).collect();
    let wire = chunked_wire(&entity);
    let mut response =
        b"HTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n"
            .to_vec();
    response.extend_from_slice(&chunked_wire(&response_entity));
    let (mut visitor, substream) = tokio::io::duplex(1024);
    let mut initial =
        b"POST / HTTP/1.1\r\nHost: public.local\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    initial.extend_from_slice(&wire[..31]);
    // Preload head + body before polling forward: the request probe must have
    // a nonempty body leftover, with the rest larger than its entire buffer.
    visitor.write_all(&initial).await.unwrap();
    let backend = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut received = Vec::new();
        socket.read_to_end(&mut received).await.unwrap();
        let split = received.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        assert!(std::str::from_utf8(&received[..split])
            .unwrap()
            .contains("Host: effective.local\r\n"));
        assert_eq!(&received[split..], wire);
        for fragment in response.chunks(113) {
            socket.write_all(fragment).await.unwrap();
        }
    };
    let visitor = async {
        let (mut reader, mut writer) = tokio::io::split(visitor);
        let upload = async {
            for fragment in wire[31..].chunks(149) {
                writer.write_all(fragment).await.unwrap();
            }
            writer.shutdown().await.unwrap();
        };
        let download = async {
            let mut received = Vec::new();
            reader.read_to_end(&mut received).await.unwrap();
            assert_eq!(received, response);
        };
        tokio::join!(upload, download);
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        let (result, (), ()) = tokio::join!(
            Box::pin(forward(Box::pin(substream), Target::Explicit(addr), ctx)),
            backend,
            visitor
        );
        result.unwrap();
    })
    .await
    .unwrap();
    let records = history.search("");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.context.unwrap().capture_id, records[0].capture_id);
    assert_eq!(seen.context.unwrap().backend, Some(addr));
    assert_eq!(seen.request, entity);
    assert_eq!(seen.response, response_entity);
    assert_eq!(seen.statuses, [103, 200]);
    assert!(seen.stops.is_empty());
    assert_eq!(seen.finished, 1);
    assert_eq!(seen.ends.len(), 2);
    assert!(seen
        .head_input
        .windows(b"effective.local".len())
        .any(|w| w == b"effective.local"));
    drop(seen);
}

#[tokio::test]
async fn host_auto_never_uses_display_status_to_interpret_an_informational_prefix() {
    // The bounded forwarding prefix contains an informational signature but
    // not the final response. A more capable display observer must not turn
    // that partial evidence into a different live Host Auto decision.
    let response = format!("HTTP/1.1 103 Early Hints\r\nX-Note: Blocked request\r\nX-Pad: {}\r\n\r\nHTTP/1.1 403 Forbidden\r\nContent-Length: 15\r\n\r\nBlocked request", "a".repeat(1100));
    for mode in 0..3 {
        let (_, rewrite) = host_auto_exchange(response.as_bytes(), mode).await;
        assert!(
            !rewrite,
            "inspection mode {mode} changed Host Auto's evidence"
        );
    }
}

#[tokio::test]
async fn live_status_fallback_preserves_lf_binary_and_skips_complete_informational_heads() {
    for mode in 0..3 {
        for interim in [
            "",
            "HTTP/1.1 100 Continue\n\n",
            "HTTP/1.1 103 Early Hints\nLink: </asset>\n\n",
            "HTTP/1.1 100 Continue\n\nHTTP/1.1 103 Early Hints\n\n",
        ] {
            let mut response =
                format!("{interim}HTTP/1.1 200 OK\nContent-Length: 1\n\n").into_bytes();
            response.push(0xff);
            assert_eq!(
                host_auto_exchange(&response, mode).await,
                (Some(200), false),
                "inspection mode {mode}, interim {interim:?}"
            );
        }
        for response in [
            b"HTTP/1.1 100 Continue\n".as_slice(),
            b"HTTP/1.1 103 Early Hints\nLink: </asset>\n",
            b"HTTP/1.1 100 Continue\n\nHTTP/1.1 103 Early Hints\n\n",
            b"HTTP/1.1 103 Early Hints\n\nHTTP/1.1 200 OK",
            b"HTTP/1.1 103 Early Hints\nLink: HTTP/1.1 200 OK\n",
            b"HTTP/1.1 20 OK\n\n\xff",
            b"HTTP/1.1 2000 OK\n\n\xff",
            b"HTTP/1.1 200X OK\n\n\xff",
        ] {
            assert_eq!(
                host_auto_exchange(response, mode).await,
                (None, false),
                "inspection mode {mode}, response {response:?}"
            );
        }
    }
}
