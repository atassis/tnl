//! Client reconnect loop with exponential backoff + reattach.
//!
//! Strategy:
//! - First connect: public or protected create according to immutable run options.
//!   Omitting the subdomain still requests a random name.
//! - Subsequent connects: reattach in the same public/protected mode with the
//!   `tunnel_id` from the previous successful connect.
//! - On `TunnelLost`, recreate with the same subdomain and policy. Protected
//!   recreation uses a typed code and can never select a public request.
//! - Backoff: 1s → 2s → 4s → 8s → 16s → 30s (capped). Reset on success.
//! - Auth failures (`Unauthorized` / "token rejected") are NOT recoverable.

use std::time::Duration;

use crate::client::control::SHUTDOWN_TIMEOUT;
use tracing::{info, warn};

/// Optional hooks used in tests to inject behaviour without spawning real
/// network connections.
#[derive(Default)]
pub struct Hooks {
    pub inspection: Option<crate::inspection::Inspection>,
    /// When this oneshot fires, the current control session ends so the
    /// outer loop can attempt a reattach.
    pub cancel_first_session: Option<tokio::sync::oneshot::Receiver<()>>,
    /// If set, each forwarded substream tees its head into this channel
    /// (consumed by Inspector for per-request log lines).
    pub log_tx: Option<tokio::sync::mpsc::Sender<crate::inspector::LogLine>>,
}

impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooks")
            .field("inspection", &self.inspection)
            .field(
                "cancel_first_session",
                &self.cancel_first_session.as_ref().map(|_| "<receiver>"),
            )
            .field("log_tx", &self.log_tx.as_ref().map(|_| "<sender>"))
            .finish()
    }
}

/// Run until a non-recoverable error occurs or the caller cancels this future.
/// The CLI uses `run_until_shutdown` for acknowledged signal shutdown.
///
/// Prints a one-time banner on the first successful connect.
#[allow(clippy::too_many_lines)]
pub async fn run(
    endpoint: &str,
    token: &str,
    subdomain: Option<&str>,
    target: crate::target::Target,
    host_header: crate::host_header::HostHeader,
    hooks: Hooks,
) -> anyhow::Result<()> {
    run_with_options(
        endpoint,
        token,
        subdomain,
        target,
        host_header,
        &crate::client::TunnelOptions::default(),
        hooks,
    )
    .await
}

/// The same reconnect loop retains one immutable policy through every retry,
/// reattach and typed-TunnelLost recreation. It never retries in public mode.
#[allow(clippy::too_many_lines)]
pub async fn run_with_options(
    endpoint: &str,
    token: &str,
    subdomain: Option<&str>,
    target: crate::target::Target,
    host_header: crate::host_header::HostHeader,
    options: &crate::client::TunnelOptions,
    hooks: Hooks,
) -> anyhow::Result<()> {
    run_until_shutdown(
        endpoint,
        token,
        subdomain,
        target,
        host_header,
        options,
        hooks,
        std::future::pending(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_until_shutdown(
    endpoint: &str,
    token: &str,
    subdomain: Option<&str>,
    target: crate::target::Target,
    host_header: crate::host_header::HostHeader,
    options: &crate::client::TunnelOptions,
    hooks: Hooks,
    shutdown: impl std::future::Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let tasks = crate::client::ForwardTasks::default();
    #[cfg(test)]
    tasks.observe_resources();
    let _cleanup = StopTasksOnDrop(tasks.clone());
    let result = run_loop(
        endpoint,
        token,
        subdomain,
        target,
        host_header,
        options,
        hooks,
        shutdown,
        &tasks,
    )
    .await;
    tasks.shutdown().await;
    result
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_loop(
    endpoint: &str,
    token: &str,
    subdomain: Option<&str>,
    target: crate::target::Target,
    host_header: crate::host_header::HostHeader,
    options: &crate::client::TunnelOptions,
    mut hooks: Hooks,
    shutdown: impl std::future::Future<Output = anyhow::Result<()>>,
    tasks: &crate::client::ForwardTasks,
) -> anyhow::Result<()> {
    tokio::pin!(shutdown);
    let _availability = AvailabilityGuard(hooks.inspection.clone());
    let mut backoff = RetryBackoff::new();
    let mut last_tunnel: Option<(ulid::Ulid, String)> = None;
    let mut generation = crate::inspection::TunnelGeneration::new();
    let mut create_subdomain = subdomain.map(str::to_owned);
    let mut printed_banner = false;
    let mut warned_raw_metadata = false;
    // Per-tunnel host-rewrite state, persisted across reconnects so a learned
    // dev-server block survives a reattach.
    let rewrite_active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let warned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    loop {
        let previous = last_tunnel.clone();
        let attempt = crate::client::ConnectionAttempt::new();
        let result = {
            let started = std::sync::atomic::AtomicBool::new(false);
            let mut connect = Box::pin(attempt.finish(async {
                started.store(true, std::sync::atomic::Ordering::Relaxed);
                if let Some((tid, sub)) = &last_tunnel {
                    // Clone the subdomain before the borrow on `last_tunnel` ends so
                    // we can use it after potentially setting `last_tunnel = None`.
                    let tid = *tid;
                    let sub = sub.clone();
                    // Try to reattach within the grace window.
                    match crate::client::connect_and_reattach_in_attempt(
                        endpoint, token, tid, &sub, options, &attempt,
                    )
                    .await
                    {
                        Ok(s) => Ok(s),
                        Err(e) => {
                            // On TunnelLost the grace window has elapsed; fall back to
                            // a fresh create in the SAME selected mode so the URL
                            // stays stable.
                            if is_tunnel_lost(&e, options.access.is_some()) {
                                eprintln!("info: grace window elapsed; recreating tunnel");
                                last_tunnel = None;
                                retain_recreation_name(&mut create_subdomain, &sub);
                                crate::client::connect_and_create_in_attempt(
                                    endpoint,
                                    token,
                                    Some(&sub),
                                    options,
                                    &attempt,
                                )
                                .await
                            } else {
                                Err(e)
                            }
                        }
                    }
                } else if let Some(s) = create_subdomain.as_deref() {
                    crate::client::connect_and_create_in_attempt(
                        endpoint,
                        token,
                        Some(s),
                        options,
                        &attempt,
                    )
                    .await
                } else {
                    crate::client::connect_and_create_in_attempt(
                        endpoint, token, None, options, &attempt,
                    )
                    .await
                }
            }));
            tokio::select! {
                biased;
                signal = &mut shutdown => {
                    signal?;
                    begin_shutdown(hooks.inspection.as_ref(), tasks);
                    let deadline = tokio::time::Instant::now() + SHUTDOWN_TIMEOUT;
                    if started.load(std::sync::atomic::Ordering::Relaxed) {
                        let released = tokio::time::timeout_at(deadline, &mut connect).await;
                        drop(connect);
                        if let Ok(Ok(connected)) = released {
                            release_connected(connected, deadline).await;
                        } else {
                            attempt.terminate_pending().await;
                            report_shutdown(false);
                        }
                    } else {
                        drop(connect);
                        release_disconnected(endpoint, token, previous.as_ref(), options).await;
                    }
                    return Ok(());
                }
                result = &mut connect => result,
            }
        };

        let session = match result {
            Ok(s) => {
                info!(
                    tunnel_id = %s.tunnel_id,
                    subdomain = %s.subdomain,
                    "tunnel session established"
                );
                s
            }
            Err(e) => {
                if !is_recoverable(&e, options.access.is_some()) {
                    return Err(e);
                }
                let delay = backoff.sample_delay()?;
                warn!(%e, delay_ms = delay.as_millis(), "connection failed; reconnecting");
                eprintln!(
                    "warning: connection lost ({e:#}); reconnecting in {}s",
                    delay.as_secs_f32()
                );
                tokio::select! {
                    biased;
                    signal = &mut shutdown => {
                        signal?;
                        begin_shutdown(hooks.inspection.as_ref(), tasks);
                        release_disconnected(endpoint, token, last_tunnel.as_ref(), options).await;
                        return Ok(());
                    }
                    () = tokio::time::sleep(delay) => {},
                }
                continue;
            }
        };

        // Registration alone is not evidence of a stable control session.
        let established_at = tokio::time::Instant::now();
        generation = generation_for_session(
            last_tunnel.as_ref().map(|(id, _)| *id),
            session.tunnel_id,
            generation,
        );
        last_tunnel = Some((session.tunnel_id, session.subdomain.clone()));
        if let Some(inspection) = &hooks.inspection {
            inspection.activate_tunnel(generation, session.tunnel_id);
        }

        if !printed_banner {
            printed_banner = true;
            println!("┌─ tnl ─────────────────────────────────────────");
            println!("│ Tunnel:    https://{}", session.hostname);
            println!("│ Subdomain: {}", session.subdomain);
            println!("│ Forward:   {}", target.display());
            println!("│ Press Ctrl-C to stop.");
            println!("└────────────────────────────────────────────────");
        }

        if session.data_stream_protocol
            == tnl_protocol::request_metadata::DataStreamProtocol::RawHttp1
            && !warned_raw_metadata
        {
            warned_raw_metadata = true;
            eprintln!("warning: legacy raw HTTP mode: visitor identity unavailable; request IDs are local and not shared with the daemon");
        }
        let cancel = hooks.cancel_first_session.take();
        // Keep the control stream alive for the duration of the session.
        let control_shutdown = tokio_util::sync::CancellationToken::new();
        let control = crate::client::control::ControlOwner::new(session.control, session.tunnel_id);
        let control_run = control.run(control_shutdown.clone());
        tokio::pin!(control_run);
        let ctx = crate::forwarder::ForwardCtx {
            #[cfg(test)]
            inspection_ownership_check: None,
            #[cfg(test)]
            connect_failure: None,
            inspection: hooks
                .inspection
                .clone()
                .map(|inspection| (inspection, generation)),
            data_stream_protocol: session.data_stream_protocol,
            tunnel: session.subdomain.clone(),
            log_tx: hooks.log_tx.clone(),
            version: env!("CARGO_PKG_VERSION"),
            host_header: host_header.clone(),
            host_public: session.hostname.clone(),
            rewrite_active: rewrite_active.clone(),
            warned: warned.clone(),
        };
        let mut accept = tasks.accept(session.session, target.clone(), ctx);

        let mut failed = false;
        let mut control_error = None;
        tokio::select! {
            biased;
            result = &mut control_run => {
                match result {
                    Ok(crate::client::control::ControlEnd::RemoteStop) => {
                        begin_shutdown(hooks.inspection.as_ref(), tasks);
                        accept.stop_and_join().await;
                        return Ok(());
                    }
                    Ok(crate::client::control::ControlEnd::Shutdown { .. }) => unreachable!("only this run requests shutdown"),
                    Err(error) => {
                        failed = is_physical_session_failure(&error);
                        control_error = Some(error);
                    }
                }
            }
            signal = &mut shutdown => {
                signal?;
                begin_shutdown(hooks.inspection.as_ref(), tasks);
                control_shutdown.cancel();
                match control_run.await {
                    Ok(crate::client::control::ControlEnd::RemoteStop) => {},
                    Ok(crate::client::control::ControlEnd::Shutdown { acknowledged, unhealthy }) => {
                        report_shutdown(acknowledged);
                        failed = unhealthy;
                    }
                    Err(error) => {
                        report_shutdown(false);
                        failed = is_physical_session_failure(&error);
                    }
                }
                accept.stop_and_join().await;
                if failed { accept.terminate_failed().await; }
                return Ok(());
            }
            r = accept.result() => {
                match r {
                    Ok(Ok(())) => info!("accept loop ended cleanly; reconnecting"),
                    Ok(Err(e)) => {
                        failed = is_physical_session_failure(&e);
                        warn!(%e, "accept loop ended with error; reconnecting");
                    }
                    Err(e) => warn!(%e, "accept loop task panicked; reconnecting"),
                }
            }
            () = async {
                if let Some(rx) = cancel { let _ = rx.await; }
                else { std::future::pending::<()>().await; }
            } => { eprintln!("[test hook] forced disconnect"); }
        }
        let established_duration = established_at.elapsed();
        if let Some(inspection) = &hooks.inspection {
            inspection.disconnected();
        }
        if control_error.as_ref().is_some_and(|error| {
            error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::UnexpectedEof)
        }) {
            // Driver cleanup can wake the control reader before publishing its
            // physical cause. Observe that cause before accept cancellation can
            // win its biased select and replace the accept result with Ok(()).
            let cause = tokio::select! {
                biased;
                cause = accept.close_and_observe_cause() => Some(cause),
                signal = &mut shutdown => {
                    signal?;
                    None
                }
            };
            let Some(cause) = cause else {
                // Healthy poll_close may be blocked indefinitely. Cancelling a
                // cause watch is safe; never force an unknown/healthy outcome or
                // wait for physical completion on this intentional-stop path.
                begin_shutdown(hooks.inspection.as_ref(), tasks);
                accept.stop_and_join().await;
                report_shutdown(false);
                return Ok(());
            };
            failed = matches!(
                cause,
                tnl_protocol::OpenStreamError::Io
                    | tnl_protocol::OpenStreamError::Protocol
                    | tnl_protocol::OpenStreamError::InboundOverflow
                    | tnl_protocol::OpenStreamError::Capacity
            );
        }
        // Revoke availability before cleanup and join this generation's accept
        // owner. Ordinary reconnect (including the test hook) leaves accepted
        // forwards run-owned; only proven physical failure cancels this generation.
        accept.stop_and_join().await;
        if failed {
            accept.terminate_failed().await;
        }
        if let Some(error) = control_error {
            if !is_recoverable(&error, options.access.is_some()) {
                return Err(error);
            }
        }
        backoff.observe_session(established_duration);
        let delay = backoff.sample_delay()?;
        tokio::select! {
            biased;
            signal = &mut shutdown => {
                signal?;
                begin_shutdown(hooks.inspection.as_ref(), tasks);
                release_disconnected(endpoint, token, last_tunnel.as_ref(), options).await;
                return Ok(());
            }
            () = tokio::time::sleep(delay) => {},
        }
        // Loop restarts; next iteration tries ReattachTunnel.
    }
}

pub(crate) fn is_physical_session_failure(error: &anyhow::Error) -> bool {
    error.is::<crate::client::control::LifecycleDeadline>()
        || matches!(
            error.downcast_ref::<crate::client::ControlError>(),
            Some(
                crate::client::ControlError::Framing
                    | crate::client::ControlError::Schema
                    | crate::client::ControlError::Acknowledgment
            )
        )
        || matches!(
            error.downcast_ref::<crate::client::ProtectedControlError>(),
            Some(
                crate::client::ProtectedControlError::Framing
                    | crate::client::ProtectedControlError::Schema
            )
        )
        || matches!(
            error.downcast_ref::<tnl_protocol::OpenStreamError>(),
            Some(
                tnl_protocol::OpenStreamError::Io
                    | tnl_protocol::OpenStreamError::Protocol
                    | tnl_protocol::OpenStreamError::InboundOverflow
                    | tnl_protocol::OpenStreamError::Capacity
            )
        )
        || error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() != std::io::ErrorKind::UnexpectedEof)
}

struct StopTasksOnDrop(crate::client::ForwardTasks);
impl Drop for StopTasksOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn begin_shutdown(
    inspection: Option<&crate::inspection::Inspection>,
    tasks: &crate::client::ForwardTasks,
) {
    if let Some(inspection) = inspection {
        // Revoke replay authority before any bounded network cleanup awaits.
        inspection.disconnected();
    }
    tasks.stop_forwarding();
}

fn report_shutdown(confirmed: bool) {
    if confirmed {
        println!("✓ tunnel stopped; session released");
    } else {
        eprintln!("warning: shutdown was not acknowledged; the name may remain reserved until the server's reconnect grace expires");
    }
}

async fn release_disconnected(
    endpoint: &str,
    token: &str,
    previous: Option<&(ulid::Ulid, String)>,
    options: &crate::client::TunnelOptions,
) {
    let Some((id, subdomain)) = previous else {
        report_shutdown(false);
        return;
    };
    let deadline = tokio::time::Instant::now() + SHUTDOWN_TIMEOUT;
    let attempt = crate::client::ConnectionAttempt::new();
    // Reattach only: cleanup never creates or deletes by a reusable name.
    let released = tokio::time::timeout_at(
        deadline,
        crate::client::connect_and_reattach_in_attempt(
            endpoint, token, *id, subdomain, options, &attempt,
        ),
    )
    .await;
    if let Ok(Ok(connected)) = released {
        release_connected(connected, deadline).await;
    } else {
        attempt.terminate_pending().await;
        report_shutdown(false);
    }
}

async fn release_connected(
    mut connected: crate::client::ConnectedSession,
    deadline: tokio::time::Instant,
) {
    let termination = connected.session.termination();
    let released = tokio::time::timeout_at(
        deadline,
        crate::client::close_control(&mut connected.control),
    )
    .await;
    let confirmed = matches!(released, Ok(Ok(())));
    let unhealthy = match released {
        Err(_) => true,
        Ok(Err(error)) => is_physical_session_failure(&error),
        Ok(Ok(())) => false,
    };
    drop(connected);
    if unhealthy {
        termination.terminate_failed().await;
    }
    report_shutdown(confirmed);
}

struct AvailabilityGuard(Option<crate::inspection::Inspection>);
impl Drop for AvailabilityGuard {
    fn drop(&mut self) {
        if let Some(inspection) = &self.0 {
            inspection.disconnected();
        }
    }
}

/// Returns `false` for errors that cannot be fixed by retrying (bad token, etc.).
fn retain_recreation_name(requested: &mut Option<String>, assigned: &str) {
    // A failed recreation must not turn an originally assigned name into a
    // random-name request on the next retry, in either access mode.
    *requested = Some(assigned.to_owned());
}

fn reattach_disposition(
    e: &anyhow::Error,
) -> Option<tnl_protocol::control::ReattachErrorDisposition> {
    let code = match e.downcast_ref::<crate::client::ControlError>() {
        Some(crate::client::ControlError::Server(code)) => Some(*code),
        _ => match e.downcast_ref::<crate::client::ProtectedControlError>() {
            Some(crate::client::ProtectedControlError::Server(code)) => Some(*code),
            _ => None,
        },
    }?;
    Some(code.reattach_disposition())
}

fn is_tunnel_lost(e: &anyhow::Error, _protected: bool) -> bool {
    matches!(
        reattach_disposition(e),
        Some(tnl_protocol::control::ReattachErrorDisposition::Recreate)
    )
}

fn is_recoverable(e: &anyhow::Error, _protected: bool) -> bool {
    if e.is::<crate::client::control::LifecycleDeadline>() {
        return true;
    }
    if let Some(error) = e.downcast_ref::<tnl_protocol::OpenStreamError>() {
        return error.is_retryable_transport();
    }
    if let Some(disposition) = reattach_disposition(e) {
        return disposition == tnl_protocol::control::ReattachErrorDisposition::Retry;
    }
    if e.downcast_ref::<crate::client::ControlError>().is_some()
        || e.downcast_ref::<crate::client::ProtectedControlError>()
            .is_some()
    {
        return false;
    }
    if e.downcast_ref::<std::io::Error>().is_some() {
        return true;
    }
    matches!(
        e.downcast_ref::<async_tungstenite::tungstenite::Error>(),
        Some(
            async_tungstenite::tungstenite::Error::Io(_)
                | async_tungstenite::tungstenite::Error::ConnectionClosed
                | async_tungstenite::tungstenite::Error::AlreadyClosed
        )
    )
}

fn generation_for_session(
    previous: Option<ulid::Ulid>,
    current: ulid::Ulid,
    generation: crate::inspection::TunnelGeneration,
) -> crate::inspection::TunnelGeneration {
    if previous == Some(current) {
        generation
    } else {
        crate::inspection::TunnelGeneration::new()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn failed_session_cleanup_requires_physical_failure_not_eof_or_server_rejection() {
        assert!(super::is_physical_session_failure(
            &tnl_protocol::OpenStreamError::Io.into()
        ));
        assert!(super::is_physical_session_failure(
            &tnl_protocol::OpenStreamError::Protocol.into()
        ));
        assert!(!super::is_physical_session_failure(
            &tnl_protocol::OpenStreamError::ConnectionClosed.into()
        ));
        // Native yamux Capacity is fatal Cleanup/Closed, not local queue pressure.
        for cause in [
            tnl_protocol::OpenStreamError::Capacity,
            tnl_protocol::OpenStreamError::InboundOverflow,
        ] {
            assert!(super::is_physical_session_failure(&cause.into()));
        }
        assert!(!super::is_physical_session_failure(
            &std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into()
        ));
        for code in [
            tnl_protocol::ErrorCode::TunnelAlreadyActive,
            tnl_protocol::ErrorCode::TunnelLost,
            tnl_protocol::ErrorCode::Unauthorized,
            tnl_protocol::ErrorCode::AccessPolicyMismatch,
        ] {
            assert!(!super::is_physical_session_failure(&anyhow::Error::new(
                crate::client::ControlError::Server(code)
            )));
        }
    }

    use super::generation_for_session;
    #[test]
    fn generation_survives_reattach_but_not_recreation() {
        let tunnel = ulid::Ulid::new();
        let initial = crate::inspection::TunnelGeneration::new();
        let reattached = generation_for_session(Some(tunnel), tunnel, initial);
        assert_eq!(reattached, initial);
        assert_ne!(generation_for_session(None, tunnel, initial), initial);
        assert_ne!(
            generation_for_session(Some(tunnel), ulid::Ulid::new(), initial),
            initial
        );
    }
}

#[cfg(test)]
mod bootstrap_retry_tests {
    use std::pin::Pin;
    use std::time::Duration;

    use tnl_protocol::{
        ClosingReason, ControlMsg, CreateTunnelReq, Session as _, Stream, TunnelCreatedResp,
    };
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::sync::oneshot;

    use super::*;

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::result_large_err)] // tungstenite fixes the callback error type.
    async fn run_loop_retries_after_a_control_bootstrap_transport_close() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (first_closed_tx, first_closed_rx) = oneshot::channel();
        let (registered_tx, registered_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            // The first accepted WebSocket closes before it can provide a
            // control stream. The second attempt is an ordered recovery proof.
            let (first, _) = listener.accept().await.unwrap();
            let first = async_tungstenite::tokio::accept_hdr_async(
                first,
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
            first_closed_tx.send(()).unwrap();
            drop(first);

            let (second, _) = listener.accept().await.unwrap();
            let second = async_tungstenite::tokio::accept_hdr_async(
                second,
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
            let mut session = tnl_protocol::server_session_from_ws_generic(second);
            let mut control = session.accept_stream().await.unwrap().unwrap();
            assert!(matches!(
                read_control(&mut control).await,
                ControlMsg::CreateTunnel(CreateTunnelReq { .. })
            ));
            write_control(
                &mut control,
                &ControlMsg::TunnelCreated(TunnelCreatedResp {
                    tunnel_id: ulid::Ulid::new(),
                    hostname: "retry.t.test".into(),
                    subdomain: "retry".into(),
                }),
            )
            .await;
            registered_tx.send(()).unwrap();
            assert_eq!(read_control(&mut control).await, ControlMsg::Close);
            write_control(
                &mut control,
                &ControlMsg::Closing {
                    reason: ClosingReason::ClientShutdown,
                },
            )
            .await;
        });

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let run = tokio::spawn(async move {
            run_until_shutdown(
                &endpoint,
                "tnl_TEST",
                Some("retry"),
                crate::target::Target::Explicit("127.0.0.1:1".parse().unwrap()),
                crate::host_header::HostHeader::Preserve,
                &crate::client::TunnelOptions::default(),
                Hooks::default(),
                async { shutdown_rx.await.map_err(anyhow::Error::from) },
            )
            .await
        });

        first_closed_rx.await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), registered_rx)
            .await
            .expect("run loop did not reach a second registration attempt")
            .unwrap();
        shutdown_tx.send(()).unwrap();
        run.await.unwrap().unwrap();
        server.await.unwrap();
    }
}

#[cfg(test)]
mod protected_tests {
    use super::*;
    use crate::client::{ControlError, ProtectedControlError};
    use tnl_protocol::ErrorCode;

    #[test]
    fn protected_recreation_is_selected_by_typed_tunnel_lost_only() {
        let typed = anyhow::Error::new(ProtectedControlError::Server(ErrorCode::TunnelLost));
        assert!(is_tunnel_lost(&typed, true));
        assert!(!is_tunnel_lost(
            &anyhow::anyhow!("server message says TunnelLost"),
            true
        ));
        assert!(!is_tunnel_lost(
            &anyhow::Error::new(ProtectedControlError::Server(
                ErrorCode::AccessPolicyMismatch
            )),
            true
        ));
    }

    #[test]
    fn public_and_protected_active_owner_retries_are_typed_only() {
        for error in [
            anyhow::Error::new(ControlError::Server(ErrorCode::TunnelAlreadyActive)),
            anyhow::Error::new(ProtectedControlError::Server(
                ErrorCode::TunnelAlreadyActive,
            )),
        ] {
            assert!(is_recoverable(&error, false));
            assert!(!is_tunnel_lost(&error, false));
        }
        for error in [
            anyhow::Error::new(tnl_protocol::OpenStreamError::Protocol),
            anyhow::Error::new(tnl_protocol::OpenStreamError::Capacity),
            anyhow::Error::new(ControlError::Server(ErrorCode::Unauthorized)),
            anyhow::Error::new(ProtectedControlError::Server(
                ErrorCode::AccessPolicyMismatch,
            )),
            anyhow::anyhow!("server message says TunnelAlreadyActive"),
        ] {
            assert!(!is_recoverable(&error, false));
            assert!(!is_tunnel_lost(&error, false));
        }
        for error in [
            anyhow::Error::new(tnl_protocol::OpenStreamError::ConnectionClosed),
            anyhow::Error::new(tnl_protocol::OpenStreamError::Io).context("open control substream"),
        ] {
            assert!(is_recoverable(&error, false));
            assert!(is_recoverable(&error, true));
        }
        let lost = anyhow::Error::new(ControlError::Server(ErrorCode::TunnelLost));
        assert!(is_tunnel_lost(&lost, false));
        assert!(!is_recoverable(&lost, false));
    }

    #[test]
    fn invalid_protected_acks_and_policy_errors_are_terminal_but_io_can_retry() {
        for failure in [
            ProtectedControlError::Capability,
            ProtectedControlError::Acknowledgment,
            ProtectedControlError::Framing,
            ProtectedControlError::Schema,
            ProtectedControlError::Server(ErrorCode::AccessUnavailable),
            ProtectedControlError::Server(ErrorCode::AccessPolicyMismatch),
        ] {
            assert!(!is_recoverable(&anyhow::Error::new(failure), true));
        }
        assert!(is_recoverable(
            &anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::ConnectionReset)),
            true
        ));
        assert!(
            is_recoverable(
                &anyhow::Error::new(async_tungstenite::tungstenite::Error::Io(
                    std::io::Error::from(std::io::ErrorKind::InvalidData)
                )),
                true
            ),
            "TLS transport IO must retain its same-mode retry classification"
        );
        assert!(!is_recoverable(
            &anyhow::anyhow!("malformed peer response"),
            true
        ));
    }
    #[test]
    fn protected_random_run_recreation_retry_retains_assigned_hostname() {
        let mut requested = None;
        retain_recreation_name(&mut requested, "assigned-random");
        assert_eq!(requested.as_deref(), Some("assigned-random"));
        let mut public = None;
        retain_recreation_name(&mut public, "assigned-random");
        assert_eq!(public.as_deref(), Some("assigned-random"));
    }
}

#[cfg(test)]
mod ownership_tests;

#[cfg(test)]
mod control_tests;

struct RetryBackoff {
    base_ms: u64,
}
impl RetryBackoff {
    const fn new() -> Self {
        Self { base_ms: 1_000 }
    }
    fn next_delay(&mut self, sample: u64) -> Duration {
        // The capped base and 32-bit sample keep this product within u64.
        let jitter = (self.base_ms * 2 / 5) * (sample >> 32) / u64::from(u32::MAX);
        let delay = (self.base_ms * 4 / 5 + jitter).min(30_000);
        self.base_ms = (self.base_ms * 2).min(30_000);
        Duration::from_millis(delay)
    }
    fn observe_session(&mut self, elapsed: Duration) {
        // Two complete heartbeat intervals distinguish a stable session from
        // successful registration followed by immediate loss.
        if elapsed >= Duration::from_secs(30) {
            self.base_ms = 1_000;
        }
    }
    fn sample_delay(&mut self) -> anyhow::Result<Duration> {
        let mut bytes = [0; 8];
        getrandom::fill(&mut bytes).map_err(|_| anyhow::anyhow!("retry jitter unavailable"))?;
        Ok(self.next_delay(u64::from_ne_bytes(bytes)))
    }
}

#[cfg(test)]
mod backoff_tests {
    use super::*;
    #[test]
    fn bounded_jitter_and_only_stable_session_resets_backoff() {
        let mut backoff = RetryBackoff::new();
        assert_eq!(backoff.next_delay(0), Duration::from_millis(800));
        backoff.observe_session(Duration::from_secs(29));
        assert_eq!(backoff.next_delay(u64::MAX), Duration::from_millis(2400));
        for _ in 0..10 {
            backoff.next_delay(0);
        }
        assert_eq!(backoff.next_delay(0), Duration::from_secs(24));
        assert_eq!(backoff.next_delay(u64::MAX), Duration::from_secs(30));
        backoff.observe_session(Duration::from_secs(30));
        assert_eq!(backoff.next_delay(u64::MAX), Duration::from_millis(1200));
    }
}
