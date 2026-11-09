use std::{pin::Pin, time::Duration};

use futures::{SinkExt as _, StreamExt as _};
use tnl_protocol::{ControlMsg, Session as _, Stream, TunnelCreatedResp};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    sync::{mpsc, oneshot},
};

use super::*;

const RESPONSE_PREFIX: &[u8] = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx\r\n";

async fn read_control(stream: &mut Pin<Box<dyn Stream>>) -> ControlMsg {
    let length = stream.read_u32().await.unwrap();
    let mut payload = vec![0; length as usize];
    stream.read_exact(&mut payload).await.unwrap();
    serde_json::from_slice(&payload).unwrap()
}

async fn write_control(stream: &mut Pin<Box<dyn Stream>>, message: &ControlMsg) {
    let payload = serde_json::to_vec(message).unwrap();
    stream
        .write_u32(u32::try_from(payload.len()).unwrap())
        .await
        .unwrap();
    stream.write_all(&payload).await.unwrap();
    stream.flush().await.unwrap();
}

struct FinishedForwards(mpsc::Sender<String>);
struct ForwardObservation {
    ended: mpsc::Sender<String>,
    target: String,
}
impl crate::inspection::ConsumerFactory for FinishedForwards {
    fn admit(
        &self,
        _: crate::inspection::CaptureContext,
    ) -> Option<Box<dyn crate::inspection::ObservationConsumer>> {
        Some(Box::new(ForwardObservation {
            ended: self.0.clone(),
            target: String::new(),
        }))
    }
}
impl crate::inspection::ObservationConsumer for ForwardObservation {
    fn observe(&mut self, event: &crate::inspection::Observation<'_>) {
        match event {
            crate::inspection::Observation::RequestHead { target, .. } => {
                self.target = (*target).into();
            }
            crate::inspection::Observation::Finished => {
                self.ended.try_send(self.target.clone()).ok();
            }
            _ => {}
        }
    }
}

/// A real WS edge injects physical yamux input, without calling the
/// generation's cancellation or failed-cleanup methods.
#[allow(clippy::result_large_err)] // tungstenite fixes the callback error type.
async fn daemon_connection(
    socket: tokio::net::TcpStream,
    tunnel_id: ulid::Ulid,
    corrupt: oneshot::Receiver<Vec<u8>>,
    ready: oneshot::Sender<()>,
    generation: usize,
    heartbeat_ready: Option<oneshot::Sender<()>>,
) {
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
    let (io, relay_io) = tokio::io::duplex(64 * 1024);
    let stop = tokio_util::sync::CancellationToken::new();
    let relay_stop = stop.clone();
    let relay = async move {
        let (mut sink, mut source) = ws.split();
        let (mut read, mut write) = tokio::io::split(relay_io);
        let incoming = async {
            while let Some(Ok(message)) = source.next().await {
                if let async_tungstenite::tungstenite::Message::Binary(bytes) = message {
                    if write.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
            }
        };
        let outgoing = async {
            let mut corrupt = Some(corrupt);
            let mut buffer = [0; 8192];
            loop {
                tokio::select! {
                    signal = async { corrupt.as_mut().unwrap().await }, if corrupt.is_some() => {
                        corrupt = None;
                        if let Ok(bytes) = signal {
                            sink.send(async_tungstenite::tungstenite::Message::Binary(bytes.into())).await.unwrap();
                        }
                    }
                    bytes = read.read(&mut buffer) => {
                        let Ok(n) = bytes else { break; };
                        if n == 0 { break; }
                        if sink.send(async_tungstenite::tungstenite::Message::Binary(buffer[..n].to_vec().into())).await.is_err() { break; }
                    }
                }
            }
        };
        tokio::select! {
            () = relay_stop.cancelled() => {},
            () = incoming => {},
            () = outgoing => {},
        }
    };
    let serve = async {
        let mut session = tnl_protocol::transport::YamuxSession::new_client(io);
        let termination = session.termination();
        let mut control = session.accept_stream().await.unwrap().unwrap();
        assert!(matches!(
            read_control(&mut control).await,
            ControlMsg::CreateTunnel(_) | ControlMsg::ReattachTunnel(_)
        ));
        write_control(
            &mut control,
            &ControlMsg::TunnelCreated(TunnelCreatedResp {
                tunnel_id,
                hostname: "ownership.t.x".into(),
                subdomain: "ownership".into(),
            }),
        )
        .await;
        let mut data = session.open_stream().await.unwrap();
        data.write_all(
            format!("GET /{generation} HTTP/1.1\r\nHost: ownership.t.x\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
        let mut prefix = vec![0; RESPONSE_PREFIX.len()];
        data.read_exact(&mut prefix).await.unwrap();
        assert_eq!(prefix, RESPONSE_PREFIX);
        ready.send(()).unwrap();
        if let Some(ready) = heartbeat_ready {
            assert_eq!(
                read_control(&mut control).await,
                ControlMsg::Heartbeat {
                    tunnel_id,
                    sequence: 0
                }
            );
            // No ACK: the client must be waiting in its control decoder when
            // the independent malformed physical frame arrives.
            ready.send(()).unwrap();
        }
        // Keep the fixture's stream owner alive until the client ends acceptance.
        let _ = control.read_u8().await;
        session.close().await.unwrap();
        termination.completed().await;
        stop.cancel();
    };
    tokio::join!(serve, relay);
}

async fn hold_backend_responses(
    backend: tokio::net::TcpListener,
    stop: tokio_util::sync::CancellationToken,
) {
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..2 {
        let (mut socket, _) = backend.accept().await.unwrap();
        let stop = stop.clone();
        tasks.spawn(async move {
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(socket.read_u8().await.unwrap());
            }
            socket.write_all(RESPONSE_PREFIX).await.unwrap();
            // Upload half-close is not response-read cancellation. Keep
            // this backend response open until the fixture itself ends.
            stop.cancelled().await;
            drop(socket);
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
}

#[tokio::test]
async fn real_reconnect_cancels_only_the_physically_failed_generations_backend_read() {
    assert_physical_failure_isolation(false).await;
}

#[tokio::test]
async fn real_inbound_overflow_cancels_only_its_generations_held_backend_read() {
    assert_physical_failure_isolation(true).await;
}

#[allow(clippy::too_many_lines)] // Actual two-generation heartbeat/physical-failure boundary.
async fn assert_physical_failure_isolation(overflow: bool) {
    tokio::time::timeout(Duration::from_secs(45), async {
        let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = backend.local_addr().unwrap().port();
        let backend_stop = tokio_util::sync::CancellationToken::new();
        let stop = backend_stop.clone();
        let backend_task = tokio::spawn(hold_backend_responses(backend, stop));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (first_ready, first) = oneshot::channel();
        let (second_ready, second) = oneshot::channel();
        let (heartbeat_ready, heartbeat) = oneshot::channel();
        let (_no_corrupt, first_corrupt) = oneshot::channel();
        let (inject, second_corrupt) = oneshot::channel();
        let daemon = tokio::spawn(async move {
            let tunnel_id = ulid::Ulid::new();
            let mut sessions = tokio::task::JoinSet::new();
            let (socket, _) = listener.accept().await.unwrap();
            sessions.spawn(daemon_connection(
                socket,
                tunnel_id,
                first_corrupt,
                first_ready,
                0,
                None,
            ));
            let (socket, _) = listener.accept().await.unwrap();
            sessions.spawn(daemon_connection(
                socket,
                tunnel_id,
                second_corrupt,
                second_ready,
                1,
                Some(heartbeat_ready),
            ));
            while let Some(result) = sessions.join_next().await {
                result.unwrap();
            }
        });
        let (disconnect, disconnect_rx) = oneshot::channel();
        let (shutdown, shutdown_rx) = oneshot::channel();
        let (ended_tx, mut ended) = mpsc::channel(8);
        let history = crate::inspection::Inspection::with_consumer(
            crate::inspection::PreviewMode::Off,
            Some(std::sync::Arc::new(FinishedForwards(ended_tx))),
        );
        let run_history = history.clone();
        let gate = std::sync::Arc::new(crate::client::accept_gate_tests::AcceptGate::default());
        let run_future = async move {
            run_until_shutdown(
                &endpoint,
                "token",
                Some("ownership"),
                crate::target::Target::LocalhostPort(port),
                crate::host_header::HostHeader::Auto,
                &crate::client::TunnelOptions::default(),
                Hooks {
                    cancel_first_session: Some(disconnect_rx),
                    inspection: Some(run_history),
                    ..Hooks::default()
                },
                async {
                    shutdown_rx.await?;
                    Ok(())
                },
            )
            .await
        };
        let run = if overflow {
            tokio::spawn(
                crate::client::accept_gate_tests::TEST_ACCEPT_GATE.scope(gate.clone(), run_future),
            )
        } else {
            tokio::spawn(run_future)
        };
        first.await.unwrap();
        disconnect.send(()).unwrap();
        second.await.unwrap();
        if overflow {
            gate.entered.cancelled().await;
        }
        assert!(
            ended.try_recv().is_err(),
            "ordinary reconnect cancelled the first backend read"
        );
        assert_eq!(history.search("").len(), 2);
        assert!(history.search("").iter().all(|record| !record.terminal));
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(15)).await;
        tokio::time::resume();
        tokio::time::timeout(Duration::from_secs(1), heartbeat)
            .await
            .unwrap()
            .unwrap();
        let bytes = if overflow {
            let mut bytes = Vec::new();
            // Valid, empty DATA/SYN frames from the daemon's Client role.
            // ID 1 is the established held request. IDs 3..515 fill inbound256
            // and overflow it once, below the native active-stream limit512.
            for id in (3_u32..).step_by(2).take(257) {
                bytes.extend_from_slice(&[0, 0, 0, 1]);
                bytes.extend_from_slice(&id.to_be_bytes());
                bytes.extend_from_slice(&[0; 4]);
            }
            bytes
        } else {
            // Invalid yamux version, complete header: Protocol, not EOF.
            vec![1; 12]
        };
        inject.send(bytes).unwrap();
        if overflow {
            let termination = gate.termination.lock().unwrap().clone().unwrap();
            termination.completed().await;
            assert_eq!(
                termination.close_and_observe_cause().await,
                tnl_protocol::OpenStreamError::InboundOverflow
            );
        }
        let ended_failed = tokio::time::timeout(Duration::from_secs(3), ended.recv())
            .await
            .expect("real physical failure never activated failed-generation cleanup")
            .unwrap();
        assert_eq!(ended_failed, "/1");
        let failed = history.search("/1");
        assert!(failed[0].terminal);
        assert_eq!(failed[0].failure_kind, Some("cancelled"));
        assert!(
            !history.search("/0")[0].terminal,
            "other generation's forward stopped"
        );
        assert!(
            ended.try_recv().is_err(),
            "physical failure cancelled another generation"
        );
        shutdown.send(()).unwrap();
        run.await.unwrap().unwrap();
        assert_eq!(ended.recv().await.unwrap(), "/0");
        backend_stop.cancel();
        backend_task.await.unwrap();
        daemon.await.unwrap();
    })
    .await
    .expect("real reconnect ownership fixture stalled");
}

#[tokio::test]
async fn delivered_remote_stop_revokes_history_authority_and_cancels_all_run_generations() {
    assert_control_end_ownership(ControlEndScenario::RemoteStop).await;
}

#[tokio::test]
async fn healthy_control_eof_with_heartbeat_pending_preserves_all_run_generations() {
    assert_control_end_ownership(ControlEndScenario::HealthyEof).await;
}

#[tokio::test]
async fn shutdown_interrupts_healthy_control_eof_cause_wait_without_forcing_physical_close() {
    assert_control_end_ownership(ControlEndScenario::BlockedClose).await;
}

enum ControlEndScenario {
    RemoteStop,
    HealthyEof,
    BlockedClose,
}

#[allow(clippy::too_many_lines, clippy::result_large_err)]
async fn assert_control_end_ownership(scenario: ControlEndScenario) {
    let remote_stop = matches!(scenario, ControlEndScenario::RemoteStop);
    let blocked_close = matches!(scenario, ControlEndScenario::BlockedClose);
    let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = crate::target::Target::LocalhostPort(backend.local_addr().unwrap().port());
    let backend_stop = tokio_util::sync::CancellationToken::new();
    let backend_task = tokio::spawn(hold_backend_responses(backend, backend_stop.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (ready, mut registered) = mpsc::channel(2);
    let release = tokio_util::sync::CancellationToken::new();
    let release_server = release.clone();
    let daemon = tokio::spawn(async move {
        let mut sessions = tokio::task::JoinSet::new();
        let id = ulid::Ulid::new();
        for generation in 0..2 {
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
            let ready = ready.clone();
            let release = release_server.clone();
            sessions.spawn(async move {
                let mut session = tnl_protocol::server_session_from_ws_generic(ws);
                let mut control = session.accept_stream().await.unwrap().unwrap();
                let msg = read_control(&mut control).await;
                assert!(matches!(
                    (generation, msg),
                    (0, ControlMsg::CreateTunnel(_)) | (1, ControlMsg::ReattachTunnel(_))
                ));
                write_control(
                    &mut control,
                    &ControlMsg::TunnelCreated(TunnelCreatedResp {
                        tunnel_id: id,
                        hostname: "ownership.t.x".into(),
                        subdomain: "ownership".into(),
                    }),
                )
                .await;
                let mut data = session.open_stream().await.unwrap();
                data.write_all(
                    format!("GET /{generation} HTTP/1.1\r\nHost: ownership.t.x\r\n\r\n").as_bytes(),
                )
                .await
                .unwrap();
                let mut prefix = vec![0; RESPONSE_PREFIX.len()];
                data.read_exact(&mut prefix).await.unwrap();
                assert_eq!(prefix, RESPONSE_PREFIX);
                ready.send(generation).await.unwrap();
                if generation == 1 {
                    assert_eq!(
                        read_control(&mut control).await,
                        ControlMsg::Heartbeat {
                            tunnel_id: id,
                            sequence: 0
                        }
                    );
                    if remote_stop {
                        write_control(
                            &mut control,
                            &ControlMsg::Closing {
                                reason: tnl_protocol::ClosingReason::RemoteStop,
                            },
                        )
                        .await;
                    } else {
                        // Healthy control FIN, not a malformed physical frame.
                        // Keep both held backend responses alive across recovery.
                        control.shutdown().await.unwrap();
                    }
                }
                // Neither EOF nor a fixture-side close can finish these held
                // response reads; only the client's run-owned cancellation can.
                release.cancelled().await;
                drop((data, control, session));
            });
        }
        if !remote_stop && !blocked_close {
            let (next_attempt, _) = listener.accept().await.unwrap();
            ready.send(2).await.unwrap();
            release_server.cancelled().await;
            drop(next_attempt);
        }
        while let Some(result) = sessions.join_next().await {
            result.unwrap();
        }
        assert!(
            futures::poll!(Box::pin(listener.accept())).is_pending(),
            "remote stop reconnected"
        );
    });
    let (ended_tx, mut ended) = mpsc::channel(8);
    let history = crate::inspection::Inspection::with_consumer(
        crate::inspection::PreviewMode::Off,
        Some(std::sync::Arc::new(FinishedForwards(ended_tx))),
    );
    let run_history = history.clone();
    let (disconnect, disconnected) = oneshot::channel();
    let (shutdown, shutdown_rx) = oneshot::channel();
    let gate = std::sync::Arc::new(crate::client::close_gate_tests::CloseGate::default());
    let run_gate = gate.clone();
    let run = tokio::spawn(async move {
        let options = crate::client::TunnelOptions::default();
        let run = run_until_shutdown(
            &endpoint,
            "token",
            None,
            target,
            crate::host_header::HostHeader::Preserve,
            &options,
            Hooks {
                inspection: Some(run_history),
                cancel_first_session: Some(disconnected),
                ..Hooks::default()
            },
            async {
                shutdown_rx.await?;
                Ok(())
            },
        );
        if blocked_close {
            crate::client::close_gate_tests::TEST_CLOSE_GATE
                .scope(run_gate, run)
                .await
        } else {
            run.await
        }
    });
    assert_eq!(registered.recv().await.unwrap(), 0);
    disconnect.send(()).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), registered.recv())
            .await
            .unwrap()
            .unwrap(),
        1
    );
    assert!(ended.try_recv().is_err());
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(15)).await;
    tokio::time::resume();
    if blocked_close {
        tokio::time::timeout(Duration::from_secs(1), gate.entered.cancelled())
            .await
            .unwrap();
        assert!(
            ended.try_recv().is_err(),
            "healthy close waiting for physical IO cancelled a generation"
        );
        shutdown.send(()).unwrap();
    } else if !remote_stop {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(4), registered.recv())
                .await
                .unwrap()
                .unwrap(),
            2
        );
        assert!(
            ended.try_recv().is_err(),
            "healthy EOF cancelled a generation"
        );
        assert!(history.search("").iter().all(|record| !record.terminal));
        shutdown.send(()).unwrap();
    }
    tokio::time::timeout(Duration::from_secs(if remote_stop { 2 } else { 4 }), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    if blocked_close {
        let termination = gate.termination.lock().unwrap().clone().unwrap();
        assert!(
            futures::poll!(Box::pin(termination.forced())).is_pending(),
            "healthy/unknown cause was forced"
        );
        assert!(
            futures::poll!(Box::pin(termination.completed())).is_pending(),
            "physical close barrier was bypassed"
        );
        assert!(
            futures::poll!(Box::pin(registered.recv())).is_pending(),
            "shutdown recreated the tunnel"
        );
        gate.release();
        tokio::time::timeout(Duration::from_secs(1), termination.completed())
            .await
            .unwrap();
        assert!(futures::poll!(Box::pin(termination.forced())).is_pending());
    }
    let mut finished = vec![ended.recv().await.unwrap(), ended.recv().await.unwrap()];
    finished.sort();
    assert_eq!(finished, ["/0", "/1"]);
    let records = history.search("");
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|record| record.terminal
        && record.failure_kind == Some("cancelled")
        && record.connection == crate::inspection::GenerationState::Disconnected));
    release.cancel();
    backend_stop.cancel();
    backend_task.await.unwrap();
    daemon.await.unwrap();
}
