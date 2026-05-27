use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::Config;

pub async fn run(config: PathBuf) -> Result<()> {
    let cfg =
        Config::load(&config).with_context(|| format!("load config at {}", config.display()))?;
    let url = format!("http://{}/healthz", cfg.listen);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        anyhow::bail!("/healthz returned {}", resp.status())
    }
}
