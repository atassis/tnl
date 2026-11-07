//! Isolated integration-test process: the timeout override must not affect
//! other forwarder scenarios running in parallel.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tnl::forwarder::{forward, ForwardCtx, LOCAL_FIRST_BYTE_TIMEOUT, TEST_FIRST_BYTE_TIMEOUT_MS};
use tnl::peek::{peek_request_head, RequestHeadProbe, REQUEST_HEAD_PROBE_MAX};
use tnl::target::Target;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn first_response_deadline_expires_while_upload_makes_progress() {
    assert_eq!(LOCAL_FIRST_BYTE_TIMEOUT, Duration::from_secs(15));
    TEST_FIRST_BYTE_TIMEOUT_MS.store(250, Ordering::Relaxed);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (visitor, substream) = tokio::io::duplex(1024);
    let (mut reader, mut writer) = tokio::io::split(visitor);
    let (log_tx, mut log_rx) = tokio::sync::mpsc::channel(1);
    let backend_body_bytes = AtomicU64::new(0);
    let backend = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let RequestHeadProbe::Complete { leftover, .. } =
            peek_request_head(&mut socket, REQUEST_HEAD_PROBE_MAX, Duration::from_secs(2))
                .await
                .unwrap()
        else {
            panic!("missing request head")
        };
        backend_body_bytes.fetch_add(leftover.len() as u64, Ordering::Relaxed);
        let mut buf = [0; 4096];
        loop {
            let n = socket.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            backend_body_bytes.fetch_add(n as u64, Ordering::Relaxed);
        }
    };
    let send = async {
        writer
            .write_all(b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 1000000000\r\n\r\n")
            .await
            .unwrap();
        loop {
            if writer.write_all(&[b'x'; 2048]).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    let receive = async {
        let mut response = Vec::new();
        reader.read_to_end(&mut response).await.unwrap();
        let text = String::from_utf8(response).unwrap();
        assert!(text.starts_with("HTTP/1.1 502 Bad Gateway\r\n"), "{text}");
        assert!(
            text.contains("X-Tnl-Origin-Failure: local-no-response\r\n"),
            "{text}"
        );
        text.len()
    };
    let outcome = tokio::time::timeout(Duration::from_secs(3), async {
        let (result, (), (), response_len) = tokio::join!(
            Box::pin(forward(
                Box::pin(substream),
                Target::Explicit(addr),
                ForwardCtx::new("deadline".into(), Some(log_tx), env!("CARGO_PKG_VERSION"))
            )),
            backend,
            send,
            receive,
        );
        result.unwrap();
        let body_bytes = backend_body_bytes.load(Ordering::Relaxed);
        assert!(
            body_bytes > REQUEST_HEAD_PROBE_MAX as u64,
            "upload must progress during probe: {body_bytes}"
        );
        let log = log_rx.recv().await.unwrap();
        assert_eq!(log.status, Some(502));
        assert_eq!(log.failure_kind.as_deref(), Some("local-no-response"));
        assert!(log.bytes_in > body_bytes);
        assert_eq!(log.bytes_out, response_len as u64);
        assert!(log_rx.recv().await.is_none());
    })
    .await;
    TEST_FIRST_BYTE_TIMEOUT_MS.store(0, Ordering::Relaxed);
    outcome.expect("upload reset the first-response deadline or a pump leaked");
}
