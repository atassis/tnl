//! Client survives a forced disconnect within the grace window and resumes
//! handling requests against the same subdomain via `ReattachTunnel`.

use std::time::Duration;

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;

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
        session_grace_sec: 30,
    };
    let h = spawn_server(cfg).await.unwrap();
    let daemon_addr = h.local_addr.to_string();

    // 3. Start the reconnect loop with a fixed subdomain "foo".
    //    Pass a cancel oneshot so we can force the first session to drop.
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let endpoint = format!("http://{daemon_addr}");
    let task = tokio::spawn(async move {
        tnl::reconnect::run(
            &endpoint,
            "tnl_U",
            Some("foo"),
            tnl::target::Target::LocalhostPort(backend_port),
            tnl::host_header::HostHeader::Auto,
            tnl::reconnect::Hooks {
                cancel_first_session: Some(cancel_rx),
                log_tx: None,
            },
        )
        .await
    });

    // Give the client time to connect and the daemon to register the tunnel.
    tokio::time::sleep(Duration::from_millis(400)).await;

    // 4. First request: must succeed via the active tunnel.
    let c = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
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

    // 5. Force a disconnect; the reconnect loop should attempt ReattachTunnel.
    cancel_tx.send(()).unwrap();

    // 6. Give the reconnect loop time to: detect the disconnect, wait for the
    //    1-second backoff, reattach, and start accepting again.
    tokio::time::sleep(Duration::from_millis(2_500)).await;

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

    task.abort();
}
