//! End-to-end: with an `mpsc::Sender` threaded through `run_accept_loop`, the
//! forwarder emits exactly one `LogLine` per data-plane request.

use std::time::Duration;

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn inspector_emits_one_log_per_request() {
    // Backend.
    let backend = axum::Router::new().route("/api/ping", axum::routing::get(|| async { "pong" }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(l, backend).await.unwrap();
    });

    // Daemon.
    let tf = TokensFile {
        tokens: vec![TokenEntry {
            name: "u".into(),
            hash: hash_plaintext("tnl_U").unwrap(),
        }],
    };
    let tmp = tempfile::NamedTempFile::new().unwrap();
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

    // Client.
    let session =
        tnl::client::connect_and_create(&format!("http://{daemon_addr}"), "tnl_U", "demo")
            .await
            .unwrap();
    let _ctrl_keep = session.control;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<tnl::inspector::LogLine>(8);
    let _accept = tokio::spawn(tnl::client::run_accept_loop(
        session.session,
        backend_port,
        Some(tx),
    ));
    tokio::time::sleep(Duration::from_millis(150)).await;

    // One request.
    let _ = reqwest::Client::new()
        .get(format!("http://{daemon_addr}/api/ping"))
        .header("Host", "demo.t.x")
        .send()
        .await
        .unwrap();

    let log = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("LogLine should arrive within 2s")
        .expect("channel not closed");
    assert_eq!(log.method, "GET");
    assert_eq!(log.path, "/api/ping");
    assert_eq!(log.status, Some(200));
}
