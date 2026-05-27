use anyhow::Context;

use crate::commands::config::resolve_config_path;
use crate::config::Config;

pub async fn run_login(endpoint: &str, token: &str) -> anyhow::Result<()> {
    let url = format!("{}/whoami", endpoint.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .with_context(|| format!("connect to {url}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("token rejected by server ({})", resp.status());
    }
    let cfg = Config {
        endpoint: endpoint.to_string(),
        token: token.to_string(),
    };
    let path = resolve_config_path()?;
    cfg.save_to(&path)?;
    println!("✓ logged in; config written to {}", path.display());
    Ok(())
}

pub async fn run_pair(invite_url: &str) -> anyhow::Result<()> {
    let invite = crate::invite::parse(invite_url)?;
    let url = format!("{}/pair/redeem", invite.endpoint.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let resp = client
        .post(&url)
        .json(&serde_json::json!({"code": invite.code}))
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("redeem failed: {}", resp.status());
    }
    let body: tnl_protocol::messages::PairRedeemResp = resp.json().await?;
    let name = body.name.clone();
    let cfg = Config {
        endpoint: body.endpoint,
        token: body.token,
    };
    let path = resolve_config_path()?;
    cfg.save_to(&path)?;
    eprintln!(
        "✓ paired as {:?}; config written to {}",
        name,
        path.display()
    );
    Ok(())
}
