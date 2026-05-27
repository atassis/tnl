//! Client reconnect loop with exponential backoff + reattach.
//!
//! Strategy:
//! - First connect: `CreateTunnel` (or random if subdomain is `None`).
//! - Subsequent connects (after a disconnect): `ReattachTunnel` with the
//!   `tunnel_id` from the previous successful connect.
//! - If reattach returns `TunnelLost`, fall back to `CreateTunnel` with the
//!   same subdomain — the grace window has elapsed and we need a fresh tunnel.
//! - Backoff: 1s → 2s → 4s → 8s → 16s → 30s (capped). Reset on success.
//! - Auth failures (`Unauthorized` / "token rejected") are NOT recoverable.

use std::time::Duration;

use tracing::{info, warn};

/// Optional hooks used in tests to inject behaviour without spawning real
/// network connections.
#[derive(Default)]
pub struct Hooks {
    /// When this oneshot fires, the current accept loop is cancelled so the
    /// outer loop can attempt a reattach.
    pub cancel_first_session: Option<tokio::sync::oneshot::Receiver<()>>,
    /// If set, each forwarded substream tees its head into this channel
    /// (consumed by Inspector for per-request log lines).
    pub log_tx: Option<tokio::sync::mpsc::Sender<crate::inspector::LogLine>>,
}

impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooks")
            .field(
                "cancel_first_session",
                &self.cancel_first_session.as_ref().map(|_| "<receiver>"),
            )
            .field("log_tx", &self.log_tx.as_ref().map(|_| "<sender>"))
            .finish()
    }
}

/// Run the reconnect loop until a non-recoverable error occurs or `Ctrl-C`
/// cancels the outer `tokio::select!`.
///
/// Prints a one-time banner on the first successful connect.
pub async fn run(
    endpoint: &str,
    token: &str,
    subdomain: Option<&str>,
    local_port: u16,
    mut hooks: Hooks,
) -> anyhow::Result<()> {
    let mut backoff_ms: u64 = 1_000;
    let mut last_tunnel: Option<(ulid::Ulid, String)> = None;
    let mut printed_banner = false;

    loop {
        let result = if let Some((tid, sub)) = &last_tunnel {
            // Clone the subdomain before the borrow on `last_tunnel` ends so
            // we can use it after potentially setting `last_tunnel = None`.
            let tid = *tid;
            let sub = sub.clone();
            // Try to reattach within the grace window.
            match crate::client::connect_and_reattach(endpoint, token, tid, &sub).await {
                Ok(s) => Ok(s),
                Err(e) => {
                    // On TunnelLost the grace window has elapsed; fall back to
                    // a fresh CreateTunnel with the same subdomain so the URL
                    // stays stable.
                    if format!("{e:#}").contains("TunnelLost") {
                        eprintln!("info: grace window elapsed; recreating tunnel");
                        last_tunnel = None;
                        crate::client::connect_and_create(endpoint, token, &sub).await
                    } else {
                        Err(e)
                    }
                }
            }
        } else if let Some(s) = subdomain {
            crate::client::connect_and_create(endpoint, token, s).await
        } else {
            crate::client::connect_and_create_random(endpoint, token).await
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
                if !is_recoverable(&e) {
                    return Err(e);
                }
                warn!(%e, "connection failed; reconnecting in {}s", backoff_ms / 1000);
                eprintln!(
                    "warning: connection lost ({e:#}); reconnecting in {}s",
                    backoff_ms / 1_000
                );
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(30_000);
                continue;
            }
        };

        // Successful connect: reset backoff, remember tunnel for the next reattach.
        backoff_ms = 1_000;
        last_tunnel = Some((session.tunnel_id, session.subdomain.clone()));

        if !printed_banner {
            printed_banner = true;
            println!("┌─ tnl ─────────────────────────────────────────");
            println!("│ Tunnel:    https://{}", session.hostname);
            println!("│ Subdomain: {}", session.subdomain);
            println!("│ Forward:   127.0.0.1:{local_port}");
            println!("│ Press Ctrl-C to stop.");
            println!("└────────────────────────────────────────────────");
        }

        let cancel = hooks.cancel_first_session.take();
        // Keep the control stream alive for the duration of the session.
        let _ctrl_keep = session.control;
        let log_tx = hooks.log_tx.clone();
        let accept = tokio::spawn(crate::client::run_accept_loop(
            session.session,
            local_port,
            log_tx,
        ));

        tokio::select! {
            r = accept => {
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
        // Loop restarts; next iteration tries ReattachTunnel.
    }
}

/// Returns `false` for errors that cannot be fixed by retrying (bad token, etc.).
fn is_recoverable(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}").to_lowercase();
    !(s.contains("unauthor") || s.contains("token rejected"))
}
