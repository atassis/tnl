use std::pin::Pin;

use anyhow::{bail, Context};
use async_tungstenite::tungstenite::handshake::client::generate_key;
use tnl_protocol::{ControlMsg, CreateTunnelReq, Session as _, TunnelCreatedResp};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};

use crate::forwarder::forward;

pub struct ConnectedSession {
    pub session: Box<dyn tnl_protocol::Session>,
    pub control: Pin<Box<dyn tnl_protocol::Stream>>,
    pub hostname: String,
    pub subdomain: String,
    pub tunnel_id: ulid::Ulid,
}

impl std::fmt::Debug for ConnectedSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectedSession")
            .field("hostname", &self.hostname)
            .field("subdomain", &self.subdomain)
            .field("tunnel_id", &self.tunnel_id)
            .finish_non_exhaustive()
    }
}

pub async fn connect_and_create(
    endpoint: &str,
    token: &str,
    subdomain: &str,
) -> anyhow::Result<ConnectedSession> {
    connect_and_create_inner(endpoint, token, Some(subdomain)).await
}

pub async fn connect_and_create_random(
    endpoint: &str,
    token: &str,
) -> anyhow::Result<ConnectedSession> {
    connect_and_create_inner(endpoint, token, None).await
}

async fn connect_and_create_inner(
    endpoint: &str,
    token: &str,
    subdomain: Option<&str>,
) -> anyhow::Result<ConnectedSession> {
    let ws_url = endpoint
        .replace("http://", "ws://")
        .replace("https://", "wss://");
    let url = format!("{}/control", ws_url.trim_end_matches('/'));

    let authority = url
        .parse::<http::Uri>()
        .ok()
        .and_then(|u| u.authority().map(ToString::to_string))
        .unwrap_or_else(|| "tnl-api".to_string());

    let req = http::Request::builder()
        .uri(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Host", authority)
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Key", generate_key())
        .header("Sec-WebSocket-Version", "13")
        .body(())
        .context("build ws upgrade request")?;
    let (ws, _resp) = async_tungstenite::tokio::connect_async(req)
        .await
        .with_context(|| format!("connect to {url}"))?;
    info!(%url, "connected via WSS");

    let mut session = tnl_protocol::transport::client_session_from_ws_generic(ws);

    let mut ctrl: Pin<Box<dyn tnl_protocol::Stream>> = session
        .open_stream()
        .await
        .context("open control substream")?;

    let req = ControlMsg::CreateTunnel(CreateTunnelReq {
        subdomain: subdomain.map(ToString::to_string),
    });
    let payload = serde_json::to_vec(&req)?;
    let len = u32::try_from(payload.len())
        .context("payload too large")?
        .to_be_bytes();
    ctrl.write_all(&len).await?;
    ctrl.write_all(&payload).await?;
    ctrl.flush().await?;

    let mut lenbuf = [0u8; 4];
    ctrl.read_exact(&mut lenbuf).await?;
    let n = u32::from_be_bytes(lenbuf) as usize;
    let mut respbuf = vec![0u8; n];
    ctrl.read_exact(&mut respbuf).await?;
    let resp: ControlMsg = serde_json::from_slice(&respbuf)?;
    let (hostname, subdomain, tunnel_id) = match resp {
        ControlMsg::TunnelCreated(TunnelCreatedResp {
            hostname,
            subdomain,
            tunnel_id,
        }) => (hostname, subdomain, tunnel_id),
        ControlMsg::Error { code, message } => bail!("server error ({code:?}): {message}"),
        other => bail!("unexpected control response: {other:?}"),
    };

    Ok(ConnectedSession {
        session: Box::new(session),
        control: ctrl,
        hostname,
        subdomain,
        tunnel_id,
    })
}

pub async fn run_accept_loop(
    mut session: Box<dyn tnl_protocol::Session>,
    port: u16,
) -> anyhow::Result<()> {
    loop {
        match session.accept_stream().await {
            Ok(Some(stream)) => {
                tokio::spawn(async move {
                    if let Err(e) = forward(stream, port).await {
                        warn!(?e, "forward failed");
                    }
                });
            }
            Ok(None) => {
                info!("session closed cleanly");
                return Ok(());
            }
            Err(e) => {
                warn!(?e, "accept_stream error");
                return Err(e);
            }
        }
    }
}
