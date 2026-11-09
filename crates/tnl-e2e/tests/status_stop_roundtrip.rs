//! Real admin stop through the daemon heartbeat and the CLI's sole control
//! owner. An undelivered stop may recreate; a delivered stop ends the whole run.
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use tnl::inspection::{GenerationState, Inspection, PreviewMode};
use tnld::{
    auth::{hash_plaintext, TokenEntry, TokensFile},
    config::Config,
    serve::{spawn_server_with_state, AppState},
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

async fn binding(state: &AppState, previous_id: Option<&str>) -> (String, String) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(tunnel) = state.registry.find_by_subdomain("demo") {
                if Some(tunnel.id.as_str()) != previous_id {
                    if let Some(session) = state.registry.current_session_id(&tunnel.id) {
                        if state.session_handles.contains_key(&session) {
                            return (tunnel.id.clone(), session);
                        }
                    }
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("client failed to register/recreate")
}
async fn stop(client: &reqwest::Client, endpoint: &str) {
    assert_eq!(
        client
            .delete(format!("{endpoint}/tunnels/demo"))
            .bearer_auth("tnl_U")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
}
async fn infos(
    client: &reqwest::Client,
    endpoint: &str,
) -> Vec<tnl_protocol::messages::TunnelInfo> {
    client
        .get(format!("{endpoint}/tunnels"))
        .bearer_auth("tnl_U")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
async fn ticket(
    replay: &tnl::replay::ReplayController,
    capture: tnl::inspection::CaptureId,
) -> tnl::replay::ReplayPreview {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match replay.prepare(capture) {
                Ok(ticket) => return ticket,
                Err(tnl::replay::ReplayError::Incomplete) => tokio::task::yield_now().await,
                Err(error) => panic!("expected replay authority: {error}"),
            }
        }
    })
    .await
    .unwrap()
}
async fn records(history: &Inspection, count: usize) -> Vec<tnl::inspection::InspectionRecord> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let rows = history.search("");
            if rows.len() == count && rows.iter().all(|row| row.status == Some(200)) {
                return rows;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[allow(clippy::too_many_lines)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_and_stop_round_trip() {
    // Hold two response bodies open, one in each physical/routing generation.
    // Inspection Finished, not backend upload EOF, proves forward cancellation.
    let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_port = backend.local_addr().unwrap().port();
    let executions = Arc::new(AtomicUsize::new(0));
    let count = executions.clone();
    let (entered_tx, mut entered) = tokio::sync::mpsc::channel(4);
    let backend_task = tokio::spawn(async move {
        // Retaining the socket owners deliberately holds both response bodies.
        #[allow(clippy::collection_is_never_read)]
        let mut held_responses = Vec::new();
        loop {
            let (mut socket, _) = backend.accept().await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(socket.read_u8().await.unwrap());
            }
            count.fetch_add(1, Ordering::SeqCst);
            if head.starts_with(b"GET /history ") || head.starts_with(b"GET /current-history ") {
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 1\r\n\r\nx",
                    )
                    .await
                    .unwrap();
            } else {
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx\r\n")
                    .await
                    .unwrap();
                held_responses.push(socket);
            }
            entered_tx.send(()).await.unwrap();
        }
    });
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        tmp.path(),
        toml::to_string(&TokensFile {
            tokens: vec![TokenEntry {
                name: "u".into(),
                hash: hash_plaintext("tnl_U").unwrap(),
            }],
        })
        .unwrap(),
    )
    .unwrap();
    let (server, state) = spawn_server_with_state(Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://x".into(),
        hostname_root: "t.x".into(),
        tokens_file: tmp.path().to_str().unwrap().into(),
        trusted_proxies: vec![],
        session_grace_sec: 30,
    })
    .await
    .unwrap();
    let endpoint = format!("http://{}", server.local_addr);
    let run_endpoint = endpoint.clone();
    let inspector = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let replay =
        tnl::replay::ReplayController::new(true, inspector.local_addr().unwrap().port()).unwrap();
    let history = Inspection::with_replay(PreviewMode::Off, replay.clone());
    let run_history = history.clone();
    let (disconnect, disconnected) = tokio::sync::oneshot::channel();
    let mut run = tokio::spawn(async move {
        tnl::reconnect::run(
            &run_endpoint,
            "tnl_U",
            Some("demo"),
            tnl::target::Target::LocalhostPort(backend_port),
            tnl::host_header::HostHeader::Preserve,
            tnl::reconnect::Hooks {
                inspection: Some(run_history),
                cancel_first_session: Some(disconnected),
                ..Default::default()
            },
        )
        .await
    });
    let (first_id, _) = binding(&state, None).await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .build()
        .unwrap();
    let listed = infos(&client, &endpoint).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].subdomain, "demo");
    assert!(listed[0].active);
    assert_eq!(
        client
            .get(format!("{endpoint}/history"))
            .header("Host", "demo.t.x")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "x"
    );
    entered.recv().await.unwrap();
    let completed = records(&history, 1).await;
    let first_ticket = ticket(&replay, completed[0].capture_id).await;
    let first = client
        .get(format!("{endpoint}/first"))
        .header("Host", "demo.t.x")
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    entered.recv().await.unwrap();
    let initial = records(&history, 2).await;
    assert_eq!(
        initial
            .iter()
            .filter(|row| row.state == tnl::inspection::State::Active)
            .count(),
        1
    );

    // Honest allowed race: delete, then lose control BEFORE the next heartbeat
    // can deliver RemoteStop. The retained settings may recreate this name.
    stop(&client, &endpoint).await;
    assert!(infos(&client, &endpoint).await.is_empty());
    disconnect.send(()).unwrap();
    let (second_id, second_session) = binding(&state, Some(&first_id)).await;
    assert_ne!(first_id, second_id, "undelivered stop is not a durable ban");
    assert_eq!(
        client
            .get(format!("{endpoint}/current-history"))
            .header("Host", "demo.t.x")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "x"
    );
    entered.recv().await.unwrap();
    let completed = records(&history, 3).await;
    let current = completed
        .iter()
        .find(|row| row.request_target == "/current-history")
        .unwrap();
    let second_ticket = ticket(&replay, current.capture_id).await;
    let second = client
        .get(format!("{endpoint}/second"))
        .header("Host", "demo.t.x")
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::OK);
    entered.recv().await.unwrap();
    let before = records(&history, 4).await;
    assert_eq!(
        before
            .iter()
            .filter(|row| row.state == tnl::inspection::State::Active)
            .count(),
        2,
        "ordinary reconnect cancelled a held forward"
    );
    assert_ne!(
        before
            .iter()
            .find(|row| row.request_target == "/first")
            .unwrap()
            .generation,
        before
            .iter()
            .find(|row| row.request_target == "/second")
            .unwrap()
            .generation
    );
    assert!(before
        .iter()
        .any(|row| row.connection == GenerationState::Superseded));
    assert!(before
        .iter()
        .any(|row| row.connection == GenerationState::Connected));

    assert!(
        replay
            .confirm(
                first_ticket.capture_id,
                first_ticket.replay_id,
                &first_ticket.confirmation_token,
                true
            )
            .is_err(),
        "recreation restored an old-generation replay ticket"
    );

    // This stop is delivered by the real daemon on the next 15s heartbeat.
    stop(&client, &endpoint).await;
    tokio::time::timeout(Duration::from_secs(20), &mut run)
        .await
        .expect("connected CLI did not terminate on real admin stop")
        .unwrap()
        .unwrap();
    assert!(
        infos(&client, &endpoint).await.is_empty(),
        "delivered stop recreated the tunnel"
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while state.session_handles.contains_key(&second_session) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("daemon did not remove the stopped session handle after peer EOF");
    let stopped = history.search("");
    assert_eq!(stopped.len(), 4, "retain local history across stop");
    for row in &stopped {
        assert!(matches!(
            replay.prepare(row.capture_id),
            Err(tnl::replay::ReplayError::TunnelUnavailable)
        ));
    }
    assert!(
        replay
            .confirm(
                second_ticket.capture_id,
                second_ticket.replay_id,
                &second_ticket.confirmation_token,
                true
            )
            .is_err(),
        "remote stop left a replay execution ticket usable"
    );
    assert!(
        stopped
            .iter()
            .all(|row| row.state != tnl::inspection::State::Active
                && matches!(
                    row.connection,
                    GenerationState::Disconnected | GenerationState::Superseded
                )),
        "remote stop failed to revoke/cancel both generations: {stopped:?}"
    );
    assert_eq!(
        stopped
            .iter()
            .filter(|row| row.failure_kind == Some("cancelled"))
            .count(),
        2
    );
    assert_eq!(
        stopped
            .iter()
            .filter(|row| row.state == tnl::inspection::State::Complete)
            .count(),
        2
    );
    assert_eq!(
        client
            .get(format!("{endpoint}/after-stop"))
            .header("Host", "demo.t.x")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    assert_eq!(
        executions.load(Ordering::SeqCst),
        4,
        "backend executed after terminal stop"
    );

    // Names remain reusable. Joined old run cannot create/reattach or remove a
    // subsequent owner, and old inspection rows do not gain its authority.
    let reused = tnl::client::connect_and_create(&endpoint, "tnl_U", "demo")
        .await
        .unwrap();
    assert_ne!(reused.tunnel_id.to_string(), second_id);
    assert_eq!(
        state.registry.find_by_subdomain("demo").unwrap().id,
        reused.tunnel_id.to_string()
    );
    assert!(history.search("").iter().all(|row| matches!(
        row.connection,
        GenerationState::Disconnected | GenerationState::Superseded
    )));
    assert!(matches!(
        replay.prepare(current.capture_id),
        Err(tnl::replay::ReplayError::TunnelUnavailable)
    ));
    assert_eq!(executions.load(Ordering::SeqCst), 4);
    drop((first, second, reused));
    backend_task.abort();
    server.join.abort();
}
