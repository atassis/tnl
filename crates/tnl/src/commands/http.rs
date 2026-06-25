use crate::commands::config::resolve_config_path;
use crate::config::Config;
use crate::target::Target;

pub async fn run(
    target: Target,
    subdomain: Option<&str>,
    verbosity: crate::inspector::Verbosity,
    format: crate::inspector::Format,
) -> anyhow::Result<()> {
    let cfg_path = resolve_config_path()?;
    let cfg = Config::load_resolved(&cfg_path)?;

    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<crate::inspector::LogLine>(1024);
    let inspector = crate::inspector::Inspector::new(log_rx, verbosity, format);
    let _inspector_task = tokio::spawn(inspector.run());

    tokio::select! {
        r = crate::reconnect::run(
            &cfg.endpoint,
            &cfg.token,
            subdomain,
            target,
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
