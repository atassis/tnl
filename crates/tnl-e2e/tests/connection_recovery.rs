//! Real loopback production-wire qualification, with production heartbeat/lease clocks.
#[path = "support/relay.rs"]
mod relay;
use relay::{Mode, Relay};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use tnld::serve::{AppState, ServerHandle};

struct Fixture {
    _tokens: tempfile::NamedTempFile,
    daemon: ServerHandle,
    state: AppState,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.daemon.join.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let tokens = tempfile::NamedTempFile::new().unwrap();
        let file = tnld::auth::TokensFile {
            tokens: vec![tnld::auth::TokenEntry {
                name: "local".into(),
                hash: tnld::auth::hash_plaintext("tnl_LOCAL").unwrap(),
            }],
        };
        std::fs::write(tokens.path(), toml::to_string(&file).unwrap()).unwrap();
        let (daemon, state) = tnld::serve::spawn_server_with_state(tnld::config::Config {
            listen: "127.0.0.1:0".into(),
            public_url: "http://x".into(),
            hostname_root: "t.x".into(),
            tokens_file: tokens.path().to_string_lossy().into_owned(),
            trusted_proxies: vec![],
            session_grace_sec: 120,
        })
        .await
        .unwrap();
        Self {
            _tokens: tokens,
            daemon,
            state,
        }
    }
    fn endpoint(&self) -> String {
        format!("http://{}", self.daemon.local_addr)
    }
    async fn binding(&self, name: &str, previous: Option<&str>) -> (String, String) {
        until(Duration::from_secs(75), || {
            let t = self.state.registry.find_by_subdomain(name)?;
            let session = self.state.registry.current_session_id(&t.id)?;
            (Some(session.as_str()) != previous
                && self.state.session_handles.contains_key(&session))
            .then(|| (t.id.clone(), session))
        })
        .await
    }
    async fn get(&self, name: &str, path: &str) -> String {
        let response = http()
            .get(format!("{}{path}", self.endpoint()))
            .header("host", format!("{name}.t.x"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        response.text().await.unwrap()
    }
}
fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .pool_max_idle_per_host(0)
        .build()
        .unwrap()
}
async fn until<T>(budget: Duration, mut probe: impl FnMut() -> Option<T>) -> T {
    tokio::time::timeout(budget, async {
        loop {
            if let Some(value) = probe() {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("bounded observable lifecycle transition")
}
struct Task(tokio::task::JoinHandle<anyhow::Result<()>>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}
fn client(
    endpoint: String,
    name: &'static str,
    port: u16,
    history: tnl::inspection::Inspection,
) -> Task {
    Task(tokio::spawn(async move {
        tnl::reconnect::run(
            &endpoint,
            "tnl_LOCAL",
            Some(name),
            tnl::target::Target::LocalhostPort(port),
            tnl::host_header::HostHeader::Auto,
            tnl::reconnect::Hooks {
                inspection: Some(history),
                ..Default::default()
            },
        )
        .await
    }))
}
struct Backend {
    port: u16,
    count: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Backend {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn backend(identity: &'static str) -> Backend {
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let router = axum::Router::new().fallback(move |uri: axum::http::Uri| {
        let seen = seen.clone();
        async move {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            format!("{identity}:{n}:{}", uri.path())
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Backend { port, count, task }
}
fn history() -> tnl::inspection::Inspection {
    tnl::inspection::Inspection::new(tnl::inspection::PreviewMode::Off)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn two_way_blackhole_holds_old_owner_until_lease_then_recovers() {
    blackhole(Mode::Both).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn client_to_server_blackhole_holds_old_owner_until_lease_then_recovers() {
    blackhole(Mode::ClientToServer).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn server_to_client_blackhole_times_out_heartbeat_then_recovers() {
    blackhole(Mode::ServerToClient).await;
}

async fn blackhole(mode: Mode) {
    tokio::time::timeout(Duration::from_secs(110), async {
        let f = Fixture::new().await;
        let a = backend("A").await;
        let b = backend("B").await;
        let mut edge = Relay::start(f.daemon.local_addr).await;
        let history = history();
        let _a = client(
            format!("http://{}", edge.addr),
            "alpha",
            a.port,
            history.clone(),
        );
        let old = edge.next().await;
        let (id, session) = f.binding("alpha", None).await;
        let _b = client(f.endpoint(), "beta", b.port, self::history());
        let b_binding = f.binding("beta", None).await;
        assert_eq!(f.get("alpha", "/before").await, "A:0:/before");
        assert_eq!(f.get("beta", "/before").await, "B:0:/before");
        let old_tunnel = f.state.registry.find_by_subdomain("alpha").unwrap();
        old.blackhole(mode).await;
        // This is a real heartbeat-driven reconnection, not injected cancellation.
        let retry = edge.next().await;
        assert_eq!(
            f.state.registry.current_session_id(&id),
            Some(session.clone()),
            "no takeover before lease expiry"
        );
        if mode != Mode::ServerToClient {
            assert!(
                f.state.session_handles.contains_key(&session),
                "daemon still owns held-open old TCP"
            );
            assert_eq!(
                old.counters.eof[1].load(Ordering::SeqCst),
                0,
                "server has not sent EOF"
            );
        }
        assert!(
            old.counters
                .dropped
                .iter()
                .any(|v| v.load(Ordering::SeqCst) > 0),
            "fault dropped actual wire bytes"
        );
        assert_eq!(f.get("beta", "/during").await, "B:1:/during");
        assert_eq!(
            f.binding("beta", None).await,
            b_binding,
            "B generation unchanged"
        );
        let current = f.binding("alpha", Some(&session)).await;
        assert_eq!(current.0, id, "reattach keeps original name/id");
        assert!(Arc::ptr_eq(
            &old_tunnel,
            &f.state.registry.find_by_subdomain("alpha").unwrap()
        ));
        assert_eq!(f.get("alpha", "/after").await, "A:1:/after");
        assert_eq!(f.get("beta", "/after").await, "B:2:/after");
        assert_eq!(a.count.load(Ordering::SeqCst), 2, "no visitor replay");
        // Release only AFTER expiry/reattach proof; never forward the CLI FIN to the daemon.
        old.release();
        retry.release();
        assert!(history
            .search("")
            .iter()
            .all(|r| r.connection != tnl::inspection::GenerationState::Superseded));
        eprintln!(
            "qualified {mode:?}: retained id; B unchanged; dropped={:?}; eof={:?}",
            old.counters
                .dropped
                .each_ref()
                .map(|v| v.load(Ordering::SeqCst)),
            old.counters
                .eof
                .each_ref()
                .map(|v| v.load(Ordering::SeqCst))
        );
    })
    .await
    .expect("independent wall-clock watchdog");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn independent_session_same_name_race_has_one_winner_and_forwards_winning_backend() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let f = Fixture::new().await;
        let endpoint = f.endpoint();
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let barrier = barrier.clone();
            let endpoint = endpoint.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                tnl::client::connect_and_create(&endpoint, "tnl_LOCAL", "race").await
            }));
        }
        barrier.wait().await;
        let mut winner = None;
        for task in tasks {
            match task.await.unwrap() {
                Ok(session) => {
                    assert!(winner.is_none(), "exactly one winner");
                    winner = Some(session);
                }
                Err(error) => assert!(matches!(
                    error.downcast_ref::<tnl::client::ControlError>(),
                    Some(tnl::client::ControlError::Server(
                        tnl_protocol::ErrorCode::SubdomainTaken
                    ))
                )),
            }
        }
        let winner = winner.expect("one registration succeeds");
        let winning_id = winner.tunnel_id.to_string();
        let backend = backend("WINNER").await;
        let accept = Task(tokio::spawn(async move {
            let _control = winner.control;
            let ctx = tnl::forwarder::ForwardCtx {
                data_stream_protocol: winner.data_stream_protocol,
                ..tnl::forwarder::ForwardCtx::new("race".into(), None, env!("CARGO_PKG_VERSION"))
            };
            tnl::client::run_accept_loop(
                winner.session,
                tnl::target::Target::LocalhostPort(backend.port),
                ctx,
            )
            .await
        }));
        assert_eq!(f.binding("race", None).await.0, winning_id);
        assert_eq!(f.get("race", "/identity").await, "WINNER:0:/identity");
        drop(accept);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[allow(clippy::too_many_lines)]
async fn executed_post_response_lost_and_interrupted_websocket_are_not_replayed() {
    use async_tungstenite::tungstenite::Message;
    use futures::StreamExt;
    tokio::time::timeout(Duration::from_secs(110), async {
        let f = Fixture::new().await;
        let executions = Arc::new(AtomicUsize::new(0));
        let sockets = Arc::new(AtomicUsize::new(0));
        let (executed, mut events) = tokio::sync::mpsc::channel(4);
        let response_gate = Arc::new(tokio::sync::Notify::new());
        let router = axum::Router::new()
            .route(
                "/execute",
                axum::routing::post({
                    let executions = executions.clone();
                    let response_gate = response_gate.clone();
                    move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                        let executions = executions.clone();
                        let response_gate = response_gate.clone();
                        let executed = executed.clone();
                        async move {
                            executions.fetch_add(1, Ordering::SeqCst);
                            executed
                                .send((
                                    headers["x-identity"].to_str().unwrap().to_owned(),
                                    body.to_vec(),
                                ))
                                .await
                                .unwrap();
                            response_gate.notified().await;
                            "EXECUTED-ONCE-RESPONSE-LOST"
                        }
                    }
                }),
            )
            .route(
                "/ws",
                axum::routing::get({
                    let sockets = sockets.clone();
                    move |upgrade: axum::extract::ws::WebSocketUpgrade| {
                        let id = sockets.fetch_add(1, Ordering::SeqCst);
                        async move {
                            upgrade.on_upgrade(move |mut socket| async move {
                                if socket
                                    .send(axum::extract::ws::Message::Text(
                                        format!("connection-{id}").into(),
                                    ))
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                                while let Some(Ok(message)) = socket.recv().await {
                                    if socket.send(message).await.is_err() {
                                        break;
                                    }
                                }
                            })
                        }
                    }
                }),
            )
            .route("/fresh", axum::routing::get(|| async { "FRESH-DISTINCT" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let backend = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut edge = Relay::start(f.daemon.local_addr).await;
        let _client = client(format!("http://{}", edge.addr), "alpha", port, history());
        let old = edge.next().await;
        let (id, session) = f.binding("alpha", None).await;
        let ws_request = || {
            http::Request::builder()
                .uri(format!("ws://{}/ws", f.daemon.local_addr))
                .header("host", "alpha.t.x")
                .header("connection", "upgrade")
                .header("upgrade", "websocket")
                .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
                .header("sec-websocket-version", "13")
                .body(())
                .unwrap()
        };
        let (mut old_ws, _) = async_tungstenite::tokio::connect_async(ws_request())
            .await
            .unwrap();
        assert_eq!(
            old_ws.next().await.unwrap().unwrap(),
            Message::Text("connection-0".into())
        );
        old_ws
            .send(Message::Text("old-payload".into()))
            .await
            .unwrap();
        assert_eq!(
            old_ws.next().await.unwrap().unwrap(),
            Message::Text("old-payload".into())
        );
        let endpoint = f.endpoint();
        let mut pending_post = tokio::spawn(async move {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(100))
                .build()
                .unwrap()
                .post(format!("{endpoint}/execute"))
                .header("host", "alpha.t.x")
                .header("x-identity", "unique-operation-17")
                .body("exact-post-body-17")
                .send()
                .await
        });
        let wire = events.recv().await.unwrap();
        assert_eq!(
            wire,
            ("unique-operation-17".into(), b"exact-post-body-17".to_vec())
        );
        old.blackhole(Mode::Both).await;
        response_gate.notify_one();
        let current = f.binding("alpha", Some(&session)).await;
        assert_eq!(current.0, id);
        assert_eq!(f.get("alpha", "/fresh").await, "FRESH-DISTINCT");
        match (&mut pending_post).await.unwrap() {
            Err(error) => assert!(
                !error.is_timeout(),
                "old POST must terminate, not hit visitor deadline"
            ),
            Ok(response) => assert!(
                !response.status().is_success(),
                "lost response cannot succeed"
            ),
        }
        let old_result = tokio::time::timeout(Duration::from_secs(5), old_ws.next())
            .await
            .unwrap();
        assert!(
            matches!(old_result, None | Some(Err(_) | Ok(Message::Close(_)))),
            "old WS cannot migrate"
        );
        let (mut fresh_ws, _) = async_tungstenite::tokio::connect_async(ws_request())
            .await
            .unwrap();
        assert_eq!(
            fresh_ws.next().await.unwrap().unwrap(),
            Message::Text("connection-1".into())
        );
        fresh_ws
            .send(Message::Text("fresh-payload".into()))
            .await
            .unwrap();
        assert_eq!(
            fresh_ws.next().await.unwrap().unwrap(),
            Message::Text("fresh-payload".into())
        );
        assert_eq!(
            executions.load(Ordering::SeqCst),
            1,
            "no automatic POST replay"
        );
        assert_eq!(sockets.load(Ordering::SeqCst), 2, "no automatic WS replay");
        assert!(events.try_recv().is_err());
        old.release();
        backend.abort();
    })
    .await
    .expect("independent scenario watchdog");
}

struct DaemonProcess(std::process::Child);
impl DaemonProcess {
    fn start(binary: &std::path::Path, config: &std::path::Path, home: &std::path::Path) -> Self {
        Self(
            std::process::Command::new(binary)
                .env_clear()
                .env("HOME", home)
                .args(["serve", "--config"])
                .arg(config)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
    fn terminate_and_wait(&mut self) {
        self.0.kill().unwrap();
        let status = self.0.wait().unwrap();
        assert!(!status.success());
    }
}
impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Run explicitly after building this worktree's daemon, supplying its absolute path.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore = "requires TNL_R6_TNLD pointing at the freshly built isolated candidate"]
async fn actual_daemon_process_restart_recreates_original_name_new_generation_and_revokes_replay() {
    tokio::time::timeout(Duration::from_secs(45), async {
        let binary = std::path::PathBuf::from(std::env::var_os("TNL_R6_TNLD").expect("explicit built candidate"));
        assert!(binary.is_absolute() && binary.is_file());
        let home = tempfile::tempdir().unwrap();
        let tokens = home.path().join("tokens.toml");
        std::fs::write(&tokens, toml::to_string(&tnld::auth::TokensFile { tokens: vec![tnld::auth::TokenEntry {
            name: "local".into(), hash: tnld::auth::hash_plaintext("tnl_LOCAL").unwrap(),
        }] }).unwrap()).unwrap();
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        let config = home.path().join("daemon.toml");
        std::fs::write(&config, format!("listen = \"{address}\"\npublic_url = \"http://x\"\nhostname_root = \"t.x\"\ntokens_file = {:?}\nsession_grace_sec = 120\n", tokens.to_str().unwrap())).unwrap();
        drop(reservation);
        let mut daemon = DaemonProcess::start(&binary, &config, home.path());
        let endpoint = format!("http://{address}");
        let health = http();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop { if health.get(format!("{endpoint}/healthz")).send().await.is_ok() { break; }
                tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await.unwrap();
        let backend = backend("PROCESS").await;
        let inspector = tnl::inspection::BoundInspector::bind(0).await.unwrap();
        let replay = tnl::replay::ReplayController::new(true, inspector.addr().port()).unwrap();
        let history = tnl::inspection::Inspection::with_replay(tnl::inspection::PreviewMode::Off, replay.clone());
        let _service = inspector.start(history.clone());
        let _client = client(endpoint.clone(), "original", backend.port, history.clone());
        let get = |path: &'static str| health.get(format!("{endpoint}{path}")).header("host", "original.t.x");
        tokio::time::timeout(Duration::from_secs(10), async {
            loop { if let Ok(response) = get("/ticket").send().await { if response.status().is_success() {
                assert!(response.text().await.unwrap().starts_with("PROCESS:")); break;
            }} tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await.unwrap();
        let old = until(Duration::from_secs(5), || history.search("/ticket").into_iter()
            .find(|r| r.state == tnl::inspection::State::Complete)).await;
        let ticket = replay.prepare(old.capture_id).unwrap();
        let old_pid = daemon.0.id();
        daemon.terminate_and_wait();
        until(Duration::from_secs(8), || history.get(old.capture_id)
            .filter(|r| r.connection == tnl::inspection::GenerationState::Disconnected)).await;
        assert!(replay.confirm(old.capture_id, ticket.replay_id, &ticket.confirmation_token, true).is_err());
        assert!(replay.prepare(old.capture_id).is_err(), "disconnected history has no replay authority");
        daemon = DaemonProcess::start(&binary, &config, home.path());
        assert_ne!(daemon.0.id(), old_pid);
        tokio::time::timeout(Duration::from_secs(20), async {
            loop { if let Ok(response) = get("/after-restart").send().await { if response.status().is_success() {
                assert!(response.text().await.unwrap().starts_with("PROCESS:")); break;
            }} tokio::time::sleep(Duration::from_millis(25)).await; }
        }).await.unwrap();
        let current = until(Duration::from_secs(5), || history.search("/after-restart").into_iter()
            .find(|r| r.state == tnl::inspection::State::Complete)).await;
        assert_ne!(current.generation, old.generation);
        let retained = history.get(old.capture_id).unwrap();
        assert_eq!(retained.state, tnl::inspection::State::Complete);
        assert_eq!(retained.connection, tnl::inspection::GenerationState::Superseded);
        assert_eq!(current.connection, tnl::inspection::GenerationState::Connected);
        assert!(replay.prepare(old.capture_id).is_err(), "new generation cannot replay retained old history");
        eprintln!("actual process restart: old pid={old_pid}, new pid={}, binary={}", daemon.0.id(), binary.display());
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn repeated_blackholes_settle_to_one_live_relay_and_one_daemon_handle() {
    tokio::time::timeout(Duration::from_secs(260), async {
        let f = Fixture::new().await;
        let backend = backend("CYCLE").await;
        let mut edge = Relay::start(f.daemon.local_addr).await;
        let _client = client(format!("http://{}", edge.addr), "cycles", backend.port, history());
        let mut connection = edge.next().await;
        let (id, mut session) = f.binding("cycles", None).await;
        for cycle in 0..3 {
            assert_eq!(f.get("cycles", "/cycle").await, format!("CYCLE:{cycle}:/cycle"));
            connection.blackhole(Mode::Both).await;
            let current = f.binding("cycles", Some(&session)).await;
            assert_eq!(current.0, id);
            session = current.1;
            // All retries have completed registration before this routing barrier.
            // Release obsolete relay peers, retaining the final successful peer.
            connection.release();
            let mut latest = edge.connections.try_recv().expect("at least one physical reconnect");
            while let Ok(next) = edge.connections.try_recv() { latest.release(); latest = next; }
            connection = latest;
            until(Duration::from_secs(5), || (edge.active.load(Ordering::SeqCst) == 1
                && f.state.session_handles.len() == 1).then_some(())).await;
            assert_eq!(f.state.registry.snapshot_infos().len(), 1);
            assert_eq!(backend.count.load(Ordering::SeqCst), cycle + 1);
            eprintln!("settled cycle {cycle}: relay=1, daemon_handles=1, routing_tunnels=1, backend_executions={}", cycle + 1);
        }
        assert_eq!(f.get("cycles", "/final").await, "CYCLE:3:/final");
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn remote_stop_of_a_preserves_b_ongoing_and_new_forwarding() {
    use futures::StreamExt;
    tokio::time::timeout(Duration::from_secs(35), async {
        let f = Fixture::new().await;
        let a = backend("A").await;
        let gate = Arc::new(tokio::sync::Notify::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let b_router = axum::Router::new()
            .route(
                "/ongoing",
                axum::routing::get({
                    let gate = gate.clone();
                    move || {
                        let gate = gate.clone();
                        async move {
                            let first =
                                futures::stream::once(async { Ok::<_, std::io::Error>("B-BEGIN") });
                            let last = futures::stream::once(async move {
                                gate.notified().await;
                                Ok::<_, std::io::Error>("B-END")
                            });
                            axum::body::Body::from_stream(first.chain(last))
                        }
                    }
                }),
            )
            .route("/new", axum::routing::get(|| async { "B-NEW" }));
        let b_backend = tokio::spawn(async move {
            axum::serve(listener, b_router).await.unwrap();
        });
        let mut a_client = client(f.endpoint(), "alpha", a.port, history());
        let _b_client = client(f.endpoint(), "beta", port, history());
        f.binding("alpha", None).await;
        let b_binding = f.binding("beta", None).await;
        assert_eq!(f.get("alpha", "/before-stop").await, "A:0:/before-stop");
        // This visitor deliberately outlives A's production 15-second heartbeat.
        let ongoing_http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        let mut ongoing = ongoing_http
            .get(format!("{}/ongoing", f.endpoint()))
            .header("host", "beta.t.x")
            .send()
            .await
            .unwrap();
        assert_eq!(ongoing.chunk().await.unwrap().unwrap(), "B-BEGIN");
        f.state
            .registry
            .close_by_subdomain("alpha", "local")
            .unwrap();
        tokio::time::timeout(Duration::from_secs(22), &mut a_client.0)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(f.state.registry.find_by_subdomain("alpha").is_none());
        assert_eq!(f.get("beta", "/new").await, "B-NEW");
        assert_eq!(f.binding("beta", None).await, b_binding);
        gate.notify_one();
        assert_eq!(ongoing.bytes().await.unwrap(), "B-END");
        b_backend.abort();
    })
    .await
    .unwrap();
}
