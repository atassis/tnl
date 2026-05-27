//! End-to-end: create a tunnel via the client, list it via GET /tunnels,
//! close it via DELETE /tunnels/{sub}, verify subsequent list shows no
//! active tunnel.

use std::time::Duration;

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_and_stop_round_trip() {
    // Local backend.
    let backend = axum::Router::new().route("/", axum::routing::get(|| async { "x" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, backend).await.unwrap();
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
    let daemon_handle = spawn_server(cfg).await.unwrap();
    let daemon_addr = daemon_handle.local_addr.to_string();

    // Open a tunnel "demo".
    let session =
        tnl::client::connect_and_create(&format!("http://{daemon_addr}"), "tnl_U", "demo")
            .await
            .unwrap();
    let _ctrl_keep = session.control;
    let _accept = tokio::spawn(tnl::client::run_accept_loop(session.session, backend_port));
    tokio::time::sleep(Duration::from_millis(150)).await;

    // GET /tunnels — exactly one active tunnel named "demo".
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{daemon_addr}/tunnels"))
        .bearer_auth("tnl_U")
        .send()
        .await
        .unwrap();
    let tunnels: Vec<tnl_protocol::messages::TunnelInfo> = resp.json().await.unwrap();
    assert_eq!(
        tunnels.len(),
        1,
        "expected exactly one tunnel, got {tunnels:?}"
    );
    assert_eq!(tunnels[0].subdomain, "demo");
    assert!(
        tunnels[0].active,
        "tunnel should be active immediately after create"
    );

    // DELETE /tunnels/demo — 204.
    let del_resp = client
        .delete(format!("http://{daemon_addr}/tunnels/demo"))
        .bearer_auth("tnl_U")
        .send()
        .await
        .unwrap();
    assert_eq!(del_resp.status().as_u16(), 204);

    // Give the server a beat to fully drop the tunnel from registry maps.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Subsequent GET — no entries at all (close_by_subdomain removes the tunnel).
    let resp2 = client
        .get(format!("http://{daemon_addr}/tunnels"))
        .bearer_auth("tnl_U")
        .send()
        .await
        .unwrap();
    let tunnels2: Vec<tnl_protocol::messages::TunnelInfo> = resp2.json().await.unwrap();
    assert_eq!(
        tunnels2.len(),
        0,
        "tunnel should be gone from registry after DELETE"
    );
}
