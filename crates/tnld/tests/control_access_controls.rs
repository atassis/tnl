//! A fail-closed checkpoint, not trusted-WSS protected end-to-end coverage.
use tnl_protocol::{ControlMsg, ErrorCode, Session as _};
use tnld::{
    auth::{hash_plaintext, TokenEntry, TokensFile},
    config::Config,
    serve::spawn_server_with_state,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protected_control_cannot_publish_until_admission_is_integrated() {
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
    for input in [
        r#"{"type":"CreateProtectedTunnel","subdomain":"guarded","access":{"version":1,"basic":null,"allow_ips":[],"rate_limit":null,"max_concurrent":1}}"#.to_owned(),
        format!(r#"{{"type":"ReattachProtectedTunnel","tunnel_id":"00000000000000000000000000","subdomain":"guarded","policy_fingerprint":"{}"}}"#, "a".repeat(64)),
    ] {
        ctrl.write_u32(u32::try_from(input.len()).unwrap()).await.unwrap();
        ctrl.write_all(input.as_bytes()).await.unwrap();
        ctrl.flush().await.unwrap();
        let n = ctrl.read_u32().await.unwrap();
        assert!(n < 4096);
        let mut bytes = vec![0; n as usize];
        ctrl.read_exact(&mut bytes).await.unwrap();
        let msg: ControlMsg = serde_json::from_slice(&bytes).unwrap();
        assert!(matches!(msg, ControlMsg::Error { code: ErrorCode::AccessUnavailable, .. }));
        assert!(state.registry.find_by_subdomain("guarded").is_none());
        assert!(state.registry.snapshot_infos().is_empty());
    }
    server.join.abort();
}
