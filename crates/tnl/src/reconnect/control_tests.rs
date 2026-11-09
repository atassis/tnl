use std::{pin::Pin, time::Duration};

use tnl_protocol::{ControlMsg, Session as _, Stream, TunnelCreatedResp};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    sync::{mpsc, oneshot},
};

async fn read(stream: &mut Pin<Box<dyn Stream>>) -> ControlMsg {
    let length = stream.read_u32().await.unwrap();
    assert!(length < 16 * 1024);
    let mut payload = vec![0; length as usize];
    stream.read_exact(&mut payload).await.unwrap();
    serde_json::from_slice(&payload).unwrap()
}
async fn write(stream: &mut Pin<Box<dyn Stream>>, msg: &ControlMsg) {
    let payload = serde_json::to_vec(msg).unwrap();
    stream
        .write_u32(u32::try_from(payload.len()).unwrap())
        .await
        .unwrap();
    stream.write_all(&payload).await.unwrap();
    stream.flush().await.unwrap();
}

#[allow(clippy::result_large_err)]
async fn fixture() -> (
    String,
    mpsc::Receiver<ControlMsg>,
    mpsc::Sender<ControlMsg>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (seen_tx, seen) = mpsc::channel(8);
    let (reply, mut replies) = mpsc::channel(8);
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let ws = async_tungstenite::tokio::accept_hdr_async(
            socket,
            |_: &http::Request<()>, mut response: http::Response<()>| {
                response.headers_mut().insert(
                    tnl_protocol::control::CONTROL_PROTOCOL_HEADER,
                    http::HeaderValue::from_static(tnl_protocol::control::CONTROL_PROTOCOL_VERSION),
                );
                Ok(response)
            },
        )
        .await
        .unwrap();
        let mut session = tnl_protocol::server_session_from_ws_generic(ws);
        let mut control = session.accept_stream().await.unwrap().unwrap();
        assert!(matches!(
            read(&mut control).await,
            ControlMsg::CreateTunnel(_)
        ));
        let id = ulid::Ulid::new();
        write(
            &mut control,
            &ControlMsg::TunnelCreated(TunnelCreatedResp {
                tunnel_id: id,
                hostname: "demo.t.x".into(),
                subdomain: "demo".into(),
            }),
        )
        .await;
        // An actual accepted forward proves the run has entered its serving
        // select (and polled the control owner), without a scheduling sleep.
        let mut probe = session.open_stream().await.unwrap();
        probe
            .write_all(b"GET /ready HTTP/1.1\r\nHost: demo.t.x\r\n\r\n")
            .await
            .unwrap();
        probe.flush().await.unwrap();
        probe.read_u8().await.unwrap();
        drop(probe);
        seen_tx
            .send(ControlMsg::TunnelCreated(TunnelCreatedResp {
                tunnel_id: id,
                hostname: "demo.t.x".into(),
                subdomain: "demo".into(),
            }))
            .await
            .unwrap();
        loop {
            let msg = read(&mut control).await;
            seen_tx.send(msg).await.unwrap();
            let Some(reply) = replies.recv().await else {
                return;
            };
            let closing = matches!(reply, ControlMsg::Closing { .. });
            write(&mut control, &reply).await;
            if closing {
                return;
            }
        }
    });
    (endpoint, seen, reply, server)
}

#[tokio::test]
async fn delivered_remote_stop_ends_run_without_reconnect() {
    let (endpoint, mut seen, replies, server) = fixture().await;
    let run = tokio::spawn(async move {
        super::run_with_options(
            &endpoint,
            "tnl_TEST",
            None,
            crate::target::Target::LocalhostPort(1),
            crate::host_header::HostHeader::Preserve,
            &crate::client::TunnelOptions::default(),
            super::Hooks::default(),
        )
        .await
    });
    let ControlMsg::TunnelCreated(ack) = seen.recv().await.unwrap() else {
        panic!("registration")
    };
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(15)).await;
    tokio::time::resume();
    let heartbeat = tokio::time::timeout(Duration::from_secs(1), seen.recv())
        .await
        .expect("heartbeat due after 15 seconds")
        .unwrap();
    assert_eq!(
        heartbeat,
        ControlMsg::Heartbeat {
            tunnel_id: ack.tunnel_id,
            sequence: 0
        }
    );
    replies
        .send(ControlMsg::Closing {
            reason: tnl_protocol::ClosingReason::RemoteStop,
        })
        .await
        .unwrap();
    // The fixture returns immediately after flushing stop, making physical EOF
    // available alongside the valid control result.
    assert!(tokio::time::timeout(Duration::from_secs(1), run)
        .await
        .unwrap()
        .unwrap()
        .is_ok());
    server.await.unwrap();
}

#[tokio::test]
async fn whole_attempt_bounds_a_stalled_websocket_handshake() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (accepted_tx, accepted) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        accepted_tx.send(()).unwrap();
        let _ = released.await;
        drop(socket);
    });
    let connect = tokio::spawn(async move {
        crate::client::connect_and_create(&endpoint, "tnl_TEST", "demo").await
    });
    accepted.await.unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(20)).await;
    let result = tokio::time::timeout(Duration::from_millis(10), connect)
        .await
        .expect("whole attempt must expire at 20 seconds")
        .unwrap();
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("connection attempt deadline"));
    let _ = release.send(());
    server.await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines, clippy::result_large_err)]
async fn reattach_and_recreate_share_deadline_and_public_random_name_survives_timeout() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (seen_tx, mut seen) = mpsc::channel(8);
    let (lost_tx, mut lost_rx) = oneshot::channel();
    let (keep_tx, keep_rx) = oneshot::channel::<()>();
    let id = ulid::Ulid::new();
    let server = tokio::spawn(async move {
        let mut sessions = tokio::task::JoinSet::new();
        let mut keep_rx = Some(keep_rx);
        for connection in 1..=4 {
            let (socket, _) = listener.accept().await.unwrap();
            let ws = async_tungstenite::tokio::accept_hdr_async(
                socket,
                |_: &http::Request<()>, mut response: http::Response<()>| {
                    response.headers_mut().insert(
                        tnl_protocol::control::CONTROL_PROTOCOL_HEADER,
                        http::HeaderValue::from_static(
                            tnl_protocol::control::CONTROL_PROTOCOL_VERSION,
                        ),
                    );
                    Ok(response)
                },
            )
            .await
            .unwrap();
            let mut session = tnl_protocol::server_session_from_ws_generic(ws);
            let mut control = session.accept_stream().await.unwrap().unwrap();
            let msg = read(&mut control).await;
            match connection {
                1 => {
                    assert!(matches!(
                        msg,
                        ControlMsg::CreateTunnel(tnl_protocol::CreateTunnelReq { subdomain: None })
                    ));
                    write(
                        &mut control,
                        &ControlMsg::TunnelCreated(TunnelCreatedResp {
                            tunnel_id: id,
                            hostname: "assigned.t.x".into(),
                            subdomain: "assigned".into(),
                        }),
                    )
                    .await;
                    let mut probe = session.open_stream().await.unwrap();
                    probe
                        .write_all(b"GET /ready HTTP/1.1\r\nHost: assigned.t.x\r\n\r\n")
                        .await
                        .unwrap();
                    probe.flush().await.unwrap();
                    probe.read_u8().await.unwrap();
                    seen_tx.send(msg).await.unwrap();
                    sessions.spawn(async move {
                        let _session = session;
                        let _ = control.read_u8().await;
                    });
                }
                2 => {
                    assert!(
                        matches!(&msg, ControlMsg::ReattachTunnel(req) if req.tunnel_id == id && req.subdomain == "assigned")
                    );
                    seen_tx.send(msg).await.unwrap();
                    (&mut lost_rx).await.unwrap();
                    write(
                        &mut control,
                        &ControlMsg::Error {
                            code: tnl_protocol::ErrorCode::TunnelLost,
                            message: "not retained".into(),
                        },
                    )
                    .await;
                }
                3 => {
                    assert!(
                        matches!(&msg, ControlMsg::CreateTunnel(req) if req.subdomain.as_deref() == Some("assigned"))
                    );
                    seen_tx.send(msg).await.unwrap();
                    let keep = keep_rx.take().unwrap();
                    sessions.spawn(async move {
                        let _session = session;
                        let _control = control;
                        let _ = keep.await;
                    });
                }
                4 => {
                    assert!(
                        matches!(&msg, ControlMsg::CreateTunnel(req) if req.subdomain.as_deref() == Some("assigned"))
                    );
                    seen_tx.send(msg).await.unwrap();
                    write(
                        &mut control,
                        &ControlMsg::Error {
                            code: tnl_protocol::ErrorCode::Unauthorized,
                            message: "terminal".into(),
                        },
                    )
                    .await;
                }
                _ => unreachable!(),
            }
        }
        while let Some(result) = sessions.join_next().await {
            result.unwrap();
        }
    });
    let (disconnect, disconnected) = oneshot::channel();
    let run = tokio::spawn(async move {
        super::run_with_options(
            &endpoint,
            "tnl_TEST",
            None,
            crate::target::Target::LocalhostPort(1),
            crate::host_header::HostHeader::Preserve,
            &crate::client::TunnelOptions::default(),
            super::Hooks {
                cancel_first_session: Some(disconnected),
                ..super::Hooks::default()
            },
        )
        .await
    });
    seen.recv().await.unwrap();
    disconnect.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), seen.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(12)).await;
    tokio::time::resume();
    lost_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), seen.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(8)).await;
    tokio::time::resume();
    tokio::time::timeout(Duration::from_secs(4), seen.recv())
        .await
        .expect("fallback reset the 20-second attempt budget")
        .unwrap();
    let error = run.await.unwrap().unwrap_err();
    assert!(matches!(
        error.downcast_ref::<crate::client::ControlError>(),
        Some(crate::client::ControlError::Server(
            tnl_protocol::ErrorCode::Unauthorized
        ))
    ));
    let _ = keep_tx.send(());
    server.await.unwrap();
}
