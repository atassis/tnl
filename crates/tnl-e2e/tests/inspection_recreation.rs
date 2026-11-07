//! Distinguish retained-session recreation (exact old bytes) from control loss
//! (bounded exact completion or specifically typed premature-body termination).
//! Neither contract treats a request timeout as successful termination.
use std::time::Duration;
use tnl::inspection::{GenerationState, Inspection, PreviewMode, State};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn head(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        socket.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
        assert!(bytes.len() < 8192);
    }
    bytes
}

fn consume_live_prefix<'a>(seen: &mut usize, chunk: &'a [u8]) -> &'a [u8] {
    let take = (4 - *seen).min(chunk.len());
    assert_eq!(
        &chunk[..take],
        &b"LIVE"[*seen..*seen + take],
        "LIVE prefix mismatch"
    );
    *seen += take;
    &chunk[take..]
}

async fn read_live_prefix(response: &mut reqwest::Response) -> Vec<u8> {
    let mut seen = 0;
    let mut excess = Vec::new();
    while seen < 4 {
        let chunk = response
            .chunk()
            .await
            .expect("LIVE prefix body error")
            .expect("LIVE prefix premature EOF");
        excess.extend_from_slice(consume_live_prefix(&mut seen, &chunk));
    }
    excess
}

#[test]
fn live_prefix_consumes_fragmentation_and_keeps_excess() {
    for chunks in [
        vec![&b"LIVEDONE"[..]],
        vec![&b"L"[..], &b"I"[..], &b"VEDONE"[..]],
        vec![&b"LI"[..], &b"VE"[..], &b"DONE"[..]],
    ] {
        let mut seen = 0;
        let mut excess = Vec::new();
        for chunk in chunks {
            excess.extend_from_slice(consume_live_prefix(&mut seen, chunk));
        }
        assert_eq!(seen, 4);
        assert_eq!(excess, b"DONE");
    }
}

#[test]
#[should_panic(expected = "LIVE prefix mismatch")]
fn live_prefix_rejects_mismatched_bytes() {
    consume_live_prefix(&mut 0, b"LIXE");
}

#[tokio::test]
#[should_panic(expected = "LIVE prefix premature EOF")]
async fn live_prefix_rejects_premature_eof() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        head(&mut socket).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nLIV")
            .await
            .unwrap();
    });
    let mut response = reqwest::get(format!("http://{addr}/")).await.unwrap();
    server.await.unwrap();
    read_live_prefix(&mut response).await;
}

#[tokio::test]
#[should_panic(expected = "LIVE prefix body error")]
async fn live_prefix_rejects_incomplete_body_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        head(&mut socket).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nLIV")
            .await
            .unwrap();
    });
    let mut response = reqwest::get(format!("http://{addr}/")).await.unwrap();
    server.await.unwrap();
    read_live_prefix(&mut response).await;
}

#[tokio::test]
async fn live_prefix_waits_for_fragmented_bytes_and_preserves_remaining_body() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (release, wait_release) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        head(&mut socket).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nL")
            .await
            .unwrap();
        wait_release.await.unwrap();
        socket.write_all(b"IVEDONE").await.unwrap();
    });
    let mut response = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut reading = Box::pin(read_live_prefix(&mut response));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut reading)
            .await
            .is_err(),
        "must wait for the remaining prefix, not mistake a body chunk for the whole prefix"
    );
    release.send(()).unwrap();
    let mut rest = reading.await;
    rest.extend_from_slice(&response.bytes().await.unwrap());
    assert_eq!(rest, b"DONE");
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn retained_session_recreation_drains_exact_old_body_and_revokes_ticket() {
    tokio::time::timeout(Duration::from_secs(15), retained_session())
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn control_loss_recreation_bounds_old_body_termination_with_inspection() {
    tokio::time::timeout(Duration::from_secs(15), control_loss(true))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn control_loss_recreation_bounds_old_body_termination_without_inspection() {
    tokio::time::timeout(Duration::from_secs(15), control_loss(false))
        .await
        .unwrap();
}

struct RecreationFixture {
    _tokens: tempfile::NamedTempFile,
    daemon: tnld::serve::ServerHandle,
    server: tnld::serve::AppState,
    backend_addr: std::net::SocketAddr,
    backend: tokio::task::JoinHandle<usize>,
    release: tokio::sync::oneshot::Sender<()>,
    history: Inspection,
    replay: tnl::replay::ReplayController,
    service: Option<tnl::inspection::InspectorService>,
    endpoint: String,
}
impl RecreationFixture {
    async fn new(inspect: bool) -> Self {
        let tokens = tempfile::NamedTempFile::new().unwrap();
        let tf = tnld::auth::TokensFile {
            tokens: vec![tnld::auth::TokenEntry {
                name: "u".into(),
                hash: tnld::auth::hash_plaintext("tnl_U").unwrap(),
            }],
        };
        std::fs::write(tokens.path(), toml::to_string(&tf).unwrap()).unwrap();
        let (daemon, server) = tnld::serve::spawn_server_with_state(tnld::config::Config {
            listen: "127.0.0.1:0".into(),
            public_url: "http://x".into(),
            hostname_root: "t.x".into(),
            tokens_file: tokens.path().to_string_lossy().into_owned(),
            trusted_proxies: vec![],
            session_grace_sec: 30,
        })
        .await
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = listener.local_addr().unwrap();
        let (release, wait_release) = tokio::sync::oneshot::channel();
        let backend = tokio::spawn(async move {
            let (mut ticket, _) = listener.accept().await.unwrap();
            assert!(head(&mut ticket)
                .await
                .starts_with(b"GET /ticket HTTP/1.1\r\n"));
            ticket
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            drop(ticket);
            let (mut old, _) = listener.accept().await.unwrap();
            let old_request = head(&mut old).await;
            assert!(old_request.starts_with(b"GET /live HTTP/1.1\r\n"));
            old.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nLIVE")
                .await
                .unwrap();
            let old_task = tokio::spawn(async move {
                wait_release.await.unwrap();
                old.write_all(b"DONE").await.unwrap();
                old_request.len()
            });
            let (mut current, _) = listener.accept().await.unwrap();
            assert!(head(&mut current)
                .await
                .starts_with(b"GET /current HTTP/1.1\r\n"));
            current
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\nCURRENT")
                .await
                .unwrap();
            drop(current);
            old_task.await.unwrap()
        });
        let bound = tnl::inspection::BoundInspector::bind(0).await.unwrap();
        let replay = tnl::replay::ReplayController::new(inspect, bound.addr().port()).unwrap();
        let history = Inspection::with_replay(PreviewMode::Off, replay.clone());
        let service = inspect.then(|| bound.start(history.clone()));
        let endpoint = format!("http://{}", daemon.local_addr);
        Self {
            _tokens: tokens,
            daemon,
            server,
            backend_addr,
            backend,
            release,
            history,
            replay,
            service,
            endpoint,
        }
    }
    fn request(&self, client: &reqwest::Client, path: &str) -> reqwest::RequestBuilder {
        client
            .get(format!("{}{path}", self.endpoint))
            .header("Host", "foo.t.x")
    }
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap()
}
async fn terminal(
    history: &Inspection,
    id: tnl::inspection::CaptureId,
) -> tnl::inspection::InspectionRecord {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let row = history.get(id).unwrap();
            if row.state != State::Active {
                break row;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("forwarding record must terminate")
}
async fn prepared_ticket(
    fixture: &RecreationFixture,
) -> (tnl::inspection::CaptureId, tnl::replay::ReplayPreview) {
    let row = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(row) = fixture
                .history
                .search("/ticket")
                .into_iter()
                .find(|r| r.state == State::Complete)
            {
                break row;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    (
        row.capture_id,
        fixture.replay.prepare(row.capture_id).unwrap(),
    )
}
fn assert_revoked(
    fixture: &RecreationFixture,
    ticket: (tnl::inspection::CaptureId, tnl::replay::ReplayPreview),
) {
    let (id, ticket) = ticket;
    assert!(fixture
        .replay
        .confirm(id, ticket.replay_id, &ticket.confirmation_token, true)
        .is_err());
    assert_eq!(fixture.replay.raw_bytes(), 0);
}
fn expected_premature_body(error: &reqwest::Error) -> bool {
    use std::error::Error;
    if error.is_timeout() || !(error.is_decode() || error.is_body()) {
        return false;
    }
    let mut source = error.source();
    while let Some(cause) = source {
        if cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::UnexpectedEof)
        {
            return true;
        }
        source = cause.source();
    }
    false
}

async fn retained_session() {
    let fixture = RecreationFixture::new(true).await;
    let client = client();
    let old = tnl::client::connect_and_create(&fixture.endpoint, "tnl_U", "foo")
        .await
        .unwrap();
    let old_id = old.tunnel_id;
    // Deliberately retain the old control and accept loop through the drain.
    let old_control = old.control;
    let old_generation = tnl::inspection::TunnelGeneration::new();
    fixture.history.activate(old_generation);
    let mut ctx = tnl::forwarder::ForwardCtx::new("foo".into(), None, env!("CARGO_PKG_VERSION"));
    ctx.inspection = Some((fixture.history.clone(), old_generation));
    ctx.data_stream_protocol = old.data_stream_protocol;
    let old_accept = tokio::spawn(tnl::client::run_accept_loop(
        old.session,
        tnl::target::Target::Explicit(fixture.backend_addr),
        ctx,
    ));
    assert_eq!(
        fixture
            .request(&client, "/ticket")
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    let ticket = prepared_ticket(&fixture).await;
    let mut live = fixture.request(&client, "/live").send().await.unwrap();
    let mut remaining = read_live_prefix(&mut live).await;
    let live_id = fixture.history.search("/live")[0].capture_id;
    fixture
        .server
        .registry
        .close_by_subdomain("foo", "u")
        .unwrap();
    let current = tnl::client::connect_and_create(&fixture.endpoint, "tnl_U", "foo")
        .await
        .unwrap();
    assert_ne!(current.tunnel_id, old_id);
    let current_control = current.control;
    let new_generation = tnl::inspection::TunnelGeneration::new();
    fixture.history.activate(new_generation);
    let old_row = fixture.history.get(live_id).unwrap();
    assert_eq!(old_row.state, State::Active);
    assert_eq!(old_row.connection, GenerationState::Superseded);
    assert_eq!(old_row.generation, old_generation);
    assert_revoked(&fixture, ticket);
    let mut ctx = tnl::forwarder::ForwardCtx::new("foo".into(), None, env!("CARGO_PKG_VERSION"));
    ctx.inspection = Some((fixture.history.clone(), new_generation));
    ctx.data_stream_protocol = current.data_stream_protocol;
    let new_accept = tokio::spawn(tnl::client::run_accept_loop(
        current.session,
        tnl::target::Target::Explicit(fixture.backend_addr),
        ctx,
    ));
    assert_eq!(
        fixture
            .request(&client, "/current")
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        b"CURRENT"
    );
    fixture.release.send(()).unwrap();
    remaining.extend_from_slice(
        &tokio::time::timeout(Duration::from_secs(2), live.bytes())
            .await
            .expect("retained old response drain deadline")
            .unwrap(),
    );
    assert_eq!(remaining, b"DONE");
    let old_written = fixture.backend.await.unwrap();
    let old_row = terminal(&fixture.history, live_id).await;
    assert_eq!(old_row.state, State::Complete);
    assert_eq!(old_row.connection, GenerationState::Superseded);
    assert_eq!(old_row.generation, old_generation);
    assert!(old_row.failure_kind.is_none());
    assert_eq!(old_row.bytes_to_backend, old_written as u64);
    assert_eq!(old_row.bytes_to_client, 46);
    assert_eq!(old_row.bytes_from_backend, 46);
    let current = fixture.history.search("/current").pop().unwrap();
    assert_eq!(current.generation, new_generation);
    assert_eq!(current.connection, GenerationState::Connected);
    fixture.service.unwrap().shutdown().await;
    drop(old_control);
    drop(current_control);
    old_accept.abort();
    new_accept.abort();
    fixture.daemon.join.abort();
}

#[allow(clippy::too_many_lines)] // Actual reconnect, no test-held old control/session reference.
async fn control_loss(inspect: bool) {
    let fixture = RecreationFixture::new(inspect).await;
    let client = client();
    let (cancel, cancel_rx) = tokio::sync::oneshot::channel();
    let endpoint = fixture.endpoint.clone();
    let history = fixture.history.clone();
    let target = tnl::target::Target::Explicit(fixture.backend_addr);
    let reconnect = tokio::spawn(async move {
        tnl::reconnect::run(
            &endpoint,
            "tnl_U",
            Some("foo"),
            target,
            tnl::host_header::HostHeader::Preserve,
            tnl::reconnect::Hooks {
                inspection: inspect.then_some(history),
                cancel_first_session: Some(cancel_rx),
                log_tx: None,
            },
        )
        .await
    });
    let old_tunnel = loop {
        if let Some(tunnel) = fixture.server.registry.find_by_subdomain("foo") {
            break tunnel.id.clone();
        }
        tokio::task::yield_now().await;
    };
    assert_eq!(
        fixture
            .request(&client, "/ticket")
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    let ticket = if inspect {
        Some(prepared_ticket(&fixture).await)
    } else {
        None
    };
    let mut live = fixture.request(&client, "/live").send().await.unwrap();
    let mut prefetched = read_live_prefix(&mut live).await;
    let live_id = inspect.then(|| fixture.history.search("/live")[0].capture_id);
    let old_generation = live_id.map(|id| fixture.history.get(id).unwrap().generation);
    if let Some(id) = live_id {
        assert_eq!(fixture.history.get(id).unwrap().state, State::Active);
    }
    fixture
        .server
        .registry
        .close_by_subdomain("foo", "u")
        .unwrap();
    cancel.send(()).unwrap();
    let new_tunnel = loop {
        if let Some(tunnel) = fixture.server.registry.find_by_subdomain("foo") {
            if tunnel.id != old_tunnel {
                break tunnel.id.clone();
            }
        }
        tokio::task::yield_now().await;
    };
    if let Some(live_id) = live_id {
        let old_row = loop {
            let row = fixture.history.get(live_id).unwrap();
            if row.connection == GenerationState::Superseded {
                break row;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(old_row.tunnel_id.unwrap().to_string(), old_tunnel);
        assert_eq!(Some(old_row.generation), old_generation);
        assert_eq!(old_row.state, State::Active);
        assert!(old_row.bytes_to_client > 0);
        assert_revoked(&fixture, ticket.unwrap());
    }
    assert_eq!(
        fixture
            .request(&client, "/current")
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        b"CURRENT"
    );
    // This suffix is produced AFTER control loss. Graceful session close can
    // flush queued data, but cannot promise delivery of these future bytes.
    fixture.release.send(()).unwrap();
    let drained = tokio::time::timeout(Duration::from_secs(2), async {
        let rest = live.bytes().await?;
        prefetched.extend_from_slice(&rest);
        Ok::<_, reqwest::Error>(prefetched)
    })
    .await
    .expect("old response termination deadline; timeout is not termination");
    let failed = match drained {
        Ok(bytes) => {
            assert_eq!(bytes, b"DONE");
            false
        }
        Err(error) => {
            assert!(
                expected_premature_body(&error),
                "unexpected old body error: {error:?}"
            );
            true
        }
    };
    let old_written = fixture.backend.await.unwrap();
    if let Some(live_id) = live_id {
        let old_row = terminal(&fixture.history, live_id).await;
        assert_eq!(old_row.connection, GenerationState::Superseded);
        assert_eq!(Some(old_row.generation), old_generation);
        assert_eq!(
            old_row.response_source,
            tnl::inspection::ResponseSource::Backend
        );
        assert_eq!(old_row.bytes_to_backend, old_written as u64);
        if failed {
            assert_eq!(old_row.state, State::Failed);
            assert_eq!(old_row.failure_kind, Some("response-io"));
            assert_eq!(old_row.bytes_to_client, 42);
            assert_eq!(old_row.bytes_from_backend, 42);
        } else {
            assert_eq!(old_row.state, State::Complete);
            assert!(old_row.failure_kind.is_none());
            assert_eq!(old_row.bytes_to_client, 46);
            assert_eq!(old_row.bytes_from_backend, 46);
        }
        let current = fixture.history.search("/current").pop().unwrap();
        assert_ne!(Some(current.generation), old_generation);
        assert_eq!(current.tunnel_id.unwrap().to_string(), new_tunnel);
        assert_eq!(current.connection, GenerationState::Connected);
    }
    if let Some(service) = fixture.service {
        service.shutdown().await;
    }
    reconnect.abort();
    fixture.daemon.join.abort();
}
