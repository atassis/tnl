use tnl::inspection::{Inspection, PreviewMode, TunnelGeneration};

#[test]
fn fragmented_informational_binary_and_redaction() {
    let history = Inspection::new(PreviewMode::Json);
    let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
    let id = capture.id();
    let request = b"POST /api?token=QUERY_CANARY&token=OTHER HTTP/1.1\r\nHost: localhost\r\nAuthorization: HEADER_CANARY\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n1d\r\n{\"token\":\"JSON_CANARY\",\"n\":1}\r\n0\r\n\r\n";
    for byte in request {
        capture.request(std::slice::from_ref(byte));
    }
    let response = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n\xff\x00\xfe";
    for byte in response {
        capture.response(std::slice::from_ref(byte));
    }
    capture.finish();
    let record = history.get(id).unwrap();
    assert_eq!(record.status, Some(200));
    let safe = format!("{record:?}");
    for canary in ["QUERY_CANARY", "HEADER_CANARY", "JSON_CANARY", "OTHER"] {
        assert!(!safe.contains(canary), "{safe}");
    }
    assert!(record
        .request
        .body
        .text
        .as_deref()
        .unwrap()
        .contains("[REDACTED]"));
    assert!(history.search("JSON_CANARY").is_empty());
}

#[test]
fn upgrade_discards_opaque_frames_and_admission_is_bounded() {
    let history = Inspection::new(PreviewMode::Off);
    let mut captures: Vec<_> = (0..64)
        .map(|_| history.admit(TunnelGeneration::new(), None).unwrap())
        .collect();
    assert!(history.admit(TunnelGeneration::new(), None).is_none());
    let capture = &mut captures[0];
    capture.request(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
    capture.response(b"HTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n\xffFRAME_CANARY");
    let record = history.get(capture.id()).unwrap();
    assert_eq!(record.status, Some(101));
    assert_eq!(record.state, tnl::inspection::State::Upgraded);
    assert!(!format!("{record:?}").contains("FRAME_CANARY"));
}

#[test]
fn ambiguous_and_truncated_messages_never_expose_partial_json() {
    for head in [
        "Content-Length: 10\r\nTransfer-Encoding: chunked",
        "Content-Length: 10\r\nContent-Length: 11",
        "Content-Length: 100",
    ] {
        let history = Inspection::new(PreviewMode::Json);
        let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
        let id = capture.id();
        capture.request(format!("POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n{head}\r\n\r\n{{\"token\":\"").as_bytes());
        capture.finish();
        let record = history.get(id).unwrap();
        assert!(record.request.body.text.is_none());
        assert!(record.observation_incomplete);
    }
}

#[test]
fn json_preview_caps_and_every_two_fragment_split() {
    let body = r#"{"token":"SPLIT_CANARY","ok":"é"}"#.as_bytes();
    let mut wire=format!("POST /?secret=QUERY_CANARY HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",body.len()).into_bytes();
    wire.extend_from_slice(body);
    for split in 0..=wire.len() {
        let history = Inspection::new(PreviewMode::Json);
        let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
        capture.request(&wire[..split]);
        capture.request(&wire[split..]);
        capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
        capture.finish();
        let record = history.get(capture.id()).unwrap();
        assert!(!record.observation_incomplete, "split {split}: {record:?}");
        assert!(record
            .request
            .body
            .text
            .as_ref()
            .is_some_and(|s| !s.contains("SPLIT_CANARY")));
    }
    for n in [4096, 4097, 65536] {
        let body = format!("\"{}\"", "a".repeat(n - 2));
        let history = Inspection::new(PreviewMode::Json);
        let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
        capture.request(format!("POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {n}\r\n\r\n").as_bytes());
        for chunk in body.as_bytes().chunks(13) {
            capture.request(chunk);
        }
        capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
        capture.finish();
        assert_eq!(
            history
                .get(capture.id())
                .unwrap()
                .request
                .body
                .text
                .is_some(),
            n == 4096
        );
    }
}

#[test]
fn cancellation_does_not_complete_close_delimited_response() {
    let history = Inspection::new(PreviewMode::Json);
    let id = {
        let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
        capture.request(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
        capture.response(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"ok\":1}");
        capture.id()
    };
    let record = history.get(id).unwrap();
    assert!(record.observation_incomplete);
    assert!(record.response.body.text.is_none());
}

#[test]
fn structural_json_budget_is_checked_before_tree_allocation() {
    for body in [
        format!("[{}]", vec!["0"; 1000].join(",")),
        format!("{}0{}", "[".repeat(33), "]".repeat(33)),
    ] {
        let history = Inspection::new(PreviewMode::Json);
        let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
        capture.request(format!("POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes());
        capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
        capture.finish();
        assert!(history
            .get(capture.id())
            .unwrap()
            .request
            .body
            .text
            .is_none());
    }
}

#[test]
fn consumer_shares_capture_id_and_receives_each_decoded_byte_once() {
    use std::sync::{Arc, Mutex};
    use tnl::inspection::{
        CaptureContext, ConsumerFactory, Direction, Observation, ObservationConsumer,
    };
    #[derive(Default)]
    struct Counts {
        id: Option<tnl::inspection::CaptureId>,
        request: usize,
        response: usize,
        heads: usize,
        finished: usize,
    }
    struct Factory(Arc<Mutex<Counts>>);
    struct Consumer(Arc<Mutex<Counts>>);
    impl ConsumerFactory for Factory {
        fn admit(&self, context: CaptureContext) -> Option<Box<dyn ObservationConsumer>> {
            self.0.lock().unwrap().id = Some(context.capture_id);
            Some(Box::new(Consumer(self.0.clone())))
        }
    }
    impl ObservationConsumer for Consumer {
        fn observe(&mut self, event: &Observation<'_>) {
            let mut counts = self.0.lock().unwrap();
            match event {
                Observation::Body(Direction::Request, bytes) => counts.request += bytes.len(),
                Observation::Body(Direction::Response, bytes) => counts.response += bytes.len(),
                Observation::RequestHead { .. } | Observation::ResponseHead { .. } => {
                    counts.heads += 1;
                }
                Observation::Finished => counts.finished += 1,
                _ => {}
            }
        }
    }
    let counts = Arc::new(Mutex::new(Counts::default()));
    let history =
        Inspection::with_consumer(PreviewMode::Off, Some(Arc::new(Factory(counts.clone()))));
    let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
    capture.request(b"POST / HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n");
    capture.response(
        b"HTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n\xff\xfe",
    );
    capture.finish();
    capture.finish();
    let counts = counts.lock().unwrap();
    assert_eq!(counts.id, Some(capture.id()));
    assert_eq!(counts.request, 5);
    assert_eq!(counts.response, 2);
    assert_eq!(counts.heads, 3);
    assert_eq!(counts.finished, 1);
    drop(counts);
}

#[test]
fn metadata_is_typed_and_not_inferred_from_headers() {
    use tnl_protocol::request_metadata::{RequestMetadataV1, VisitorIdentity, VisitorIpSource};
    let metadata = RequestMetadataV1 {
        request_id: ulid::Ulid::new(),
        visitor: VisitorIdentity {
            visitor_ip: "192.0.2.7".parse().unwrap(),
            peer_ip: "192.0.2.8".parse().unwrap(),
            source: VisitorIpSource::Peer,
        },
    };
    let history = Inspection::new(PreviewMode::Off);
    let mut capture = history
        .admit(TunnelGeneration::new(), Some(&metadata))
        .unwrap();
    capture.request(b"GET / HTTP/1.1\r\nHost: localhost\r\nX-Tnl-Request-Id: spoof\r\nX-Forwarded-For: IP_CANARY\r\n\r\n");
    let record = history.get(capture.id()).unwrap();
    assert_eq!(record.request_id, Some(metadata.request_id));
    let debug = format!("{record:?}");
    assert!(!debug.contains("192.0.2."));
    assert!(!debug.contains("IP_CANARY"));
}

#[test]
fn capped_heads_and_invalid_chunks_stop_only_observation() {
    for wire in [
        format!("GET / HTTP/1.1\r\nLong: {}", "x".repeat(20000)),
        "POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nCANARY".into(),
        "POST / HTTP/1.1\r\nTransfer-Encoding: gzip, chunked\r\n\r\nCANARY".into(),
    ] {
        let history = Inspection::new(PreviewMode::Json);
        let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
        capture.request(wire.as_bytes());
        capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
        capture.finish();
        let record = history.get(capture.id()).unwrap();
        assert!(record.observation_incomplete);
        assert!(record.request.body.text.is_none());
        assert!(!format!("{record:?}").contains("CANARY"));
    }
}

#[path = "support/inspection_consumer.rs"]
mod inspection_consumer;

fn recorded_inspection() -> (
    Inspection,
    std::sync::Arc<std::sync::Mutex<inspection_consumer::Recorded>>,
) {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(
        inspection_consumer::Recorded::default(),
    ));
    (
        Inspection::with_consumer(
            PreviewMode::Json,
            Some(std::sync::Arc::new(inspection_consumer::Recorder(
                seen.clone(),
            ))),
        ),
        seen,
    )
}

#[test]
fn trailer_syntax_is_validated_across_every_split_without_retaining_values() {
    use tnl::inspection::Direction;
    for (trailer, valid) in [
        (&b"X-Check: fine\r\n"[..], true),
        (&b"X-Check:\t\xff obs-text\t\r\n"[..], true),
        (&b":\x00\r\n"[..], false),
        (&b"Bad Name: value\r\n"[..], false),
        (&b"X: value\x00\r\n"[..], false),
        (&b"X: value\x1f\r\n"[..], false),
        (&b"X: value\x7f\r\n"[..], false),
        (&b"X: bare\nnewline\r\n"[..], false),
        (&b" folded: value\r\n"[..], false),
    ] {
        for split in 0..=trailer.len() {
            let (history, seen) = recorded_inspection();
            let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
            capture.request(b"POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n");
            capture.request(&trailer[..split]);
            capture.request(&trailer[split..]);
            capture.request(b"\r\n");
            capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
            capture.finish();
            let record = history.get(capture.id()).unwrap();
            assert_eq!(
                record.observation_incomplete, !valid,
                "trailer {trailer:?}, split {split}"
            );
            let seen = seen.lock().unwrap();
            assert_eq!(seen.ends.contains(&(Direction::Request, true)), valid);
            assert_eq!(
                seen.stops
                    .contains(&(Direction::Request, "invalid-trailer")),
                !valid
            );
            assert!(!seen.head_input.windows(4).any(|w| w == b"fine"));
            drop(seen);
        }
    }
}

#[test]
fn informational_count_is_admitted_before_callbacks() {
    for count in [8, 9] {
        for fragment in [1, 7, 8192] {
            let (history, seen) = recorded_inspection();
            let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
            capture.request(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
            let wire = format!(
                "{}HTTP/1.1 204 No Content\r\n\r\n",
                "HTTP/1.1 103 Early Hints\r\n\r\n".repeat(count)
            );
            for bytes in wire.as_bytes().chunks(fragment) {
                capture.response(bytes);
            }
            capture.finish();
            let seen = seen.lock().unwrap();
            assert_eq!(seen.statuses.iter().filter(|s| **s == 103).count(), 8);
            assert_eq!(
                seen.statuses.last(),
                Some(if count == 8 { &204 } else { &103 })
            );
            assert_eq!(
                seen.stops
                    .iter()
                    .any(|(_, reason)| *reason == "informational-limit"),
                count == 9
            );
            drop(seen);
            assert_eq!(
                history.get(capture.id()).unwrap().observation_incomplete,
                count == 9
            );
        }
    }
}

fn padded_response_head(status: u16, size: usize) -> Vec<u8> {
    let start = format!("HTTP/1.1 {status} Status\r\nX: ");
    let head = format!("{start}{}\r\n\r\n", "a".repeat(size - start.len() - 4));
    assert_eq!(head.len(), size);
    head.into_bytes()
}

#[test]
fn cumulative_response_heads_have_an_independent_32kib_callback_budget() {
    for (sizes, final_size, valid) in [
        (vec![8192; 3], 8192, true),
        (vec![8192, 8192, 8192, 8161], 32, false),
    ] {
        for fragment in [1, 97, 8192] {
            let (history, seen) = recorded_inspection();
            let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
            capture.request(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
            let mut wire: Vec<_> = sizes
                .iter()
                .flat_map(|size| padded_response_head(103, *size))
                .collect();
            wire.extend_from_slice(&padded_response_head(204, final_size));
            assert_eq!(wire.len(), if valid { 32768 } else { 32769 });
            for bytes in wire.chunks(fragment) {
                capture.response(bytes);
            }
            capture.finish();
            let seen = seen.lock().unwrap();
            assert_eq!(
                seen.statuses.iter().filter(|s| **s == 103).count(),
                sizes.len()
            );
            assert_eq!(seen.statuses.contains(&204), valid);
            assert_eq!(
                seen.stops.iter().any(|(_, reason)| *reason == "head-limit"),
                !valid
            );
            drop(seen);
            assert_eq!(
                history.get(capture.id()).unwrap().observation_incomplete,
                !valid
            );
        }
    }
}

#[test]
fn upgrade_suppresses_same_read_and_later_opaque_consumer_inputs() {
    for fragment in [1, 7, 8192] {
        let (history, seen) = recorded_inspection();
        let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
        capture.request(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
        let handshake = b"HTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n\xffSAME_READ_CANARY\x00HTTP/1.1 200 OK\r\nX: JSON_CANARY\r\n\r\n{\"token\":\"FRAME_CANARY\"}";
        for bytes in handshake.chunks(fragment) {
            capture.response(bytes);
        }
        capture.response(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\nLATER_CANARY");
        capture.request(b"POST /REQUEST_CANARY HTTP/1.1\r\n\r\n");
        let record = history.get(capture.id()).unwrap();
        assert_eq!(record.status, Some(101));
        assert_eq!(record.state, tnl::inspection::State::Upgraded);
        capture.finish();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.statuses, [103, 101]);
        assert!(seen.request.is_empty());
        assert!(seen.response.is_empty());
        assert!(!String::from_utf8_lossy(&seen.head_input).contains("CANARY"));
        assert!(seen.stops.is_empty());
        assert_eq!(seen.finished, 1);
        drop(seen);
        for safe in [
            format!("{record:?}"),
            serde_json::to_string(&record).unwrap(),
        ] {
            assert!(!safe.contains("CANARY"));
        }
        assert!(history.search("CANARY").is_empty());
    }
}

#[test]
fn message_end_is_revocable_and_repeated_stops_are_idempotent_consumer_inputs() {
    use tnl::inspection::Direction;
    let (history, seen) = recorded_inspection();
    {
        let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
        capture.request(
            b"POST / HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
        );
        assert!(seen
            .lock()
            .unwrap()
            .ends
            .contains(&(Direction::Request, false)));
        assert!(history
            .get(capture.id())
            .unwrap()
            .request
            .body
            .text
            .is_some());
        capture.request(b"unexpected bytes");
        let record = history.get(capture.id()).unwrap();
        assert!(record.observation_incomplete);
        assert!(record.request.body.text.is_none());
    } // cancellation may notify a stopped direction again
    let seen = seen.lock().unwrap();
    let request_stops: Vec<_> = seen
        .stops
        .iter()
        .filter(|(d, _)| *d == Direction::Request)
        .collect();
    assert_eq!(request_stops.len(), 2);
    assert_eq!(request_stops[0].1, "unexpected-data");
    assert_eq!(request_stops[1].1, "cancelled");
    // A consumer must apply each stop as the same sticky revocation, not undo
    // revocation on the second notification or use an exactly-once assertion.
    let mut complete = seen.ends.contains(&(Direction::Request, false));
    for _ in request_stops {
        complete = false;
    }
    assert!(!complete);
    assert_eq!(seen.finished, 1);
    drop(seen);
}

#[test]
fn header_field_and_head_byte_limits_are_exact() {
    for (fields, valid) in [(64, true), (65, false)] {
        let (history, seen) = recorded_inspection();
        let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
        capture.request(format!("GET / HTTP/1.1\r\n{}\r\n", "X: a\r\n".repeat(fields)).as_bytes());
        capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
        capture.finish();
        assert_eq!(
            history.get(capture.id()).unwrap().observation_incomplete,
            !valid
        );
        assert_eq!(
            seen.lock()
                .unwrap()
                .ends
                .iter()
                .any(|(d, _)| *d == tnl::inspection::Direction::Request),
            valid
        );
    }
    for (size, valid) in [(8192, true), (8193, false)] {
        for fragment in [1, 8193] {
            let (history, seen) = recorded_inspection();
            let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
            let start = "GET / HTTP/1.1\r\nX: ";
            let wire = format!("{start}{}\r\n\r\n", "a".repeat(size - start.len() - 4));
            for bytes in wire.as_bytes().chunks(fragment) {
                capture.request(bytes);
            }
            capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
            capture.finish();
            assert_eq!(
                history.get(capture.id()).unwrap().observation_incomplete,
                !valid
            );
            assert_eq!(
                seen.lock()
                    .unwrap()
                    .ends
                    .iter()
                    .any(|(d, _)| *d == tnl::inspection::Direction::Request),
                valid
            );
        }
    }
}

#[test]
fn trailer_aggregate_limit_is_exact_under_fragmentation() {
    for (size, valid) in [(8192, true), (8193, false)] {
        let (history, seen) = recorded_inspection();
        let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
        capture.request(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n");
        let wire = format!(
            "X: {}\r\nY: {}\r\n\r\n",
            "a".repeat(4090),
            "b".repeat(size - 4102)
        );
        assert_eq!(wire.len(), size);
        for bytes in wire.as_bytes().chunks(17) {
            capture.request(bytes);
        }
        capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
        capture.finish();
        assert_eq!(
            history.get(capture.id()).unwrap().observation_incomplete,
            !valid
        );
        assert_eq!(
            seen.lock()
                .unwrap()
                .ends
                .contains(&(tnl::inspection::Direction::Request, true)),
            valid
        );
    }
}

#[test]
fn ambiguous_lengths_and_malformed_chunk_boundaries_revoke_completion() {
    for wire in [
        &b"POST / HTTP/1.1\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}"[..],
        &b"POST / HTTP/1.1\r\nContent-Length: 18446744073709551616\r\n\r\n{}"[..],
        &b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}X\n0\r\n\r\n"[..],
        &b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n2;extension=yes\r\n{}\r\n0\r\n\r\n"
            [..],
    ] {
        for fragment in [1, 8192] {
            let (history, seen) = recorded_inspection();
            let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
            for bytes in wire.chunks(fragment) {
                capture.request(bytes);
            }
            capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
            capture.finish();
            assert!(history.get(capture.id()).unwrap().observation_incomplete);
            let seen = seen.lock().unwrap();
            assert!(!seen
                .ends
                .iter()
                .any(|(d, _)| *d == tnl::inspection::Direction::Request));
            assert!(seen
                .stops
                .iter()
                .any(|(d, _)| *d == tnl::inspection::Direction::Request));
            drop(seen);
        }
    }
}

#[test]
fn head_and_304_responses_ignore_advertised_entity_length() {
    for (method, status) in [("HEAD", 200), ("GET", 304)] {
        let (history, seen) = recorded_inspection();
        let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
        capture.request(format!("{method} / HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes());
        capture.response(
            format!("HTTP/1.1 {status} Status\r\nContent-Length: 1000\r\n\r\n").as_bytes(),
        );
        capture.finish();
        assert!(!history.get(capture.id()).unwrap().observation_incomplete);
        assert!(seen.lock().unwrap().response.is_empty());
    }
}

#[test]
fn recursive_json_query_and_header_canaries_are_absent_from_all_display_sinks() {
    let body = br#"{"nested":[{"ToKeN":"JSON_CANARY"}],"ok":true}"#;
    let history = Inspection::new(PreviewMode::Json);
    let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
    capture.request(format!("POST /api?unknown=QUERY_CANARY&unknown=SECOND_CANARY HTTP/1.1\r\nHost: localhost\r\nAuthorization: HEADER_CANARY\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes());
    for byte in body {
        capture.request(std::slice::from_ref(byte));
    }
    capture.response(b"HTTP/1.1 204 No Content\r\nSet-Cookie: COOKIE_CANARY\r\nLocation: LOCATION_CANARY\r\n\r\n");
    capture.finish();
    let record = history.get(capture.id()).unwrap();
    assert!(record.request.body.text.is_some());
    for canary in [
        "QUERY_CANARY",
        "SECOND_CANARY",
        "HEADER_CANARY",
        "JSON_CANARY",
        "COOKIE_CANARY",
        "LOCATION_CANARY",
    ] {
        assert!(!format!("{record:?}").contains(canary));
        assert!(!serde_json::to_string(&record).unwrap().contains(canary));
        assert!(history.search(canary).is_empty());
    }
    assert!(record
        .request
        .body
        .text
        .as_ref()
        .is_some_and(|s| s.contains("[REDACTED]")));
}

#[test]
fn invalid_encoded_or_ambiguous_json_is_omitted_without_raw_fallback() {
    for (headers, body) in [
        ("Content-Type: application/json\r\n", &b"\"\xff\""[..]),
        (
            "Content-Type: application/json\r\nContent-Encoding: gzip\r\n",
            &b"{\"token\":\"BODY_CANARY\"}"[..],
        ),
        ("Content-Type: text/plain\r\n", &b"BODY_CANARY"[..]),
        (
            "Content-Type: application/json\r\nContent-Type: application/json\r\n",
            &b"{\"token\":\"BODY_CANARY\"}"[..],
        ),
    ] {
        let history = Inspection::new(PreviewMode::Json);
        let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
        capture.request(
            format!(
                "POST / HTTP/1.1\r\n{headers}Content-Length: {}\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        );
        capture.request(body);
        capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
        capture.finish();
        let record = history.get(capture.id()).unwrap();
        assert!(record.request.body.text.is_none());
        assert!(!serde_json::to_string(&record).unwrap().contains("CANARY"));
        assert!(history.search("CANARY").is_empty());
    }
    let expansion = "\"[REDACTED]\"".len() - 1;
    for sanitized_size in [4096, 4097] {
        let history = Inspection::new(PreviewMode::Json);
        let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
        let input_size = sanitized_size - expansion;
        let body = format!(
            r#"{{"token":0,"padding":"{}"}}"#,
            "x".repeat(input_size - r#"{"token":0,"padding":""}"#.len())
        );
        assert_eq!(body.len(), input_size);
        assert!(body.len() <= 4096);
        capture.request(format!("POST / HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes());
        capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
        capture.finish();
        let record = history.get(capture.id()).unwrap();
        assert_eq!(record.request.body.text.is_some(), sanitized_size == 4096);
        if let Some(text) = &record.request.body.text {
            assert_eq!(text.len(), 4096);
        }
    }
}

#[test]
fn display_record_distinguishes_untracked_tunnel_availability() {
    let history = Inspection::new(PreviewMode::Off);
    let capture = history.admit(TunnelGeneration::new(), None).unwrap();
    let value = serde_json::to_value(history.get(capture.id()).unwrap()).unwrap();
    assert_eq!(value["connection"], "untracked");
}

#[test]
fn cancellation_after_both_message_ends_still_revokes_both_directions() {
    use tnl::inspection::Direction;
    for graceful in [true, false] {
        let (history, seen) = recorded_inspection();
        let id = {
            let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
            capture.request(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
            capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
            if graceful {
                capture.finish();
            }
            capture.id()
        };
        let seen = seen.lock().unwrap();
        assert_eq!(seen.ends.len(), 2);
        assert_eq!(
            seen.stops.contains(&(Direction::Request, "cancelled")),
            !graceful
        );
        assert_eq!(
            seen.stops.contains(&(Direction::Response, "cancelled")),
            !graceful
        );
        assert_eq!(seen.finished, 1);
        drop(seen);
        assert_eq!(history.get(id).unwrap().observation_incomplete, !graceful);
    }
}
