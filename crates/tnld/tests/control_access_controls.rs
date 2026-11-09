//! Actual protected daemon control and no-open visitor denial coverage.
use tnl_protocol::{ControlMsg, Session as _};
use tnld::{
    auth::{hash_plaintext, TokenEntry, TokensFile},
    config::Config,
    serve::spawn_server_with_state,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

#[allow(clippy::too_many_lines)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protected_control_exact_ack_and_basic_denial_before_any_stream_open() {
    let tokens = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        tokens.path(),
        toml::to_string(&TokensFile {
            tokens: vec![TokenEntry {
                name: "user".into(),
                hash: hash_plaintext("tnl_TEST").unwrap(),
            }],
        })
        .unwrap(),
    )
    .unwrap();
    let (server, state) = spawn_server_with_state(Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://test".into(),
        hostname_root: "t.x".into(),
        tokens_file: tokens.path().to_str().unwrap().into(),
        trusted_proxies: vec![],
        session_grace_sec: 30,
    })
    .await
    .unwrap();
    let request = http::Request::builder()
        .uri(format!("ws://{}/control", server.local_addr))
        .header("Authorization", "Bearer tnl_TEST")
        .header("Host", server.local_addr.to_string())
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header(
            "Sec-WebSocket-Key",
            async_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .header(
            tnl_protocol::control::CONTROL_PROTOCOL_HEADER,
            tnl_protocol::control::CONTROL_PROTOCOL_VERSION,
        )
        .header("Sec-WebSocket-Version", "13")
        .header("x-tnl-data-stream-protocol", "http1-metadata-v1")
        .body(())
        .unwrap();
    let (ws, response) = async_tungstenite::tokio::connect_async(request)
        .await
        .unwrap();
    assert_eq!(
        response.headers()["x-tnl-data-stream-protocol"],
        "http1-metadata-v1"
    );
    let mut session = tnl_protocol::transport::client_session_from_ws_generic(ws);
    let mut ctrl = session.open_stream().await.unwrap();
    let hash = hash_plaintext("VISITOR_SENTINEL").unwrap();
    let input = format!(
        r#"{{"type":"CreateProtectedTunnel","subdomain":"guarded","access":{{"version":1,"basic":{{"username":"user","password_hash":"{hash}"}},"allow_ips":[],"rate_limit":null,"max_concurrent":1}}}}"#
    );
    ctrl.write_u32(u32::try_from(input.len()).unwrap())
        .await
        .unwrap();
    ctrl.write_all(input.as_bytes()).await.unwrap();
    ctrl.flush().await.unwrap();
    let n = ctrl.read_u32().await.unwrap();
    let mut bytes = vec![0; n as usize];
    ctrl.read_exact(&mut bytes).await.unwrap();
    let msg: ControlMsg = serde_json::from_slice(&bytes).unwrap();
    assert!(
        matches!(msg, ControlMsg::ProtectedTunnelCreated(_)),
        "protected create must ACK exactly"
    );
    assert!(state.registry.find_by_subdomain("guarded").is_some());
    for method in [
        reqwest::Method::GET,
        reqwest::Method::POST,
        reqwest::Method::HEAD,
    ] {
        let response = reqwest::Client::new()
            .request(
                method.clone(),
                format!("http://{}/private?SECRET_QUERY", server.local_addr),
            )
            .header("Host", "guarded.t.x")
            .header("Accept", "application/json")
            .header("Authorization", "Bearer APP_SENTINEL")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()["x-tnl-component"], "access");
        assert_eq!(
            response.headers()["www-authenticate"],
            "Basic realm=\"tnl\", charset=\"UTF-8\""
        );
        assert_eq!(response.headers()["cache-control"], "no-store");
        let text = response.text().await.unwrap();
        assert!(!text.contains("SENTINEL"));
        assert!(!text.contains("SECRET_QUERY"));
        if method == reqwest::Method::HEAD {
            assert!(text.is_empty());
        }
    }
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            session.accept_stream()
        )
        .await
        .is_err(),
        "denied requests must never open a session stream"
    );
    server.join.abort();
}
