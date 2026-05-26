use std::time::Duration;

use tnl_protocol::{ControlMsg, CreateTunnelReq, Session as _, TunnelCreatedResp};
use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config;
use tnld::serve::spawn_server;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

#[tokio::test(flavor = "multi_thread")]
async fn cli_creates_tunnel_via_control_channel() {
    // ── boot server ────────────────────────────────────────────────
    let hash = hash_plaintext("tnl_TESTSECRET").unwrap();
    let tokens = TokensFile {
        tokens: vec![TokenEntry {
            name: "smoke".into(),
            hash,
        }],
    };
    let tmp_tokens = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp_tokens.path(), toml::to_string(&tokens).unwrap()).unwrap();
    let cfg = Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://test".into(),
        hostname_root: "t.example.com".into(),
        tokens_file: tmp_tokens.path().to_string_lossy().into_owned(),
    };
    let handle = spawn_server(cfg).await.unwrap();

    // ── dial /control ─────────────────────────────────────────────
    let url = format!("ws://{}/control", handle.local_addr);
    let req = http::Request::builder()
        .uri(&url)
        .header("Authorization", "Bearer tnl_TESTSECRET")
        .header("Host", handle.local_addr.to_string())
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header(
            "Sec-WebSocket-Key",
            async_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .header("Sec-WebSocket-Version", "13")
        .body(())
        .unwrap();
    let (ws, _) = async_tungstenite::tokio::connect_async(req).await.unwrap();

    // wrap as yamux client session (CLI-role: yamux Server)
    let mut session = tnl_protocol::transport::client_session_from_ws(ws);

    // ── open control substream ─────────────────────────────────────
    // CLI opens the control stream first; daemon's accept_stream picks it up.
    let mut ctrl: std::pin::Pin<Box<dyn tnl_protocol::Stream>> =
        tokio::time::timeout(Duration::from_secs(2), session.open_stream())
            .await
            .unwrap()
            .unwrap();

    // ── send CreateTunnel ──────────────────────────────────────────
    let msg = ControlMsg::CreateTunnel(CreateTunnelReq {
        subdomain: "foo".into(),
    });
    let payload = serde_json::to_vec(&msg).unwrap();
    let len = u32::try_from(payload.len()).unwrap().to_be_bytes();
    ctrl.write_all(&len).await.unwrap();
    ctrl.write_all(&payload).await.unwrap();

    // ── read TunnelCreated ─────────────────────────────────────────
    let mut lenbuf = [0u8; 4];
    ctrl.read_exact(&mut lenbuf).await.unwrap();
    let n = u32::from_be_bytes(lenbuf) as usize;
    let mut respbuf = vec![0u8; n];
    ctrl.read_exact(&mut respbuf).await.unwrap();
    let resp: ControlMsg = serde_json::from_slice(&respbuf).unwrap();
    match resp {
        ControlMsg::TunnelCreated(TunnelCreatedResp { hostname, .. }) => {
            assert_eq!(hostname, "foo.t.example.com");
        }
        other => panic!("expected TunnelCreated, got {other:?}"),
    }
}
