//! Client survives a forced disconnect within the grace window and resumes
//! handling requests against the same subdomain via `ReattachTunnel`.

use std::time::Duration;

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::{spawn_server_with_state, AppState};

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn client_reattaches_within_grace_window() {
    // 1. Local backend that returns 200 "ok".
    let backend = axum::Router::new().route("/", axum::routing::get(|| async { "ok" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, backend).await.unwrap();
    });

    // 2. Daemon with a 30-second grace window.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let tf = TokensFile {
        tokens: vec![TokenEntry {
            name: "u".into(),
            hash: hash_plaintext("tnl_U").unwrap(),
        }],
    };
    std::fs::write(tmp.path(), toml::to_string(&tf).unwrap()).unwrap();
    let cfg = TnldConfig {
        listen: "127.0.0.1:0".into(),
        public_url: "http://x".into(),
        hostname_root: "t.x".into(),
        tokens_file: tmp.path().to_string_lossy().into_owned(),
        trusted_proxies: vec![],
        session_grace_sec: 30,
    };
    let (h, state) = spawn_server_with_state(cfg).await.unwrap();
    let daemon_addr = h.local_addr.to_string();

    // 3. Start the reconnect loop with a fixed subdomain "foo".
    //    Pass a cancel oneshot so we can force the first session to drop.
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let endpoint = format!("http://{daemon_addr}");
    let (start_tx, start_rx) = tokio::sync::oneshot::channel::<()>();
    let history = tnl::inspection::Inspection::new(tnl::inspection::PreviewMode::Off);
    let capture_history = history.clone();
    let mut task = tokio::spawn(async move {
        start_rx.await?;
        tnl::reconnect::run(
            &endpoint,
            "tnl_U",
            Some("foo"),
            tnl::target::Target::LocalhostPort(backend_port),
            tnl::host_header::HostHeader::Auto,
            tnl::reconnect::Hooks {
                inspection: Some(capture_history),
                cancel_first_session: Some(cancel_rx),
                log_tx: None,
            },
        )
        .await
    });

    // Deterministically hold startup beyond the old 400ms sleep: readiness must
    // remain pending until a tunnel actually exists, regardless of elapsed time.
    let (tunnel_id, first_session_id) = {
        let ready = wait_for_tunnel_session(&state, None, &mut task);
        tokio::pin!(ready);
        assert!(
            tokio::time::timeout(Duration::from_millis(500), &mut ready)
                .await
                .is_err(),
            "readiness completed before the client was allowed to connect"
        );
        start_tx.send(()).unwrap();
        ready.await
    };

    // 4. First request: must succeed via the active tunnel.
    let c = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let r = c
        .get(format!("http://{daemon_addr}/"))
        .header("Host", "foo.t.x")
        .header("Connection", "close")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200, "first request failed");
    assert_eq!(r.text().await.unwrap(), "ok");

    // 5. Force a disconnect; the reconnect loop should attempt ReattachTunnel.
    cancel_tx.send(()).unwrap();

    // 6. Observe a new session bound to the original tunnel, not merely a
    //    still-active first session or a recreated tunnel with the same hostname.
    let (reattached_tunnel_id, _) =
        wait_for_tunnel_session(&state, Some(&first_session_id), &mut task).await;
    assert_eq!(reattached_tunnel_id, tunnel_id, "tunnel was recreated");
    assert!(
        !state.session_handles.contains_key(&first_session_id),
        "first session was not cleaned up"
    );

    // 7. Second request after reattach: same hostname, still works.
    let r = c
        .get(format!("http://{daemon_addr}/"))
        .header("Host", "foo.t.x")
        .header("Connection", "close")
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status().as_u16(),
        200,
        "second request failed after reattach"
    );

    assert_eq!(r.text().await.unwrap(), "ok");

    assert_inspection_reattach(&history).await;

    task.abort();
}

/// Observe daemon-side routing readiness without issuing/retrying HTTP requests.
/// A changed binding proves reattach; waiting for an intermediate Disconnected
/// snapshot could miss that transient state entirely.
async fn wait_for_tunnel_session(
    state: &AppState,
    previous_session: Option<&str>,
    task: &mut tokio::task::JoinHandle<anyhow::Result<()>>,
) -> (String, String) {
    let phase = if previous_session.is_some() {
        "reattachment to a new session"
    } else {
        "initial tunnel registration"
    };
    tokio::select! {
        result = &mut *task => panic!("reconnect task ended during {phase}: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(tunnel) = state.registry.find_by_subdomain("foo") {
                    if let Some(session_id) = state.registry.current_session_id(&tunnel.id) {
                        if Some(session_id.as_str()) != previous_session
                            && state.session_handles.contains_key(&session_id)
                        {
                            return (tunnel.id.clone(), session_id);
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }) => result.unwrap_or_else(|_| {
            task.abort();
            panic!(
                "timed out waiting for {phase}; previous_session={previous_session:?}, \
                 current_session={:?}, tunnels={:?}, session_handles={:?}",
                state.registry.find_by_subdomain("foo")
                    .and_then(|t| state.registry.current_session_id(&t.id)),
                state.registry.snapshot_infos(),
                state.session_handles.iter().map(|entry| entry.key().clone()).collect::<Vec<_>>(),
            )
        }),
    }
}

async fn assert_inspection_reattach(history: &tnl::inspection::Inspection) {
    let records = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let records = history.search("");
            if records.len() == 2
                && records
                    .iter()
                    .all(|r| r.state != tnl::inspection::State::Active)
            {
                break records;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        records[0].generation, records[1].generation,
        "reattach changed generation"
    );
    assert_ne!(records[0].capture_id, records[1].capture_id);
    assert!(records
        .iter()
        .all(|r| r.connection == tnl::inspection::GenerationState::Connected));
    assert!(records.iter().all(|r| r.request_id.is_some()));
}
