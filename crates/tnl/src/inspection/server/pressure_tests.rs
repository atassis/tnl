use super::*;
use crate::inspection::{PreviewMode, TunnelGeneration};
use crate::replay::ReplayController;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};

// A serialized output reserves its whole Vec even when the JSON is short.
// Embedded static bundles have no per-handler allocation.
#[allow(clippy::ptr_arg)] // A slice would hide the allocation variant and Vec capacity being tested.
fn owned_body_capacity(body: &Cow<'static, [u8]>) -> usize {
    match body {
        Cow::Owned(bytes) => bytes.capacity(),
        Cow::Borrowed(_) => 0,
    }
}

#[test]
fn static_assets_are_borrowed_and_json_accounts_reserved_capacity() {
    let state = ServerState {
        history: Inspection::new(PreviewMode::Off),
        token: Arc::new([b'a'; 64]),
        authority: "127.0.0.1:1234".into(),
        origin: "http://127.0.0.1:1234".into(),
        handlers: Semaphore::new(4),
    };
    for asset in super::super::assets::ASSETS {
        let head = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\n\r\n",
            asset.path, state.authority
        );
        let reply = route(head.as_bytes(), b"", &state);
        assert_eq!(reply.status, 200);
        assert_eq!(owned_body_capacity(&reply.body), 0);
        let Cow::Borrowed(bytes) = reply.body else {
            panic!("static asset allocated per handler")
        };
        assert!(std::ptr::eq(bytes.as_ptr(), asset.bytes.as_ptr()));
    }
    let reply = json(&serde_json::json!({"small": true}));
    assert!(reply.body.len() < RESPONSE_LIMIT);
    assert_eq!(owned_body_capacity(&reply.body), RESPONSE_LIMIT);
}

fn template(body: &[u8], response: bool) -> Vec<u8> {
    let mut head = if response {
        "HTTP/1.1 200 OK\r\n".to_owned()
    } else {
        "POST /pressure?token=QUERY_CANARY HTTP/1.1\r\nHost: local\r\n".to_owned()
    };
    for _ in 0..6 {
        write!(head, "Accept: {}\r\n", "a".repeat(1000)).unwrap();
    }
    write!(head,"Authorization: HEADER_CANARY\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",body.len()).unwrap();
    [head.as_bytes(), body].concat()
}
fn maximal_timing() -> tnl_inspector_api::timing::Timing {
    use tnl_inspector_api::timing::{
        Interval, Intervals, Outcome, Snapshot, Timing, MAX_OFFSET_US,
    };
    let span = Interval {
        outcome: Outcome::Completed,
        start_offset_us: Some(MAX_OFFSET_US - 10),
        end_offset_us: Some(MAX_OFFSET_US),
    };
    Timing::from_snapshot(Snapshot {
        as_of_offset_us: MAX_OFFSET_US,
        terminal: true,
        intervals: Intervals {
            metadata: span,
            request_head: span,
            resolve: span,
            connect: span,
            request_write: span,
            response_read: span,
            response_write: span,
            upgraded: span,
        },
        backend_first_byte_offset_us: Some(MAX_OFFSET_US - 10),
        backend_final_headers_offset_us: Some(MAX_OFFSET_US),
        request_framing_end_offset_us: Some(MAX_OFFSET_US),
        response_framing_end_offset_us: Some(MAX_OFFSET_US),
        request_framing_valid: true,
        response_framing_valid: true,
    })
}
fn attach_active_timing(capture: &crate::inspection::Capture) {
    use crate::inspection::timing::{Activity, Milestone, TimingState};
    let mut timing = TimingState::new();
    for activity in [
        Activity::Metadata,
        Activity::RequestHead,
        Activity::Resolve,
        Activity::Connect,
        Activity::RequestWrite,
        Activity::ResponseRead,
        Activity::ResponseWrite,
        Activity::Upgraded,
    ] {
        timing.start_at(activity, 0);
    }
    for milestone in [
        Milestone::BackendFirstByte,
        Milestone::BackendFinalHeaders,
        Milestone::RequestFramingEnd,
        Milestone::ResponseFramingEnd,
    ] {
        timing.milestone_at(milestone, 0);
    }
    capture.update_record(|record| {
        record.counters = Some(crate::inspection::TransportCounters {
            timing: Arc::new(parking_lot::Mutex::new(timing)),
            to_backend: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            to_client: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        });
    });
}
async fn get(addr: SocketAddr, path: &str) -> Vec<u8> {
    let mut socket = TcpStream::connect(addr).await.unwrap();
    socket
        .write_all(
            format!(
                "GET {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {}\r\n\r\n",
                "a".repeat(64)
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    let _ = socket.read_to_end(&mut bytes).await;
    assert!(bytes.len() <= RESPONSE_LIMIT + 2048);
    bytes
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[allow(clippy::too_many_lines)] // Combined bounded stores, API, observer and transport pressure.
async fn full_history_raw_observers_and_max_api_pressure_account_capacity_and_do_not_block_forwarding(
) {
    tokio::time::timeout(Duration::from_secs(10), Box::pin(combined_pressure()))
        .await
        .unwrap();
}

#[allow(clippy::too_many_lines)] // One bounded fixture shares capacities and held permits.
async fn combined_pressure() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let replay = ReplayController::new(true, addr.port()).unwrap();
    let history = Inspection::with_replay(PreviewMode::Json, replay.clone());
    let generation = TunnelGeneration::new();
    history.activate(generation);
    let body = serde_json::json!({"token":"BODY_CANARY","safe":"\\\"".repeat(800)}).to_string();
    assert!(body.len() <= 4096);
    let request = template(body.as_bytes(), false);
    let response = template(body.as_bytes(), true);
    let backend_addr = "127.0.0.1:3000".parse().unwrap();
    let mut detail_id = None;
    for _ in 0..200 {
        let mut capture = history
            .admit_backend(generation, None, backend_addr)
            .unwrap();
        detail_id = Some(capture.id());
        capture.request(&request);
        capture.response(&response);
        capture.finish();
        capture.update_record(|record| record.timing = maximal_timing());
    }
    let _terminal_detail_id = detail_id.unwrap();
    let mut active_detail_id = None;
    let mut held = Vec::with_capacity(64);
    for _ in 0..64 {
        let mut capture = history
            .admit_backend(generation, None, backend_addr)
            .unwrap();
        attach_active_timing(&capture);
        active_detail_id = Some(capture.id());
        capture.request(b"POST /active HTTP/1.1\r\nHost: local\r\nContent-Type: application/json\r\nContent-Length: 1048576\r\n\r\n");
        capture.request(&[b'x'; 4096]);
        held.push(capture);
    }
    assert!(history.admit(generation, None).is_none());
    let detail_id = active_detail_id.unwrap();
    let history_capacity = history.allocated_capacity();
    let scratch_capacity = held
        .iter()
        .map(super::super::Capture::allocated_capacity)
        .sum::<usize>();
    let raw_capacity = replay.allocated_capacity_without_execution();
    assert!(scratch_capacity <= 64 * 32 * 1024);
    assert!(replay.raw_bytes() <= 16 * 1024 * 1024);
    assert!(replay.raw_bytes() >= 14 * 1024 * 1024);
    let detail = history.api_detail(detail_id).unwrap().unwrap();
    let live_timing = detail.record.timing.snapshot().unwrap();
    assert!(!live_timing.terminal);
    assert!(live_timing.backend_first_byte_offset_us.is_some());
    assert!(live_timing.backend_final_headers_offset_us.is_some());
    assert!(live_timing.request_framing_end_offset_us.is_some());
    assert!(live_timing.response_framing_end_offset_us.is_some());
    assert!(live_timing.request_framing_valid && live_timing.response_framing_valid);
    assert!([
        live_timing.intervals.metadata,
        live_timing.intervals.request_head,
        live_timing.intervals.resolve,
        live_timing.intervals.connect,
        live_timing.intervals.request_write,
        live_timing.intervals.response_read,
        live_timing.intervals.response_write,
        live_timing.intervals.upgraded,
    ]
    .into_iter()
    .all(|interval| interval.outcome == tnl_inspector_api::timing::Outcome::Active));
    let detail_capacity = detail.record.allocated_capacity();
    let encoded = detail_json(detail);
    assert_eq!(encoded.status, 200);
    assert!(owned_body_capacity(&encoded.body) <= RESPONSE_LIMIT);
    assert!(!String::from_utf8_lossy(&encoded.body).contains("CANARY"));
    let list = history
        .api_list(&Filter::parse("limit=100").unwrap())
        .unwrap();
    let summary_capacity = list.capacity() * std::mem::size_of::<super::super::Summary>()
        + list
            .iter()
            .map(|row| row.method.capacity() + row.request_target.capacity())
            .sum::<usize>();
    assert!(summary_capacity <= 65536);
    // Simultaneous conservative per-handler envelope: clone + summaries +
    // allocated output + request-head/JSON + decoded filter/casefold + stack.
    let measured_handler_capacity = detail_capacity
        + summary_capacity
        + owned_body_capacity(&encoded.body)
        + HEAD_LIMIT
        + JSON_LIMIT
        + 16384;
    // Reserve full clone/summary quotas even where the sampled shape uses less.
    let handler_capacity = 3 * RESPONSE_LIMIT + HEAD_LIMIT + JSON_LIMIT + 16384;
    assert!(measured_handler_capacity <= handler_capacity);
    assert!(handler_capacity <= 256 * 1024);
    drop(encoded);
    drop(list);
    let total = history_capacity + scratch_capacity + raw_capacity + 4 * handler_capacity;
    assert!(total <= 35 * 1024 * 1024 + 8192);
    eprintln!("capacity bytes: history={history_capacity} observer={scratch_capacity} raw={raw_capacity} handler={handler_capacity} combined={total}");
    let state = Arc::new(ServerState {
        history: history.clone(),
        token: Arc::new([b'a'; 64]),
        authority: addr.to_string(),
        origin: format!("http://{addr}"),
        handlers: Semaphore::new(4),
    });
    let (stop, rx) = oneshot::channel();
    let service = tokio::spawn(serve(listener, state.clone(), rx));
    let backend = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let target = crate::target::Target::Explicit(backend.local_addr().unwrap());
    let payload: Vec<u8> = (0..65536).map(|n| u8::try_from(n % 256).unwrap()).collect();
    let expected = payload.clone();
    let (release, wait_release) = oneshot::channel();
    let (ready, wait_ready) = oneshot::channel();
    let backend_task = tokio::spawn(async move {
        let (mut socket, _) = backend.accept().await.unwrap();
        let mut input = Vec::new();
        socket.read_to_end(&mut input).await.unwrap();
        assert_eq!(input, b"GET /live HTTP/1.1\r\nHost: local\r\n\r\n");
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 65536\r\n\r\n")
            .await
            .unwrap();
        socket.write_all(&payload[..32768]).await.unwrap();
        wait_release.await.unwrap();
        socket.write_all(&payload[32768..]).await.unwrap();
    });
    let (mut visitor, stream) = tokio::io::duplex(4096);
    let mut ctx =
        crate::forwarder::ForwardCtx::new("pressure".into(), None, env!("CARGO_PKG_VERSION"));
    ctx.inspection = Some((history.clone(), generation));
    ctx.inspection_ownership_check = Some(|owned| assert!(!owned));
    let forward = tokio::spawn(crate::forwarder::forward(Box::pin(stream), target, ctx));
    let visitor = tokio::spawn(async move {
        visitor
            .write_all(b"GET /live HTTP/1.1\r\nHost: local\r\n\r\n")
            .await
            .unwrap();
        visitor.shutdown().await.unwrap();
        let mut response_head = Vec::new();
        while !response_head.ends_with(b"\r\n\r\n") {
            response_head.push(visitor.read_u8().await.unwrap());
        }
        let mut prefix = vec![0; 32768];
        visitor.read_exact(&mut prefix).await.unwrap();
        ready.send(()).unwrap();
        let mut tail = Vec::new();
        visitor.read_to_end(&mut tail).await.unwrap();
        prefix.extend_from_slice(&tail);
        assert_eq!(prefix, expected);
    });
    wait_ready.await.unwrap();
    let observer = tokio::spawn(async move {
        for _ in 0..16 {
            for capture in &mut held {
                capture.request(&[b'x'; 512]);
            }
            tokio::task::yield_now().await;
        }
        held
    });
    let successes = Arc::new(AtomicUsize::new(0));
    let detail_successes = Arc::new(AtomicUsize::new(0));
    let mut clients = JoinSet::new();
    for worker in 0..4 {
        let successes = successes.clone();
        let detail_successes = detail_successes.clone();
        clients.spawn(async move {
            for turn in 0..10 {
                let is_detail = (worker + turn) % 2 == 0;
                let path = if is_detail {
                    format!("/api/v1/requests/{}", detail_id.0)
                } else {
                    format!("/api/v1/requests?limit=100&q={}", "z".repeat(256))
                };
                let bytes = get(addr, &path).await;
                if bytes.starts_with(b"HTTP/1.1 200") {
                    successes.fetch_add(1, Ordering::Relaxed);
                    if is_detail {
                        let end = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                        let detail: serde_json::Value =
                            serde_json::from_slice(&bytes[end..]).unwrap();
                        assert_eq!(detail["capture_id"], detail_id.0.to_string());
                        let timing = &detail["timing"];
                        assert_eq!(timing["schema_version"], 1);
                        assert_eq!(timing["availability"], "available");
                        assert_eq!(timing["snapshot"]["terminal"], false);
                        for activity in [
                            "metadata",
                            "request_head",
                            "resolve",
                            "connect",
                            "request_write",
                            "response_read",
                            "response_write",
                            "upgraded",
                        ] {
                            assert_eq!(
                                timing["snapshot"]["intervals"][activity]["outcome"],
                                "active"
                            );
                        }
                        for milestone in [
                            "backend_first_byte_offset_us",
                            "backend_final_headers_offset_us",
                            "request_framing_end_offset_us",
                            "response_framing_end_offset_us",
                        ] {
                            assert!(timing["snapshot"][milestone].is_u64());
                        }
                        detail_successes.fetch_add(1, Ordering::Relaxed);
                    }
                    assert!(!String::from_utf8_lossy(&bytes).contains("CANARY"));
                }
                tokio::task::yield_now().await;
            }
        });
    }
    while let Some(result) = clients.join_next().await {
        result.unwrap();
    }
    assert!(successes.load(Ordering::Relaxed) > 0);
    assert!(detail_successes.load(Ordering::Relaxed) > 0);
    let held = observer.await.unwrap();
    assert!(history.allocated_capacity() <= 16 * 1024 * 1024);
    assert!(replay.allocated_capacity_without_execution() <= 16 * 1024 * 1024 + 8192);
    let mut slow = Vec::new();
    for _ in 0..4 {
        let mut socket = TcpStream::connect(addr).await.unwrap();
        socket.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
        slow.push(socket);
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        while state.handlers.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), service)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.handlers.available_permits(), 4);
    assert_eq!(replay.raw_bytes(), 0);
    release.send(()).unwrap();
    backend_task.await.unwrap();
    visitor.await.unwrap();
    forward.await.unwrap().unwrap();
    drop(held);
    drop(slow);
    assert!(history.search("").is_empty());
}
