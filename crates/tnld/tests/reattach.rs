use std::time::Duration;

use tnl_protocol::{ControlMsg, CreateTunnelReq, ReattachReq, Session as _, TunnelCreatedResp};
use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config;
use tnld::serve::spawn_server;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Dial /control, open control substream, send one `ControlMsg`, read one
/// response, and return it.  The WS connection (and any owned tunnels) is
/// dropped at the end of this helper.
async fn one_shot(addr: &str, token: &str, msg: ControlMsg) -> ControlMsg {
    let url = format!("ws://{addr}/control");
    let req = http::Request::builder()
        .uri(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Host", addr)
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
    let mut session = tnl_protocol::transport::client_session_from_ws_generic(ws);

    let mut ctrl: std::pin::Pin<Box<dyn tnl_protocol::Stream>> =
        tokio::time::timeout(Duration::from_secs(2), session.open_stream())
            .await
            .unwrap()
            .unwrap();

    let payload = serde_json::to_vec(&msg).unwrap();
    let len = u32::try_from(payload.len()).unwrap().to_be_bytes();
    ctrl.write_all(&len).await.unwrap();
    ctrl.write_all(&payload).await.unwrap();

    let mut lenbuf = [0u8; 4];
    ctrl.read_exact(&mut lenbuf).await.unwrap();
    let n = u32::from_be_bytes(lenbuf) as usize;
    let mut respbuf = vec![0u8; n];
    ctrl.read_exact(&mut respbuf).await.unwrap();
    serde_json::from_slice(&respbuf).unwrap()
    // Drop ctrl, session — closes WS, daemon marks tunnel Disconnected.
}

fn make_cfg(tokens_file: String, grace: u32) -> Config {
    Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://test".into(),
        hostname_root: "t.x".into(),
        tokens_file,
        session_grace_sec: grace,
    }
}

fn make_tokens_file() -> tempfile::NamedTempFile {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let tf = TokensFile {
        tokens: vec![TokenEntry {
            name: "user".into(),
            hash: hash_plaintext("tnl_USER").unwrap(),
        }],
    };
    std::fs::write(tmp.path(), toml::to_string(&tf).unwrap()).unwrap();
    tmp
}

/// After WS disconnect (grace=30s), a new connection can send `ReattachTunnel`
/// and receive `TunnelCreated` with the same `tunnel_id` and subdomain.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reattach_within_grace_succeeds() {
    let tmp = make_tokens_file();
    let h = spawn_server(make_cfg(tmp.path().to_string_lossy().into_owned(), 30))
        .await
        .unwrap();
    let addr = h.local_addr.to_string();

    // Create tunnel — WS drops at end of one_shot, marking it Disconnected.
    let resp = one_shot(
        &addr,
        "tnl_USER",
        ControlMsg::CreateTunnel(CreateTunnelReq {
            subdomain: Some("foo".into()),
        }),
    )
    .await;
    let tunnel_id = match resp {
        ControlMsg::TunnelCreated(TunnelCreatedResp { tunnel_id, .. }) => tunnel_id,
        other => panic!("CreateTunnel failed: {other:?}"),
    };

    // Give the daemon a moment to notice the WS drop and run drop_session.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Reattach on a fresh WS connection.
    let resp2 = one_shot(
        &addr,
        "tnl_USER",
        ControlMsg::ReattachTunnel(ReattachReq {
            tunnel_id,
            subdomain: "foo".into(),
        }),
    )
    .await;
    match resp2 {
        ControlMsg::TunnelCreated(r) => {
            assert_eq!(r.tunnel_id, tunnel_id, "tunnel_id must be preserved");
            assert_eq!(r.subdomain, "foo");
        }
        other => panic!("Reattach failed: {other:?}"),
    }
}

/// After WS disconnect and grace window expiry + GC sweep, `ReattachTunnel`
/// returns Error { code: `TunnelLost` }.
///
/// This test sleeps 7 seconds (grace=1s, GC interval=5s).  Marked `#[ignore]`
/// to skip in fast CI; run explicitly with `cargo test -p tnld --test reattach
/// reattach_after_grace -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow test: sleeps 7s waiting for GC to fire; run with --ignored"]
async fn reattach_after_grace_returns_tunnel_lost() {
    let tmp = make_tokens_file();
    // 1-second grace window.
    let h = spawn_server(make_cfg(tmp.path().to_string_lossy().into_owned(), 1))
        .await
        .unwrap();
    let addr = h.local_addr.to_string();

    let resp = one_shot(
        &addr,
        "tnl_USER",
        ControlMsg::CreateTunnel(CreateTunnelReq {
            subdomain: Some("bar".into()),
        }),
    )
    .await;
    let tunnel_id = match resp {
        ControlMsg::TunnelCreated(TunnelCreatedResp { tunnel_id, .. }) => tunnel_id,
        other => panic!("CreateTunnel failed: {other:?}"),
    };

    // Wait long enough for grace to expire AND for GC to fire (GC runs every 5s).
    tokio::time::sleep(Duration::from_millis(7000)).await;

    let resp2 = one_shot(
        &addr,
        "tnl_USER",
        ControlMsg::ReattachTunnel(ReattachReq {
            tunnel_id,
            subdomain: "bar".into(),
        }),
    )
    .await;
    match resp2 {
        ControlMsg::Error { code, .. } => {
            assert_eq!(code, tnl_protocol::ErrorCode::TunnelLost);
        }
        other => panic!("expected Error TunnelLost, got {other:?}"),
    }
}
