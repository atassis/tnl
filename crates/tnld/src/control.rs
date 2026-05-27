use std::sync::Arc;

use anyhow::Context as _;
use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use tnl_protocol::transport::server_session_from_ws_generic;
use tnl_protocol::{ControlMsg, ErrorCode, ReattachReq, Session, TunnelCreatedResp};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, error, info, warn};

use crate::registry::Registry;
use crate::serve::{AppState, AuthedToken, SessionHandle};

pub async fn handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<AuthedToken>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| run_session(socket, state, token))
}

async fn run_session(socket: WebSocket, state: AppState, token: AuthedToken) {
    let registry = state.registry.clone();
    let grace_sec = state.session_grace_sec;
    // Build async-tungstenite WS stream from axum WebSocket via the bridge.
    let ws_stream = match axum_ws_to_tungstenite(socket).await {
        Ok(ws) => ws,
        Err(e) => {
            error!("ws conversion failed: {e}");
            return;
        }
    };

    let mut session = server_session_from_ws_generic(ws_stream);
    let sess_state = registry.register_session(token.name.clone());
    let sess_id = sess_state.id.clone();
    info!(session_id = %sess_id, token = %token.name, "control session opened");

    // The CLI opens the control substream first; we accept it.
    let ctrl_stream = match session.accept_stream().await {
        Ok(Some(s)) => s,
        Ok(None) => {
            warn!("session closed before control stream");
            registry.drop_session(&sess_id, grace_sec);
            return;
        }
        Err(e) => {
            error!("accept control stream: {e}");
            registry.drop_session(&sess_id, grace_sec);
            return;
        }
    };

    // Move session into Mutex-wrapped handle and publish in session_handles
    // so the data plane (Task 14) can locate it by session id.
    let session_box: Box<dyn tnl_protocol::transport::Session> = Box::new(session);
    let handle: SessionHandle = Arc::new(tokio::sync::Mutex::new(session_box));
    state.session_handles.insert(sess_id.clone(), handle);

    if let Err(e) = control_loop(ctrl_stream, &registry, &sess_id, &token.name).await {
        warn!(session_id = %sess_id, "control loop ended: {e}");
    }

    state.session_handles.remove(&sess_id);
    registry.drop_session(&sess_id, grace_sec);
    info!(session_id = %sess_id, "control session closed");
}

#[allow(clippy::too_many_lines)]
async fn control_loop(
    mut stream: std::pin::Pin<Box<dyn tnl_protocol::Stream>>,
    registry: &Registry,
    session_id: &str,
    token_name: &str,
) -> anyhow::Result<()> {
    loop {
        let mut lenbuf = [0u8; 4];
        let read = stream.read_exact(&mut lenbuf).await;
        match read {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        let n = u32::from_be_bytes(lenbuf) as usize;
        if n > 1024 * 1024 {
            return Err(anyhow::anyhow!("control message too large: {n}"));
        }
        let mut payload = vec![0u8; n];
        stream.read_exact(&mut payload).await?;
        let msg: ControlMsg = serde_json::from_slice(&payload).context("decode ControlMsg")?;
        debug!(?msg, "control msg in");

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
                        match registry.create_tunnel(&subdomain, session_id, token_name) {
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
                match registry.try_reattach(
                    &tunnel_id.to_string(),
                    &subdomain,
                    token_name,
                    session_id,
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
                        code: ErrorCode::TunnelLost,
                        message: format!("reattach failed: {reason}"),
                    },
                }
            }

            ControlMsg::Heartbeat => ControlMsg::HeartbeatAck,
            ControlMsg::Close => return Ok(()),
            other => ControlMsg::Error {
                code: ErrorCode::Internal,
                message: format!("unexpected message: {other:?}"),
            },
        };

        let out = serde_json::to_vec(&response)?;
        let len = u32::try_from(out.len())
            .context("response too large")?
            .to_be_bytes();
        stream.write_all(&len).await?;
        stream.write_all(&out).await?;
        stream.flush().await?;
    }
}

/// Convert axum's `WebSocket` to an `async_tungstenite::WebSocketStream` that
/// `ws_stream_tungstenite::WsStream` (used in `server_session_from_ws_generic`) can
/// consume.
///
/// We create a `tokio::io::duplex` pipe.  One end (`b`) is wrapped as an
/// `async_tungstenite::WebSocketStream` with `Role::Server` — this is the stream
/// we hand to yamux.  The other end (`a`) is wrapped with `Role::Client` and
/// bridged to axum's `WebSocket` in a background task, forwarding messages in
/// both directions.
async fn axum_ws_to_tungstenite(
    axum_ws: WebSocket,
) -> anyhow::Result<
    async_tungstenite::WebSocketStream<
        async_tungstenite::tokio::TokioAdapter<tokio::io::DuplexStream>,
    >,
> {
    use futures::{SinkExt, StreamExt};

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

    // Bridge: axum_ws (real socket) ↔ client_ws (a side).
    // server_ws (b side) is returned for yamux/ws_stream_tungstenite.
    tokio::spawn(async move {
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
                    Ok(TM::Ping(b)) => {
                        axum::extract::ws::Message::Ping(bytes::Bytes::from(b.to_vec()))
                    }
                    Ok(TM::Pong(b)) => {
                        axum::extract::ws::Message::Pong(bytes::Bytes::from(b.to_vec()))
                    }
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
            () = axum_to_client => {},
            () = client_to_axum => {},
        }
    });

    Ok(server_ws)
}
