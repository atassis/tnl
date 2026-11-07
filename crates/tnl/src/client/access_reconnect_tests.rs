//! Transparent TLS/WS test edge for the actual shared reconnect loop. Records
//! only typed lifecycle summaries, never raw policy/verifier/header diagnostics.
use super::*;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, PartialEq, Eq)]
enum Lifecycle {
    Create {
        subdomain: Option<String>,
        fingerprint: String,
    },
    Reattach {
        tunnel_id: ulid::Ulid,
        subdomain: String,
        fingerprint: String,
    },
    Created {
        tunnel_id: ulid::Ulid,
        subdomain: String,
        fingerprint: String,
    },
    Error(tnl_protocol::ErrorCode),
    Public,
    Other,
}

impl Lifecycle {
    fn from_control(message: ControlMsg) -> Self {
        match message {
            ControlMsg::CreateProtectedTunnel(request) => Self::Create {
                subdomain: request.subdomain,
                fingerprint: request.access.fingerprint().unwrap(),
            },
            ControlMsg::ReattachProtectedTunnel(request) => Self::Reattach {
                tunnel_id: request.tunnel_id,
                subdomain: request.subdomain,
                fingerprint: request.policy_fingerprint,
            },
            ControlMsg::ProtectedTunnelCreated(response) => Self::Created {
                tunnel_id: response.tunnel_id,
                subdomain: response.subdomain,
                fingerprint: response.policy_fingerprint,
            },
            ControlMsg::Error { code, .. } => Self::Error(code),
            ControlMsg::CreateTunnel(_)
            | ControlMsg::ReattachTunnel(_)
            | ControlMsg::TunnelCreated(_) => Self::Public,
            _ => Self::Other,
        }
    }
}

#[derive(Debug)]
enum EdgeEvent {
    Ready {
        connection: usize,
        proceed: oneshot::Sender<()>,
        cut: oneshot::Sender<()>,
    },
    Client {
        connection: usize,
        lifecycle: Lifecycle,
        forward: oneshot::Sender<bool>,
    },
    Server {
        connection: usize,
        lifecycle: Lifecycle,
    },
}

#[derive(Default)]
struct ControlFrames {
    wire: zeroize::Zeroizing<Vec<u8>>,
    control: zeroize::Zeroizing<Vec<u8>>,
}

impl ControlFrames {
    fn receive(&mut self, bytes: &[u8]) -> Vec<Lifecycle> {
        self.wire.extend_from_slice(bytes);
        let mut messages = Vec::new();
        while self.wire.len() >= 12 {
            // Yamux headers are fixed 12 bytes; only Data has a payload.
            assert_eq!(self.wire[0], 0);
            let data = self.wire[1] == 0;
            let stream_id = u32::from_be_bytes(self.wire[4..8].try_into().unwrap());
            let length = if data {
                u32::from_be_bytes(self.wire[8..12].try_into().unwrap()) as usize
            } else {
                0
            };
            assert!(length <= 1024 * 1024);
            if self.wire.len() < 12 + length {
                break;
            }
            // CLI role opens its first (control) stream as even ID 2. Visitor
            // streams are daemon-opened odd IDs and are never inspected here.
            if data && stream_id == 2 {
                self.control.extend_from_slice(&self.wire[12..12 + length]);
            }
            self.wire.drain(..12 + length);
        }
        while self.control.len() >= 4 {
            let length = u32::from_be_bytes(self.control[..4].try_into().unwrap()) as usize;
            assert!(length <= 16 * 1024);
            if self.control.len() < 4 + length {
                break;
            }
            let message: ControlMsg = serde_json::from_slice(&self.control[4..4 + length])
                .unwrap_or_else(|_| panic!("invalid control schema at test edge"));
            messages.push(Lifecycle::from_control(message));
            self.control.drain(..4 + length);
        }
        messages
    }
}

async fn read_upgrade<R: tokio::io::AsyncRead + Unpin>(io: &mut R) -> zeroize::Zeroizing<Vec<u8>> {
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    while !bytes.ends_with(b"\r\n\r\n") {
        bytes.push(io.read_u8().await.unwrap());
        assert!(bytes.len() <= 32768);
    }
    bytes
}

async fn proxy_connection(
    mut tls: tokio_rustls::server::TlsStream<TcpStream>,
    daemon: SocketAddr,
    connection: usize,
    events: mpsc::UnboundedSender<EdgeEvent>,
) {
    let request = read_upgrade(&mut tls).await;
    let (proceed, wait) = oneshot::channel();
    let (cut, cut_rx) = oneshot::channel();
    events
        .send(EdgeEvent::Ready {
            connection,
            proceed,
            cut,
        })
        .unwrap();
    if wait.await.is_err() {
        return;
    }
    let mut upstream = TcpStream::connect(daemon).await.unwrap();
    // Forward the original authenticated offer and the daemon's actual echo.
    upstream.write_all(&request).await.unwrap();
    let response = read_upgrade(&mut upstream).await;
    assert!(response.starts_with(b"HTTP/1.1 101"));
    tls.write_all(&response).await.unwrap();
    let mut client = async_tungstenite::WebSocketStream::from_raw_socket(
        async_tungstenite::tokio::TokioAdapter::new(tls),
        async_tungstenite::tungstenite::protocol::Role::Server,
        None,
    )
    .await;
    let mut server = async_tungstenite::WebSocketStream::from_raw_socket(
        async_tungstenite::tokio::TokioAdapter::new(upstream),
        async_tungstenite::tungstenite::protocol::Role::Client,
        None,
    )
    .await;
    let mut client_frames = ControlFrames::default();
    let mut server_frames = ControlFrames::default();
    tokio::pin!(cut_rx);
    loop {
        tokio::select! {
            _ = &mut cut_rx => break,
            message = client.next() => {
                let Some(Ok(message)) = message else { break; };
                if let async_tungstenite::tungstenite::Message::Binary(bytes) = &message {
                    for lifecycle in client_frames.receive(bytes) {
                        let (forward, decision) = oneshot::channel();
                        if events.send(EdgeEvent::Client { connection, lifecycle, forward }).is_err() { return; }
                        if !decision.await.unwrap_or(false) { return; }
                    }
                }
                if server.send(message).await.is_err() { break; }
            }
            message = server.next() => {
                let Some(Ok(message)) = message else { break; };
                if let async_tungstenite::tungstenite::Message::Binary(bytes) = &message {
                    for lifecycle in server_frames.receive(bytes) {
                        if events.send(EdgeEvent::Server { connection, lifecycle }).is_err() { return; }
                    }
                }
                if client.send(message).await.is_err() { break; }
            }
        }
    }
}

async fn recording_edge(fixture: &mut Fixture) -> mpsc::UnboundedReceiver<EdgeEvent> {
    fixture.edge_task.abort();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    fixture.endpoint = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    let daemon = fixture.server.local_addr;
    let acceptor = fixture.tls_acceptor.clone();
    let (events, receiver) = mpsc::unbounded_channel();
    fixture.edge_task = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        let mut connection = 0;
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            connection += 1;
            let tls = acceptor.accept(tcp).await.unwrap();
            tasks.spawn(proxy_connection(tls, daemon, connection, events.clone()));
        }
    });
    receiver
}

async fn event(events: &mut mpsc::UnboundedReceiver<EdgeEvent>) -> EdgeEvent {
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn ready(
    events: &mut mpsc::UnboundedReceiver<EdgeEvent>,
    expected: usize,
) -> (oneshot::Sender<()>, oneshot::Sender<()>) {
    let EdgeEvent::Ready {
        connection,
        proceed,
        cut,
    } = event(events).await
    else {
        panic!("expected TLS edge readiness");
    };
    assert_eq!(connection, expected);
    (proceed, cut)
}

async fn client_message(
    events: &mut mpsc::UnboundedReceiver<EdgeEvent>,
    expected: usize,
    lifecycle: Lifecycle,
    forward: bool,
) {
    let EdgeEvent::Client {
        connection,
        lifecycle: actual,
        forward: decision,
    } = event(events).await
    else {
        panic!("expected client lifecycle message");
    };
    assert_eq!(connection, expected);
    assert_eq!(actual, lifecycle);
    decision.send(forward).unwrap();
}

async fn server_message(
    events: &mut mpsc::UnboundedReceiver<EdgeEvent>,
    expected: usize,
) -> Lifecycle {
    let EdgeEvent::Server {
        connection,
        lifecycle,
    } = event(events).await
    else {
        panic!("expected real daemon lifecycle response");
    };
    assert_eq!(connection, expected);
    lifecycle
}

async fn disconnected(fixture: &Fixture, session_id: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.state.session_handles.contains_key(session_id)
            || fixture
                .state
                .registry
                .snapshot_infos()
                .iter()
                .any(|tunnel| tunnel.active)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[allow(clippy::too_many_lines)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_shared_run_retains_policy_name_and_generation_through_network_loss_and_failed_recreation(
) {
    let mut fixture = fixture().await;
    let mut events = recording_edge(&mut fixture).await;
    let options = options(true);
    let original_policy = options.access.as_ref().unwrap().clone();
    let fingerprint = original_policy.fingerprint().unwrap();
    let run_options = options.clone();
    assert!(Arc::ptr_eq(
        &original_policy,
        run_options.access.as_ref().unwrap()
    ));
    let endpoint = fixture.endpoint.clone();
    let backend = fixture.backend;
    let connector = fixture.connector.clone();
    let history = crate::inspection::Inspection::new(crate::inspection::PreviewMode::Off);
    let run_history = history.clone();
    let run = tokio::spawn(TEST_TLS_CONNECTOR.scope(connector, async move {
        crate::reconnect::run_with_options(
            &endpoint,
            "tnl_TEST",
            None,
            crate::target::Target::Explicit(backend),
            crate::host_header::HostHeader::Preserve,
            &run_options,
            crate::reconnect::Hooks {
                inspection: Some(run_history),
                ..crate::reconnect::Hooks::default()
            },
        )
        .await
    }));
    let (proceed, cut_first) = ready(&mut events, 1).await;
    proceed.send(()).unwrap();
    client_message(
        &mut events,
        1,
        Lifecycle::Create {
            subdomain: None,
            fingerprint: fingerprint.clone(),
        },
        true,
    )
    .await;
    let Lifecycle::Created {
        tunnel_id: first_id,
        subdomain: assigned,
        fingerprint: ack,
    } = server_message(&mut events, 1).await
    else {
        panic!("expected initial protected ACK");
    };
    assert_eq!(ack, fingerprint);
    let first = fixture.state.registry.find_by_subdomain(&assigned).unwrap();
    assert_eq!(first.id, first_id.to_string());
    let first_access = first.access.as_ref().unwrap().clone();
    let first_session = fixture
        .state
        .registry
        .current_session_id(&first.id)
        .unwrap();
    let initial_record = observed_request(&mut fixture, &assigned, &history).await;
    assert_eq!(initial_record.tunnel_id, Some(first_id));
    assert_eq!(initial_record.tunnel, assigned);
    cut_first.send(()).unwrap();

    let (proceed, cut_second) = ready(&mut events, 2).await;
    disconnected(&fixture, &first_session).await;
    proceed.send(()).unwrap();
    client_message(
        &mut events,
        2,
        Lifecycle::Reattach {
            tunnel_id: first_id,
            subdomain: assigned.clone(),
            fingerprint: fingerprint.clone(),
        },
        true,
    )
    .await;
    assert_eq!(
        server_message(&mut events, 2).await,
        Lifecycle::Created {
            tunnel_id: first_id,
            subdomain: assigned.clone(),
            fingerprint: fingerprint.clone()
        }
    );
    let reattached = fixture.state.registry.find_by_subdomain(&assigned).unwrap();
    assert!(Arc::ptr_eq(&first, &reattached));
    assert!(Arc::ptr_eq(
        &first_access,
        reattached.access.as_ref().unwrap()
    ));
    let second_session = fixture
        .state
        .registry
        .current_session_id(&first.id)
        .unwrap();
    assert_ne!(second_session, first_session);
    let reattached_record = observed_request(&mut fixture, &assigned, &history).await;
    assert_eq!(initial_record.generation, reattached_record.generation);
    assert_eq!(reattached_record.tunnel_id, Some(first_id));
    assert_eq!(reattached_record.tunnel, assigned);
    assert_ne!(initial_record.request_id, reattached_record.request_id);
    cut_second.send(()).unwrap();

    let (proceed, _cut_third) = ready(&mut events, 3).await;
    disconnected(&fixture, &second_session).await;
    // Use the real existing expiry interface, not sleeps or production flags.
    fixture.state.registry.gc_disconnected(Duration::ZERO);
    assert!(fixture
        .state
        .registry
        .find_by_subdomain(&assigned)
        .is_none());
    proceed.send(()).unwrap();
    client_message(
        &mut events,
        3,
        Lifecycle::Reattach {
            tunnel_id: first_id,
            subdomain: assigned.clone(),
            fingerprint: fingerprint.clone(),
        },
        true,
    )
    .await;
    assert_eq!(
        server_message(&mut events, 3).await,
        Lifecycle::Error(tnl_protocol::ErrorCode::TunnelLost)
    );

    let (proceed, _cut_fourth) = ready(&mut events, 4).await;
    proceed.send(()).unwrap();
    // A genuine post-upgrade socket loss interrupts the first recreation
    // before its policy reaches the daemon, exercising the IO retry branch.
    client_message(
        &mut events,
        4,
        Lifecycle::Create {
            subdomain: Some(assigned.clone()),
            fingerprint: fingerprint.clone(),
        },
        false,
    )
    .await;
    let (proceed, _cut_fifth) = ready(&mut events, 5).await;
    assert!(fixture
        .state
        .registry
        .find_by_subdomain(&assigned)
        .is_none());
    proceed.send(()).unwrap();
    client_message(
        &mut events,
        5,
        Lifecycle::Create {
            subdomain: Some(assigned.clone()),
            fingerprint: fingerprint.clone(),
        },
        true,
    )
    .await;
    let Lifecycle::Created {
        tunnel_id: recreated_id,
        subdomain: recreated_subdomain,
        fingerprint: ack,
    } = server_message(&mut events, 5).await
    else {
        panic!("expected recreated protected ACK");
    };
    assert_ne!(
        recreated_id, first_id,
        "recreation must use a new tunnel generation"
    );
    assert_eq!(recreated_subdomain, assigned);
    assert_eq!(ack, fingerprint);
    let recreated = fixture.state.registry.find_by_subdomain(&assigned).unwrap();
    assert_eq!(recreated.id, recreated_id.to_string());
    assert!(!Arc::ptr_eq(&first, &recreated));
    assert!(!Arc::ptr_eq(
        &first_access,
        recreated.access.as_ref().unwrap()
    ));
    assert_eq!(
        recreated.access.as_ref().unwrap().fingerprint(),
        fingerprint
    );
    assert!(Arc::ptr_eq(
        &original_policy,
        options.access.as_ref().unwrap()
    ));
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        reqwest::Client::new()
            .get(format!(
                "http://{}/after-recreate",
                fixture.server.local_addr
            ))
            .header("Host", format!("{assigned}.t.x"))
            .basic_auth("user", Some("VISITOR_SENTINEL"))
            .send(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status(), 200);
    let request_id = response.headers()["x-tnl-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(response.text().await.unwrap(), "ok");
    let backend_head = fixture.seen.recv().await.unwrap().to_ascii_lowercase();
    assert!(!backend_head.contains("authorization:"));
    assert!(!backend_head.contains("visitor_sentinel"));
    assert!(backend_head.contains(&request_id.to_ascii_lowercase()));
    let recreated_record = captured_record(&history, &request_id).await;
    assert_ne!(initial_record.generation, recreated_record.generation);
    assert_eq!(recreated_record.tunnel_id, Some(recreated_id));
    assert_eq!(recreated_record.tunnel, assigned);
    assert!(!run.is_finished());
    assert!(
        events.try_recv().is_err(),
        "unexpected/public lifecycle message"
    );
    run.abort();
    let _ = run.await;
    drop(fixture);
}

async fn captured_record(
    history: &crate::inspection::Inspection,
    request_id: &str,
) -> crate::inspection::InspectionRecord {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(record) = history.search("").into_iter().find(|record| {
                record
                    .request_id
                    .is_some_and(|id| id.to_string() == request_id)
                    && record.state != crate::inspection::State::Active
            }) {
                assert_eq!(record.visitor_source, Some("peer"));
                return record;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

async fn observed_request(
    fixture: &mut Fixture,
    assigned: &str,
    history: &crate::inspection::Inspection,
) -> crate::inspection::InspectionRecord {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .get(format!("http://{}/observed", fixture.server.local_addr))
        .header("host", format!("{assigned}.t.x"))
        .basic_auth("user", Some("VISITOR_SENTINEL"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let request_id = response.headers()["x-tnl-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(response.text().await.unwrap(), "ok");
    let backend = fixture.seen.recv().await.unwrap().to_ascii_lowercase();
    assert!(backend.contains(&request_id.to_ascii_lowercase()));
    assert!(!backend.contains("authorization:"));
    captured_record(history, &request_id).await
}
