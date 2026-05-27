//! End-to-end: client calls `connect_and_create_random`, server assigns
//! adj-noun-N, an HTTP request via the assigned hostname reaches the
//! local backend.

use std::time::Duration;

use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config as TnldConfig;
use tnld::serve::spawn_server;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn server_picks_subdomain_when_client_omits() {
    // Local backend on 127.0.0.1:<port> that always returns "ok".
    let backend = axum::Router::new().route("/", axum::routing::get(|| async { "ok" }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(l, backend).await.unwrap();
    });

    // tnld with a single token "tnl_RND".
    let tf = TokensFile {
        tokens: vec![TokenEntry {
            name: "rnd".into(),
            hash: hash_plaintext("tnl_RND").unwrap(),
        }],
    };
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), toml::to_string(&tf).unwrap()).unwrap();
    let cfg = TnldConfig {
        listen: "127.0.0.1:0".into(),
        public_url: "http://test".into(),
        hostname_root: "t.example.com".into(),
        tokens_file: tmp.path().to_string_lossy().into_owned(),
        session_grace_sec: 30,
    };
    let h = spawn_server(cfg).await.unwrap();
    let daemon_addr = h.local_addr.to_string();

    // Client dials with subdomain = None → server picks.
    let session =
        tnl::client::connect_and_create_random(&format!("http://{daemon_addr}"), "tnl_RND")
            .await
            .unwrap();
    assert!(
        session.hostname.ends_with(".t.example.com"),
        "got: {}",
        session.hostname
    );
    assert!(
        session.subdomain.split('-').count() >= 3,
        "expected adj-noun-N, got {}",
        session.subdomain
    );

    let hostname = session.hostname.clone();
    let session_box = session.session;
    let _ctrl_keep = session.control;
    let _accept = tokio::spawn(tnl::client::run_accept_loop(
        session_box,
        backend_port,
        None,
    ));
    tokio::time::sleep(Duration::from_millis(250)).await;

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();
    let r = client
        .get(format!("http://{daemon_addr}/"))
        .header("Host", &hostname)
        .header("Connection", "close")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
}
