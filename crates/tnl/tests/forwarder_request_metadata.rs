use std::time::Duration;

use tnl::forwarder::{forward, ForwardCtx};
use tnl::target::Target;
use tnl_protocol::request_metadata::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn metadata() -> RequestMetadataV1 {
    RequestMetadataV1 {
        request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        visitor: VisitorIdentity {
            visitor_ip: "2001:db8::7".parse().unwrap(),
            peer_ip: "127.0.0.1".parse().unwrap(),
            source: VisitorIpSource::XForwardedFor,
        },
    }
}

async fn request(port: u16, mode: DataStreamProtocol, prelude: &[u8]) -> tnl::inspector::LogLine {
    let (mut writer, reader) = tokio::io::duplex(65536);
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let mut ctx = ForwardCtx::new("demo".into(), Some(tx), env!("CARGO_PKG_VERSION"));
    ctx.data_stream_protocol = mode;
    let task = tokio::spawn(forward(Box::pin(reader), Target::LocalhostPort(port), ctx));
    writer.write_all(prelude).await.unwrap();
    writer
        .write_all(b"GET /hello HTTP/1.1\r\nHost: x\r\nX-Forwarded-For: 203.0.113.66\r\nX-Tnl-Request-Id: 01BX5ZZKBKACTAV9WEVGEMMVRZ\r\nAccept: application/json\r\n\r\n")
        .await
        .unwrap();
    writer.shutdown().await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), writer.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    task.await.unwrap().unwrap();
    let log = rx.recv().await.unwrap();
    assert_eq!(
        log.bytes_in,
        u64::try_from(
            b"GET /hello HTTP/1.1\r\nHost: x\r\nX-Forwarded-For: 203.0.113.66\r\nX-Tnl-Request-Id: 01BX5ZZKBKACTAV9WEVGEMMVRZ\r\nAccept: application/json\r\n\r\n".len()
        )
        .unwrap()
    );
    assert!(rx.recv().await.is_none(), "duplicate record");
    if mode == DataStreamProtocol::Http1MetadataV1 {
        assert_eq!(log.req_id, metadata().request_id);
        if log.status == Some(502) {
            let response = String::from_utf8(response).unwrap();
            assert!(
                response.to_ascii_lowercase().contains(&format!(
                    "x-tnl-request-id: {}",
                    metadata().request_id.to_string().to_ascii_lowercase()
                )),
                "{response}"
            );
            let body: serde_json::Value =
                serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body["request_id"], metadata().request_id.to_string());
        }
    } else {
        assert_ne!(log.req_id.to_string(), "01BX5ZZKBKACTAV9WEVGEMMVRZ");
        assert_ne!(log.req_id, metadata().request_id);
        let response = String::from_utf8(response).unwrap();
        let body: serde_json::Value =
            serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(
            body["request_id"],
            log.req_id.to_string(),
            "raw synth ID stays local"
        );
    }
    log
}

#[tokio::test]
async fn forwarder_request_metadata_keeps_identity_on_connect_failure_and_legacy_never_infers() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut frame = Vec::new();
    write_request_metadata(&mut frame, &metadata())
        .await
        .unwrap();
    let log = request(port, DataStreamProtocol::Http1MetadataV1, &frame).await;
    assert_eq!(log.req_id, metadata().request_id);
    assert_eq!(log.visitor_ip, Some(metadata().visitor.visitor_ip));
    assert_eq!(log.peer_ip, Some(metadata().visitor.peer_ip));
    assert_eq!(log.visitor_ip_source, Some(VisitorIpSource::XForwardedFor));
    assert_eq!(log.failure_kind.as_deref(), Some("connect-refused"));
    let legacy = request(port, DataStreamProtocol::RawHttp1, &[]).await;
    let json = serde_json::to_value(legacy).unwrap();
    for field in ["visitor_ip", "peer_ip", "visitor_ip_source"] {
        assert!(json.get(field).unwrap().is_null());
    }
}

#[tokio::test]
async fn forwarder_request_metadata_is_removed_before_backend_and_survives_phase2_failure() {
    let mut frame = Vec::new();
    write_request_metadata(&mut frame, &metadata())
        .await
        .unwrap();
    for failure in [
        None,
        Some("local-malformed"),
        Some("local-eof"),
        Some("local-no-response"),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let backend = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            while !bytes.windows(4).any(|v| v == b"\r\n\r\n") {
                bytes.push(socket.read_u8().await.unwrap());
            }
            assert!(bytes.starts_with(b"GET /hello HTTP/1.1\r\n"));
            if failure == Some("local-malformed") {
                socket.write_all(b"garbage\r\n").await.unwrap();
            } else if failure.is_none() {
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .await
                    .unwrap();
            } else if failure == Some("local-no-response") {
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });
        let log = request(port, DataStreamProtocol::Http1MetadataV1, &frame).await;
        assert_eq!(log.status, Some(if failure.is_some() { 502 } else { 200 }));
        assert_eq!(log.req_id, metadata().request_id);
        assert_eq!(log.visitor_ip, Some(metadata().visitor.visitor_ip));
        assert_eq!(log.visitor_ip_source, Some(VisitorIpSource::XForwardedFor));
        assert_eq!(log.failure_kind.as_deref(), failure);
        backend.abort();
    }
}

#[tokio::test(start_paused = true)]
async fn forwarder_request_metadata_bad_or_stalled_prelude_never_connects() {
    let good_json = serde_json::to_string(&metadata()).unwrap();
    let mut invalid_frames = vec![vec![0, 0, 4, 1], vec![0, 0, 0, 2, b'{'], vec![]];
    for json in [
        good_json.replace("01ARZ3NDEKTSV4RRFFQ69G5FAV", "00000000000000000000000000"),
        good_json.replace("01ARZ3NDEKTSV4RRFFQ69G5FAV", "invalid"),
        good_json.replace("2001:db8::7", "not-an-ip"),
    ] {
        let mut frame = u32::try_from(json.len()).unwrap().to_be_bytes().to_vec();
        frame.extend_from_slice(json.as_bytes());
        invalid_frames.push(frame);
    }
    for bytes in invalid_frames {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (mut writer, reader) = tokio::io::duplex(65536);
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let mut ctx = ForwardCtx::new("demo".into(), Some(tx), env!("CARGO_PKG_VERSION"));
        ctx.data_stream_protocol = DataStreamProtocol::Http1MetadataV1;
        let task = tokio::spawn(forward(
            Box::pin(reader),
            Target::LocalhostPort(listener.local_addr().unwrap().port()),
            ctx,
        ));
        writer.write_all(&bytes).await.unwrap();
        if !bytes.is_empty() {
            writer.shutdown().await.unwrap();
        }
        assert!(tokio::time::timeout(Duration::from_secs(6), task)
            .await
            .unwrap()
            .unwrap()
            .is_err());
        assert!(rx.recv().await.is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(1), listener.accept())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn forwarder_request_metadata_stall_closes_only_bad_yamux_substream() {
    use tnl_protocol::Session as _;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let ws = async_tungstenite::tokio::accept_async(socket)
            .await
            .unwrap();
        tnl_protocol::transport::server_session_from_ws_generic(ws)
    });
    let (ws, _) = async_tungstenite::tokio::connect_async(format!("ws://{addr}"))
        .await
        .unwrap();
    let client = tnl_protocol::transport::client_session_from_ws_generic(ws);
    let mut server = server.await.unwrap();
    let reserved = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = reserved.local_addr().unwrap().port();
    drop(reserved);
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let ctx = ForwardCtx {
        data_stream_protocol: DataStreamProtocol::Http1MetadataV1,
        ..ForwardCtx::new("demo".into(), Some(tx), env!("CARGO_PKG_VERSION"))
    };
    let accept = tokio::spawn(tnl::client::run_accept_loop(
        Box::new(client),
        Target::LocalhostPort(port),
        ctx,
    ));
    let mut bad = server.open_stream().await.unwrap();
    bad.write_all(&[0, 0]).await.unwrap();
    bad.flush().await.unwrap();
    let mut good = server.open_stream().await.unwrap();
    write_request_metadata(&mut good, &metadata())
        .await
        .unwrap();
    good.write_all(b"GET /good HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    good.shutdown().await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), good.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(response.starts_with(b"HTTP/1.1 502"));
    assert_eq!(
        rx.recv().await.unwrap().visitor_ip,
        Some(metadata().visitor.visitor_ip)
    );
    let result = tokio::time::timeout(Duration::from_secs(6), bad.read_u8())
        .await
        .unwrap();
    assert!(result.is_err(), "stalled stream must be closed");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .is_err(),
        "no record for undecodable prelude"
    );
    // A later stream on the same session still works after the timeout.
    let mut later = server.open_stream().await.unwrap();
    write_request_metadata(&mut later, &metadata())
        .await
        .unwrap();
    later
        .write_all(b"GET /later HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    later.shutdown().await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), later.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(response.starts_with(b"HTTP/1.1 502"));
    assert_eq!(rx.recv().await.unwrap().path, "/later");
    accept.abort();
}

#[tokio::test]
async fn forwarder_request_metadata_preserves_coalesced_chunked_body_bytes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let http = b"POST /body HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nbody\r\n0\r\n\r\n";
    let backend = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut received = vec![0; http.len()];
        socket.read_exact(&mut received).await.unwrap();
        assert_eq!(received, http);
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .unwrap();
    });
    let (mut writer, reader) = tokio::io::duplex(65536);
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let ctx = ForwardCtx {
        data_stream_protocol: DataStreamProtocol::Http1MetadataV1,
        ..ForwardCtx::new("demo".into(), Some(tx), env!("CARGO_PKG_VERSION"))
    };
    let task = tokio::spawn(forward(Box::pin(reader), Target::LocalhostPort(port), ctx));
    let mut frame = Vec::new();
    write_request_metadata(&mut frame, &metadata())
        .await
        .unwrap();
    frame.extend_from_slice(http);
    writer.write_all(&frame).await.unwrap();
    writer.shutdown().await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), writer.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    task.await.unwrap().unwrap();
    backend.await.unwrap();
    let log = rx.recv().await.unwrap();
    assert_eq!(log.bytes_in, u64::try_from(http.len()).unwrap());
    assert_eq!(log.req_id, metadata().request_id);
    assert_eq!(log.visitor_ip, Some(metadata().visitor.visitor_ip));
    assert_eq!(log.status, Some(200));
}

#[tokio::test]
async fn forwarder_request_metadata_survives_capped_http_probe() {
    let reserved = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = reserved.local_addr().unwrap().port();
    drop(reserved);
    let (mut writer, reader) = tokio::io::duplex(65536);
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let ctx = ForwardCtx {
        data_stream_protocol: DataStreamProtocol::Http1MetadataV1,
        ..ForwardCtx::new("demo".into(), Some(tx), env!("CARGO_PKG_VERSION"))
    };
    let task = tokio::spawn(forward(Box::pin(reader), Target::LocalhostPort(port), ctx));
    write_request_metadata(&mut writer, &metadata())
        .await
        .unwrap();
    writer
        .write_all(
            format!(
                "GET /large HTTP/1.1\r\nHost: x\r\nX-Large: {}\r\n\r\n",
                "a".repeat(9000)
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    writer.shutdown().await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), writer.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    task.await.unwrap().unwrap();
    let log = rx.recv().await.unwrap();
    assert_eq!(log.path, "/large");
    assert_eq!(log.req_id, metadata().request_id);
    assert_eq!(log.visitor_ip, Some(metadata().visitor.visitor_ip));
    assert_eq!(log.visitor_ip_source, Some(VisitorIpSource::XForwardedFor));
    assert_eq!(log.failure_kind.as_deref(), Some("connect-refused"));
}
