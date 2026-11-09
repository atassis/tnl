//! Deadline tests drive the production transport owner and both real bridge
//! directions. The socket sink can be held without blocking inbound traffic.
use super::activation_tests::{relay_client_bytes, state, Socket};
use super::*;
use std::{
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, Notify};

type Control = Pin<Box<dyn tnl_protocol::Stream>>;
struct Peer {
    state: AppState,
    client: tnl_protocol::transport::YamuxSession,
    daemon: tokio::task::JoinHandle<()>,
    relay: tokio::task::JoinHandle<()>,
    stop_relay: tokio_util::sync::CancellationToken,
    released: oneshot::Receiver<()>,
    blocked: Arc<AtomicBool>,
    sending: Arc<Notify>,
}
impl Peer {
    async fn start() -> Self {
        let state = state();
        let (incoming, receiver) = mpsc::unbounded_channel();
        let (outgoing, sent) = mpsc::unbounded_channel();
        let blocked = Arc::new(AtomicBool::new(false));
        let sending = Arc::new(Notify::new());
        let (released, release) = oneshot::channel();
        let socket = Socket {
            incoming: receiver,
            outgoing,
            blocked: blocked.clone(),
            sending: sending.clone(),
            released: Some(released),
        };
        let (daemon_ws, bridge_ws) = websocket_bridge_pair().await;
        let daemon_state = state.clone();
        let daemon = tokio::spawn(async move {
            let cancel = tokio_util::sync::CancellationToken::new();
            let token = AuthedToken {
                name: "owner".into(),
            };
            tokio::join!(
                run_transport_session(
                    daemon_ws,
                    &daemon_state,
                    &token,
                    DataStreamProtocol::RawHttp1,
                    &cancel
                ),
                bridge_messages(socket, bridge_ws, &cancel),
            );
        });
        let (client_io, relay_io) = tokio::io::duplex(64 * 1024);
        let stop_relay = tokio_util::sync::CancellationToken::new();
        let relay = tokio::spawn(relay_client_bytes(
            relay_io,
            incoming,
            sent,
            stop_relay.clone(),
        ));
        let client = tnl_protocol::transport::YamuxSession::new_server(client_io);
        // Poll the actual owner into bootstrap without advancing virtual time.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        Self {
            state,
            client,
            daemon,
            relay,
            stop_relay,
            released: release,
            blocked,
            sending,
        }
    }
    async fn register(&mut self) -> (Control, ulid::Ulid, String) {
        let mut control = self.client.open_stream().await.unwrap();
        write(
            &mut control,
            &ControlMsg::CreateTunnel(tnl_protocol::CreateTunnelReq {
                subdomain: Some("lease".into()),
            }),
        )
        .await;
        let length = control.read_u32().await.unwrap();
        let mut payload = vec![0; length as usize];
        control.read_exact(&mut payload).await.unwrap();
        let ControlMsg::TunnelCreated(created) = serde_json::from_slice(&payload).unwrap() else {
            panic!("registration failed")
        };
        let session = self
            .state
            .registry
            .current_session_id(&created.tunnel_id.to_string())
            .unwrap();
        (control, created.tunnel_id, session)
    }
    async fn finished(mut self) {
        tokio::time::timeout(Duration::from_secs(3), &mut self.daemon)
            .await
            .expect("deadline retained physical owner/bridge")
            .unwrap();
        assert!(self.released.try_recv().is_ok(), "real socket retained");
        assert!(self.state.session_handles.is_empty());
        self.stop_relay.cancel();
        self.relay.await.unwrap();
    }
}
async fn write(control: &mut Control, message: &ControlMsg) {
    let payload = serde_json::to_vec(message).unwrap();
    control
        .write_u32(u32::try_from(payload.len()).unwrap())
        .await
        .unwrap();
    control.write_all(&payload).await.unwrap();
    control.flush().await.unwrap();
}
async fn settle() {
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn bootstrap_without_control_stream_expires() {
    let peer = Peer::start().await;
    tokio::time::advance(Duration::from_secs(19)).await;
    settle().await;
    assert!(!peer.daemon.is_finished());
    tokio::time::advance(Duration::from_secs(1)).await;
    peer.finished().await;
}

#[tokio::test(start_paused = true)]
async fn initial_partial_prefix_and_payload_share_bootstrap_deadline() {
    for payload in [false, true] {
        let mut peer = Peer::start().await;
        tokio::time::advance(Duration::from_secs(19)).await;
        let mut control = peer.client.open_stream().await.unwrap();
        if payload {
            control.write_all(&[0, 0, 0, 100, b'{']).await.unwrap();
        } else {
            control.write_all(&[0]).await.unwrap();
        }
        control.flush().await.unwrap();
        settle().await;
        assert!(!peer.daemon.is_finished());
        tokio::time::advance(Duration::from_secs(1)).await;
        peer.finished().await;
    }
}

#[tokio::test(start_paused = true)]
async fn active_partial_prefix_and_payload_do_not_renew_silence_lease() {
    for payload in [false, true] {
        let mut peer = Peer::start().await;
        let (mut control, id, session) = peer.register().await;
        let stale_handle = peer.state.session_handles.get(&session).unwrap().clone();
        let held_mutex = stale_handle.session.lock().await;
        tokio::time::advance(Duration::from_secs(44)).await;
        if payload {
            control.write_all(&[0, 0, 0, 100, b'{']).await.unwrap();
        } else {
            control.write_all(&[0]).await.unwrap();
        }
        control.flush().await.unwrap();
        settle().await;
        assert!(peer.state.session_handles.contains_key(&session));
        tokio::time::advance(Duration::from_secs(1)).await;
        settle().await;
        assert!(
            !peer.state.session_handles.contains_key(&session),
            "partial bytes renewed the lease"
        );
        assert!(futures::poll!(Box::pin(stale_handle.invalidated())).is_ready());
        drop(held_mutex);
        assert!(!peer.state.registry.snapshot_infos()[0].active);
        assert_eq!(
            peer.state
                .registry
                .heartbeat_status(&id.to_string(), &session)
                .unwrap(),
            HeartbeatStatus::InvalidSession,
            "lease expiry retained a registered session"
        );
        peer.finished().await;
    }
}

#[tokio::test(start_paused = true)]
async fn complete_valid_heartbeat_renews_lease() {
    let mut peer = Peer::start().await;
    let (mut control, id, session) = peer.register().await;
    tokio::time::advance(Duration::from_secs(44)).await;
    write(
        &mut control,
        &ControlMsg::Heartbeat {
            tunnel_id: id,
            sequence: 42,
        },
    )
    .await;
    let length = control.read_u32().await.unwrap();
    let mut payload = vec![0; length as usize];
    control.read_exact(&mut payload).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<ControlMsg>(&payload).unwrap(),
        ControlMsg::HeartbeatAck {
            tunnel_id: id,
            sequence: 42
        }
    );
    tokio::time::advance(Duration::from_secs(44)).await;
    settle().await;
    assert!(peer.state.session_handles.contains_key(&session));
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert!(!peer.state.session_handles.contains_key(&session));
    peer.finished().await;
}

#[tokio::test(start_paused = true)]
async fn blocked_ack_invalidates_before_failed_cleanup_and_releases_bridge() {
    let mut peer = Peer::start().await;
    let (mut control, id, session) = peer.register().await;
    let handle = peer.state.session_handles.get(&session).unwrap().clone();
    let termination = handle.session.lock().await.termination();
    // Hold the actual socket sink, then fill the finite duplex/stream windows.
    // The peer keeps uploading complete heartbeats but consumes no ACKs.
    peer.blocked.store(true, Ordering::SeqCst);
    let writer = tokio::spawn(async move {
        for sequence in 0..20_000 {
            let bytes = serde_json::to_vec(&ControlMsg::Heartbeat {
                tunnel_id: id,
                sequence,
            })
            .unwrap();
            if control
                .write_u32(u32::try_from(bytes.len()).unwrap())
                .await
                .is_err()
                || control.write_all(&bytes).await.is_err()
                || control.flush().await.is_err()
            {
                break;
            }
        }
    });
    peer.sending.notified().await;
    // With paused time and exclusively in-memory IO, automatic advancement
    // happens only once all runnable work has drained into backpressure. This
    // virtual tick is a quiescence barrier, not a wall-clock scheduling sleep.
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert!(
        !writer.is_finished(),
        "fixture did not reach bounded IO backpressure"
    );
    assert!(peer.state.session_handles.contains_key(&session));
    tokio::time::advance(Duration::from_secs(4)).await;
    settle().await;
    assert!(peer.state.session_handles.contains_key(&session));
    tokio::time::advance(Duration::from_millis(999)).await;
    settle().await;
    assert!(
        !peer.state.session_handles.contains_key(&session),
        "blocked ACK retained valid session past exchange deadline"
    );
    assert!(
        !peer.state.registry.snapshot_infos()[0].active,
        "routing invalidation awaited physical cleanup"
    );
    assert!(futures::poll!(Box::pin(handle.invalidated())).is_ready());
    assert!(
        peer.released.try_recv().is_err(),
        "blocked physical owner released before failed cleanup budget"
    );
    assert!(futures::poll!(Box::pin(termination.forced())).is_pending());
    tokio::time::advance(Duration::from_secs(2)).await;
    settle().await;
    assert!(futures::poll!(Box::pin(termination.forced())).is_ready());
    peer.finished().await;
    writer.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn initial_response_backpressure_shares_the_bootstrap_deadline() {
    let mut peer = Peer::start().await;
    tokio::time::advance(Duration::from_secs(19)).await;
    let mut control = peer.client.open_stream().await.unwrap();
    peer.blocked.store(true, Ordering::SeqCst);
    let writer = tokio::spawn(async move {
        // Valid requests with business rejections do not select failed cleanup
        // or reset registration. Fill real response buffers without registering.
        let bytes = serde_json::to_vec(&ControlMsg::CreateTunnel(tnl_protocol::CreateTunnelReq {
            subdomain: Some("Invalid!".into()),
        }))
        .unwrap();
        for _ in 0..20_000 {
            if control
                .write_u32(u32::try_from(bytes.len()).unwrap())
                .await
                .is_err()
                || control.write_all(&bytes).await.is_err()
                || control.flush().await.is_err()
            {
                break;
            }
        }
    });
    peer.sending.notified().await;
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert!(!writer.is_finished());
    assert_eq!(peer.state.session_handles.len(), 1);
    assert!(peer.state.registry.snapshot_infos().is_empty());
    tokio::time::advance(Duration::from_millis(998)).await;
    settle().await;
    assert_eq!(
        peer.state.session_handles.len(),
        1,
        "business rejection itself forced cleanup"
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    settle().await;
    assert!(
        peer.state.session_handles.is_empty(),
        "initial response reset the common 20s budget"
    );
    assert!(peer.released.try_recv().is_err());
    peer.finished().await;
    writer.await.unwrap();
}

async fn receive(control: &mut Control) -> Option<ControlMsg> {
    let length = control.read_u32().await.ok()?;
    let mut payload = vec![0; length as usize];
    control.read_exact(&mut payload).await.ok()?;
    Some(serde_json::from_slice(&payload).unwrap())
}

fn stop_and_reuse(peer: &Peer) -> Arc<crate::registry::Tunnel> {
    peer.state
        .registry
        .close_by_subdomain("lease", "owner")
        .unwrap();
    let replacement = peer.state.registry.register_session("owner");
    peer.state
        .registry
        .create_tunnel("lease", &replacement.id, "owner")
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn remote_stop_retains_physical_owner_until_the_existing_lease_expires() {
    let mut peer = Peer::start().await;
    let (mut control, id, session) = peer.register().await;
    let handle = peer.state.session_handles.get(&session).unwrap().clone();
    let termination = handle.session.lock().await.termination();
    tokio::time::advance(Duration::from_secs(44)).await;
    let replacement = stop_and_reuse(&peer);
    peer.blocked.store(true, Ordering::SeqCst);
    write(
        &mut control,
        &ControlMsg::Heartbeat {
            tunnel_id: id,
            sequence: 1,
        },
    )
    .await;
    // The response has reached the actual blocked socket sink, but the peer
    // has NOT consumed it. Drain runnable in-memory IO at the same boundary.
    peer.sending.notified().await;
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert!(
        futures::poll!(Box::pin(termination.close_requested())).is_pending(),
        "RemoteStop write initiated physical closure before peer consumption"
    );
    assert!(peer.state.session_handles.contains_key(&session));
    assert_eq!(
        peer.state
            .registry
            .heartbeat_status(&id.to_string(), &session)
            .unwrap(),
        HeartbeatStatus::Removed
    );
    // Neither sending stop nor receiving partial post-stop bytes resets the
    // lease renewed by the last valid ACTIVE message at registration (t=0).
    control.write_all(&[0]).await.unwrap();
    control.flush().await.unwrap();
    tokio::time::advance(Duration::from_millis(999)).await;
    settle().await;
    assert!(
        !peer.state.session_handles.contains_key(&session),
        "stop/partial bytes renewed the remaining lease"
    );
    assert_eq!(
        peer.state
            .registry
            .heartbeat_status(&id.to_string(), &session)
            .unwrap(),
        HeartbeatStatus::InvalidSession
    );
    assert_eq!(
        peer.state.registry.find_by_subdomain("lease").unwrap().id,
        replacement.id
    );
    assert_eq!(
        peer.state
            .registry
            .heartbeat_status(
                &replacement.id,
                &peer
                    .state
                    .registry
                    .current_session_id(&replacement.id)
                    .unwrap()
            )
            .unwrap(),
        HeartbeatStatus::Active
    );
    assert!(futures::poll!(Box::pin(termination.close_requested())).is_ready());
    // Small queued writes can finish driver cleanup without forcing. Either
    // way, the failed path must also join/release the blocked real socket.
    peer.finished().await;
    assert!(futures::poll!(Box::pin(termination.completed())).is_ready());
}

#[tokio::test(start_paused = true)]
async fn remote_stop_peer_close_or_eof_preserves_normal_drain_and_reused_owner() {
    for explicit_close in [false, true] {
        let mut peer = Peer::start().await;
        let (mut control, id, session) = peer.register().await;
        let handle = peer.state.session_handles.get(&session).unwrap().clone();
        let termination = handle.session.lock().await.termination();
        let replacement = stop_and_reuse(&peer);
        write(
            &mut control,
            &ControlMsg::Heartbeat {
                tunnel_id: id,
                sequence: 2,
            },
        )
        .await;
        assert_eq!(
            receive(&mut control).await,
            Some(ControlMsg::Closing {
                reason: ClosingReason::RemoteStop
            })
        );
        assert!(
            futures::poll!(Box::pin(termination.close_requested())).is_pending(),
            "stop response itself closed the physical owner"
        );
        if explicit_close {
            write(&mut control, &ControlMsg::Close).await;
            assert_eq!(
                receive(&mut control).await,
                Some(ControlMsg::Closing {
                    reason: ClosingReason::ClientShutdown
                })
            );
        }
        drop(control);
        let registry = peer.state.registry.clone();
        peer.finished().await;
        assert!(
            futures::poll!(Box::pin(termination.forced())).is_pending(),
            "healthy peer stop selected failed cleanup"
        );
        assert_eq!(
            registry.find_by_subdomain("lease").unwrap().id,
            replacement.id
        );
        assert!(registry.snapshot_infos()[0].active);
        assert!(futures::poll!(Box::pin(termination.completed())).is_ready());
    }
}

#[tokio::test(start_paused = true)]
async fn remote_stop_rejects_post_stop_registration_and_heartbeat_without_rebinding() {
    for register in [false, true] {
        let mut peer = Peer::start().await;
        let (mut control, id, session) = peer.register().await;
        let replacement = stop_and_reuse(&peer);
        write(
            &mut control,
            &ControlMsg::Heartbeat {
                tunnel_id: id,
                sequence: 3,
            },
        )
        .await;
        assert_eq!(
            receive(&mut control).await,
            Some(ControlMsg::Closing {
                reason: ClosingReason::RemoteStop
            })
        );
        let message = if register {
            ControlMsg::CreateTunnel(tnl_protocol::CreateTunnelReq {
                subdomain: Some("post-stop".into()),
            })
        } else {
            ControlMsg::Heartbeat {
                tunnel_id: id,
                sequence: 4,
            }
        };
        write(&mut control, &message).await;
        assert!(
            receive(&mut control).await.is_none(),
            "post-stop message was accepted"
        );
        assert!(!peer.state.session_handles.contains_key(&session));
        assert!(peer.state.registry.find_by_subdomain("post-stop").is_none());
        assert_eq!(
            peer.state.registry.find_by_subdomain("lease").unwrap().id,
            replacement.id
        );
        assert!(peer.state.registry.snapshot_infos()[0].active);
        peer.finished().await;
    }
}

#[tokio::test(start_paused = true)]
async fn post_stop_late_close_ack_uses_only_the_remaining_logical_write_budget() {
    // A one-byte duplex deterministically blocks the logical ACK prefix itself,
    // unlike a small queued yamux write that has already reached healthy drain.
    let registry = Arc::new(Registry::new("t.x"));
    let session = registry.register_session("owner");
    let (client, server) = tokio::io::duplex(1);
    let mut control: Control = Box::pin(client);
    let owner_registry = registry.clone();
    let owner_session = session.id.clone();
    let deadline = tokio::time::Instant::now() + INITIAL_REGISTRATION_BUDGET;
    let owner = tokio::spawn(async move {
        control_loop(
            Box::pin(server),
            &owner_registry,
            &owner_session,
            "owner",
            DataStreamProtocol::RawHttp1,
            deadline,
        )
        .await
    });
    write(
        &mut control,
        &ControlMsg::CreateTunnel(tnl_protocol::CreateTunnelReq {
            subdomain: Some("late-close".into()),
        }),
    )
    .await;
    let Some(ControlMsg::TunnelCreated(created)) = receive(&mut control).await else {
        panic!("registration failed")
    };
    registry.close_by_subdomain("late-close", "owner").unwrap();
    let new_session = registry.register_session("owner");
    let replacement = registry
        .create_tunnel("late-close", &new_session.id, "owner")
        .unwrap();
    write(
        &mut control,
        &ControlMsg::Heartbeat {
            tunnel_id: created.tunnel_id,
            sequence: 1,
        },
    )
    .await;
    assert_eq!(
        receive(&mut control).await,
        Some(ControlMsg::Closing {
            reason: ClosingReason::RemoteStop
        })
    );
    tokio::time::advance(Duration::from_millis(44_900)).await;
    write(&mut control, &ControlMsg::Close).await;
    assert_eq!(
        control.read_u8().await.unwrap(),
        0,
        "ACK prefix was not started"
    );
    // Do not consume the rest: the full prefix/body/flush cannot complete.
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert!(!owner.is_finished());
    tokio::time::advance(Duration::from_millis(99)).await;
    settle().await;
    assert!(
        owner.is_finished(),
        "late Close obtained a fresh five-second budget"
    );
    assert!(owner
        .await
        .unwrap()
        .unwrap_err()
        .is::<ControlDeadlineFailure>());
    assert_eq!(
        registry.find_by_subdomain("late-close").unwrap().id,
        replacement.id
    );
    assert_eq!(
        registry
            .heartbeat_status(&replacement.id, &new_session.id)
            .unwrap(),
        HeartbeatStatus::Active
    );
}
