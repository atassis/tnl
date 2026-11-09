//! Missing R6 boundaries; production clocks and verified fixture TLS only.
use super::*;
#[path = "../../../tnl-e2e/tests/support/relay.rs"]
mod relay;
use crate::client::resource_observation_tests::{Resources, OBSERVE};
use relay::{Mode, Relay};
use tokio::sync::Notify;

struct Run(tokio::task::JoinHandle<anyhow::Result<()>>);
impl Drop for Run {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn until<T>(budget: u64, mut probe: impl FnMut() -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(budget), async {
        loop {
            if let Some(value) = probe() {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("bounded observable transition")
}
async fn binding(f: &Fixture, name: &str, previous: Option<&str>) -> (String, String) {
    until(80, || {
        let tunnel = f.state.registry.find_by_subdomain(name)?;
        let session = f.state.registry.current_session_id(&tunnel.id)?;
        (Some(session.as_str()) != previous && f.state.session_handles.contains_key(&session))
            .then(|| (tunnel.id.clone(), session))
    })
    .await
}
async fn tls_edge(f: &mut Fixture, upstream: SocketAddr) {
    f.edge_task.abort();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    f.endpoint = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    let acceptor = f.tls_acceptor.clone();
    f.edge_task = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                Some(result) = tasks.join_next(), if !tasks.is_empty() => { result.unwrap(); }
                connection = listener.accept() => {
                    let (tcp, _) = connection.unwrap(); let acceptor = acceptor.clone();
                    assert!(tasks.len() < 8, "bounded TLS edge");
                    tasks.spawn(async move {
                        let Ok(mut tls) = acceptor.accept(tcp).await else { return; };
                        let mut upstream = TcpStream::connect(upstream).await.unwrap();
                        let _ = tokio::io::copy_bidirectional(&mut tls, &mut upstream).await;
                    });
                }
            }
        }
    });
}
fn run(f: &Fixture, name: &'static str, options: TunnelOptions, observer: Arc<Resources>) -> Run {
    let endpoint = f.endpoint.clone();
    let connector = f.connector.clone();
    let backend = f.backend;
    Run(tokio::spawn(OBSERVE.scope(
        observer,
        TEST_TLS_CONNECTOR.scope(connector, async move {
            crate::reconnect::run_with_options(
                &endpoint,
                "tnl_TEST",
                Some(name),
                crate::target::Target::Explicit(backend),
                crate::host_header::HostHeader::Preserve,
                &options,
                crate::reconnect::Hooks::default(),
            )
            .await
        }),
    )))
}
async fn public_backend() -> (SocketAddr, tokio::task::JoinHandle<()>, Arc<Notify>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let release = Arc::new(Notify::new());
    let gate = release.clone();
    let task = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                Some(result) = tasks.join_next(), if !tasks.is_empty() => { result.unwrap(); }
                connection = listener.accept() => {
                    let (mut io, _) = connection.unwrap(); let gate = gate.clone();
                    tasks.spawn(async move {
                        let request = head(&mut io).await;
                        if request.starts_with("GET /hold ") {
                            io.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 22\r\n\r\nPUBLIC-BEGIN").await.unwrap();
                            gate.notified().await; let _ = io.write_all(b"PUBLIC-END").await;
                        } else { io.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nPUBLIC-NEW").await.unwrap(); }
                    });
                }
            }
        }
    });
    (addr, task, release)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn protected_both_way_lease_preserves_policy_quota_and_public_ongoing_forward() {
    protected_lease(Mode::Both).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn protected_client_to_server_lease_preserves_policy_quota_and_public_ongoing_forward() {
    protected_lease(Mode::ClientToServer).await;
}
#[allow(clippy::too_many_lines)]
async fn protected_lease(mode: Mode) {
    tokio::time::timeout(Duration::from_secs(130), async {
        let mut f = fixture().await;
        let mut edge = Relay::start(f.server.local_addr).await;
        tls_edge(&mut f, edge.addr).await;
        let options = options(true);
        let fingerprint = options.access.as_ref().unwrap().fingerprint().unwrap();
        let mut owner = run(
            &f,
            "guarded",
            options.clone(),
            Arc::new(Resources::default()),
        );
        let old = edge.next().await;
        let (id, session) = binding(&f, "guarded", None).await;
        let tunnel = f.state.registry.find_by_subdomain("guarded").unwrap();
        let access = tunnel.access.as_ref().unwrap().clone();
        assert_eq!(access.fingerprint(), fingerprint);
        assert_eq!(
            visitor(&f, "/authorized")
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "ok"
        );
        let public = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .unwrap();
        let denied = public
            .get(format!("http://{}/denied", f.server.local_addr))
            .header("host", "guarded.t.x")
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), 401);
        // Use the actual Basic authorization generated by the visitor builder, without logging it.
        let request = visitor(&f, "/").build().unwrap();
        let mut headers = request.headers().clone();
        let held = access
            .admit(Some("127.0.0.1".parse().unwrap()), &mut headers)
            .await
            .unwrap();
        let (backend, backend_task, release) = public_backend().await;
        let endpoint = format!("http://{}", f.server.local_addr);
        let public_endpoint = endpoint.clone();
        let _public_run = Run(tokio::spawn(async move {
            crate::reconnect::run(
                &public_endpoint,
                "tnl_TEST",
                Some("public"),
                crate::target::Target::Explicit(backend),
                crate::host_header::HostHeader::Preserve,
                crate::reconnect::Hooks::default(),
            )
            .await
        }));
        let public_binding = binding(&f, "public", None).await;
        let mut ongoing = public
            .get(format!("{endpoint}/hold"))
            .header("host", "public.t.x")
            .send()
            .await
            .unwrap();
        assert_eq!(ongoing.chunk().await.unwrap().unwrap(), "PUBLIC-BEGIN");
        old.blackhole(mode).await;
        let retry = edge.next().await;
        assert_eq!(
            f.state.registry.current_session_id(&id),
            Some(session.clone())
        );
        assert!(f.state.session_handles.contains_key(&session));
        assert_eq!(
            old.counters.eof[1].load(Ordering::SeqCst),
            0,
            "no server FIN before lease"
        );
        let error = connect_and_reattach_with_connector(
            &f.endpoint,
            "tnl_TEST",
            tunnel.id.parse().unwrap(),
            "guarded",
            &options,
            Some(f.connector.clone()),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<ProtectedControlError>(),
            Some(ProtectedControlError::Server(
                tnl_protocol::ErrorCode::TunnelAlreadyActive
            ))
        ));
        let current = binding(&f, "guarded", Some(&session)).await;
        assert_eq!(current.0, id);
        let reattached = f.state.registry.find_by_subdomain("guarded").unwrap();
        assert!(Arc::ptr_eq(&tunnel, &reattached));
        assert!(Arc::ptr_eq(&access, reattached.access.as_ref().unwrap()));
        assert_eq!(
            reattached.access.as_ref().unwrap().fingerprint(),
            fingerprint
        );
        assert_eq!(
            public
                .get(format!("{endpoint}/still-protected"))
                .header("host", "guarded.t.x")
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            visitor(&f, "/held-quota").send().await.unwrap().status(),
            429
        );
        drop(held);
        assert_eq!(
            visitor(&f, "/reattached")
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "ok"
        );
        assert_eq!(
            public
                .get(format!("{endpoint}/new"))
                .header("host", "public.t.x")
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "PUBLIC-NEW"
        );
        assert_eq!(binding(&f, "public", None).await, public_binding);
        f.state
            .registry
            .close_by_subdomain("guarded", "owner")
            .unwrap();
        tokio::time::timeout(Duration::from_secs(22), &mut owner.0)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            public
                .get(format!("{endpoint}/new"))
                .header("host", "public.t.x")
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "PUBLIC-NEW"
        );
        release.notify_one();
        assert_eq!(ongoing.bytes().await.unwrap(), "PUBLIC-END");
        assert_eq!(binding(&f, "public", None).await, public_binding);
        old.release();
        retry.release();
        backend_task.abort();
        assert!(old.counters.dropped[0].load(Ordering::SeqCst) > 0);
        drop(f);
    })
    .await
    .expect("protected lease/mixed-client wall budget");
}

#[allow(clippy::too_many_lines)]
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn actual_run_three_physical_failures_settle_private_tracker_generations_and_backend_io() {
    tokio::time::timeout(Duration::from_secs(260), async {
        let mut f = fixture().await;
        let mut edge = Relay::start(f.server.local_addr).await;
        tls_edge(&mut f, edge.addr).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        f.backend = listener.local_addr().unwrap();
        let (eof_tx, mut eof_rx) = tokio::sync::mpsc::channel(4);
        let backend = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop { tokio::select! {
                Some(result) = tasks.join_next(), if !tasks.is_empty() => { result.unwrap(); }
                connection = listener.accept() => {
                    let (mut io, _) = connection.unwrap(); let eof_tx = eof_tx.clone();
                    tasks.spawn(async move {
                        assert!(head(&mut io).await.starts_with("GET /hold "));
                        io.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nHELD").await.unwrap();
                        let mut rest = Vec::new(); let _ = io.read_to_end(&mut rest).await;
                        eof_tx.send(()).await.unwrap();
                    });
                }
            }}
        });
        let observer = Arc::new(Resources::default());
        assert_eq!(observer.snapshot(), (0, 0, 0));
        let mut owner = run(&f, "guarded", options(false), observer.clone());
        let mut old = edge.next().await;
        let (id, mut session) = binding(&f, "guarded", None).await;
        for cycle in 0..3 {
            until(5, || (observer.snapshot() == (1, 1, 1)).then_some(())).await;
            let mut held = visitor(&f, "/hold").send().await.unwrap();
            assert_eq!(held.chunk().await.unwrap().unwrap(), "HELD");
            until(5, || (observer.snapshot() == (2, 1, 1)).then_some(())).await;
            old.blackhole(Mode::Both).await;
            tokio::time::timeout(Duration::from_secs(30), eof_rx.recv()).await.unwrap().unwrap();
            let body_error = held.bytes().await.unwrap_err();
            assert!(!body_error.is_timeout());
            let current = binding(&f, "guarded", Some(&session)).await;
            assert_eq!(current.0, id); session = current.1;
            let settled = tokio::time::timeout(Duration::from_secs(5), async {
                while observer.snapshot() != (1, 1, 1) || observer.completed() != cycle + 1 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await;
            assert!(settled.is_ok(), "cycle {cycle} leaked actual resources: tracker/handles/accepts={:?}, physical_completed={}", observer.snapshot(), observer.completed());
            old.release();
            let mut newest = edge.connections.try_recv().unwrap();
            while let Ok(next) = edge.connections.try_recv() { newest.release(); newest = next; }
            old = newest;
            until(5, || (edge.active.load(Ordering::SeqCst) == 1).then_some(())).await;
            eprintln!("private settled cycle {cycle}: tracker/handles/accepts={:?}, physical_completed={}, backend EOF observed", observer.snapshot(), observer.completed());
        }
        f.state.registry.close_by_subdomain("guarded", "owner").unwrap();
        tokio::time::timeout(Duration::from_secs(22), &mut owner.0).await.unwrap().unwrap().unwrap();
        assert_eq!(observer.snapshot(), (0, 0, 0), "settled explicit run exit, before fixture teardown");
        assert_eq!(observer.completed(), 3);
        backend.abort();
        drop(f);
    }).await.expect("independent real-cycle budget");
}

struct Process(std::process::Child);
impl Process {
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
    fn kill_wait(&mut self) {
        self.0.kill().unwrap();
        assert!(!self.0.wait().unwrap().success());
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[allow(clippy::too_many_lines)]
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore = "requires TNL_R6_TNLD identifying the freshly built isolated candidate"]
async fn protected_actual_process_restart_recreates_exact_complete_policy_and_history() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let binary = std::path::PathBuf::from(std::env::var_os("TNL_R6_TNLD").expect("explicit candidate binary"));
        assert!(binary.is_absolute() && binary.is_file());
        let mut f = fixture().await;
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = port.local_addr().unwrap(); drop(port);
        let home = tempfile::tempdir().unwrap(); let config = home.path().join("daemon.toml");
        std::fs::write(&config, format!("listen = \"{address}\"\npublic_url = \"https://test\"\nhostname_root = \"t.x\"\ntokens_file = {:?}\nsession_grace_sec = 120\n", f.tokens.path().to_str().unwrap())).unwrap();
        let mut daemon = Process::start(&binary, &config, home.path());
        f.server.local_addr = address;
        let mut events = recording_edge(&mut f).await;
        let base = options(true);
        let policy = Arc::new(tnl_protocol::access::VisitorAccessPolicyV1::new(
            base.access.as_ref().unwrap().basic().cloned(), &["127.0.0.0/8".into()],
            Some(tnl_protocol::access::VisitorRateLimit { requests_per_second: 4, burst: 8 }), Some(1)).unwrap());
        assert_eq!(policy.max_concurrent(), Some(1));
        assert_eq!(policy.rate_limit().unwrap().burst, 8);
        assert!(!policy.allows_ip("192.0.2.1".parse().unwrap()));
        let fingerprint = policy.fingerprint().unwrap();
        let inspector = crate::inspection::BoundInspector::bind(0).await.unwrap();
        let replay = crate::replay::ReplayController::new(true, inspector.addr().port()).unwrap();
        let history = crate::inspection::Inspection::with_replay(crate::inspection::PreviewMode::Off, replay.clone());
        let _service = inspector.start(history.clone());
        let run_history = history.clone(); let endpoint = f.endpoint.clone(); let connector = f.connector.clone(); let target = f.backend;
        let _owner = Run(tokio::spawn(TEST_TLS_CONNECTOR.scope(connector, async move {
            crate::reconnect::run_with_options(&endpoint, "tnl_TEST", Some("guarded"), crate::target::Target::Explicit(target),
                crate::host_header::HostHeader::Preserve, &TunnelOptions { access: Some(policy) },
                crate::reconnect::Hooks { inspection: Some(run_history), ..Default::default() }).await
        })));
        let (proceed, _cut) = ready(&mut events, 1).await;
        // Health readiness is independent of the TLS/control registration barrier.
        let http = reqwest::Client::builder().timeout(Duration::from_secs(5)).build().unwrap();
        tokio::time::timeout(Duration::from_secs(10), async { loop {
            if http.get(format!("http://{address}/healthz")).send().await.is_ok() { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }}).await.unwrap();
        proceed.send(()).unwrap();
        client_message(&mut events, 1, Lifecycle::Create { subdomain: Some("guarded".into()), fingerprint: fingerprint.clone() }, true).await;
        let Lifecycle::Created { tunnel_id: old_id, subdomain, fingerprint: ack } = server_message(&mut events, 1).await else { panic!("protected ACK required"); };
        assert_eq!(subdomain, "guarded"); assert_eq!(ack, fingerprint);
        assert_eq!(visitor(&f, "/ticket").send().await.unwrap().text().await.unwrap(), "ok");
        let old = until(5, || history.search("/ticket").into_iter().find(|r| r.state == crate::inspection::State::Complete)).await;
        let ticket = replay.prepare(old.capture_id).unwrap();
        let previous_process = daemon.0.id(); daemon.kill_wait();
        until(8, || history.get(old.capture_id).filter(|r| r.connection == crate::inspection::GenerationState::Disconnected)).await;
        assert!(replay.confirm(old.capture_id, ticket.replay_id, &ticket.confirmation_token, true).is_err());
        daemon = Process::start(&binary, &config, home.path()); assert_ne!(daemon.0.id(), previous_process);
        let (proceed, _cut) = ready(&mut events, 2).await; proceed.send(()).unwrap();
        client_message(&mut events, 2, Lifecycle::Reattach { tunnel_id: old_id, subdomain: "guarded".into(), fingerprint: fingerprint.clone() }, true).await;
        assert_eq!(server_message(&mut events, 2).await, Lifecycle::Error(tnl_protocol::ErrorCode::TunnelLost));
        let (proceed, _cut) = ready(&mut events, 3).await; proceed.send(()).unwrap();
        client_message(&mut events, 3, Lifecycle::Create { subdomain: Some("guarded".into()), fingerprint: fingerprint.clone() }, true).await;
        let Lifecycle::Created { tunnel_id: new_id, subdomain, fingerprint: ack } = server_message(&mut events, 3).await else { panic!("protected recreation ACK required"); };
        assert_ne!(new_id, old_id); assert_eq!(subdomain, "guarded"); assert_eq!(ack, fingerprint);
        let denied = http.get(format!("http://{address}/wrong")).header("host", "guarded.t.x").basic_auth("user", Some("wrong")).send().await.unwrap();
        assert_eq!(denied.status(), 401);
        let mut held = visitor(&f, "/hold").send().await.unwrap(); assert_eq!(held.chunk().await.unwrap().unwrap(), "part");
        assert_eq!(visitor(&f, "/concurrent").send().await.unwrap().status(), 429);
        f.release.send(true).unwrap(); assert!(held.bytes().await.unwrap().is_empty());
        assert_eq!(visitor(&f, "/after-process").send().await.unwrap().text().await.unwrap(), "ok");
        let new = until(5, || history.search("/after-process").into_iter().find(|r| r.state == crate::inspection::State::Complete)).await;
        assert_ne!(new.generation, old.generation); assert_eq!(new.tunnel_id, Some(new_id));
        let retained = history.get(old.capture_id).unwrap(); assert_eq!(retained.state, crate::inspection::State::Complete);
        assert_eq!(retained.connection, crate::inspection::GenerationState::Superseded);
        assert!(replay.prepare(old.capture_id).is_err());
        eprintln!("protected process restart previous_process={previous_process} new_pid={} same-policy ACK, new tunnel/generation, retained history", daemon.0.id());
        drop(f);
    }).await.expect("protected actual process wall budget");
}
