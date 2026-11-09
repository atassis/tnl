//! The HTTP handler's opening budget includes lock contention, not body lifetime.
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{ConnectInfo, State};
use tnl_protocol::request_metadata::DataStreamProtocol;
use tnl_protocol::{Session, Stream};
use tnld::serve::{AppState, SessionHandle};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::{Mutex, Notify};

struct OpeningDrop(Arc<AtomicUsize>);
impl Drop for OpeningDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct ControlledSession {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    dropped: Arc<AtomicUsize>,
    streams: VecDeque<DuplexStream>,
    block_next: bool,
}

#[async_trait::async_trait]
impl Session for ControlledSession {
    fn termination(&self) -> tnl_protocol::SessionTermination {
        panic!("opening-only fixture has no physical driver; lifecycle belongs to registry")
    }
    async fn open_stream(&mut self) -> anyhow::Result<Pin<Box<dyn Stream>>> {
        let _opening = OpeningDrop(self.dropped.clone());
        self.entered.notify_one();
        if std::mem::take(&mut self.block_next) {
            self.release.notified().await;
        }
        Ok(Box::pin(
            self.streams
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("no stream"))?,
        ))
    }
    async fn accept_stream(&mut self) -> anyhow::Result<Option<Pin<Box<dyn Stream>>>> {
        panic!("opening-only fixture")
    }
    async fn ping(&mut self) -> anyhow::Result<Duration> {
        panic!("opening-only fixture")
    }
    async fn close(&mut self) -> anyhow::Result<()> {
        panic!("opening-only fixture")
    }
}

struct Fixture {
    state: AppState,
    server: tnld::serve::ServerHandle,
    session: Arc<tnld::registry::SessionState>,
    tunnel: Arc<tnld::registry::Tunnel>,
    handle: SessionHandle,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    dropped: Arc<AtomicUsize>,
    peer: DuplexStream,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.join.abort();
    }
}

async fn fixture(block_next: bool) -> Fixture {
    let file = tempfile::NamedTempFile::new().unwrap();
    let (server, state) = tnld::serve::spawn_server_with_state(tnld::config::Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://x".into(),
        hostname_root: "t.x".into(),
        tokens_file: file.path().to_string_lossy().into_owned(),
        session_grace_sec: 30,
        trusted_proxies: vec![],
    })
    .await
    .unwrap();
    let session = state.registry.register_session("test");
    let tunnel = state
        .registry
        .create_tunnel_bound(
            "demo",
            &session.id,
            "test",
            DataStreamProtocol::Http1MetadataV1,
            None,
        )
        .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicUsize::new(0));
    let (stream, peer) = tokio::io::duplex(4096);
    let handle = SessionHandle::new(
        &session,
        Arc::new(Mutex::new(Box::new(ControlledSession {
            entered: entered.clone(),
            release: release.clone(),
            dropped: dropped.clone(),
            streams: VecDeque::from([stream]),
            block_next,
        }))),
        DataStreamProtocol::Http1MetadataV1,
    );
    state
        .session_handles
        .insert(session.id.clone(), handle.clone());
    Fixture {
        state,
        server,
        session,
        tunnel,
        handle,
        entered,
        release,
        dropped,
        peer,
    }
}

fn request(state: AppState) -> tokio::task::JoinHandle<axum::response::Response> {
    tokio::spawn(tnld::data_plane::handler(
        State(state),
        ConnectInfo("127.0.0.1:1234".parse().unwrap()),
        axum::http::Request::builder()
            .uri("/private")
            .header("Host", "demo.t.x")
            .header("Accept", "application/json")
            .body(axum::body::Body::from("private upload"))
            .unwrap(),
    ))
}

async fn transport_error(task: tokio::task::JoinHandle<axum::response::Response>, kind: &str) {
    let response = task.await.unwrap();
    assert_eq!(response.status(), 502);
    assert_eq!(response.headers()["x-tnl-component"], "transport");
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["kind"], kind);
    assert!(!String::from_utf8_lossy(&bytes).contains("private upload"));
}

#[tokio::test(start_paused = true)]
async fn held_mutex_alone_consumes_open_deadline_without_deleting_tunnel() {
    let f = fixture(false).await;
    let guard = f.handle.session.lock().await;
    let task = request(f.state.clone());
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(5)).await;
    tokio::task::yield_now().await;
    assert!(
        task.is_finished(),
        "mutex wait was not included in five-second opening budget"
    );
    transport_error(task, "yamux-open-timeout").await;
    assert_eq!(
        f.state.registry.find_by_subdomain("demo").unwrap().id,
        f.tunnel.id
    );
    assert_eq!(f.dropped.load(Ordering::SeqCst), 0);
    drop(guard);
}

#[tokio::test(start_paused = true)]
async fn mutex_and_opening_share_budget_cancellation_releases_following_visitor() {
    let mut f = fixture(true).await;
    let guard = f.handle.session.lock().await;
    let task = request(f.state.clone());
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(3)).await;
    drop(guard);
    f.entered.notified().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::task::yield_now().await;
    assert!(
        task.is_finished(),
        "opening incorrectly received a fresh budget after mutex wait"
    );
    transport_error(task, "yamux-open-timeout").await;
    assert_eq!(
        f.dropped.load(Ordering::SeqCst),
        1,
        "timed-out opening still retained"
    );
    f.release.notify_one();
    let next = request(f.state.clone());
    let _metadata = tnl_protocol::request_metadata::read_request_metadata(&mut f.peer)
        .await
        .unwrap();
    let mut head = Vec::new();
    while !head.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        head.push(f.peer.read_u8().await.unwrap());
    }
    f.peer
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    assert_eq!(next.await.unwrap().status(), 200);
    assert_eq!(f.dropped.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn stop_and_name_reuse_during_opening_prevent_metadata_or_application_bytes() {
    let mut f = fixture(true).await;
    let task = request(f.state.clone());
    f.entered.notified().await;
    f.state.registry.close_by_subdomain("demo", "test").unwrap();
    let new_session = f.state.registry.register_session("new-owner");
    let newer = f
        .state
        .registry
        .create_tunnel("demo", &new_session.id, "new-owner")
        .unwrap();
    f.release.notify_one();
    tokio::task::yield_now().await;
    assert!(
        task.is_finished(),
        "stopped binding admitted request after asynchronous opening"
    );
    transport_error(task, "stale-session").await;
    let mut bytes = Vec::new();
    f.peer.read_to_end(&mut bytes).await.unwrap();
    assert!(
        bytes.is_empty(),
        "stale opening leaked metadata/application bytes"
    );
    assert_eq!(
        f.state.registry.find_by_subdomain("demo").unwrap().id,
        newer.id
    );
}

#[tokio::test(start_paused = true)]
async fn registry_invalidation_cancels_stale_clones_before_and_while_waiting_for_mutex() {
    for invalidate_first in [false, true] {
        let f = fixture(false).await;
        let stale_clone = f.handle.clone();
        let guard = stale_clone.session.lock().await;
        if invalidate_first {
            f.state.registry.drop_session(&f.session.id, 30);
        }
        let task = request(f.state.clone());
        tokio::task::yield_now().await;
        if !invalidate_first {
            f.state.registry.drop_session(&f.session.id, 30);
        }
        tokio::task::yield_now().await;
        assert!(
            task.is_finished(),
            "stale clone waited for mutex or full timeout"
        );
        transport_error(task, "stale-session").await;
        stale_clone.invalidated().await;
        assert_eq!(f.dropped.load(Ordering::SeqCst), 0);
        drop(guard);
    }
}

#[tokio::test(start_paused = true)]
async fn expiry_and_supersession_cancel_opening_without_touching_new_binding() {
    use futures::FutureExt as _;
    let mut f = fixture(true).await;
    let task = request(f.state.clone());
    f.entered.notified().await;
    // This is the authoritative transition used by the real silence-lease loop.
    f.state.registry.drop_session(&f.session.id, 30);
    let newer = f.state.registry.register_session("test");
    let reattached = f
        .state
        .registry
        .try_reattach(&f.tunnel.id, "demo", "test", &newer.id)
        .unwrap();
    tokio::task::yield_now().await;
    assert!(task.is_finished(), "invalidated queued open remained live");
    transport_error(task, "stale-session").await;
    assert_eq!(f.dropped.load(Ordering::SeqCst), 1);
    assert!(f.handle.session.try_lock().is_ok());
    f.release.notify_one();
    tokio::task::yield_now().await;
    let mut byte = [0];
    assert!(
        f.peer.read(&mut byte).now_or_never().is_none(),
        "canceled open leaked bytes after release"
    );
    // Late original-session cleanup is fenced and cannot retire the reattachment.
    f.state.registry.drop_session(&f.session.id, 0);
    assert_eq!(
        f.state.registry.current_session_id(&reattached.id).unwrap(),
        newer.id
    );
    assert_eq!(
        f.state.registry.find_by_subdomain("demo").unwrap().id,
        f.tunnel.id
    );
}

#[tokio::test(start_paused = true)]
async fn unrelated_real_session_forwards_while_original_opening_is_stalled() {
    let f = fixture(true).await;
    let stalled = request(f.state.clone());
    f.entered.notified().await;
    let owner = f.state.registry.register_session("other");
    let tunnel = f
        .state
        .registry
        .create_tunnel_bound(
            "other",
            &owner.id,
            "other",
            DataStreamProtocol::Http1MetadataV1,
            None,
        )
        .unwrap();
    let (server_io, client_io) = tokio::io::duplex(4096);
    let real = tnl_protocol::transport::YamuxSession::new_client(server_io);
    let termination = real.termination();
    f.state.session_handles.insert(
        owner.id.clone(),
        SessionHandle::new(
            &owner,
            Arc::new(Mutex::new(Box::new(real))),
            DataStreamProtocol::Http1MetadataV1,
        ),
    );
    let backend = tokio::spawn(async move {
        let mut client = tnl_protocol::transport::YamuxSession::new_server(client_io);
        let mut stream = client.accept_stream().await.unwrap().unwrap();
        tnl_protocol::request_metadata::read_request_metadata(&mut stream)
            .await
            .unwrap();
        let mut bytes = Vec::new();
        while !bytes.windows(4).any(|b| b == b"\r\n\r\n") {
            bytes.push(stream.read_u8().await.unwrap());
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .unwrap();
        stream.shutdown().await.unwrap();
        client.close().await.unwrap();
    });
    let response = tnld::data_plane::handler(
        State(f.state.clone()),
        ConnectInfo("127.0.0.1:1234".parse().unwrap()),
        axum::http::Request::builder()
            .header("Host", "other.t.x")
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap(),
        "ok"
    );
    backend.await.unwrap();
    assert!(!stalled.is_finished());
    tokio::time::advance(Duration::from_secs(5)).await;
    transport_error(stalled, "yamux-open-timeout").await;
    assert_eq!(
        f.state.registry.find_by_subdomain("other").unwrap().id,
        tunnel.id
    );
    termination.completed().await;
}

#[tokio::test(start_paused = true)]
async fn protected_timeout_releases_early_reservation_and_reattach_preserves_quota_arc() {
    let f = fixture(true).await;
    f.state.registry.close_by_subdomain("demo", "test").unwrap();
    let access = Arc::new(
        tnld::access::CompiledAccess::new(Arc::new(
            tnl_protocol::access::VisitorAccessPolicyV1::new(None, &[], None, Some(1)).unwrap(),
        ))
        .unwrap(),
    );
    let protected = f
        .state
        .registry
        .create_tunnel_bound(
            "demo",
            &f.session.id,
            "test",
            DataStreamProtocol::Http1MetadataV1,
            Some(access.clone()),
        )
        .unwrap();
    let first = request(f.state.clone());
    f.entered.notified().await;
    let denied = request(f.state.clone()).await.unwrap();
    assert_eq!(
        denied.status(),
        429,
        "early quota denial must precede mutex/opening"
    );
    assert_eq!(f.dropped.load(Ordering::SeqCst), 0);
    tokio::time::advance(Duration::from_secs(5)).await;
    transport_error(first, "yamux-open-timeout").await;
    let reservation = access
        .admit(None, &mut axum::http::HeaderMap::new())
        .await
        .unwrap();
    f.state.registry.drop_session(&f.session.id, 30);
    let owner = f.state.registry.register_session("test");
    let reattached = f
        .state
        .registry
        .try_reattach_bound(
            &protected.id,
            "demo",
            "test",
            &owner.id,
            Some(DataStreamProtocol::Http1MetadataV1),
            Some(access.fingerprint()),
        )
        .unwrap();
    assert!(Arc::ptr_eq(reattached.access.as_ref().unwrap(), &access));
    assert!(matches!(
        reattached
            .access
            .as_ref()
            .unwrap()
            .admit(None, &mut axum::http::HeaderMap::new())
            .await,
        Err(tnld::access::AccessDenial::Concurrency)
    ));
    drop(reservation);
}

#[tokio::test(start_paused = true)]
async fn real_ack_blocked_timeout_has_no_ghost_and_following_http_visitor_progresses() {
    let f = fixture(false).await;
    let (io, mut peer_io) = tokio::io::duplex(64 * 1024);
    let mut real = tnl_protocol::transport::YamuxSession::new_client(io);
    // Actual native ACK backlog, not an artificial sleep in open_stream.
    let mut unacknowledged = Vec::new();
    for _ in 0..256 {
        unacknowledged.push(real.open_stream().await.unwrap());
    }
    let handle = SessionHandle::new(
        &f.session,
        Arc::new(Mutex::new(Box::new(real))),
        DataStreamProtocol::Http1MetadataV1,
    );
    f.state
        .session_handles
        .insert(f.session.id.clone(), handle.clone());
    let timed_out = request(f.state.clone());
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(5)).await;
    transport_error(timed_out, "yamux-open-timeout").await;
    assert!(
        handle.session.try_lock().is_ok(),
        "timed-out HTTP opening retained session mutex"
    );
    let next = request(f.state.clone());
    // Release exactly ONE real native ACK slot. A canceled ghost would consume
    // that slot and strand this next actual HTTP visitor until its own deadline.
    peer_io
        .write_all(&[0, 1, 0, 2, 0, 0, 0, 1, 0, 0, 0, 0])
        .await
        .unwrap();
    let mut client = tnl_protocol::transport::YamuxSession::new_server(peer_io);
    let backend = async {
        let mut stream = client.accept_stream().await.unwrap().unwrap();
        let metadata = tnl_protocol::request_metadata::read_request_metadata(&mut stream)
            .await
            .unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(stream.read_u8().await.unwrap());
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        stream.shutdown().await.unwrap();
        let response = next.await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()["x-tnl-request-id"],
            metadata.request_id.to_string()
        );
    };
    tokio::time::timeout(Duration::from_secs(1), backend)
        .await
        .expect("canceled opening stole the only ACK slot from next visitor");
    assert_eq!(
        f.state.registry.find_by_subdomain("demo").unwrap().id,
        f.tunnel.id
    );
    drop(unacknowledged);
    client.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn opening_ready_at_expired_deadline_cannot_admit_wire_bytes() {
    let mut f = fixture(false).await;
    let guard = f.handle.session.lock().await;
    let mut handler = Box::pin(tnld::data_plane::handler(
        State(f.state.clone()),
        ConnectInfo("127.0.0.1:1234".parse().unwrap()),
        axum::http::Request::builder()
            .header("Host", "demo.t.x")
            .body(axum::body::Body::empty())
            .unwrap(),
    ));
    assert!(futures::poll!(&mut handler).is_pending());
    tokio::time::advance(Duration::from_secs(5)).await;
    drop(guard);
    let response = match futures::poll!(&mut handler) {
        std::task::Poll::Ready(response) => response,
        std::task::Poll::Pending => panic!("ready opening bypassed expired overall deadline"),
    };
    assert_eq!(response.status(), 502);
    let mut bytes = Vec::new();
    f.peer.read_to_end(&mut bytes).await.unwrap();
    assert!(
        bytes.is_empty(),
        "late readiness admitted metadata/application bytes"
    );
}
