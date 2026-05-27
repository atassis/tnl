//! Dynamic shell-completion helpers.
//!
//! [`complete_live_subdomains`] is a synchronous completion callback that
//! fetches the list of active tunnel subdomains from the configured daemon's
//! `GET /tunnels` endpoint.  It is wired to `tnl stop <subdomain>` via
//! [`clap_complete::engine::ArgValueCompleter`].
//!
//! Design constraints
//! - Must be synchronous (`clap_complete` calls it synchronously on every TAB).
//! - Must never panic; returns an empty vec on any failure so TAB still works.
//! - Uses a 2-second timeout so a slow/dead daemon does not hang the shell.

use std::ffi::OsStr;
use std::time::Duration;

use clap_complete::engine::CompletionCandidate;

use crate::commands::config::resolve_config_path;
use crate::config::Config;

/// Completion callback for `tnl stop <subdomain>`.
///
/// Returns the list of currently-known subdomain names from `GET /tunnels`.
/// On any failure (config unreadable, daemon down, timeout, bad auth) returns
/// an empty vec — the shell will fall back to path/filename completion.
pub fn complete_live_subdomains(_current: &OsStr) -> Vec<CompletionCandidate> {
    fetch_subdomains().unwrap_or_default()
}

fn fetch_subdomains() -> Option<Vec<CompletionCandidate>> {
    let cfg_path = resolve_config_path().ok()?;
    let cfg = Config::load_from(&cfg_path).ok()?;

    let url = format!("{}/tunnels", cfg.endpoint.trim_end_matches('/'));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;

    let infos: Vec<tnl_protocol::messages::TunnelInfo> = rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .ok()?;
        let resp = client.get(&url).bearer_auth(&cfg.token).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json().await.ok()
    })?;

    Some(
        infos
            .into_iter()
            .map(|t| CompletionCandidate::new(t.subdomain))
            .collect(),
    )
}
