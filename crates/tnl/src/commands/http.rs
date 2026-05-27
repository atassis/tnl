use crate::commands::config::resolve_config_path;
use crate::config::Config;

pub async fn run(port: u16, subdomain: Option<&str>) -> anyhow::Result<()> {
    let cfg_path = resolve_config_path()?;
    let cfg = Config::load_from(&cfg_path)?;

    tokio::select! {
        r = crate::reconnect::run(
            &cfg.endpoint,
            &cfg.token,
            subdomain,
            port,
            crate::reconnect::Hooks::default(),
        ) => r,
        _ = tokio::signal::ctrl_c() => {
            println!("\n✓ stopping tunnel");
            Ok(())
        }
    }
}
