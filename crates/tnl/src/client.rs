pub(crate) mod control;
pub(crate) use control::close_control;
pub use control::ControlError;
use control::{read_msg, write_msg};
use std::pin::Pin;

use anyhow::Context;
use async_tungstenite::tungstenite::handshake::client::generate_key;
use tnl_protocol::request_metadata::{
    DataStreamProtocol, DATA_STREAM_PROTOCOL_HEADER, HTTP1_METADATA_V1,
};
#[cfg(test)]
use tnl_protocol::ErrorCode;
use tnl_protocol::{
    messages::ReattachReq, ControlMsg, CreateTunnelReq, Session as _, TunnelCreatedResp,
};
#[cfg(test)]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};

pub struct ConnectedSession {
    pub data_stream_protocol: DataStreamProtocol,
    pub session: Box<dyn tnl_protocol::Session>,
    pub control: Pin<Box<dyn tnl_protocol::Stream>>,
    pub hostname: String,
    pub subdomain: String,
    pub tunnel_id: ulid::Ulid,
}

impl std::fmt::Debug for ConnectedSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectedSession")
            .field("data_stream_protocol", &self.data_stream_protocol)
            .field("hostname", &self.hostname)
            .field("subdomain", &self.subdomain)
            .field("tunnel_id", &self.tunnel_id)
            .finish_non_exhaustive()
    }
}

/// Dial the WS control endpoint and open the control substream.
/// Returns a type-erased session plus the pinned control stream.
async fn dial_and_open_control(
    endpoint: &str,
    token: &str,
    attempt: &ConnectionAttempt,
) -> anyhow::Result<(
    Box<dyn tnl_protocol::Session>,
    Pin<Box<dyn tnl_protocol::Stream>>,
    DataStreamProtocol,
)> {
    dial_and_open_control_with_connector(endpoint, token, None, attempt).await
}

async fn dial_and_open_control_with_connector(
    endpoint: &str,
    token: &str,
    connector: Option<tokio_rustls::TlsConnector>,
    attempt: &ConnectionAttempt,
) -> anyhow::Result<(
    Box<dyn tnl_protocol::Session>,
    Pin<Box<dyn tnl_protocol::Stream>>,
    DataStreamProtocol,
)> {
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
        .header(
            tnl_protocol::control::CONTROL_PROTOCOL_HEADER,
            tnl_protocol::control::CONTROL_PROTOCOL_VERSION,
        )
        .header(DATA_STREAM_PROTOCOL_HEADER, HTTP1_METADATA_V1)
        .header("Host", authority)
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Key", generate_key())
        .header("Sec-WebSocket-Version", "13")
        .body(())
        .context("build ws upgrade request")?;
    let dial = async_tungstenite::tokio::connect_async_with_tls_connector(req, connector);
    #[cfg(test)]
    let dial = test_connect_boundary(ConnectBoundary::Dial, dial);
    let (ws, resp) = Box::pin(dial).await
        .map_err(|error| {
            if matches!(&error, async_tungstenite::tungstenite::Error::Http(response) if response.status() == http::StatusCode::UPGRADE_REQUIRED) {
                anyhow::Error::new(tnl_protocol::control::ControlProtocolIncompatible)
            } else {
                anyhow::Error::new(error).context("connect to control endpoint")
            }
        })?;
    if !tnl_protocol::control::has_control_protocol(resp.headers()) {
        return Err(tnl_protocol::control::ControlProtocolIncompatible.into());
    }
    let data_stream_protocol = DataStreamProtocol::from_echo(resp.headers())?;
    info!(%url, "connected via WSS");

    #[cfg(not(test))]
    let mut session = tnl_protocol::transport::client_session_from_ws_generic(ws);
    #[cfg(test)]
    let mut session = close_gate_tests::session_from_ws(ws);
    attempt.track(session.termination());
    let open = session.open_stream();
    #[cfg(test)]
    let open = test_connect_boundary(ConnectBoundary::Open, open);
    let ctrl: Pin<Box<dyn tnl_protocol::Stream>> = open.await.context("open control substream")?;
    Ok((
        Box::new(session) as Box<dyn tnl_protocol::Session>,
        ctrl,
        data_stream_protocol,
    ))
}

/// Run-owned immutable policy. Prepared once before the async runtime and
/// reused for protected reattach/recreation; no password or file is retained.
#[derive(Debug, Clone, Default)]
pub struct TunnelOptions {
    pub access: Option<std::sync::Arc<tnl_protocol::access::VisitorAccessPolicyV1>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProtectedControlError {
    #[error("protected endpoint must use verified HTTPS")]
    Endpoint,
    #[error("daemon did not acknowledge the required metadata capability")]
    Capability,
    #[error("daemon did not acknowledge the exact protected tunnel policy/binding")]
    Acknowledgment,
    #[error("protected control response exceeds size bound")]
    Framing,
    #[error("invalid protected control response schema")]
    Schema,
    #[error("protected tunnel rejected ({0:?})")]
    Server(tnl_protocol::ErrorCode),
}

// Only protocol/schema failures become terminal protected errors. Keep genuine
// I/O and TLS transport errors intact for the existing same-mode retry policy.
fn protected_response_error(error: anyhow::Error) -> anyhow::Error {
    let protected = match error.downcast_ref::<ControlError>() {
        Some(ControlError::Framing) => Some(ProtectedControlError::Framing),
        Some(ControlError::Schema) => Some(ProtectedControlError::Schema),
        _ => match error.downcast_ref::<tnl_protocol::request_metadata::RequestMetadataError>() {
            Some(tnl_protocol::request_metadata::RequestMetadataError::Negotiation) => {
                Some(ProtectedControlError::Capability)
            }
            _ => None,
        },
    };
    protected.map_or(error, anyhow::Error::new)
}

fn require_protected_capability(protocol: DataStreamProtocol) -> Result<(), ProtectedControlError> {
    if protocol != DataStreamProtocol::Http1MetadataV1 {
        return Err(ProtectedControlError::Capability);
    }
    Ok(())
}

fn validate_public_ack(response: ControlMsg) -> Result<TunnelCreatedResp, ControlError> {
    match response {
        ControlMsg::TunnelCreated(ack) => Ok(ack),
        ControlMsg::Error { code, .. } => Err(ControlError::Server(code)),
        _ => Err(ControlError::Acknowledgment),
    }
}

fn validate_protected_ack(
    response: ControlMsg,
    expected: &str,
    binding: Option<(ulid::Ulid, &str)>,
) -> Result<tnl_protocol::messages::ProtectedTunnelCreatedResp, ProtectedControlError> {
    match response {
        ControlMsg::ProtectedTunnelCreated(ack)
            if ack.policy_fingerprint == expected
                && binding.is_none_or(|(id, sub)| ack.tunnel_id == id && ack.subdomain == sub) =>
        {
            Ok(ack)
        }
        ControlMsg::Error { code, .. } => Err(ProtectedControlError::Server(code)),
        _ => Err(ProtectedControlError::Acknowledgment),
    }
}

// Scope test CA roots to one task driving the real shared reconnect loop. This
// is absent in production and never disables certificate verification.
#[cfg(test)]
tokio::task_local! {
    static TEST_TLS_CONNECTOR: tokio_rustls::TlsConnector;
}

pub async fn connect_and_create_with_options(
    endpoint: &str,
    token: &str,
    subdomain: Option<&str>,
    options: &TunnelOptions,
) -> anyhow::Result<ConnectedSession> {
    let attempt = ConnectionAttempt::new();
    attempt
        .finish(connect_and_create_in_attempt(
            endpoint, token, subdomain, options, &attempt,
        ))
        .await
}

pub(crate) async fn connect_and_create_in_attempt(
    endpoint: &str,
    token: &str,
    subdomain: Option<&str>,
    options: &TunnelOptions,
    attempt: &ConnectionAttempt,
) -> anyhow::Result<ConnectedSession> {
    #[cfg(not(test))]
    let connector = None;
    #[cfg(test)]
    let connector = TEST_TLS_CONNECTOR.try_with(Clone::clone).ok();
    let result = Box::pin(connect_and_create_with_connector_inner(
        endpoint, token, subdomain, options, connector, attempt,
    ))
    .await;
    attempt
        .registration_finished(
            result
                .as_ref()
                .is_err_and(super::reconnect::is_physical_session_failure),
        )
        .await;
    result
}

#[cfg(test)]
async fn connect_and_create_with_connector(
    endpoint: &str,
    token: &str,
    subdomain: Option<&str>,
    options: &TunnelOptions,
    connector: Option<tokio_rustls::TlsConnector>,
) -> anyhow::Result<ConnectedSession> {
    let attempt = ConnectionAttempt::new();
    let result = attempt
        .finish(Box::pin(connect_and_create_with_connector_inner(
            endpoint, token, subdomain, options, connector, &attempt,
        )))
        .await;
    attempt
        .registration_finished(
            result
                .as_ref()
                .is_err_and(super::reconnect::is_physical_session_failure),
        )
        .await;
    result
}

async fn connect_and_create_with_connector_inner(
    endpoint: &str,
    token: &str,
    subdomain: Option<&str>,
    options: &TunnelOptions,
    connector: Option<tokio_rustls::TlsConnector>,
    attempt: &ConnectionAttempt,
) -> anyhow::Result<ConnectedSession> {
    let Some(policy) = &options.access else {
        return connect_and_create_inner(endpoint, token, subdomain, attempt).await;
    };
    crate::access::validate_protected_endpoint(endpoint)
        .map_err(|_| ProtectedControlError::Endpoint)?;
    let (session, mut control, data_stream_protocol) =
        dial_and_open_control_with_connector(endpoint, token, connector, attempt)
            .await
            .map_err(protected_response_error)?;
    require_protected_capability(data_stream_protocol)?;
    let fingerprint = policy.fingerprint()?;
    write_msg(
        &mut control,
        &ControlMsg::CreateProtectedTunnel(tnl_protocol::messages::CreateProtectedTunnelReq {
            subdomain: subdomain.map(str::to_owned),
            access: policy.as_ref().clone(),
        }),
    )
    .await?;
    let ack = validate_protected_ack(
        read_msg(&mut control)
            .await
            .map_err(protected_response_error)?,
        &fingerprint,
        None,
    )?;
    if subdomain.is_some_and(|sub| sub != ack.subdomain) {
        return Err(ProtectedControlError::Acknowledgment.into());
    }
    Ok(ConnectedSession {
        session,
        control,
        data_stream_protocol,
        tunnel_id: ack.tunnel_id,
        hostname: ack.hostname,
        subdomain: ack.subdomain,
    })
}

pub async fn connect_and_reattach_with_options(
    endpoint: &str,
    token: &str,
    tunnel_id: ulid::Ulid,
    subdomain: &str,
    options: &TunnelOptions,
) -> anyhow::Result<ConnectedSession> {
    let attempt = ConnectionAttempt::new();
    attempt
        .finish(connect_and_reattach_in_attempt(
            endpoint, token, tunnel_id, subdomain, options, &attempt,
        ))
        .await
}

pub(crate) async fn connect_and_reattach_in_attempt(
    endpoint: &str,
    token: &str,
    tunnel_id: ulid::Ulid,
    subdomain: &str,
    options: &TunnelOptions,
    attempt: &ConnectionAttempt,
) -> anyhow::Result<ConnectedSession> {
    #[cfg(not(test))]
    let connector = None;
    #[cfg(test)]
    let connector = TEST_TLS_CONNECTOR.try_with(Clone::clone).ok();
    let result = Box::pin(connect_and_reattach_with_connector_inner(
        endpoint, token, tunnel_id, subdomain, options, connector, attempt,
    ))
    .await;
    attempt
        .registration_finished(
            result
                .as_ref()
                .is_err_and(super::reconnect::is_physical_session_failure),
        )
        .await;
    result
}

#[cfg(test)]
async fn connect_and_reattach_with_connector(
    endpoint: &str,
    token: &str,
    tunnel_id: ulid::Ulid,
    subdomain: &str,
    options: &TunnelOptions,
    connector: Option<tokio_rustls::TlsConnector>,
) -> anyhow::Result<ConnectedSession> {
    let attempt = ConnectionAttempt::new();
    let result = attempt
        .finish(Box::pin(connect_and_reattach_with_connector_inner(
            endpoint, token, tunnel_id, subdomain, options, connector, &attempt,
        )))
        .await;
    attempt
        .registration_finished(
            result
                .as_ref()
                .is_err_and(super::reconnect::is_physical_session_failure),
        )
        .await;
    result
}

async fn connect_and_reattach_with_connector_inner(
    endpoint: &str,
    token: &str,
    tunnel_id: ulid::Ulid,
    subdomain: &str,
    options: &TunnelOptions,
    connector: Option<tokio_rustls::TlsConnector>,
    attempt: &ConnectionAttempt,
) -> anyhow::Result<ConnectedSession> {
    let Some(policy) = &options.access else {
        return connect_and_reattach_inner(endpoint, token, tunnel_id, subdomain, attempt).await;
    };
    crate::access::validate_protected_endpoint(endpoint)
        .map_err(|_| ProtectedControlError::Endpoint)?;
    let (session, mut control, data_stream_protocol) =
        dial_and_open_control_with_connector(endpoint, token, connector, attempt)
            .await
            .map_err(protected_response_error)?;
    require_protected_capability(data_stream_protocol)?;
    let fingerprint = policy.fingerprint()?;
    write_msg(
        &mut control,
        &ControlMsg::ReattachProtectedTunnel(tnl_protocol::messages::ReattachProtectedTunnelReq {
            tunnel_id,
            subdomain: subdomain.into(),
            policy_fingerprint: fingerprint.clone(),
        }),
    )
    .await?;
    let ack = validate_protected_ack(
        read_msg(&mut control)
            .await
            .map_err(protected_response_error)?,
        &fingerprint,
        Some((tunnel_id, subdomain)),
    )?;
    Ok(ConnectedSession {
        session,
        control,
        data_stream_protocol,
        tunnel_id: ack.tunnel_id,
        hostname: ack.hostname,
        subdomain: ack.subdomain,
    })
}

pub async fn connect_and_create(
    endpoint: &str,
    token: &str,
    subdomain: &str,
) -> anyhow::Result<ConnectedSession> {
    connect_and_create_with_options(endpoint, token, Some(subdomain), &TunnelOptions::default())
        .await
}

pub async fn connect_and_create_random(
    endpoint: &str,
    token: &str,
) -> anyhow::Result<ConnectedSession> {
    connect_and_create_with_options(endpoint, token, None, &TunnelOptions::default()).await
}

async fn connect_and_create_inner(
    endpoint: &str,
    token: &str,
    subdomain: Option<&str>,
    attempt: &ConnectionAttempt,
) -> anyhow::Result<ConnectedSession> {
    let (session, mut ctrl, data_stream_protocol) =
        dial_and_open_control(endpoint, token, attempt).await?;

    let req = ControlMsg::CreateTunnel(CreateTunnelReq {
        subdomain: subdomain.map(ToString::to_string),
    });
    write_msg(&mut ctrl, &req).await?;
    let resp = read_msg(&mut ctrl).await?;

    let ack = validate_public_ack(resp)?;
    if subdomain.is_some_and(|requested| requested != ack.subdomain) {
        return Err(ControlError::Acknowledgment.into());
    }

    Ok(ConnectedSession {
        data_stream_protocol,
        session,
        control: ctrl,
        hostname: ack.hostname,
        subdomain: ack.subdomain,
        tunnel_id: ack.tunnel_id,
    })
}

/// Attempt to reattach to an existing tunnel by its id and subdomain.
/// Returns `Err` if the server responds with `TunnelLost` (grace window elapsed)
/// or any other error.
pub async fn connect_and_reattach(
    endpoint: &str,
    token: &str,
    tunnel_id: ulid::Ulid,
    subdomain: &str,
) -> anyhow::Result<ConnectedSession> {
    connect_and_reattach_with_options(
        endpoint,
        token,
        tunnel_id,
        subdomain,
        &TunnelOptions::default(),
    )
    .await
}

async fn connect_and_reattach_inner(
    endpoint: &str,
    token: &str,
    tunnel_id: ulid::Ulid,
    subdomain: &str,
    attempt: &ConnectionAttempt,
) -> anyhow::Result<ConnectedSession> {
    let (session, mut ctrl, data_stream_protocol) =
        dial_and_open_control(endpoint, token, attempt).await?;

    let req = ControlMsg::ReattachTunnel(ReattachReq {
        tunnel_id,
        subdomain: subdomain.to_string(),
    });
    write_msg(&mut ctrl, &req).await?;
    let resp = read_msg(&mut ctrl).await?;

    let ack = validate_public_ack(resp)?;
    if ack.tunnel_id != tunnel_id || ack.subdomain != subdomain {
        return Err(ControlError::Acknowledgment.into());
    }

    Ok(ConnectedSession {
        data_stream_protocol,
        session,
        control: ctrl,
        hostname: ack.hostname,
        subdomain: ack.subdomain,
        tunnel_id: ack.tunnel_id,
    })
}

/// Shared across reconnect generations so an ended accept loop cannot truncate old forwards.
#[derive(Clone, Debug, Default)]
pub(crate) struct ForwardTasks {
    tracker: tokio_util::task::TaskTracker,
    stop_forwarding: tokio_util::sync::CancellationToken,
    stop_accepting: tokio_util::sync::CancellationToken,
}

/// One accept owner per physical connection. Accepted forwards belong to the
/// run tracker, not this handle. Only explicit physical failure cancels them.
pub(crate) struct AcceptGeneration {
    #[cfg(test)]
    observed_handle: Option<resource_observation_tests::Guard>,
    join: tokio::task::JoinHandle<anyhow::Result<()>>,
    joined: bool,
    stop_accepting: tokio_util::sync::CancellationToken,
    stop_forwarding: tokio_util::sync::CancellationToken,
    termination: tnl_protocol::transport::SessionTermination,
}

impl AcceptGeneration {
    pub(crate) async fn result(&mut self) -> Result<anyhow::Result<()>, tokio::task::JoinError> {
        let result = (&mut self.join).await;
        self.joined = true;
        result
    }

    pub(crate) async fn stop_and_join(&mut self) {
        self.stop_accepting.cancel();
        if !self.joined {
            let _ = self.result().await;
        }
    }

    pub(crate) async fn close_and_observe_cause(&self) -> tnl_protocol::OpenStreamError {
        self.termination.close_and_observe_cause().await
    }

    pub(crate) async fn terminate_failed(&self) {
        self.stop_forwarding.cancel();
        self.termination.terminate_failed().await;
        #[cfg(test)]
        if let Some(observed) = &self.observed_handle {
            observed.physical_completed();
        }
    }
}

impl Drop for AcceptGeneration {
    fn drop(&mut self) {
        self.stop_accepting.cancel();
    }
}

impl ForwardTasks {
    #[cfg(test)]
    pub(crate) fn observe_resources(&self) {
        resource_observation_tests::run(&self.tracker);
    }
    pub(crate) fn accept(
        &self,
        session: Box<dyn tnl_protocol::Session>,
        target: crate::target::Target,
        ctx: crate::forwarder::ForwardCtx,
    ) -> AcceptGeneration {
        let termination = session.termination();
        let stop_accepting = self.stop_accepting.child_token();
        let stop_forwarding = self.stop_forwarding.child_token();
        let acceptance = run_accept_loop_owned(
            session,
            target,
            ctx,
            self.clone(),
            stop_accepting.clone(),
            stop_forwarding.clone(),
        );
        #[cfg(test)]
        let acceptance = accept_gate_tests::scope(acceptance, termination.clone());
        #[cfg(test)]
        let (observed_handle, observed_accept) = resource_observation_tests::generation();
        #[cfg(test)]
        let acceptance = async move {
            let _lifetime = observed_accept;
            acceptance.await
        };
        let join = self.tracker.spawn(acceptance);
        AcceptGeneration {
            #[cfg(test)]
            observed_handle,
            join,
            joined: false,
            stop_accepting,
            stop_forwarding,
            termination,
        }
    }

    pub(crate) fn stop_forwarding(&self) {
        self.stop_forwarding.cancel();
    }

    pub(crate) fn cancel(&self) {
        self.stop_forwarding.cancel();
        self.stop_accepting.cancel();
    }

    pub(crate) async fn shutdown(&self) {
        self.cancel();
        self.tracker.close();
        self.tracker.wait().await;
    }
}

pub async fn run_accept_loop(
    session: Box<dyn tnl_protocol::Session>,
    target: crate::target::Target,
    ctx: crate::forwarder::ForwardCtx,
) -> anyhow::Result<()> {
    let tasks = ForwardTasks::default();
    run_accept_loop_owned(
        session,
        target,
        ctx,
        tasks.clone(),
        tasks.stop_accepting.clone(),
        tasks.stop_forwarding.clone(),
    )
    .await
}

async fn run_accept_loop_owned(
    mut session: Box<dyn tnl_protocol::Session>,
    target: crate::target::Target,
    ctx: crate::forwarder::ForwardCtx,
    tasks: ForwardTasks,
    stop_accepting: tokio_util::sync::CancellationToken,
    stop_forwarding: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    loop {
        let stream = tokio::select! {
            biased;
            () = stop_accepting.cancelled() => return Ok(()),
            stream = async {
                #[cfg(test)]
                accept_gate_tests::before_accept().await;
                session.accept_stream().await
            } => stream,
        };
        match stream {
            Ok(Some(stream)) => {
                let t = target.clone();
                let ctx2 = ctx.clone();
                let cancel = stop_forwarding.clone();
                tasks.tracker.spawn(async move {
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => {},
                        result = Box::pin(crate::forwarder::forward(stream, t, ctx2)) => {
                            if let Err(e) = result {
                                warn!(?e, "forward failed");
                            }
                        }
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

#[cfg(test)]
mod protected_tests {
    use super::*;
    use tnl_protocol::messages::ProtectedTunnelCreatedResp;

    fn ack(fingerprint: &str) -> ControlMsg {
        ControlMsg::ProtectedTunnelCreated(ProtectedTunnelCreatedResp {
            tunnel_id: ulid::Ulid::nil(),
            hostname: "demo.t.x".into(),
            subdomain: "demo".into(),
            policy_fingerprint: fingerprint.into(),
        })
    }

    #[test]
    fn public_ack_errors_are_typed_and_redact_server_diagnostics() {
        let error = validate_public_ack(ControlMsg::Error {
            code: ErrorCode::TunnelAlreadyActive,
            message: "CONTROL_DIAGNOSTIC_SECRET".into(),
        })
        .unwrap_err();
        assert!(matches!(
            error,
            ControlError::Server(ErrorCode::TunnelAlreadyActive)
        ));
        assert!(!error.to_string().contains("CONTROL_DIAGNOSTIC_SECRET"));
        assert!(matches!(
            validate_public_ack(ControlMsg::Close),
            Err(ControlError::Acknowledgment)
        ));
    }

    #[test]
    fn protected_ack_never_accepts_public_or_different_policy_or_binding() {
        let expected = "a".repeat(64);
        assert!(validate_protected_ack(
            ack(&expected),
            &expected,
            Some((ulid::Ulid::nil(), "demo"))
        )
        .is_ok());
        let public = ControlMsg::TunnelCreated(TunnelCreatedResp {
            tunnel_id: ulid::Ulid::nil(),
            hostname: "demo.t.x".into(),
            subdomain: "demo".into(),
        });
        assert!(validate_protected_ack(public, &expected, None).is_err());
        assert!(validate_protected_ack(ack(&"b".repeat(64)), &expected, None).is_err());
        assert!(validate_protected_ack(
            ack(&expected),
            &expected,
            Some((ulid::Ulid::new(), "demo"))
        )
        .is_err());
        assert!(validate_protected_ack(
            ack(&expected),
            &expected,
            Some((ulid::Ulid::nil(), "other"))
        )
        .is_err());
    }

    #[test]
    fn protected_protocol_conversion_preserves_io_and_tls_transport_types() {
        let io = protected_response_error(
            std::io::Error::from(std::io::ErrorKind::ConnectionReset).into(),
        );
        assert!(io.is::<std::io::Error>());
        assert!(!io.is::<ProtectedControlError>());
        let tls = protected_response_error(
            async_tungstenite::tungstenite::Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "TLS transport failure",
            ))
            .into(),
        );
        assert!(tls.is::<async_tungstenite::tungstenite::Error>());
        assert!(!tls.is::<ProtectedControlError>());
    }

    #[tokio::test]
    async fn protected_endpoint_rejected_before_dial_and_capability_has_no_raw_fallback() {
        let policy = std::sync::Arc::new(
            tnl_protocol::access::VisitorAccessPolicyV1::new(None, &[], None, Some(1)).unwrap(),
        );
        let options = TunnelOptions {
            access: Some(policy),
        };
        assert!(
            connect_and_create_with_options("http://127.0.0.1:1", "token", None, &options)
                .await
                .unwrap_err()
                .is::<ProtectedControlError>()
        );
        assert!(require_protected_capability(DataStreamProtocol::RawHttp1).is_err());
        assert!(require_protected_capability(DataStreamProtocol::Http1MetadataV1).is_ok());
    }
}

#[cfg(test)]
mod trusted_wss_tests {
    use super::*;
    use std::{sync::Arc, time::Duration};

    #[derive(Clone, Copy, Debug)]
    enum PeerReply {
        Exact,
        ChangedPolicy,
        Legacy,
        NoEcho,
        Oversized,
        Malformed,
        InvalidEcho,
        UntrustedCa,
    }

    #[allow(clippy::result_large_err)] // tungstenite fixes the callback error type.
    #[allow(clippy::too_many_lines)] // One trusted-WSS fixture covers protocol and transport failures.
    async fn peer(
        reply: PeerReply,
        expected_fingerprint: String,
    ) -> (
        String,
        tokio_rustls::TlsConnector,
        tokio::task::JoinHandle<()>,
    ) {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let mut ca_params = rcgen::CertificateParams::new(vec!["tnl-test-ca".into()]).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let leaf_key = rcgen::KeyPair::generate().unwrap();
        let leaf = rcgen::CertificateParams::new(vec!["localhost".into()])
            .unwrap()
            .signed_by(&leaf_key, &ca, &ca_key)
            .unwrap();
        let key = rustls::pki_types::PrivateKeyDer::try_from(leaf_key.serialize_der()).unwrap();
        let server = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![leaf.der().clone()], key)
            .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        if !matches!(reply, PeerReply::UntrustedCa) {
            roots.add(ca.der().clone()).unwrap();
        }
        let client = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!(
            "https://localhost:{}",
            listener.local_addr().unwrap().port()
        );
        let task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let tls = acceptor.accept(tcp).await;
            if matches!(reply, PeerReply::UntrustedCa) {
                assert!(tls.is_err());
                return;
            }
            let tls = tls.unwrap();
            let ws = async_tungstenite::tokio::accept_hdr_async(tls, move |request: &async_tungstenite::tungstenite::handshake::server::Request, mut response: async_tungstenite::tungstenite::handshake::server::Response| {
                response.headers_mut().insert(tnl_protocol::control::CONTROL_PROTOCOL_HEADER, http::HeaderValue::from_static(tnl_protocol::control::CONTROL_PROTOCOL_VERSION));
                assert_eq!(request.headers()["authorization"], "Bearer tnl_TEST");
                assert_eq!(request.headers()[DATA_STREAM_PROTOCOL_HEADER], HTTP1_METADATA_V1);
                if !matches!(reply, PeerReply::NoEcho) { response.headers_mut().insert(DATA_STREAM_PROTOCOL_HEADER, http::HeaderValue::from_static(if matches!(reply, PeerReply::InvalidEcho) { "INVALID_ECHO_SENTINEL" } else { HTTP1_METADATA_V1 })); }
                Ok(response)
            }).await.unwrap();
            if matches!(reply, PeerReply::NoEcho | PeerReply::InvalidEcho) {
                super::metadata_rejection_tests::observe(
                    ws,
                    matches!(reply, PeerReply::InvalidEcho),
                )
                .await;
                return;
            }
            let mut session = tnl_protocol::server_session_from_ws_generic(ws);
            let mut control = session.accept_stream().await.unwrap().unwrap();
            let length = control.read_u32().await;
            let length = length.unwrap();
            assert!(length < 16 * 1024);
            let mut bytes = vec![0; length as usize];
            control.read_exact(&mut bytes).await.unwrap();
            let request: ControlMsg = serde_json::from_slice(&bytes).unwrap();
            let ControlMsg::CreateProtectedTunnel(request) = request else {
                panic!("client sent a public/downgraded request");
            };
            assert_eq!(request.access.fingerprint().unwrap(), expected_fingerprint);
            if matches!(reply, PeerReply::Oversized) {
                control.write_u32(u32::MAX).await.unwrap();
            } else if matches!(reply, PeerReply::Malformed) {
                let malformed = b"{\"SCHEMA_SECRET_SENTINEL\": not-json}";
                control
                    .write_u32(u32::try_from(malformed.len()).unwrap())
                    .await
                    .unwrap();
                control.write_all(malformed).await.unwrap();
            } else {
                let fingerprint = if matches!(reply, PeerReply::ChangedPolicy) {
                    "b".repeat(64)
                } else {
                    request.access.fingerprint().unwrap()
                };
                let response = if matches!(reply, PeerReply::Legacy) {
                    ControlMsg::TunnelCreated(TunnelCreatedResp {
                        tunnel_id: ulid::Ulid::nil(),
                        hostname: "demo.t.x".into(),
                        subdomain: "demo".into(),
                    })
                } else {
                    ControlMsg::ProtectedTunnelCreated(
                        tnl_protocol::messages::ProtectedTunnelCreatedResp {
                            tunnel_id: ulid::Ulid::nil(),
                            hostname: "demo.t.x".into(),
                            subdomain: "demo".into(),
                            policy_fingerprint: fingerprint,
                        },
                    )
                };
                let bytes = serde_json::to_vec(&response).unwrap();
                control
                    .write_u32(u32::try_from(bytes.len()).unwrap())
                    .await
                    .unwrap();
                control.write_all(&bytes).await.unwrap();
            }
            control.flush().await.unwrap();
            // The tested operation must finish/drop rather than issue a legacy
            // create on this connection after any unsupported or wrong ACK.
            assert!(control.read_u32().await.is_err());
        });
        (endpoint, connector, task)
    }

    async fn assert_peer_reply(reply: PeerReply) {
        let options = TunnelOptions {
            access: Some(Arc::new(
                tnl_protocol::access::VisitorAccessPolicyV1::new(None, &[], None, Some(1)).unwrap(),
            )),
        };
        let fingerprint = options.access.as_ref().unwrap().fingerprint().unwrap();
        let (endpoint, connector, server) = peer(reply, fingerprint).await;
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            connect_and_create_with_connector(
                &endpoint,
                "tnl_TEST",
                Some("demo"),
                &options,
                Some(connector),
            ),
        )
        .await
        .unwrap();
        if matches!(reply, PeerReply::Exact) {
            assert!(result.is_ok());
        } else {
            let error = result.as_ref().unwrap_err();
            assert_eq!(
                crate::exit::classify(error),
                crate::exit::EX_SERVER_ERROR,
                "{reply:?} must be a typed server/protocol failure"
            );
            assert!(error.is::<ProtectedControlError>());
            let diagnostic = format!("{error:#?}");
            assert!(!diagnostic.contains("SCHEMA_SECRET_SENTINEL"));
            assert!(!diagnostic.contains("INVALID_ECHO_SENTINEL"));
        }
        drop(result);
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn verified_test_ca_wss_requires_capability_and_exact_policy_ack_without_public_fallback()
    {
        for reply in [
            PeerReply::Exact,
            PeerReply::ChangedPolicy,
            PeerReply::Legacy,
            PeerReply::NoEcho,
        ] {
            assert_peer_reply(reply).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn verified_wss_attempts_retain_run_policy_arc_and_verifier() {
        let verifier = "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY";
        let policy = Arc::new(
            tnl_protocol::access::VisitorAccessPolicyV1::from_json_slice(
                format!(r#"{{"version":1,"basic":{{"username":"fixture","password_hash":"{verifier}"}},"allow_ips":[],"rate_limit":null,"max_concurrent":1}}"#).as_bytes()
            ).unwrap()
        );
        let fingerprint = policy.fingerprint().unwrap();
        let options = TunnelOptions {
            access: Some(policy.clone()),
        };
        let run_options = options.clone();
        for reply in [PeerReply::Exact, PeerReply::ChangedPolicy, PeerReply::Exact] {
            let (endpoint, connector, server) = peer(reply, fingerprint.clone()).await;
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                connect_and_create_with_connector(
                    &endpoint,
                    "tnl_TEST",
                    Some("demo"),
                    &run_options,
                    Some(connector),
                ),
            )
            .await
            .unwrap();
            assert_eq!(result.is_ok(), matches!(reply, PeerReply::Exact));
            drop(result);
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap();
            let retained = run_options.access.as_ref().unwrap();
            assert!(Arc::ptr_eq(retained, options.access.as_ref().unwrap()));
            assert!(Arc::ptr_eq(retained, &policy));
            assert_eq!(retained.fingerprint().unwrap(), fingerprint);
            assert_eq!(
                retained
                    .basic()
                    .unwrap()
                    .password_hash()
                    .expose_for_verification(),
                verifier
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn protected_untrusted_certificate_remains_a_verified_tls_transport_failure() {
        let options = TunnelOptions {
            access: Some(Arc::new(
                tnl_protocol::access::VisitorAccessPolicyV1::new(None, &[], None, Some(1)).unwrap(),
            )),
        };
        let fingerprint = options.access.as_ref().unwrap().fingerprint().unwrap();
        let (endpoint, connector, server) = peer(PeerReply::UntrustedCa, fingerprint).await;
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            connect_and_create_with_connector(
                &endpoint,
                "tnl_TEST",
                None,
                &options,
                Some(connector),
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(!error.is::<ProtectedControlError>());
        assert!(matches!(
            error.downcast_ref::<async_tungstenite::tungstenite::Error>(),
            Some(async_tungstenite::tungstenite::Error::Io(_))
        ));
        assert_eq!(
            crate::exit::classify(&error),
            crate::exit::EX_SERVER_UNREACHABLE
        );
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn protected_oversized_response_exits_70_without_public_fallback() {
        assert_peer_reply(PeerReply::Oversized).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn protected_malformed_response_exits_70_without_public_fallback() {
        assert_peer_reply(PeerReply::Malformed).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn protected_invalid_echo_exits_70_without_public_fallback() {
        assert_peer_reply(PeerReply::InvalidEcho).await;
    }
}

#[cfg(test)]
#[path = "client/access_e2e_tests.rs"]
mod access_e2e_tests;

#[cfg(test)]
mod ownership_tests;

#[cfg(test)]
pub(crate) mod accept_gate_tests;

/// One budget and cleanup owner for dial (including DNS/TLS), open, register,
/// and the typed `TunnelLost` fallback. No timer is reset between these phases.
pub(crate) struct ConnectionAttempt {
    deadline: tokio::time::Instant,
    pending: std::sync::Mutex<Vec<tnl_protocol::SessionTermination>>,
}

impl ConnectionAttempt {
    pub(crate) fn new() -> Self {
        Self {
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(20),
            pending: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn track(&self, termination: tnl_protocol::SessionTermination) {
        #[cfg(test)]
        let _ = TEST_CONNECT_BOUNDARY.try_with(|(_, probe)| {
            *probe.termination.lock().unwrap() = Some(termination.clone());
        });
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(termination);
    }

    pub(crate) async fn finish<T>(
        &self,
        future: impl std::future::Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        if let Ok(result) = tokio::time::timeout_at(self.deadline, future).await {
            result
        } else {
            self.terminate_pending().await;
            Err(control::LifecycleDeadline::Attempt.into())
        }
    }

    async fn registration_finished(&self, unhealthy: bool) {
        if unhealthy {
            self.terminate_pending().await;
        } else {
            // Typed business/policy errors are NOT evidence of unhealthy IO.
            self.pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
        }
    }

    pub(crate) async fn terminate_pending(&self) {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for termination in pending {
            termination.terminate_failed().await;
        }
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConnectBoundary {
    Dial,
    Open,
}

#[cfg(test)]
#[derive(Default)]
struct ConnectBoundaryProbe {
    entered: tokio_util::sync::CancellationToken,
    release: tokio_util::sync::CancellationToken,
    dropped: tokio_util::sync::CancellationToken,
    continued: std::sync::atomic::AtomicBool,
    termination: std::sync::Mutex<Option<tnl_protocol::SessionTermination>>,
}
#[cfg(test)]
tokio::task_local! {
    static TEST_CONNECT_BOUNDARY: (ConnectBoundary, std::sync::Arc<ConnectBoundaryProbe>);
}
#[cfg(test)]
async fn test_connect_boundary<T>(
    boundary: ConnectBoundary,
    future: impl std::future::Future<Output = T>,
) -> T {
    let probe = TEST_CONNECT_BOUNDARY
        .try_with(|(stage, probe)| (*stage == boundary).then(|| probe.clone()))
        .ok()
        .flatten();
    if let Some(probe) = probe {
        struct ObserveDrop(std::sync::Arc<ConnectBoundaryProbe>);
        impl Drop for ObserveDrop {
            fn drop(&mut self) {
                self.0.dropped.cancel();
            }
        }
        let _drop = ObserveDrop(probe.clone());
        probe.entered.cancel();
        probe.release.cancelled().await;
        probe
            .continued
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    future.await
}

#[cfg(test)]
mod connection_attempt_tests;

#[cfg(test)]
mod metadata_rejection_tests;

#[cfg(test)]
pub(crate) mod close_gate_tests;

#[cfg(test)]
pub(crate) mod resource_observation_tests;
