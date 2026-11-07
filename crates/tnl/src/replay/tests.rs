use super::*;
use crate::inspection::{
    CaptureContext, ConsumerFactory, Direction, Inspection, Observation, PreviewMode,
};

fn setup(enabled: bool) -> (ReplayController, TunnelGeneration, CaptureId) {
    let controller = ReplayController::new(enabled, 49151).unwrap();
    let generation = TunnelGeneration::new();
    controller.activate(generation);
    let display = Inspection::new(PreviewMode::Off);
    let capture = display.admit(generation, None).unwrap();
    (controller, generation, capture.id())
}
fn context(id: CaptureId, generation: TunnelGeneration) -> CaptureContext {
    CaptureContext {
        capture_id: id,
        generation,
        request_id: None,
        backend: Some("127.0.0.1:12345".parse().unwrap()),
    }
}
fn head(consumer: &mut dyn crate::inspection::ObservationConsumer) {
    consumer.observe(&Observation::RequestHead {
        method: "POST",
        target: "/signed?secret=QUERY_CANARY",
        headers: &[
            httparse::Header {
                name: "Host",
                value: b"effective.example",
            },
            httparse::Header {
                name: "Authorization",
                value: b"Bearer HEADER_CANARY",
            },
            httparse::Header {
                name: "Content-Length",
                value: b"4",
            },
        ],
    });
    consumer.observe(&Observation::Body(Direction::Request, b"\0\xffab"));
    consumer.observe(&Observation::MessageEnd {
        direction: Direction::Request,
        trailers: false,
    });
}
#[test]
fn consent_destination_and_terminal_revocation() {
    let (off, generation, id) = setup(false);
    assert!(off.admit(context(id, generation)).is_none());
    let (controller, generation, id) = setup(true);
    let mut missing = context(id, generation);
    missing.backend = None;
    assert!(controller.admit(missing).is_none());
    for address in [
        "192.0.2.1:80",
        "0.0.0.0:80",
        "127.1.2.3:49151",
        "[::ffff:127.0.0.1]:49151",
    ] {
        let mut invalid = context(id, generation);
        invalid.backend = Some(address.parse().unwrap());
        assert!(controller.admit(invalid).is_none());
    }
    let mut consumer = controller.admit(context(id, generation)).unwrap();
    head(consumer.as_mut());
    assert_eq!(
        controller.availability(id).state,
        AvailabilityState::Pending
    );
    consumer.observe(&Observation::Finished);
    assert_eq!(
        controller.availability(id).state,
        AvailabilityState::Eligible
    );
    consumer.observe(&Observation::Stopped(Direction::Request, "unexpected-data"));
    consumer.observe(&Observation::Stopped(Direction::Request, "cancelled"));
    consumer.observe(&Observation::Finished);
    assert_ne!(
        controller.availability(id).state,
        AvailabilityState::Eligible
    );
}
#[test]
fn reattach_epoch_and_old_consumer_cannot_restore_authority() {
    let (controller, generation, id) = setup(true);
    let mut old = controller.admit(context(id, generation)).unwrap();
    head(old.as_mut());
    old.observe(&Observation::Finished);
    let preview = controller.prepare(id).unwrap();
    controller.deactivate();
    assert!(controller.prepare(id).is_err());
    controller.activate(generation);
    assert_eq!(
        controller.availability(id).state,
        AvailabilityState::Eligible
    );
    assert!(controller
        .confirm(id, preview.replay_id, &preview.confirmation_token, true)
        .is_err());
    let mut pending_context = context(CaptureId(ulid::Ulid::new()), generation);
    let mut pending = controller.admit(pending_context).unwrap();
    head(pending.as_mut());
    controller.activate(generation);
    pending.observe(&Observation::Finished);
    assert_ne!(
        controller.availability(pending_context.capture_id).state,
        AvailabilityState::Eligible
    );
    pending_context.generation = TunnelGeneration::new();
    controller.activate(pending_context.generation);
    old.observe(&Observation::Finished);
    assert!(controller.prepare(id).is_err());
}
#[test]
fn raw_privacy_upgrade_and_expiry() {
    let (controller, generation, id) = setup(true);
    let mut consumer = controller.admit(context(id, generation)).unwrap();
    head(consumer.as_mut());
    consumer.observe(&Observation::Finished);
    let preview = controller.prepare(id).unwrap();
    let output = format!(
        "{controller:?} {preview:?} {:?}",
        controller.availability(id)
    );
    let json = serde_json::to_string(&controller.availability(id)).unwrap();
    for canary in ["QUERY_CANARY", "HEADER_CANARY", "effective.example"] {
        assert!(!output.contains(canary));
        assert!(!json.contains(canary));
    }
    consumer.observe(&Observation::ResponseHead {
        status: 101,
        headers: &[],
    });
    consumer.observe(&Observation::Body(Direction::Request, b"OPAQUE_CANARY"));
    assert!(controller.prepare(id).is_err());
    controller.sweep_at(std::time::Instant::now() + retention::TTL);
    assert_eq!(controller.raw_bytes(), 0);
}
#[tokio::test]
async fn confirmed_exact_entity_host_no_redirect_and_single_use() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = backend.local_addr().unwrap();
    let (controller, generation, id) = setup(true);
    let mut ctx = context(id, generation);
    ctx.backend = Some(address);
    let mut consumer = controller.admit(ctx).unwrap();
    head(consumer.as_mut());
    consumer.observe(&Observation::Finished);
    let preview = controller.prepare(id).unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), backend.accept())
            .await
            .is_err()
    );
    assert!(controller
        .confirm(id, preview.replay_id, &preview.confirmation_token, false)
        .is_err());
    let receipt = controller
        .confirm(id, preview.replay_id, &preview.confirmation_token, true)
        .unwrap();
    let duplicate = controller
        .confirm(id, preview.replay_id, &preview.confirmation_token, true)
        .unwrap();
    assert_eq!(receipt.replay_id, duplicate.replay_id);
    let (mut stream, _) = backend.accept().await.unwrap();
    let mut wire = Vec::new();
    loop {
        let mut buf = [0; 1024];
        let n = stream.read(&mut buf).await.unwrap();
        wire.extend_from_slice(&buf[..n]);
        if wire.ends_with(b"\0\xffab") {
            break;
        }
    }
    assert!(wire.starts_with(b"POST /signed?secret=QUERY_CANARY HTTP/1.1\r\n"));
    assert!(wire
        .windows(b"host: effective.example".len())
        .any(|w| w == b"host: effective.example"));
    stream.write_all(b"HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:9/\r\nContent-Length: 0\r\n\r\n").await.unwrap();
    drop(stream);
    for _ in 0..100 {
        if controller.outcome(receipt.replay_id).unwrap().state != AttemptState::Running {
            break;
        }
        tokio::task::yield_now().await;
    }
    let outcome = controller.outcome(receipt.replay_id).unwrap();
    assert_eq!(outcome.state, AttemptState::Complete);
    assert_eq!(outcome.status, Some(307));
    controller
        .confirm(id, preview.replay_id, &preview.confirmation_token, true)
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), backend.accept())
            .await
            .is_err()
    );
}

#[test]
fn malformed_requests_trailers_limits_and_empty_entities() {
    type Case<'a> = (&'a str, &'a str, &'a [(&'a str, &'a [u8])]);
    let cases: &[Case<'_>] = &[
        ("CONNECT", "/", &[("Host", b"ok")]),
        ("TRACE", "/", &[("Host", b"ok")]),
        ("GET", "http://127.0.0.1/", &[("Host", b"ok")]),
        ("GET", "/x\r\nInjected: bad", &[("Host", b"ok")]),
        ("GET", "/", &[]),
        ("GET", "/", &[("Host", b"a"), ("Host", b"b")]),
        ("GET", "/", &[("Host", b"a"), ("Connection", b"Host")]),
        ("GET", "/", &[("Host", b"a"), ("Upgrade", b"websocket")]),
        ("GET", "/", &[("Host", b"a"), ("X-Secret", b"bad\0value")]),
        (
            "POST",
            "/",
            &[
                ("Host", b"a"),
                ("Content-Length", b"0"),
                ("Content-Length", b"0"),
            ],
        ),
        (
            "POST",
            "/",
            &[
                ("Host", b"a"),
                ("Content-Length", b"0"),
                ("Transfer-Encoding", b"chunked"),
            ],
        ),
        (
            "POST",
            "/",
            &[("Host", b"a"), ("Transfer-Encoding", b"gzip, chunked")],
        ),
    ];
    for (method, target, fields) in cases {
        let (controller, generation, id) = setup(true);
        let mut consumer = controller.admit(context(id, generation)).unwrap();
        let fields: Vec<_> = fields
            .iter()
            .map(|(name, value)| httparse::Header { name, value })
            .collect();
        consumer.observe(&Observation::RequestHead {
            method,
            target,
            headers: &fields,
        });
        consumer.observe(&Observation::MessageEnd {
            direction: Direction::Request,
            trailers: false,
        });
        consumer.observe(&Observation::Finished);
        assert!(controller.prepare(id).is_err(), "{method} {target}");
        assert_eq!(controller.raw_bytes(), 0);
    }
    for trailers in [false, true] {
        let (controller, generation, id) = setup(true);
        let mut consumer = controller.admit(context(id, generation)).unwrap();
        consumer.observe(&Observation::RequestHead {
            method: "GET",
            target: "/",
            headers: &[httparse::Header {
                name: "Host",
                value: b"a",
            }],
        });
        consumer.observe(&Observation::MessageEnd {
            direction: Direction::Request,
            trailers,
        });
        consumer.observe(&Observation::Finished);
        assert_eq!(controller.prepare(id).is_ok(), !trailers);
    }
}

#[test]
fn caps_include_unfinished_entries_pinned_bytes_and_expiry() {
    let (controller, generation, _) = setup(true);
    let mut consumers = Vec::new();
    for _ in 0..21 {
        let id = CaptureId(ulid::Ulid::new());
        consumers.push(controller.admit(context(id, generation)).unwrap());
    }
    assert_eq!(consumers.len(), 21);
    assert_eq!(controller.inner.state.lock().entries.len(), 20);
    assert_eq!(controller.raw_bytes(), 20 * retention::HEAD_CHARGE);
    controller.sweep_at(Instant::now() + retention::TTL);
    assert_eq!(controller.raw_bytes(), 0);
    let mut pinned = None;
    let chunk = vec![b'x'; retention::BODY_LIMIT];
    for index in 0..20 {
        let id = CaptureId(ulid::Ulid::new());
        let mut consumer = controller.admit(context(id, generation)).unwrap();
        consumer.observe(&Observation::RequestHead {
            method: "POST",
            target: "/",
            headers: &[
                httparse::Header {
                    name: "Host",
                    value: b"a",
                },
                httparse::Header {
                    name: "Transfer-Encoding",
                    value: b"chunked",
                },
            ],
        });
        consumer.observe(&Observation::Body(Direction::Request, &chunk));
        consumer.observe(&Observation::MessageEnd {
            direction: Direction::Request,
            trailers: false,
        });
        consumer.observe(&Observation::Finished);
        if index == 0 {
            pinned = Some(
                controller
                    .inner
                    .state
                    .lock()
                    .entries
                    .back()
                    .unwrap()
                    .raw
                    .clone(),
            );
        }
        assert!(controller.raw_bytes() <= retention::MAX_BYTES);
    }
    controller.sweep_at(Instant::now() + retention::TTL);
    assert_eq!(
        controller.raw_bytes(),
        retention::HEAD_CHARGE + retention::BODY_LIMIT
    );
    drop(pinned);
    assert_eq!(controller.raw_bytes(), 0);
}

#[test]
fn original_backend_failure_does_not_imply_request_incomplete() {
    let (controller, generation, id) = setup(true);
    let mut consumer = controller.admit(context(id, generation)).unwrap();
    head(consumer.as_mut());
    consumer.observe(&Observation::Stopped(Direction::Response, "incomplete"));
    consumer.observe(&Observation::Finished);
    assert_eq!(
        controller.availability(id).state,
        AvailabilityState::Eligible
    );
}

#[test]
fn drop_after_both_ends_revokes_but_graceful_finish_remains_eligible() {
    #[derive(Clone)]
    struct Factory(ReplayController);
    impl ConsumerFactory for Factory {
        fn admit(
            &self,
            mut context: CaptureContext,
        ) -> Option<Box<dyn crate::inspection::ObservationConsumer>> {
            context.backend = Some("127.0.0.1:12345".parse().unwrap());
            self.0.admit(context)
        }
    }
    for graceful in [false, true] {
        let (controller, generation, _) = setup(true);
        let display = Inspection::with_consumer(
            PreviewMode::Off,
            Some(Arc::new(Factory(controller.clone()))),
        );
        let mut capture = display.admit(generation, None).unwrap();
        let id = capture.id();
        capture.request(b"GET / HTTP/1.1\r\nHost: local\r\n\r\n");
        capture.response(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        if graceful {
            capture.finish();
        }
        drop(capture);
        assert_eq!(controller.prepare(id).is_ok(), graceful);
    }
}

#[test]
fn confirmation_capacity_expiry_capture_binding_and_forget() {
    let (controller, generation, id) = setup(true);
    let mut consumer = controller.admit(context(id, generation)).unwrap();
    head(consumer.as_mut());
    consumer.observe(&Observation::Finished);
    let first = controller.prepare(id).unwrap();
    for _ in 1..8 {
        controller.prepare(id).unwrap();
    }
    assert!(matches!(
        controller.prepare(id),
        Err(ReplayError::Overloaded)
    ));
    assert!(controller
        .confirm(
            CaptureId(ulid::Ulid::new()),
            first.replay_id,
            &first.confirmation_token,
            true
        )
        .is_err());
    assert!(controller
        .confirm(id, ulid::Ulid::new(), &first.confirmation_token, true)
        .is_err());
    controller.sweep_at(Instant::now() + TICKET_TTL);
    assert!(controller
        .confirm(id, first.replay_id, &first.confirmation_token, true)
        .is_err());
    controller.prepare(id).unwrap();
    controller.forget(id);
    consumer.observe(&Observation::Finished);
    assert_eq!(controller.raw_bytes(), 0);
    assert!(controller.prepare(id).is_err());
}

#[tokio::test]
async fn cancellation_is_bounded_and_delivery_conservative() {
    let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (controller, generation, id) = setup(true);
    let mut ctx = context(id, generation);
    ctx.backend = Some(backend.local_addr().unwrap());
    let mut consumer = controller.admit(ctx).unwrap();
    head(consumer.as_mut());
    consumer.observe(&Observation::Finished);
    let preview = controller.prepare(id).unwrap();
    controller
        .confirm(id, preview.replay_id, &preview.confirmation_token, true)
        .unwrap();
    let (mut stream, _) = backend.accept().await.unwrap();
    let mut byte = [0];
    tokio::io::AsyncReadExt::read_exact(&mut stream, &mut byte)
        .await
        .unwrap();
    controller.shutdown();
    let outcome = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let outcome = controller.outcome(preview.replay_id).unwrap();
            if outcome.state != AttemptState::Running {
                break outcome;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(outcome.state, AttemptState::Cancelled);
    assert_eq!(outcome.delivery, Delivery::PossiblySent);
    assert_eq!(controller.raw_bytes(), 0);
}

#[test]
fn head_and_body_boundaries_and_pinned_slot_pressure() {
    let (controller, generation, id) = setup(true);
    let mut consumer = controller.admit(context(id, generation)).unwrap();
    let value = vec![b'v'; 8192];
    consumer.observe(&Observation::RequestHead {
        method: "GET",
        target: "/",
        headers: &[
            httparse::Header {
                name: "Host",
                value: b"a",
            },
            httparse::Header {
                name: "Secret",
                value: &value,
            },
        ],
    });
    assert_eq!(controller.raw_bytes(), 0);
    let mut pins = Vec::new();
    for _ in 0..20 {
        let id = CaptureId(ulid::Ulid::new());
        let mut consumer = controller.admit(context(id, generation)).unwrap();
        consumer.observe(&Observation::RequestHead {
            method: "GET",
            target: "/",
            headers: &[httparse::Header {
                name: "Host",
                value: b"a",
            }],
        });
        consumer.observe(&Observation::MessageEnd {
            direction: Direction::Request,
            trailers: false,
        });
        consumer.observe(&Observation::Finished);
        pins.push(
            controller
                .inner
                .state
                .lock()
                .entries
                .back()
                .unwrap()
                .raw
                .clone(),
        );
    }
    controller.sweep_at(Instant::now() + retention::TTL);
    assert!(controller
        .admit(context(CaptureId(ulid::Ulid::new()), generation))
        .is_none());
    assert_eq!(controller.raw_bytes(), 20 * retention::HEAD_CHARGE);
    drop(pins);
    let id = CaptureId(ulid::Ulid::new());
    let mut consumer = controller.admit(context(id, generation)).unwrap();
    consumer.observe(&Observation::RequestHead {
        method: "POST",
        target: "/",
        headers: &[
            httparse::Header {
                name: "Host",
                value: b"a",
            },
            httparse::Header {
                name: "Transfer-Encoding",
                value: b"chunked",
            },
        ],
    });
    let body = vec![0; retention::BODY_LIMIT];
    consumer.observe(&Observation::Body(Direction::Request, &body));
    assert_eq!(
        controller.raw_bytes(),
        retention::BODY_LIMIT + retention::HEAD_CHARGE
    );
    consumer.observe(&Observation::Body(Direction::Request, b"x"));
    assert_eq!(controller.raw_bytes(), 0);
    consumer.observe(&Observation::Finished);
    assert!(controller.prepare(id).is_err());
}

async fn finished(controller: &ReplayController, id: ulid::Ulid) -> ReplayOutcome {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let outcome = controller.outcome(id).unwrap();
            if outcome.state != AttemptState::Running {
                break outcome;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simultaneous_confirmation_sends_one_and_busy_never_queues() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (controller, generation, id) = setup(true);
    let mut ctx = context(id, generation);
    ctx.backend = Some(backend.local_addr().unwrap());
    let mut consumer = controller.admit(ctx).unwrap();
    head(consumer.as_mut());
    consumer.observe(&Observation::Finished);
    let preview = controller.prepare(id).unwrap();
    let second = controller.prepare(id).unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut attempts = Vec::new();
    for _ in 0..8 {
        let controller = controller.clone();
        let barrier = barrier.clone();
        let token = preview.confirmation_token.clone();
        let replay = preview.replay_id;
        attempts.push(tokio::spawn(async move {
            barrier.wait().await;
            controller
                .confirm(id, replay, &token, true)
                .unwrap()
                .replay_id
        }));
    }
    for attempt in attempts {
        assert_eq!(attempt.await.unwrap(), preview.replay_id);
    }
    let (mut stream, _) = backend.accept().await.unwrap();
    let mut buf = [0; 8192];
    assert!(stream.read(&mut buf).await.unwrap() > 0);
    assert!(matches!(
        controller.confirm(id, second.replay_id, &second.confirmation_token, true),
        Err(ReplayError::Busy)
    ));
    stream
        .write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n")
        .await
        .unwrap();
    let outcome = finished(&controller, preview.replay_id).await;
    assert_eq!(outcome.state, AttemptState::Complete);
    assert_eq!(outcome.status, Some(503));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), backend.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn binary_encoded_chunked_fidelity_hop_stripping_and_informational_response() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (controller, generation, id) = setup(true);
    let mut ctx = context(id, generation);
    ctx.backend = Some(backend.local_addr().unwrap());
    let mut consumer = controller.admit(ctx).unwrap();
    consumer.observe(&Observation::RequestHead {
        method: "PATCH",
        target: "/%2F?a=1&a=%00",
        headers: &[
            httparse::Header {
                name: "Host",
                value: b"effective.example",
            },
            httparse::Header {
                name: "Content-Encoding",
                value: b"gzip",
            },
            httparse::Header {
                name: "Transfer-Encoding",
                value: b"chunked",
            },
            httparse::Header {
                name: "Connection",
                value: b"X-Drop, keep-alive",
            },
            httparse::Header {
                name: "X-Drop",
                value: b"HOP_CANARY",
            },
            httparse::Header {
                name: "Expect",
                value: b"100-continue",
            },
            httparse::Header {
                name: "X-Signature",
                value: b"one",
            },
            httparse::Header {
                name: "X-Signature",
                value: b"two",
            },
        ],
    });
    let body = [0x1f, 0x8b, 0, 0xff, b'x'];
    consumer.observe(&Observation::Body(Direction::Request, &body));
    consumer.observe(&Observation::MessageEnd {
        direction: Direction::Request,
        trailers: false,
    });
    consumer.observe(&Observation::Finished);
    let preview = controller.prepare(id).unwrap();
    controller
        .confirm(id, preview.replay_id, &preview.confirmation_token, true)
        .unwrap();
    let (mut stream, _) = backend.accept().await.unwrap();
    let mut wire = Vec::new();
    loop {
        let mut buf = [0; 8192];
        let n = stream.read(&mut buf).await.unwrap();
        assert!(n > 0);
        wire.extend_from_slice(&buf[..n]);
        if wire.ends_with(&body) {
            break;
        }
    }
    assert!(wire.starts_with(b"PATCH /%2F?a=1&a=%00 HTTP/1.1\r\n"));
    let text = String::from_utf8_lossy(&wire);
    for value in ["HOP_CANARY", "transfer-encoding", "expect:"] {
        assert!(!text.contains(value));
    }
    for value in [
        "content-encoding: gzip",
        "content-length: 5",
        "x-signature: one",
        "x-signature: two",
    ] {
        assert!(text.contains(value));
    }
    stream
        .write_all(
            b"HTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n",
        )
        .await
        .unwrap();
    let outcome = finished(&controller, preview.replay_id).await;
    assert_eq!(outcome.status, Some(201));
    assert_eq!(outcome.delivery, Delivery::ResponseReceived);
}

#[tokio::test]
async fn response_upgrade_malformed_and_entity_limit_are_not_success() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for response in [b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\nOPAQUE_CANARY".as_slice(), b"NOT HTTP\r\n\r\n", b"HTTP/1.1 200 OK\r\nContent-Length: 1048577\r\n\r\n"] {
        let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (controller, generation, id) = setup(true);
        let mut ctx = context(id, generation); ctx.backend = Some(backend.local_addr().unwrap());
        let mut consumer = controller.admit(ctx).unwrap(); head(consumer.as_mut()); consumer.observe(&Observation::Finished);
        let preview = controller.prepare(id).unwrap(); controller.confirm(id, preview.replay_id, &preview.confirmation_token, true).unwrap();
        let (mut stream, _) = backend.accept().await.unwrap(); let mut buf = [0;8192]; assert!(stream.read(&mut buf).await.unwrap() > 0);
        stream.write_all(response).await.unwrap();
        if response.ends_with(b"1048577\r\n\r\n") {
            let payload = vec![0;1_048_577]; let _ = stream.write_all(&payload).await;
        }
        drop(stream);
        let outcome = finished(&controller, preview.replay_id).await;
        assert_eq!(outcome.state, AttemptState::Failed);
        assert_eq!(outcome.delivery, Delivery::PossiblySent);
        assert!(!serde_json::to_string(&outcome).unwrap().contains("OPAQUE_CANARY"));
    }
}

#[tokio::test]
async fn response_informational_count_and_cumulative_wire_head_are_bounded() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for response in [
        format!("{}HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n", "HTTP/1.1 103 Early Hints\r\n\r\n".repeat(9)),
        format!("HTTP/1.1 103 Early Hints\r\nX: {}\r\n\r\nHTTP/1.1 200 OK\r\nX: {}\r\nContent-Length: 0\r\n\r\n", "x".repeat(17_000), "y".repeat(17_000)),
    ] {
        let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (controller, generation, id) = setup(true);
        let mut ctx = context(id, generation); ctx.backend = Some(backend.local_addr().unwrap());
        let mut consumer = controller.admit(ctx).unwrap(); head(consumer.as_mut()); consumer.observe(&Observation::Finished);
        let preview = controller.prepare(id).unwrap(); controller.confirm(id, preview.replay_id, &preview.confirmation_token, true).unwrap();
        let (mut stream, _) = backend.accept().await.unwrap(); let mut buf = [0;8192]; assert!(stream.read(&mut buf).await.unwrap() > 0);
        let _ = stream.write_all(response.as_bytes()).await;
        let outcome = finished(&controller, preview.replay_id).await;
        assert_eq!(outcome.state, AttemptState::Failed);
        assert_eq!(outcome.error_code, Some(EngineError::ResponseInvalid));
    }
}

#[tokio::test]
async fn first_response_deadline_never_retries() {
    use tokio::io::AsyncReadExt;
    let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (controller, generation, id) = setup(true);
    let mut ctx = context(id, generation);
    ctx.backend = Some(backend.local_addr().unwrap());
    let mut consumer = controller.admit(ctx).unwrap();
    head(consumer.as_mut());
    consumer.observe(&Observation::Finished);
    let preview = controller.prepare(id).unwrap();
    controller
        .confirm(id, preview.replay_id, &preview.confirmation_token, true)
        .unwrap();
    let (mut stream, _) = backend.accept().await.unwrap();
    let mut byte = [0];
    stream.read_exact(&mut byte).await.unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(16)).await;
    tokio::time::resume();
    let outcome = finished(&controller, preview.replay_id).await;
    assert_eq!(outcome.error_code, Some(EngineError::ResponseTimeout));
    assert_eq!(outcome.delivery, Delivery::PossiblySent);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), backend.accept())
            .await
            .is_err()
    );
}

async fn response_prefix_cases_rejected(cumulative: bool) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let excessive = if cumulative {
        format!("HTTP/1.1 103 Early Hints\r\nX: {}\r\n\r\nHTTP/1.1 200 OK\r\nX: {}\r\nContent-Length: 0\r\n\r\n", "x".repeat(17_000), "y".repeat(17_000))
    } else {
        format!(
            "{}HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 103 Early Hints\r\n\r\n".repeat(9)
        )
    };
    let mut failures = Vec::new();
    for leading in ["\r\n", "\n", "\r\n\r\n", "\n\n"] {
        for fragment in [1, 7, 12, 8192] {
            let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let (controller, generation, id) = setup(true);
            let mut ctx = context(id, generation);
            ctx.backend = Some(backend.local_addr().unwrap());
            let mut consumer = controller.admit(ctx).unwrap();
            head(consumer.as_mut());
            consumer.observe(&Observation::Finished);
            let preview = controller.prepare(id).unwrap();
            controller
                .confirm(id, preview.replay_id, &preview.confirmation_token, true)
                .unwrap();
            let (mut stream, _) = backend.accept().await.unwrap();
            let mut buf = [0; 8192];
            assert!(stream.read(&mut buf).await.unwrap() > 0);
            let response = format!("{leading}{excessive}");
            // Split the prefix across writes; yield lets the driver consume it
            // before the remainder. The long heads remain individually bounded.
            for chunk in response.as_bytes()[..32].chunks(fragment) {
                if stream.write_all(chunk).await.is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            let _ = stream.write_all(&response.as_bytes()[32..]).await;
            let outcome = finished(&controller, preview.replay_id).await;
            if outcome.state != AttemptState::Failed
                || outcome.error_code != Some(EngineError::ResponseInvalid)
            {
                failures.push((leading, fragment, outcome.state));
            }
        }
    }
    assert!(failures.is_empty(), "prefix guard bypasses: {failures:?}");
}

#[tokio::test]
async fn leading_blank_lines_cannot_bypass_informational_limit() {
    response_prefix_cases_rejected(false).await;
}

#[tokio::test]
async fn leading_blank_lines_cannot_bypass_cumulative_head_limit() {
    response_prefix_cases_rejected(true).await;
}

#[tokio::test]
async fn invalid_host_syntax_is_rejected_before_backend_connect() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut failures = Vec::new();
    for host in [
        "backend.example:not-a-port",
        "backend.example:",
        "backend.example:65536",
        "backend.example:-1",
        "backend.example:+80",
        ":80",
        "[::1]:not-a-port",
        "[::1]:65536",
        "[::1]suffix",
        "[not-an-ip]",
        "[]",
        "::1",
        "user@backend.example:80",
    ] {
        let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (controller, generation, id) = setup(true);
        let mut ctx = context(id, generation);
        ctx.backend = Some(backend.local_addr().unwrap());
        let mut consumer = controller.admit(ctx).unwrap();
        consumer.observe(&Observation::RequestHead {
            method: "GET",
            target: "/",
            headers: &[httparse::Header {
                name: "Host",
                value: host.as_bytes(),
            }],
        });
        consumer.observe(&Observation::MessageEnd {
            direction: Direction::Request,
            trailers: false,
        });
        consumer.observe(&Observation::Finished);
        let preview = controller.prepare(id);
        let eligible = preview.is_ok();
        if let Ok(preview) = preview {
            controller
                .confirm(id, preview.replay_id, &preview.confirmation_token, true)
                .unwrap();
        }
        let connected = if let Ok(Ok((mut stream, _))) =
            tokio::time::timeout(Duration::from_millis(20), backend.accept()).await
        {
            let mut byte = [0];
            stream.read_exact(&mut byte).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            true
        } else {
            false
        };
        if eligible || connected {
            failures.push((host, eligible, connected));
        }
        controller.shutdown();
    }
    assert!(
        failures.is_empty(),
        "invalid Host accepted/connected: {failures:?}"
    );
}

#[test]
fn valid_host_forms_remain_eligible_without_dns_label_restrictions() {
    // URI reg-name syntax is not a DNS hostname policy. Preserve underscores,
    // trailing dots and sub-delimiters; Host never selects the backend socket.
    for host in [
        "backend.example",
        "Backend.Example.",
        "localhost",
        "service_name",
        "backend!$&'()*+,;=.example",
        "127.0.0.1",
        "192.0.2.3:0",
        "backend.example:00080",
        "backend.example:65535",
        "[::1]",
        "[::ffff:192.0.2.3]:443",
        "[2001:db8::1]:8080",
        "[v1.example:address]:80",
        "[fe80::1%25eth0]:80",
    ] {
        let (controller, generation, id) = setup(true);
        let mut consumer = controller.admit(context(id, generation)).unwrap();
        consumer.observe(&Observation::RequestHead {
            method: "GET",
            target: "/",
            headers: &[httparse::Header {
                name: "Host",
                value: host.as_bytes(),
            }],
        });
        consumer.observe(&Observation::MessageEnd {
            direction: Direction::Request,
            trailers: false,
        });
        consumer.observe(&Observation::Finished);
        assert_eq!(
            controller.availability(id).state,
            AvailabilityState::Eligible,
            "{host}"
        );
    }
}

#[tokio::test]
async fn canonical_fragmented_response_prefixes_allow_binary_entity() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for version in ["HTTP/1.0", "HTTP/1.1"] {
        for fragment in [1, 7, 12] {
            let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let (controller, generation, id) = setup(true);
            let mut ctx = context(id, generation);
            ctx.backend = Some(backend.local_addr().unwrap());
            let mut consumer = controller.admit(ctx).unwrap();
            head(consumer.as_mut());
            consumer.observe(&Observation::Finished);
            let preview = controller.prepare(id).unwrap();
            controller
                .confirm(id, preview.replay_id, &preview.confirmation_token, true)
                .unwrap();
            let (mut stream, _) = backend.accept().await.unwrap();
            let mut buf = [0; 8192];
            assert!(stream.read(&mut buf).await.unwrap() > 0);
            let response = format!("{version} 103 Early Hints\r\n\r\n{version} 200 OK\r\nContent-Length: 4\r\n\r\n\0\n\rX");
            for chunk in response.as_bytes().chunks(fragment) {
                stream.write_all(chunk).await.unwrap();
                tokio::task::yield_now().await;
            }
            let outcome = finished(&controller, preview.replay_id).await;
            assert_eq!(
                outcome.state,
                AttemptState::Complete,
                "{version}/{fragment}"
            );
            assert_eq!(outcome.response_body_bytes, 4);
            assert!(outcome.response_complete);
        }
    }
}

#[test]
fn consumed_confirmation_is_metadata_only_and_preserves_outcome_expiry() {
    let controller = ReplayController::new(false, 49151).unwrap();
    let capture = CaptureId(ulid::Ulid::new());
    let replay = ulid::Ulid::new();
    let token = [7; 32];
    let encoded = encode_token(&token);
    controller
        .inner
        .state
        .lock()
        .outcomes
        .push_back(SavedOutcome {
            token,
            created: Instant::now(),
            outcome: ReplayOutcome {
                replay_id: replay,
                parent_capture_id: capture,
                tunnel_generation: TunnelGeneration::new(),
                state: AttemptState::Complete,
                delivery: Delivery::ResponseReceived,
                status: Some(204),
                duration_ms: 1,
                request_body_bytes: 0,
                response_body_bytes: 0,
                response_complete: true,
                error_code: None,
            },
        });
    // No runtime, retention consent, active generation, raw entry or ticket is
    // needed to read exact already-consumed metadata. No new authority is added.
    assert!(controller
        .consumed_confirmation(capture, replay, &encoded, true)
        .unwrap()
        .is_some());
    assert!(controller.confirm(capture, replay, &encoded, true).is_ok());
    assert_eq!(
        controller
            .consumed_confirmation(capture, replay, &encoded, false)
            .unwrap_err(),
        ReplayError::ConfirmationRequired
    );
    let mut state = controller.inner.state.lock();
    assert!(state.entries.is_empty());
    assert!(state.tickets.is_empty());
    assert!(state.execution.is_none());
    assert_eq!(controller.raw_bytes(), 0);
    state.outcomes[0].created = Instant::now().checked_sub(OUTCOME_TTL).unwrap();
    drop(state);
    assert!(controller
        .consumed_confirmation(capture, replay, &encoded, true)
        .unwrap()
        .is_none());
    assert!(controller.confirm(capture, replay, &encoded, true).is_err());
}
