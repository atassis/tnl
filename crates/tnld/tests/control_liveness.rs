//! Binding-aware control behavior through the real authenticated WS/yamux edge.
use std::{pin::Pin, time::Duration};

use tnl_protocol::{ClosingReason, ControlMsg, CreateTunnelReq, Session as _, Stream};
use tnld::{
    auth::{hash_plaintext, TokenEntry, TokensFile},
    config::Config,
    serve::{spawn_server_with_state, AppState, ServerHandle},
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

type Control = Pin<Box<dyn Stream>>;

struct Daemon {
    server: ServerHandle,
    state: AppState,
    _tokens: tempfile::NamedTempFile,
}
impl Drop for Daemon {
    fn drop(&mut self) {
        self.server.join.abort();
    }
}
impl Daemon {
    async fn start() -> Self {
        let tokens = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tokens.path(),
            toml::to_string(&TokensFile {
                tokens: vec![TokenEntry {
                    name: "owner".into(),
                    hash: hash_plaintext("tnl_TEST").unwrap(),
                }],
            })
            .unwrap(),
        )
        .unwrap();
        let (server, state) = spawn_server_with_state(Config {
            listen: "127.0.0.1:0".into(),
            public_url: "http://x".into(),
            hostname_root: "t.x".into(),
            tokens_file: tokens.path().to_str().unwrap().into(),
            trusted_proxies: vec![],
            session_grace_sec: 30,
        })
        .await
        .unwrap();
        Self {
            server,
            state,
            _tokens: tokens,
        }
    }
    async fn connect(&self) -> (tnl_protocol::transport::YamuxSession, Control) {
        let request = http::Request::builder()
            .uri(format!("ws://{}/control", self.server.local_addr))
            .header("Authorization", "Bearer tnl_TEST")
            .header("Host", self.server.local_addr.to_string())
            .header("Upgrade", "websocket")
            .header("Connection", "Upgrade")
            .header(
                "Sec-WebSocket-Key",
                async_tungstenite::tungstenite::handshake::client::generate_key(),
            )
            .header("Sec-WebSocket-Version", "13")
            .header(
                tnl_protocol::control::CONTROL_PROTOCOL_HEADER,
                tnl_protocol::control::CONTROL_PROTOCOL_VERSION,
            )
            .body(())
            .unwrap();
        let (ws, _) = async_tungstenite::tokio::connect_async(request)
            .await
            .unwrap();
        let mut session = tnl_protocol::transport::client_session_from_ws_generic(ws);
        let control = session.open_stream().await.unwrap();
        (session, control)
    }
    async fn stop(&self, name: &str) {
        let response = reqwest::Client::new()
            .delete(format!("http://{}/tunnels/{name}", self.server.local_addr))
            .bearer_auth("tnl_TEST")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    }
}
async fn send(control: &mut Control, message: &ControlMsg) {
    let bytes = serde_json::to_vec(message).unwrap();
    control
        .write_u32(u32::try_from(bytes.len()).unwrap())
        .await
        .unwrap();
    control.write_all(&bytes).await.unwrap();
    control.flush().await.unwrap();
}
async fn receive(control: &mut Control) -> Option<ControlMsg> {
    let n = control.read_u32().await.ok()?;
    assert!(n as usize <= tnl_protocol::control::MAX_CONTROL_MESSAGE_BYTES);
    let mut bytes = vec![0; n as usize];
    control.read_exact(&mut bytes).await.ok()?;
    Some(serde_json::from_slice(&bytes).unwrap())
}
async fn create(control: &mut Control, name: &str) -> ulid::Ulid {
    send(
        control,
        &ControlMsg::CreateTunnel(CreateTunnelReq {
            subdomain: Some(name.into()),
        }),
    )
    .await;
    let Some(ControlMsg::TunnelCreated(created)) = receive(control).await else {
        panic!("registration failed")
    };
    created.tunnel_id
}
async fn rejected(control: &mut Control, id: ulid::Ulid) {
    send(
        control,
        &ControlMsg::Heartbeat {
            tunnel_id: id,
            sequence: 99,
        },
    )
    .await;
    let response = tokio::time::timeout(Duration::from_secs(2), receive(control))
        .await
        .unwrap();
    assert!(
        !matches!(
            response,
            Some(
                ControlMsg::HeartbeatAck { .. }
                    | ControlMsg::Closing {
                        reason: ClosingReason::RemoteStop
                    }
            )
        ),
        "invalid owner must neither ACK nor invent RemoteStop: {response:?}"
    );
}

#[tokio::test]
async fn exact_heartbeat_and_admin_stop_survive_name_reuse_and_late_close() {
    let daemon = Daemon::start().await;
    let (_old_session, mut old) = daemon.connect().await;
    let old_id = create(&mut old, "reuse").await;
    send(
        &mut old,
        &ControlMsg::Heartbeat {
            tunnel_id: old_id,
            sequence: u64::MAX,
        },
    )
    .await;
    assert_eq!(
        receive(&mut old).await,
        Some(ControlMsg::HeartbeatAck {
            tunnel_id: old_id,
            sequence: u64::MAX
        })
    );
    daemon.stop("reuse").await;
    let (_new_session, mut new) = daemon.connect().await;
    let new_id = create(&mut new, "reuse").await;
    assert_ne!(old_id, new_id);
    send(
        &mut old,
        &ControlMsg::Heartbeat {
            tunnel_id: old_id,
            sequence: 2,
        },
    )
    .await;
    assert_eq!(
        receive(&mut old).await,
        Some(ControlMsg::Closing {
            reason: ClosingReason::RemoteStop
        })
    );
    // Old Close/cleanup cannot remove the newly published owner, even if queued late.
    let bytes = serde_json::to_vec(&ControlMsg::Close).unwrap();
    let _ = old.write_u32(u32::try_from(bytes.len()).unwrap()).await;
    let _ = old.write_all(&bytes).await;
    send(
        &mut new,
        &ControlMsg::Heartbeat {
            tunnel_id: new_id,
            sequence: 3,
        },
    )
    .await;
    assert_eq!(
        receive(&mut new).await,
        Some(ControlMsg::HeartbeatAck {
            tunnel_id: new_id,
            sequence: 3
        })
    );
    assert_eq!(
        daemon.state.registry.find_by_subdomain("reuse").unwrap().id,
        new_id.to_string()
    );
}

#[tokio::test]
async fn unregistered_and_wrong_tunnel_heartbeats_have_no_authority() {
    let daemon = Daemon::start().await;
    let (_owner_session, mut owner) = daemon.connect().await;
    let id = create(&mut owner, "owned").await;
    let (_unbound_session, mut unbound) = daemon.connect().await;
    rejected(&mut unbound, id).await;
    let (_other_session, mut other) = daemon.connect().await;
    let other_id = create(&mut other, "other").await;
    let other_session_id = daemon
        .state
        .registry
        .current_session_id(&other_id.to_string())
        .unwrap();
    rejected(&mut other, id).await;
    // Validation is read-only; subsequent lifecycle cleanup disconnects ONLY
    // the offending owner, not the tunnel named by its invalid heartbeat.
    assert!(!daemon.state.session_handles.contains_key(&other_session_id));
    assert!(
        !daemon
            .state
            .registry
            .snapshot_infos()
            .iter()
            .find(|t| t.subdomain == "other")
            .unwrap()
            .active
    );
    assert_eq!(
        daemon.state.registry.find_by_subdomain("other").unwrap().id,
        other_id.to_string()
    );
    send(
        &mut owner,
        &ControlMsg::Heartbeat {
            tunnel_id: id,
            sequence: 4,
        },
    )
    .await;
    assert_eq!(
        receive(&mut owner).await,
        Some(ControlMsg::HeartbeatAck {
            tunnel_id: id,
            sequence: 4
        })
    );
}

#[tokio::test]
async fn removed_session_is_not_remote_stop_even_if_tunnel_was_deleted() {
    let daemon = Daemon::start().await;
    let (_session, mut control) = daemon.connect().await;
    let id = create(&mut control, "expired").await;
    let session_id = daemon
        .state
        .registry
        .current_session_id(&id.to_string())
        .unwrap();
    daemon.state.registry.drop_session(&session_id, 0);
    rejected(&mut control, id).await;
    assert!(daemon.state.registry.find_by_subdomain("expired").is_none());
}

#[tokio::test]
async fn superseded_session_cannot_ack_or_stop_new_binding() {
    let daemon = Daemon::start().await;
    let (_session, mut old) = daemon.connect().await;
    let id = create(&mut old, "superseded").await;
    let old_session_id = daemon
        .state
        .registry
        .current_session_id(&id.to_string())
        .unwrap();
    daemon.state.registry.drop_session(&old_session_id, 30);
    let new_session = daemon.state.registry.register_session("owner");
    daemon
        .state
        .registry
        .try_reattach(&id.to_string(), "superseded", "owner", &new_session.id)
        .unwrap();
    rejected(&mut old, id).await;
    assert_eq!(
        daemon
            .state
            .registry
            .current_session_id(&id.to_string())
            .unwrap(),
        new_session.id
    );
    assert!(daemon.state.registry.snapshot_infos()[0].active);
}

#[tokio::test]
async fn wrong_id_after_stop_is_not_authority_over_the_reused_name() {
    let daemon = Daemon::start().await;
    let (_old_session, mut old) = daemon.connect().await;
    let old_id = create(&mut old, "wrong-after-stop").await;
    daemon.stop("wrong-after-stop").await;
    let (_new_session, mut new) = daemon.connect().await;
    let new_id = create(&mut new, "wrong-after-stop").await;
    assert_ne!(new_id, old_id);
    rejected(&mut old, new_id).await;
    send(
        &mut new,
        &ControlMsg::Heartbeat {
            tunnel_id: new_id,
            sequence: 5,
        },
    )
    .await;
    assert_eq!(
        receive(&mut new).await,
        Some(ControlMsg::HeartbeatAck {
            tunnel_id: new_id,
            sequence: 5
        })
    );
}

#[tokio::test]
async fn successful_registration_cannot_be_replaced_on_the_same_control() {
    let daemon = Daemon::start().await;
    let (_session, mut control) = daemon.connect().await;
    let id = create(&mut control, "original").await;
    send(
        &mut control,
        &ControlMsg::CreateTunnel(CreateTunnelReq {
            subdomain: Some("replacement".into()),
        }),
    )
    .await;
    assert!(
        tokio::time::timeout(Duration::from_secs(2), receive(&mut control))
            .await
            .unwrap()
            .is_none()
    );
    assert!(daemon
        .state
        .registry
        .find_by_subdomain("replacement")
        .is_none());
    assert_eq!(
        daemon
            .state
            .registry
            .find_by_subdomain("original")
            .unwrap()
            .id,
        id.to_string()
    );
    assert!(!daemon.state.registry.snapshot_infos()[0].active);
}
