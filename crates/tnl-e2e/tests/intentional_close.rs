use std::time::Duration;

use tnl_protocol::{ClosingReason, ControlMsg};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn fixture() -> (
    tempfile::NamedTempFile,
    tnld::serve::ServerHandle,
    tnld::serve::AppState,
) {
    let tokens = tempfile::NamedTempFile::new().unwrap();
    let tf = tnld::auth::TokensFile {
        tokens: vec![
            tnld::auth::TokenEntry {
                name: "fixture".into(),
                hash: tnld::auth::hash_plaintext("tnl_U").unwrap(),
            },
            tnld::auth::TokenEntry {
                name: "other".into(),
                hash: tnld::auth::hash_plaintext("tnl_OTHER").unwrap(),
            },
        ],
    };
    std::fs::write(tokens.path(), toml::to_string(&tf).unwrap()).unwrap();
    let (server, state) = tnld::serve::spawn_server_with_state(tnld::config::Config {
        listen: "127.0.0.1:0".into(),
        public_url: "http://x".into(),
        hostname_root: "t.x".into(),
        tokens_file: tokens.path().to_string_lossy().into_owned(),
        trusted_proxies: vec![],
        session_grace_sec: 3600,
    })
    .await
    .unwrap();
    (tokens, server, state)
}

async fn close(session: &mut tnl::client::ConnectedSession) {
    tokio::time::timeout(Duration::from_secs(3), async {
        let payload = serde_json::to_vec(&ControlMsg::Close).unwrap();
        session
            .control
            .write_u32(u32::try_from(payload.len()).unwrap())
            .await
            .unwrap();
        session.control.write_all(&payload).await.unwrap();
        session.control.flush().await.unwrap();
        let n = session
            .control
            .read_u32()
            .await
            .expect("intentional Close must receive an acknowledgment, not EOF");
        assert!(n < 1024);
        let mut payload = vec![0; n as usize];
        session.control.read_exact(&mut payload).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<ControlMsg>(&payload).unwrap(),
            ControlMsg::Closing {
                reason: ClosingReason::ClientShutdown
            }
        );
    })
    .await
    .expect("bounded intentional Close acknowledgment");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn close_acknowledges_immediate_release_despite_long_network_grace() {
    let (_tokens, server, state) = fixture().await;
    let endpoint = format!("http://{}", server.local_addr);
    let mut first = tnl::client::connect_and_create(&endpoint, "tnl_U", "demo")
        .await
        .unwrap();
    let id = first.tunnel_id;
    close(&mut first).await;
    assert!(
        state.registry.find_by_subdomain("demo").is_none(),
        "ack must follow removal"
    );
    let mut second = tnl::client::connect_and_create(&endpoint, "tnl_U", "demo")
        .await
        .expect("same name immediately reusable");
    assert_ne!(second.tunnel_id, id);
    close(&mut second).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stale_session_close_does_not_remove_a_new_reattached_binding() {
    let (_tokens, server, state) = fixture().await;
    let endpoint = format!("http://{}", server.local_addr);
    let mut old = tnl::client::connect_and_create(&endpoint, "tnl_U", "demo")
        .await
        .unwrap();
    let id = old.tunnel_id;
    let old_session = state.registry.current_session_id(&id.to_string()).unwrap();
    state.registry.drop_session(&old_session, 3600);
    let mut current = tnl::client::connect_and_reattach(&endpoint, "tnl_U", id, "demo")
        .await
        .unwrap();
    let current_session = state.registry.current_session_id(&id.to_string()).unwrap();
    assert_ne!(old_session, current_session);
    close(&mut old).await;
    assert_eq!(
        state.registry.current_session_id(&id.to_string()),
        Some(current_session)
    );
    assert_eq!(
        state.registry.find_by_subdomain("demo").unwrap().id,
        id.to_string()
    );
    close(&mut current).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_old_owner_close_cannot_remove_other_owners_reused_name() {
    let (_tokens, server, state) = fixture().await;
    let endpoint = format!("http://{}", server.local_addr);
    let mut old = tnl::client::connect_and_create(&endpoint, "tnl_U", "demo")
        .await
        .unwrap();
    state
        .registry
        .close_by_subdomain("demo", "fixture")
        .unwrap();
    let mut newer = tnl::client::connect_and_create(&endpoint, "tnl_OTHER", "demo")
        .await
        .unwrap();
    let id = newer.tunnel_id.to_string();
    let binding = state.registry.current_session_id(&id).unwrap();
    assert_ne!(old.tunnel_id, newer.tunnel_id);
    close(&mut old).await;
    assert_eq!(state.registry.current_session_id(&id), Some(binding));
    let tunnel = state.registry.find_by_subdomain("demo").unwrap();
    assert_eq!(tunnel.id, id);
    assert_eq!(tunnel.created_by, "other");
    close(&mut newer).await;
}
