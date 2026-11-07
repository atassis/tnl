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
    let mut backoff_ms: u64 = 1_000;
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
        let result = {
            let started = std::sync::atomic::AtomicBool::new(false);
            let mut connect = Box::pin(async {
                started.store(true, std::sync::atomic::Ordering::Relaxed);
                if let Some((tid, sub)) = &last_tunnel {
                    // Clone the subdomain before the borrow on `last_tunnel` ends so
                    // we can use it after potentially setting `last_tunnel = None`.
                    let tid = *tid;
                    let sub = sub.clone();
                    // Try to reattach within the grace window.
                    match crate::client::connect_and_reattach_with_options(
                        endpoint, token, tid, &sub, options,
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
                                retain_protected_recreation_name(
                                    &mut create_subdomain,
                                    &sub,
                                    options.access.is_some(),
                                );
                                crate::client::connect_and_create_with_options(
                                    endpoint,
                                    token,
                                    Some(&sub),
                                    options,
                                )
                                .await
                            } else {
                                Err(e)
                            }
                        }
                    }
                } else if let Some(s) = create_subdomain.as_deref() {
                    crate::client::connect_and_create_with_options(
                        endpoint,
                        token,
                        Some(s),
                        options,
                    )
                    .await
                } else {
                    crate::client::connect_and_create_with_options(endpoint, token, None, options)
                        .await
                }
            });
            tokio::select! {
                biased;
                signal = &mut shutdown => {
                    signal?;
                    begin_shutdown(hooks.inspection.as_ref(), tasks);
                    let deadline = tokio::time::Instant::now() + SHUTDOWN_TIMEOUT;
                    if started.load(std::sync::atomic::Ordering::Relaxed) {
                        // Retain the in-flight registration long enough to close a late ACK.
                        let released = tokio::time::timeout_at(deadline, async {
                            let mut connected = connect.await?;
                            crate::client::close_control(&mut connected.control).await
                        }).await;
                        report_shutdown(matches!(released, Ok(Ok(()))));
                    } else {
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
                warn!(%e, "connection failed; reconnecting in {}s", backoff_ms / 1000);
                eprintln!(
                    "warning: connection lost ({e:#}); reconnecting in {}s",
                    backoff_ms / 1_000
                );
                tokio::select! {
                    biased;
                    signal = &mut shutdown => {
                        signal?;
                        begin_shutdown(hooks.inspection.as_ref(), tasks);
                        release_disconnected(endpoint, token, last_tunnel.as_ref(), options).await;
                        return Ok(());
                    }
                    () = tokio::time::sleep(Duration::from_millis(backoff_ms)) => {},
                }
                backoff_ms = (backoff_ms * 2).min(30_000);
                continue;
            }
        };

        // Successful connect: reset backoff, remember tunnel for the next reattach.
        backoff_ms = 1_000;
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
        let mut control = session.control;
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

        tokio::select! {
            biased;
            signal = &mut shutdown => {
                signal?;
                begin_shutdown(hooks.inspection.as_ref(), tasks);
                let released = tokio::time::timeout(SHUTDOWN_TIMEOUT, crate::client::close_control(&mut control)).await;
                report_shutdown(matches!(released, Ok(Ok(()))));
                return Ok(());
            }
            r = &mut accept => {
                match r {
                    Ok(Ok(())) => info!("accept loop ended cleanly; reconnecting"),
                    Ok(Err(e)) => warn!(%e, "accept loop ended with error; reconnecting"),
                    Err(e) => warn!(%e, "accept loop task panicked; reconnecting"),
                }
            }
            () = async {
                if let Some(rx) = cancel {
                    let _ = rx.await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                eprintln!("[test hook] forced disconnect");
            }
        }
        if let Some(inspection) = &hooks.inspection {
            inspection.disconnected();
        }
        // Availability is not a claim that detached forwarding tasks ended.
        // Loop restarts; next iteration tries ReattachTunnel.
    }
}

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

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
    let released = tokio::time::timeout(SHUTDOWN_TIMEOUT, async {
        let (id, subdomain) = previous.ok_or_else(|| anyhow::anyhow!("no confirmed session"))?;
        // Reattach only: cleanup must never create or delete by a reusable name.
        let mut connected = crate::client::connect_and_reattach_with_options(
            endpoint, token, *id, subdomain, options,
        )
        .await?;
        crate::client::close_control(&mut connected.control).await
    })
    .await;
    report_shutdown(matches!(released, Ok(Ok(()))));
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
fn retain_protected_recreation_name(
    requested: &mut Option<String>,
    assigned: &str,
    protected: bool,
) {
    // A failed recreation after an originally random protected create must not
    // turn the next retry into another random-hostname request.
    if protected {
        *requested = Some(assigned.to_owned());
    }
}

fn is_tunnel_lost(e: &anyhow::Error, protected: bool) -> bool {
    if protected {
        matches!(
            e.downcast_ref::<crate::client::ProtectedControlError>(),
            Some(crate::client::ProtectedControlError::Server(
                tnl_protocol::ErrorCode::TunnelLost
            ))
        )
    } else {
        format!("{e:#}").contains("TunnelLost")
    }
}

fn is_recoverable(e: &anyhow::Error, protected: bool) -> bool {
    if protected {
        if e.downcast_ref::<crate::client::ProtectedControlError>()
            .is_some()
        {
            return false;
        }
        if e.downcast_ref::<std::io::Error>().is_some() {
            return true;
        }
        return matches!(
            e.downcast_ref::<async_tungstenite::tungstenite::Error>(),
            Some(
                async_tungstenite::tungstenite::Error::Io(_)
                    | async_tungstenite::tungstenite::Error::ConnectionClosed
                    | async_tungstenite::tungstenite::Error::AlreadyClosed
            )
        );
    }
    let s = format!("{e:#}").to_lowercase();
    !(s.contains("unauthor") || s.contains("token rejected"))
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
mod protected_tests {
    use super::*;
    use crate::client::ProtectedControlError;
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
        retain_protected_recreation_name(&mut requested, "assigned-random", true);
        assert_eq!(requested.as_deref(), Some("assigned-random"));
        let mut public = None;
        retain_protected_recreation_name(&mut public, "assigned-random", false);
        assert!(public.is_none());
    }
}
