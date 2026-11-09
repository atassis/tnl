use std::time::Duration;
use tnl::{
    forwarder::{forward, ForwardCtx},
    inspection::{Inspection, PreviewMode, TunnelGeneration},
    target::Target,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn context(history: &Inspection) -> ForwardCtx {
    let mut ctx = ForwardCtx::new("fixture".into(), None, env!("CARGO_PKG_VERSION"));
    ctx.inspection = Some((history.clone(), TunnelGeneration::new()));
    ctx
}
fn record(history: &Inspection) -> serde_json::Value {
    let records = history.search("");
    assert_eq!(records.len(), 1, "one accepted-substream record");
    let text = serde_json::to_string(&records[0]).unwrap();
    for secret in ["QUERY_CANARY", "HEADER_CANARY", "COOKIE_CANARY"] {
        assert!(!text.contains(secret));
        assert!(!format!("{:?}", records[0]).contains(secret));
    }
    serde_json::from_str(&text).unwrap()
}
#[tokio::test]
async fn refused_connect_has_sanitized_preconnect_synth_and_zero_backend_writes() {
    let vacant = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = vacant.local_addr().unwrap();
    drop(vacant);
    let history = Inspection::new(PreviewMode::Json);
    let ctx = context(&history);
    let (mut visitor, stream) = tokio::io::duplex(8192);
    let client = async {
        visitor.write_all(b"GET /failure?token=QUERY_CANARY HTTP/1.1\r\nHost: public.local\r\nAuthorization: HEADER_CANARY\r\nCookie: COOKIE_CANARY\r\n\r\n").await.unwrap();
        visitor.shutdown().await.unwrap();
        let mut response = Vec::new();
        visitor.read_to_end(&mut response).await.unwrap();
        assert!(response.starts_with(b"HTTP/1.1 502"));
        response.len()
    };
    let (result, count) = tokio::join!(
        forward(Box::pin(stream), Target::Explicit(addr), ctx),
        client
    );
    result.unwrap();
    let row = record(&history);
    assert_eq!(row["schema_version"], 1);
    assert_eq!(row["request_boundary"], "preconnect_unavailable");
    assert_eq!(row["response_source"], "client_synth");
    assert_eq!(row["failure_kind"], "connect-refused");
    assert_eq!(row["state"], "failed");
    assert_eq!(row["bytes_to_backend"], 0);
    assert_eq!(row["bytes_from_backend"], 0);
    assert_eq!(row["bytes_to_client"], count);
    assert_eq!(row["origin"], "external");
    assert_eq!(row["tunnel"], "fixture");
    assert_eq!(row["request_id"], serde_json::Value::Null);
    assert_eq!(row["request"]["headers_state"], "complete");
    assert_eq!(row["response"]["headers_state"], "complete");
    assert!(row["started_at_ms"].as_u64().unwrap() > 1_000_000);
    let timing = &row["timing"];
    assert_eq!(timing["availability"], "available");
    let snapshot = &timing["snapshot"];
    assert_eq!(snapshot["terminal"], true);
    let intervals = &snapshot["intervals"];
    assert_eq!(intervals["metadata"]["outcome"], "skipped");
    assert_eq!(intervals["request_head"]["outcome"], "completed");
    assert_eq!(intervals["resolve"]["outcome"], "skipped");
    assert_eq!(
        intervals["resolve"]["start_offset_us"],
        serde_json::Value::Null
    );
    assert_eq!(intervals["connect"]["outcome"], "failed");
    assert_eq!(intervals["request_write"]["outcome"], "not_started");
    assert_eq!(
        snapshot["backend_first_byte_offset_us"],
        serde_json::Value::Null
    );
    assert_eq!(
        snapshot["backend_final_headers_offset_us"],
        serde_json::Value::Null
    );
}
#[tokio::test]
async fn backend_malformed_probe_is_client_synth_not_a_backend_502() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let history = Inspection::new(PreviewMode::Off);
    let ctx = context(&history);
    let (mut visitor, stream) = tokio::io::duplex(8192);
    let backend = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut head = [0; 1024];
        assert!(socket.read(&mut head).await.unwrap() > 0);
        socket.write_all(b"NOT_HTTP!").await.unwrap();
    };
    let client = async {
        visitor
            .write_all(b"GET / HTTP/1.1\r\nHost: local\r\n\r\n")
            .await
            .unwrap();
        visitor.shutdown().await.unwrap();
        let mut body = Vec::new();
        visitor.read_to_end(&mut body).await.unwrap();
        assert!(body.starts_with(b"HTTP/1.1 502"));
    };
    let (result, (), ()) = tokio::join!(
        forward(Box::pin(stream), Target::Explicit(addr), ctx),
        backend,
        client
    );
    result.unwrap();
    let row = record(&history);
    assert_eq!(row["response_source"], "client_synth");
    assert_eq!(row["request_boundary"], "effective_backend");
    assert_eq!(row["failure_kind"], "local-malformed");
    assert_eq!(row["state"], "failed");
    assert_eq!(row["status"], 502);
    assert_eq!(row["bytes_from_backend"], 0);
    let timing = &row["timing"]["snapshot"];
    assert!(timing["backend_first_byte_offset_us"].as_u64().is_some());
    assert_eq!(
        timing["backend_final_headers_offset_us"],
        serde_json::Value::Null
    );
    assert_eq!(timing["intervals"]["response_read"]["outcome"], "failed");
    assert_eq!(
        timing["intervals"]["response_write"]["outcome"],
        "completed"
    );
}
#[tokio::test]
async fn partial_http_looking_backend_prefix_eof_keeps_first_byte_without_final_header() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let history = Inspection::new(PreviewMode::Off);
    let ctx = context(&history);
    let (mut visitor, stream) = tokio::io::duplex(8192);
    let backend = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 1024];
        assert!(socket.read(&mut request).await.unwrap() > 0);
        socket.write_all(b"HTTP/1.").await.unwrap();
    };
    let client = async {
        visitor
            .write_all(b"GET / HTTP/1.1\r\nHost: local\r\n\r\n")
            .await
            .unwrap();
        visitor.shutdown().await.unwrap();
        let mut response = Vec::new();
        visitor.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, b"HTTP/1.");
    };
    let (result, (), ()) = tokio::join!(
        forward(Box::pin(stream), Target::Explicit(addr), ctx),
        backend,
        client
    );
    result.unwrap();
    let row = record(&history);
    assert_eq!(row["response_source"], "backend");
    assert_eq!(row["state"], "incomplete");
    let timing = &row["timing"]["snapshot"];
    assert!(timing["backend_first_byte_offset_us"].as_u64().is_some());
    assert_eq!(
        timing["backend_final_headers_offset_us"],
        serde_json::Value::Null
    );
    assert_eq!(
        timing["response_framing_end_offset_us"],
        serde_json::Value::Null
    );
    assert_eq!(timing["intervals"]["response_read"]["outcome"], "completed");
}

#[tokio::test]
async fn cancellation_publishes_incomplete_transport_and_monotonic_duration() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let history = Inspection::new(PreviewMode::Off);
    let ctx = context(&history);
    let (mut visitor, stream) = tokio::io::duplex(8192);
    visitor
        .write_all(b"POST / HTTP/1.1\r\nHost: local\r\nContent-Length: 99999\r\n\r\nabc")
        .await
        .unwrap();
    let task = tokio::spawn(forward(Box::pin(stream), Target::Explicit(addr), ctx));
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut head = [0; 1024];
    assert!(socket.read(&mut head).await.unwrap() > 0);
    tokio::time::sleep(Duration::from_millis(5)).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let row = record(&history);
    assert_eq!(row["state"], "incomplete");
    assert_eq!(row["failure_kind"], "cancelled");
    assert_eq!(row["response_source"], "unavailable");
    assert!(row["duration_ms"].as_u64().unwrap() >= 1);
    assert!(row["bytes_to_backend"].as_u64().unwrap() > 0);
    assert_eq!(row["bytes_from_backend"], 0);
}

#[tokio::test]
async fn cancellation_while_waiting_for_a_head_or_after_upload_is_not_an_io_failure() {
    for send_head in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let history = Inspection::new(PreviewMode::Off);
        let ctx = context(&history);
        let (mut visitor, stream) = tokio::io::duplex(8192);
        if send_head {
            visitor
                .write_all(b"GET / HTTP/1.1\r\nHost: local\r\n\r\n")
                .await
                .unwrap();
            visitor.shutdown().await.unwrap();
        }
        let task = tokio::spawn(forward(Box::pin(stream), Target::Explicit(addr), ctx));
        let _socket = if send_head {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 1024];
            assert!(socket.read(&mut bytes).await.unwrap() > 0);
            Some(socket)
        } else {
            None
        };
        tokio::time::sleep(Duration::from_millis(5)).await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let row = record(&history);
        assert_eq!(row["failure_kind"], "cancelled");
        assert_eq!(row["state"], "incomplete");
    }
}

#[tokio::test]
async fn backend_error_statuses_do_not_gain_client_synth_provenance_from_headers() {
    for status in [400, 502, 503] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let history = Inspection::new(PreviewMode::Off);
        let ctx = context(&history);
        let (mut visitor, stream) = tokio::io::duplex(8192);
        let backend = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            socket.read_to_end(&mut request).await.unwrap();
            socket.write_all(format!("HTTP/1.1 {status} Error\r\nX-Tnl-Component: client\r\nContent-Length: 0\r\n\r\n").as_bytes()).await.unwrap();
        };
        let client = async {
            visitor
                .write_all(b"GET / HTTP/1.1\r\nHost: local\r\n\r\n")
                .await
                .unwrap();
            visitor.shutdown().await.unwrap();
            let mut response = Vec::new();
            visitor.read_to_end(&mut response).await.unwrap();
        };
        let (result, (), ()) = tokio::join!(
            forward(Box::pin(stream), Target::Explicit(addr), ctx),
            backend,
            client
        );
        result.unwrap();
        let row = record(&history);
        assert_eq!(row["status"], status);
        assert_eq!(row["response_source"], "backend");
        assert_eq!(row["state"], "complete");
        assert!(row["failure_kind"].is_null());
    }
}
#[tokio::test]
async fn no_head_and_probe_read_error_still_publish_one_record() {
    for no_head in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let history = Inspection::new(PreviewMode::Off);
        let ctx = context(&history);
        let (mut visitor, stream) = tokio::io::duplex(8192);
        let backend = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            if !no_head {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
            }
            drop(socket);
        };
        let client = async {
            if !no_head {
                visitor
                    .write_all(b"GET /?token=QUERY_CANARY HTTP/1.1\r\nHost: local\r\n\r\n")
                    .await
                    .unwrap();
            }
            visitor.shutdown().await.unwrap();
            let mut response = Vec::new();
            let _ = visitor.read_to_end(&mut response).await;
        };
        let (result, (), ()) = tokio::join!(
            forward(Box::pin(stream), Target::Explicit(addr), ctx),
            backend,
            client
        );
        let row = record(&history);
        if no_head {
            result.unwrap();
            assert_eq!(row["request_boundary"], "preconnect_unavailable");
            assert_eq!(row["request"]["headers_state"], "unavailable");
            assert_eq!(row["response_source"], "client_synth");
            let timing = &row["timing"]["snapshot"];
            assert_eq!(
                timing["backend_first_byte_offset_us"],
                serde_json::Value::Null
            );
            assert_eq!(
                timing["backend_final_headers_offset_us"],
                serde_json::Value::Null
            );
            assert_eq!(
                timing["response_framing_end_offset_us"],
                serde_json::Value::Null
            );
        } else {
            assert!(result.is_err());
            assert_eq!(row["failure_kind"], "response-probe");
            assert_eq!(row["response_source"], "unavailable");
        }
    }
}
#[tokio::test]
async fn truncated_backend_body_remains_incomplete_not_complete() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let history = Inspection::new(PreviewMode::Json);
    let ctx = context(&history);
    let (mut visitor, stream) = tokio::io::duplex(8192);
    let response=b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 99\r\n\r\n{\"token\":\"QUERY_CANARY";
    let backend = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        socket.read_to_end(&mut head).await.unwrap();
        socket.write_all(response).await.unwrap();
    };
    let client = async {
        visitor
            .write_all(b"GET / HTTP/1.1\r\nHost: local\r\n\r\n")
            .await
            .unwrap();
        visitor.shutdown().await.unwrap();
        let mut body = Vec::new();
        visitor.read_to_end(&mut body).await.unwrap();
        assert_eq!(body, response);
    };
    let (result, (), ()) = tokio::join!(
        forward(Box::pin(stream), Target::Explicit(addr), ctx),
        backend,
        client
    );
    result.unwrap();
    let row = record(&history);
    assert_eq!(row["state"], "incomplete");
    assert_eq!(row["response"]["headers_state"], "complete");
    assert!(row["response"]["body"]["text"].is_null());
    assert_eq!(row["bytes_from_backend"], response.len());
}

#[tokio::test]
async fn partial_upload_counts_only_successful_writes_and_preserves_early_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let history = Inspection::new(PreviewMode::Off);
    let ctx = context(&history);
    let (visitor, stream) = tokio::io::duplex(8192);
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    let response = b"HTTP/1.1 413 Too Large\r\nContent-Length: 0\r\n\r\n";
    let backend = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut head = [0; 1024];
        assert!(socket.read(&mut head).await.unwrap() > 0);
        socket.write_all(response).await.unwrap();
        socket.shutdown().await.unwrap();
        let _ = seen_rx.await;
        drop(socket);
    };
    let client = async {
        let (mut reader, mut writer) = tokio::io::split(visitor);
        let upload = async {
            let _ = writer
                .write_all(b"POST / HTTP/1.1\r\nHost: local\r\nContent-Length: 67108864\r\n\r\n")
                .await;
            for _ in 0..8192 {
                if writer.write_all(&[b'x'; 8192]).await.is_err() {
                    break;
                }
            }
        };
        let receive = async {
            let mut received = Vec::new();
            reader.read_to_end(&mut received).await.unwrap();
            assert_eq!(received, response);
            let _ = seen_tx.send(());
        };
        tokio::join!(upload, receive);
    };
    let (result, (), ()) = tokio::time::timeout(
        Duration::from_secs(5),
        Box::pin(async {
            tokio::join!(
                forward(Box::pin(stream), Target::Explicit(addr), ctx),
                backend,
                client
            )
        }),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    let row = record(&history);
    assert_eq!(row["state"], "failed");
    assert_eq!(row["failure_kind"], "upload-io");
    assert_eq!(row["response_source"], "backend");
    assert_eq!(row["status"], 413);
    let count = row["bytes_to_backend"].as_u64().unwrap();
    assert!(count > 0 && count < 67_108_864);
    assert_eq!(row["bytes_from_backend"], response.len());
}

#[tokio::test]
async fn preconnect_uses_only_typed_identity_and_never_admits_a_raw_consumer() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tnl_protocol::request_metadata::{
        write_request_metadata, DataStreamProtocol, RequestMetadataV1, VisitorIdentity,
        VisitorIpSource,
    };
    struct Factory(Arc<AtomicUsize>);
    impl tnl::inspection::ConsumerFactory for Factory {
        fn admit(
            &self,
            _: tnl::inspection::CaptureContext,
        ) -> Option<Box<dyn tnl::inspection::ObservationConsumer>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            None
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let history =
        Inspection::with_consumer(PreviewMode::Json, Some(Arc::new(Factory(calls.clone()))));
    let mut ctx = context(&history);
    ctx.data_stream_protocol = DataStreamProtocol::Http1MetadataV1;
    let vacant = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = vacant.local_addr().unwrap();
    drop(vacant);
    let metadata = RequestMetadataV1 {
        request_id: ulid::Ulid::new(),
        visitor: VisitorIdentity {
            visitor_ip: "203.0.113.99".parse().unwrap(),
            peer_ip: "198.51.100.99".parse().unwrap(),
            source: VisitorIpSource::Peer,
        },
    };
    let (mut visitor, stream) = tokio::io::duplex(8192);
    let client = async {
        write_request_metadata(&mut visitor, &metadata)
            .await
            .unwrap();
        visitor.write_all(b"GET /?token=QUERY_CANARY HTTP/1.1\r\nHost: local\r\nX-Tnl-Request-Id: FORGED_CANARY\r\n\r\n").await.unwrap();
        visitor.shutdown().await.unwrap();
        let mut response = Vec::new();
        visitor.read_to_end(&mut response).await.unwrap();
    };
    let (result, ()) = tokio::join!(
        forward(Box::pin(stream), Target::Explicit(addr), ctx),
        client
    );
    result.unwrap();
    let row = record(&history);
    assert_eq!(row["request_id"], metadata.request_id.to_string());
    assert_eq!(row["visitor"]["display_ip"], "[REDACTED]");
    assert!(!row.to_string().contains("203.0.113.99"));
    assert!(!row.to_string().contains("198.51.100.99"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
