//! Regression test for the bridge-deadlock bug that left subdomains permanently
//! claimed after a client disconnect.
//!
//! Symptom: after `tnl http 5173 foo` and Ctrl-C, re-running the same command
//! returned `server error (SubdomainTaken): subdomain 'foo' already in use`,
//! because `axum_ws_to_tungstenite` used `tokio::join!` and the surviving half
//! of `WebSocketStream::split()` kept the duplex pipe alive, so the daemon's
//! yamux driver never saw EOF and `registry.drop_session` was never called.

use std::pin::Pin;
use std::time::Duration;

use tnl_protocol::{ControlMsg, CreateTunnelReq, Session as _, TunnelCreatedResp};
use tnld::auth::{hash_plaintext, TokenEntry, TokensFile};
use tnld::config::Config;
use tnld::serve::{spawn_server, ServerHandle};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

async fn open_control(
    handle: &ServerHandle,
) -> (
    Box<dyn tnl_protocol::Session>,
    Pin<Box<dyn tnl_protocol::Stream>>,
) {
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
    let mut session = tnl_protocol::transport::client_session_from_ws_generic(ws);
    let ctrl = tokio::time::timeout(Duration::from_secs(2), session.open_stream())
        .await
        .unwrap()
        .unwrap();
    (Box::new(session), ctrl)
}

async fn create_tunnel(
    ctrl: &mut Pin<Box<dyn tnl_protocol::Stream>>,
    subdomain: &str,
) -> ControlMsg {
    let msg = ControlMsg::CreateTunnel(CreateTunnelReq {
        subdomain: subdomain.into(),
    });
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
}

#[tokio::test(flavor = "multi_thread")]
async fn subdomain_is_released_after_client_disconnect() {
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

    // ── first client: claim 'foo' ─────────────────────────────────
    {
        let (mut session, mut ctrl) = open_control(&handle).await;
        let resp = create_tunnel(&mut ctrl, "foo").await;
        match resp {
            ControlMsg::TunnelCreated(TunnelCreatedResp { hostname, .. }) => {
                assert_eq!(hostname, "foo.t.example.com");
            }
            other => panic!("expected TunnelCreated, got {other:?}"),
        }
        // Drop the substream first, then the session — mirrors what happens
        // when the `tnl` client process exits on Ctrl-C.
        drop(ctrl);
        let _ = session.close().await;
    }

    // ── second client: same subdomain should be claimable ────────
    // Poll up to ~3s while the daemon notices the disconnect and cleans up.
    let mut last_err: Option<ControlMsg> = None;
    for _ in 0..30 {
        let (_session, mut ctrl) = open_control(&handle).await;
        let resp = create_tunnel(&mut ctrl, "foo").await;
        match resp {
            ControlMsg::TunnelCreated(TunnelCreatedResp { hostname, .. }) => {
                assert_eq!(hostname, "foo.t.example.com");
                return;
            }
            other => {
                last_err = Some(other);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    panic!("subdomain was never released after disconnect; last response: {last_err:?}");
}
