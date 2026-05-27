use crate::commands::config::resolve_config_path;
use crate::config::Config;

pub async fn run(
    port: u16,
    subdomain: Option<&str>,
    verbosity: crate::inspector::Verbosity,
    format: crate::inspector::Format,
) -> anyhow::Result<()> {
    let cfg_path = resolve_config_path()?;
    let cfg = Config::load_from(&cfg_path)?;

    // Inspector channel: forwarder → Inspector renderer.
    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<crate::inspector::LogLine>(1024);
    let inspector = crate::inspector::Inspector::new(log_rx, verbosity, format);
    let _inspector_task = tokio::spawn(inspector.run());

    tokio::select! {
        r = crate::reconnect::run(
            &cfg.endpoint,
            &cfg.token,
            subdomain,
            port,
            crate::reconnect::Hooks {
                cancel_first_session: None,
                log_tx: Some(log_tx),
            },
        ) => r,
        _ = tokio::signal::ctrl_c() => {
            println!("\n✓ stopping tunnel");
            Ok(())
        }
    }
}
