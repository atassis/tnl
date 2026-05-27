//! `tnl stop <subdomain>` — close a tunnel via DELETE /tunnels/{subdomain}.

use anyhow::{Context, Result};

use crate::commands::config::resolve_config_path;
use crate::config::Config;

pub async fn run(subdomain: &str) -> Result<()> {
    let cfg_path = resolve_config_path()?;
    let cfg = Config::load_from(&cfg_path)?;
    let url = format!(
        "{}/tunnels/{}",
        cfg.endpoint.trim_end_matches('/'),
        subdomain
    );
    let resp = reqwest::Client::new()
        .delete(&url)
        .bearer_auth(&cfg.token)
        .send()
        .await
        .with_context(|| format!("DELETE {url}"))?;
    match resp.status() {
        reqwest::StatusCode::NO_CONTENT => {
            eprintln!("tunnel {subdomain:?} closed");
            Ok(())
        }
        reqwest::StatusCode::NOT_FOUND => anyhow::bail!("no such tunnel: {subdomain:?}"),
        reqwest::StatusCode::FORBIDDEN => {
            anyhow::bail!("not authorised to close {subdomain:?}")
        }
        other => anyhow::bail!("DELETE failed: {other}"),
    }
}
