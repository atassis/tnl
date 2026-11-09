use std::{sync::Arc, time::Duration};

use anyhow::Context as _;
use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use tnl_protocol::control::MAX_CONTROL_MESSAGE_BYTES;
use tnl_protocol::request_metadata::{
    DataStreamProtocol, DATA_STREAM_PROTOCOL_HEADER, HTTP1_METADATA_V1,
};
use tnl_protocol::transport::server_session_from_ws_generic;
use tnl_protocol::{ClosingReason, ControlMsg, ErrorCode, ReattachReq, Session, TunnelCreatedResp};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, info, warn};

use crate::registry::{HeartbeatStatus, Registry};
use crate::serve::{AppState, AuthedToken, SessionHandle};

const INITIAL_REGISTRATION_BUDGET: Duration = Duration::from_secs(20);
const CONTROL_SILENCE_LEASE: Duration = Duration::from_secs(45);
const CONTROL_RESPONSE_BUDGET: Duration = Duration::from_secs(5);

pub async fn handler(
    ws: WebSocketUpgrade,
    headers: axum::http::HeaderMap,
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<AuthedToken>,
) -> impl IntoResponse {
    if !tnl_protocol::control::has_control_protocol(&headers) {
        return (
            axum::http::StatusCode::UPGRADE_REQUIRED,
            [(
                tnl_protocol::control::CONTROL_PROTOCOL_HEADER,
                tnl_protocol::control::CONTROL_PROTOCOL_VERSION,
            )],
            "incompatible control protocol; heartbeat-v1 required",
        )
            .into_response();
    }
    let protocol = DataStreamProtocol::from_offer(&headers);
    let mut response = ws
        .on_upgrade(move |socket| run_session(socket, state, token, protocol))
        .into_response();
    response.headers_mut().insert(
        tnl_protocol::control::CONTROL_PROTOCOL_HEADER,
        axum::http::HeaderValue::from_static(tnl_protocol::control::CONTROL_PROTOCOL_VERSION),
    );
    if protocol == DataStreamProtocol::Http1MetadataV1 {
        response.headers_mut().insert(
            DATA_STREAM_PROTOCOL_HEADER,
            axum::http::HeaderValue::from_static(HTTP1_METADATA_V1),
        );
    }
    response
}

async fn run_session(
    socket: WebSocket,
    state: AppState,
    token: AuthedToken,
    data_stream_protocol: DataStreamProtocol,
) {
    let (ws_stream, bridge_stream) = websocket_bridge_pair().await;
    let stop_bridge = tokio_util::sync::CancellationToken::new();
    // Neither future is detached. Cancelling this upgrade drops both physical
    // owners; normal completion observes both session and bridge IO release.
    tokio::join!(
        run_transport_session(
            ws_stream,
            &state,
            &token,
            data_stream_protocol,
            &stop_bridge
        ),
        bridge_messages(socket, bridge_stream, &stop_bridge),
    );
}

type BridgeWebSocket = async_tungstenite::WebSocketStream<
    async_tungstenite::tokio::TokioAdapter<tokio::io::DuplexStream>,
>;

async fn run_transport_session(
    ws_stream: BridgeWebSocket,
    state: &AppState,
    token: &AuthedToken,
    data_stream_protocol: DataStreamProtocol,
    stop_bridge: &tokio_util::sync::CancellationToken,
) {
    let registry = &state.registry;
    let grace_sec = state.session_grace_sec;
    let session = server_session_from_ws_generic(ws_stream);
    let termination = session.termination();
    let sess_state = registry.register_session(token.name.clone());
    let sess_id = sess_state.id.clone();
    info!(session_id = %sess_id, token = %token.name, "control session opened");

    let initial_deadline = tokio::time::Instant::now() + INITIAL_REGISTRATION_BUDGET;
    let result = serve_control_session(
        session,
        state,
        token,
        &sess_state,
        data_stream_protocol,
        initial_deadline,
    )
    .await;
    // Invalidate routing AND cloned handles before awaiting any cleanup. This
    // revokes pending admission without forcing healthy physical IO/draining.
    registry.drop_session(&sess_id, grace_sec);
    state.session_handles.remove(&sess_id);
    let mut failed = result.as_ref().is_err_and(is_physical_session_failure);
    if let Err(error) = result {
        warn!(session_id = %sess_id, %error, "control loop ended");
    }
    if !failed {
        // Physical cleanup can wake the control reader as EOF before publishing
        // its cause. Serialize with the driver instead of taking a watch snapshot
        // or allowing a close request to hide an in-progress physical failure.
        failed = matches!(
            termination.close_and_observe_cause().await,
            tnl_protocol::OpenStreamError::Io
                | tnl_protocol::OpenStreamError::Protocol
                | tnl_protocol::OpenStreamError::InboundOverflow
                | tnl_protocol::OpenStreamError::Capacity
        );
    }
    finish_transport(&termination, stop_bridge, failed).await;
    info!(session_id = %sess_id, "control session closed");
}

async fn finish_transport(
    termination: &tnl_protocol::SessionTermination,
    stop_bridge: &tokio_util::sync::CancellationToken,
    failed: bool,
) {
    if failed {
        let cleanup = termination.terminate_failed();
        tokio::pin!(cleanup);
        tokio::select! {
            () = &mut cleanup => {},
            () = termination.forced() => {
                // A stalled real-socket send can outlive the yamux duplex end.
                // Force releases BOTH physical owners, not just the driver.
                stop_bridge.cancel();
                cleanup.await;
            }
        }
        stop_bridge.cancel();
    } else {
        // Normal EOF/Close retains graceful queued-write semantics. There is no
        // blanket close timeout; blocked healthy OS IO may keep this pending.
        termination.request_close();
        termination.completed().await;
    }
    // A healthy driver has only flushed into the duplex. Let the bridge drain
    // those frames to the real socket and finish on duplex EOF; cancellation
    // here could discard a queued Close acknowledgment.
}

async fn serve_control_session(
    mut session: tnl_protocol::transport::YamuxSession,
    state: &AppState,
    token: &AuthedToken,
    owner: &crate::registry::SessionState,
    data_stream_protocol: DataStreamProtocol,
    initial_deadline: tokio::time::Instant,
) -> anyhow::Result<()> {
    let sess_id = &owner.id;
    // Bootstrap and complete initial registration share one deadline.
    let ctrl_stream = tokio::time::timeout_at(initial_deadline, session.accept_stream())
        .await
        .map_err(|_| ControlDeadlineFailure)??;
    if tokio::time::Instant::now() >= initial_deadline {
        return Err(ControlDeadlineFailure.into());
    }
    let Some(ctrl_stream) = ctrl_stream else {
        return Ok(());
    };
    let handle = SessionHandle::new(
        owner,
        Arc::new(tokio::sync::Mutex::new(Box::new(session))),
        data_stream_protocol,
    );
    state.session_handles.insert(sess_id.to_owned(), handle);
    control_loop(
        ctrl_stream,
        &state.registry,
        sess_id,
        &token.name,
        data_stream_protocol,
        initial_deadline,
    )
    .await
}

#[derive(Debug, thiserror::Error)]
#[error("control lifecycle deadline expired")]
struct ControlDeadlineFailure;

#[derive(Debug, thiserror::Error)]
#[error("invalid control framing, schema or binding")]
struct ControlProtocolFailure;

fn is_physical_session_failure(error: &anyhow::Error) -> bool {
    error.is::<std::io::Error>()
        || error.is::<ControlProtocolFailure>()
        || error.is::<ControlDeadlineFailure>()
        || matches!(
            error.downcast_ref::<tnl_protocol::OpenStreamError>(),
            Some(
                tnl_protocol::OpenStreamError::Io
                    | tnl_protocol::OpenStreamError::Protocol
                    | tnl_protocol::OpenStreamError::InboundOverflow
                    | tnl_protocol::OpenStreamError::Capacity
            )
        )
}

#[allow(clippy::too_many_lines)]
async fn control_loop(
    mut stream: std::pin::Pin<Box<dyn tnl_protocol::Stream>>,
    registry: &Registry,
    session_id: &str,
    token_name: &str,
    data_stream_protocol: DataStreamProtocol,
    initial_deadline: tokio::time::Instant,
) -> anyhow::Result<()> {
    let mut bound_tunnel = None;
    let mut read_deadline = initial_deadline;
    let mut remote_stop_sent = false;
    loop {
        // One timeout covers prefix AND payload. Abandon this stream on expiry;
        // never restart a decoder after cancelling a partly consumed frame.
        let msg = tokio::time::timeout_at(read_deadline, read_control(&mut stream))
            .await
            .map_err(|_| ControlDeadlineFailure)??;
        let received_at = tokio::time::Instant::now();
        if received_at >= read_deadline {
            return Err(ControlDeadlineFailure.into());
        }
        let Some(msg) = msg else {
            return Ok(());
        };
        debug!("control message decoded");
        if remote_stop_sent && !matches!(msg, ControlMsg::Close) {
            return Err(ControlProtocolFailure.into());
        }
        let registering = bound_tunnel.is_none();
        if !registering
            && matches!(
                msg,
                ControlMsg::CreateTunnel(_)
                    | ControlMsg::ReattachTunnel(_)
                    | ControlMsg::CreateProtectedTunnel(_)
                    | ControlMsg::ReattachProtectedTunnel(_)
            )
        {
            // A control owner binds once; retries after a rejected registration
            // are allowed, but cannot replace a successful original binding.
            return Err(ControlProtocolFailure.into());
        }
        let response = match msg {
            ControlMsg::CreateTunnel(req) => {
                let resolved = req.subdomain.map_or_else(
                    || {
                        let mut rng = rand::thread_rng();
                        crate::random_subdomain::generate_unique(&mut rng, registry)
                    },
                    Some,
                );
                match resolved {
                    None => ControlMsg::Error {
                        code: ErrorCode::Internal,
                        message: "could not generate a random subdomain; try again".to_string(),
                    },
                    Some(subdomain) => {
                        match registry.create_tunnel_bound(
                            &subdomain,
                            session_id,
                            token_name,
                            data_stream_protocol,
                            None,
                        ) {
                            Ok(t) => ControlMsg::TunnelCreated(TunnelCreatedResp {
                                tunnel_id: t
                                    .id
                                    .parse::<ulid::Ulid>()
                                    .context("registry produced non-ulid tunnel id")?,
                                hostname: t.hostname.clone(),
                                subdomain: t.subdomain.clone(),
                            }),
                            Err(crate::registry::RegistryError::SubdomainTaken(_)) => {
                                ControlMsg::Error {
                                    code: ErrorCode::SubdomainTaken,
                                    message: format!("subdomain '{subdomain}' already in use"),
                                }
                            }
                            Err(crate::registry::RegistryError::InvalidSubdomain(_)) => {
                                ControlMsg::Error {
                                    code: ErrorCode::InvalidSubdomain,
                                    message: format!("invalid subdomain '{subdomain}'"),
                                }
                            }
                            Err(e) => ControlMsg::Error {
                                code: ErrorCode::Internal,
                                message: e.to_string(),
                            },
                        }
                    }
                }
            }

            ControlMsg::ReattachTunnel(ReattachReq {
                tunnel_id,
                subdomain,
            }) => {
                match registry.try_reattach_bound(
                    &tunnel_id.to_string(),
                    &subdomain,
                    token_name,
                    session_id,
                    Some(data_stream_protocol),
                    None,
                ) {
                    Ok(t) => ControlMsg::TunnelCreated(TunnelCreatedResp {
                        tunnel_id: t
                            .id
                            .parse::<ulid::Ulid>()
                            .context("registry produced non-ulid tunnel id")?,
                        hostname: t.hostname.clone(),
                        subdomain: t.subdomain.clone(),
                    }),
                    Err(reason) => ControlMsg::Error {
                        code: reattach_error_code(reason, false),
                        message: "public reattach rejected".into(),
                    },
                }
            }

            ControlMsg::CreateProtectedTunnel(req) => {
                if data_stream_protocol == DataStreamProtocol::Http1MetadataV1 {
                    let access = crate::access::CompiledAccess::new(Arc::new(req.access));
                    match access {
                        Err(_) => ControlMsg::Error {
                            code: ErrorCode::InvalidAccessPolicy,
                            message: "invalid access policy".into(),
                        },
                        Ok(access) => {
                            let subdomain = req.subdomain.or_else(|| {
                                crate::random_subdomain::generate_unique(
                                    &mut rand::thread_rng(),
                                    registry,
                                )
                            });
                            match subdomain {
                                None => ControlMsg::Error {
                                    code: ErrorCode::Internal,
                                    message: "could not allocate tunnel name".into(),
                                },
                                Some(subdomain) => match registry.create_tunnel_bound(
                                    &subdomain,
                                    session_id,
                                    token_name,
                                    data_stream_protocol,
                                    Some(Arc::new(access)),
                                ) {
                                    Ok(tunnel) => protected_created(&tunnel)?,
                                    Err(crate::registry::RegistryError::SubdomainTaken(_)) => {
                                        ControlMsg::Error {
                                            code: ErrorCode::SubdomainTaken,
                                            message: "subdomain already in use".into(),
                                        }
                                    }
                                    Err(crate::registry::RegistryError::InvalidSubdomain(_)) => {
                                        ControlMsg::Error {
                                            code: ErrorCode::InvalidSubdomain,
                                            message: "invalid subdomain".into(),
                                        }
                                    }
                                    Err(_) => ControlMsg::Error {
                                        code: ErrorCode::Internal,
                                        message: "registry unavailable".into(),
                                    },
                                },
                            }
                        }
                    }
                } else {
                    ControlMsg::Error {
                        code: ErrorCode::UnsupportedProtocol,
                        message: "protected tunnels require metadata capability".into(),
                    }
                }
            }
            ControlMsg::ReattachProtectedTunnel(req) => {
                if data_stream_protocol == DataStreamProtocol::Http1MetadataV1 {
                    match registry.try_reattach_bound(
                        &req.tunnel_id.to_string(),
                        &req.subdomain,
                        token_name,
                        session_id,
                        Some(data_stream_protocol),
                        Some(&req.policy_fingerprint),
                    ) {
                        Ok(tunnel) => protected_created(&tunnel)?,
                        Err(reason) => ControlMsg::Error {
                            code: reattach_error_code(reason, true),
                            message: "protected reattach rejected".into(),
                        },
                    }
                } else {
                    ControlMsg::Error {
                        code: ErrorCode::UnsupportedProtocol,
                        message: "protected tunnels require metadata capability".into(),
                    }
                }
            }
            ControlMsg::Heartbeat {
                tunnel_id,
                sequence,
            } => {
                if bound_tunnel != Some(tunnel_id) {
                    return Err(ControlProtocolFailure.into());
                }
                match registry.heartbeat_status(&tunnel_id.to_string(), session_id)? {
                    HeartbeatStatus::Active => ControlMsg::HeartbeatAck {
                        tunnel_id,
                        sequence,
                    },
                    HeartbeatStatus::Removed => ControlMsg::Closing {
                        reason: ClosingReason::RemoteStop,
                    },
                    HeartbeatStatus::InvalidSession => return Err(ControlProtocolFailure.into()),
                }
            }
            ControlMsg::Close => {
                // Session-scoped removal cannot delete a newer reattached binding.
                registry.close_session(session_id)?;
                ControlMsg::Closing {
                    reason: ClosingReason::ClientShutdown,
                }
            }
            _ => return Err(ControlProtocolFailure.into()),
        };

        match &response {
            ControlMsg::TunnelCreated(created) => bound_tunnel = Some(created.tunnel_id),
            ControlMsg::ProtectedTunnelCreated(created) => bound_tunnel = Some(created.tunnel_id),
            _ => {}
        }
        let valid = !matches!(
            response,
            ControlMsg::Error { .. }
                | ControlMsg::Closing {
                    reason: ClosingReason::RemoteStop
                }
        );
        if valid && bound_tunnel.is_some() && !remote_stop_sent {
            read_deadline = received_at + CONTROL_SILENCE_LEASE;
        }
        let response_deadline =
            (tokio::time::Instant::now() + CONTROL_RESPONSE_BUDGET).min(if registering {
                initial_deadline
            } else {
                read_deadline
            });
        tokio::time::timeout_at(response_deadline, write_control(&mut stream, &response))
            .await
            .map_err(|_| ControlDeadlineFailure)??;
        if tokio::time::Instant::now() >= response_deadline {
            return Err(ControlDeadlineFailure.into());
        }
        match response {
            ControlMsg::Closing {
                reason: ClosingReason::ClientShutdown,
            } => return Ok(()),
            ControlMsg::Closing {
                reason: ClosingReason::RemoteStop,
            } => {
                // Do not race the unread stop with whole-session GoAway/EOF.
                // Keep the existing lease while the peer consumes it and closes;
                // stop itself and subsequent traffic cannot extend that lease.
                remote_stop_sent = true;
            }
            _ => {}
        }
    }
}

async fn read_control(
    stream: &mut std::pin::Pin<Box<dyn tnl_protocol::Stream>>,
) -> anyhow::Result<Option<ControlMsg>> {
    let mut lenbuf = [0u8; 4];
    match stream.read_exact(&mut lenbuf).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let n = u32::from_be_bytes(lenbuf) as usize;
    if n > MAX_CONTROL_MESSAGE_BYTES {
        return Err(ControlProtocolFailure.into());
    }
    let mut payload = zeroize::Zeroizing::new(vec![0u8; n]);
    stream.read_exact(&mut payload).await?;
    serde_json::from_slice(&payload).map(Some).map_err(|error| {
        tracing::debug!(class = ?error.classify(), line = error.line(), column = error.column(), "invalid control message");
        ControlProtocolFailure.into()
    })
}

async fn write_control(
    stream: &mut std::pin::Pin<Box<dyn tnl_protocol::Stream>>,
    response: &ControlMsg,
) -> anyhow::Result<()> {
    let out = serde_json::to_vec(response)?;
    let len = u32::try_from(out.len())
        .context("response too large")?
        .to_be_bytes();
    stream.write_all(&len).await?;
    stream.write_all(&out).await?;
    stream.flush().await?;
    Ok(())
}

fn reattach_error_code(reason: &str, protected: bool) -> ErrorCode {
    match reason {
        "not_found" => ErrorCode::TunnelLost,
        "already_active" => ErrorCode::TunnelAlreadyActive,
        "not_owner" => ErrorCode::Unauthorized,
        "unavailable" if protected => ErrorCode::AccessUnavailable,
        "mode_mismatch" => ErrorCode::UnsupportedProtocol,
        "subdomain_mismatch" if !protected => ErrorCode::TunnelNotFound,
        "subdomain_mismatch" | "policy_mismatch" => ErrorCode::AccessPolicyMismatch,
        _ => ErrorCode::Internal,
    }
}

fn protected_created(tunnel: &crate::registry::Tunnel) -> anyhow::Result<ControlMsg> {
    let access = tunnel
        .access
        .as_ref()
        .context("protected tunnel missing admission state")?;
    Ok(ControlMsg::ProtectedTunnelCreated(
        tnl_protocol::messages::ProtectedTunnelCreatedResp {
            tunnel_id: tunnel.id.parse().context("invalid registry tunnel id")?,
            hostname: tunnel.hostname.clone(),
            subdomain: tunnel.subdomain.clone(),
            policy_fingerprint: access.fingerprint().into(),
        },
    ))
}

/// Create the tungstenite duplex pair used by the axum WS relay. The server-role
/// end goes to yamux; the client-role end is owned by `bridge_messages` alongside
/// the real axum socket. Both relay directions run in the upgrade task itself.
async fn websocket_bridge_pair() -> (BridgeWebSocket, BridgeWebSocket) {
    let (a, b) = tokio::io::duplex(64 * 1024);
    // `b` is the server-role end that yamux/WsStream will consume.
    let server_ws = async_tungstenite::WebSocketStream::from_raw_socket(
        async_tungstenite::tokio::TokioAdapter::new(b),
        async_tungstenite::tungstenite::protocol::Role::Server,
        None,
    )
    .await;
    // `a` is the client-role end that relays frames to/from axum.
    let client_ws = async_tungstenite::WebSocketStream::from_raw_socket(
        async_tungstenite::tokio::TokioAdapter::new(a),
        async_tungstenite::tungstenite::protocol::Role::Client,
        None,
    )
    .await;

    (server_ws, client_ws)
}

/// The cancellation arm surrounds both complete relay futures, so it remains
/// observable while either direction is blocked inside `SinkExt::send`.
async fn bridge_messages<A, C, AE, CE>(
    axum_ws: A,
    client_ws: C,
    cancel: &tokio_util::sync::CancellationToken,
) where
    A: futures::Sink<axum::extract::ws::Message, Error = AE>
        + futures::Stream<Item = Result<axum::extract::ws::Message, AE>>
        + Unpin,
    C: futures::Sink<async_tungstenite::tungstenite::Message, Error = CE>
        + futures::Stream<Item = Result<async_tungstenite::tungstenite::Message, CE>>
        + Unpin,
{
    use futures::{SinkExt, StreamExt};
    let (mut client_sink, mut client_stream) = client_ws.split();
    let (mut axum_sink, mut axum_stream) = axum_ws.split();

    // axum → client_ws (server_ws sees these as incoming)
    let axum_to_client = async {
        while let Some(msg) = axum_stream.next().await {
            let m = match msg {
                Ok(axum::extract::ws::Message::Binary(b)) => {
                    async_tungstenite::tungstenite::Message::Binary(b.to_vec().into())
                }
                Ok(axum::extract::ws::Message::Text(t)) => {
                    async_tungstenite::tungstenite::Message::Text(t.to_string().into())
                }
                Ok(axum::extract::ws::Message::Ping(b)) => {
                    async_tungstenite::tungstenite::Message::Ping(b.to_vec().into())
                }
                Ok(axum::extract::ws::Message::Pong(b)) => {
                    async_tungstenite::tungstenite::Message::Pong(b.to_vec().into())
                }
                Ok(axum::extract::ws::Message::Close(_)) | Err(_) => break,
            };
            if client_sink.send(m).await.is_err() {
                break;
            }
        }
    };

    // client_ws → axum (server_ws wrote these)
    let client_to_axum = async {
        while let Some(msg) = client_stream.next().await {
            use async_tungstenite::tungstenite::Message as TM;
            let m = match msg {
                Ok(TM::Binary(b)) => {
                    axum::extract::ws::Message::Binary(bytes::Bytes::from(b.to_vec()))
                }
                Ok(TM::Text(t)) => axum::extract::ws::Message::Text(t.to_string().into()),
                Ok(TM::Ping(b)) => axum::extract::ws::Message::Ping(bytes::Bytes::from(b.to_vec())),
                Ok(TM::Pong(b)) => axum::extract::ws::Message::Pong(bytes::Bytes::from(b.to_vec())),
                Ok(TM::Close(_)) | Err(_) => break,
                Ok(TM::Frame(_)) => continue,
            };
            if axum_sink.send(m).await.is_err() {
                break;
            }
        }
    };

    // Use `select!` rather than `join!`: when one half exits (e.g. real WS
    // sees Close/FIN), the other future is dropped, which drops its captured
    // half of `client_ws.split()` and `axum_ws.split()`. Both halves of
    // `client_ws` going away closes the duplex `a` end, which makes the
    // daemon-side `server_ws` read EOF — the only signal that propagates
    // through the yamux driver into `control_loop` and triggers cleanup.
    // With `join!` only one half drops on Close, the duplex stays open, and
    // the daemon never releases the session (subdomain leaks until restart).
    tokio::select! {
        () = cancel.cancelled() => {},
        () = axum_to_client => {},
        () = client_to_axum => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reattach_errors_preserve_active_owner_and_authority_classification() {
        assert_eq!(
            reattach_error_code("already_active", false),
            ErrorCode::TunnelAlreadyActive
        );
        assert_eq!(
            reattach_error_code("already_active", true),
            ErrorCode::TunnelAlreadyActive
        );
        assert_eq!(
            reattach_error_code("not_found", false),
            ErrorCode::TunnelLost
        );
        assert_eq!(
            reattach_error_code("not_owner", false),
            ErrorCode::Unauthorized
        );
        assert_eq!(
            reattach_error_code("policy_mismatch", true),
            ErrorCode::AccessPolicyMismatch
        );
    }
}

#[cfg(test)]
mod bridge_tests;

#[cfg(test)]
mod activation_tests;

#[cfg(test)]
mod liveness_tests;
