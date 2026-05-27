use std::path::PathBuf;

use crate::config::Config;

pub fn run_show(json: bool) -> anyhow::Result<()> {
    let path = resolve_config_path()?;
    let cfg = Config::load_from(&path)?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "endpoint": cfg.endpoint,
                "token_masked": cfg.masked_token(),
                "path": path.display().to_string(),
            })
        );
    } else {
        println!("endpoint: {}", cfg.endpoint);
        println!("token:    {}", cfg.masked_token());
    }
    Ok(())
}

pub fn resolve_config_path() -> anyhow::Result<PathBuf> {
    if let Ok(p) = std::env::var("TNL_CONFIG") {
        return Ok(PathBuf::from(p));
    }
    Config::default_path()
}
