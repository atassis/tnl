//! `tnl status` — list active tunnels by bearer-authed call to GET /tunnels.

use anyhow::{Context, Result};

use crate::commands::config::resolve_config_path;
use crate::config::Config;

pub async fn run(all: bool, json: bool) -> Result<()> {
    let cfg_path = resolve_config_path()?;
    let cfg = Config::load_from(&cfg_path)?;
    let url = format!(
        "{}/tunnels{}",
        cfg.endpoint.trim_end_matches('/'),
        if all { "?all=true" } else { "" }
    );
    let resp = reqwest::Client::new()
        .get(&url)
        .bearer_auth(&cfg.token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("GET /tunnels: {}", resp.status());
    }
    let infos: Vec<tnl_protocol::messages::TunnelInfo> = resp.json().await?;
    if json {
        println!("{}", serde_json::to_string(&infos)?);
    } else {
        println!(
            "{:<20} {:<8} {:<10} {:<12} {:<12}",
            "SUBDOMAIN", "ACTIVE", "REQUESTS", "BYTES IN", "BYTES OUT"
        );
        for i in &infos {
            println!(
                "{:<20} {:<8} {:<10} {:<12} {:<12}",
                i.subdomain, i.active, i.requests, i.bytes_in, i.bytes_out
            );
        }
    }
    Ok(())
}
