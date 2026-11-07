use crate::commands::config::resolve_config_path;
use crate::config::Config;
use crate::target::Target;

#[derive(Clone, Copy, Debug)]
pub struct InspectionOptions {
    pub port: u16,
    pub replay_retain_secrets: bool,
    pub bodies: crate::inspection::PreviewMode,
}

pub async fn run(
    target: Target,
    subdomain: Option<&str>,
    host_header: crate::host_header::HostHeader,
    verbosity: crate::inspector::Verbosity,
    format: crate::inspector::Format,
) -> anyhow::Result<()> {
    run_with_inspection(target, subdomain, host_header, verbosity, format, None).await
}

pub async fn run_with_inspection(
    target: Target,
    subdomain: Option<&str>,
    host_header: crate::host_header::HostHeader,
    verbosity: crate::inspector::Verbosity,
    format: crate::inspector::Format,
    options: Option<InspectionOptions>,
) -> anyhow::Result<()> {
    let cfg_path = resolve_config_path()?;
    let cfg = Config::load_resolved(&cfg_path)?;
    run_configured(
        &cfg,
        target,
        subdomain,
        host_header,
        verbosity,
        format,
        &crate::client::TunnelOptions::default(),
        options,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_configured(
    cfg: &Config,
    target: Target,
    subdomain: Option<&str>,
    host_header: crate::host_header::HostHeader,
    verbosity: crate::inspector::Verbosity,
    format: crate::inspector::Format,
    options: &crate::client::TunnelOptions,
    inspection_options: Option<InspectionOptions>,
) -> anyhow::Result<()> {
    let (inspection, service) = if let Some(options) = inspection_options {
        let bound = crate::inspection::BoundInspector::bind(options.port).await?;
        let replay = crate::replay::ReplayController::new(
            options.replay_retain_secrets,
            bound.addr().port(),
        )?;
        let inspection = crate::inspection::Inspection::with_replay(options.bodies, replay);
        let service = bound.start(inspection.clone());
        eprintln!(
            "Inspector (private; do not share): {}",
            service.bootstrap_url()
        );
        if options.replay_retain_secrets {
            eprintln!("WARNING: raw replay retention ENABLED: original query, credentials, cookies and body secrets remain in memory. Caps: 20 requests, 10 minutes, 16 MiB total, 1 MiB body/request; one execution, explicit confirmation for every method. Previews are separate. Do not expose/share this inspector.");
        }
        (Some(inspection), Some(service))
    } else {
        (None, None)
    };

    let (log_tx, log_rx) = tokio::sync::mpsc::channel::<crate::inspector::LogLine>(1024);
    let inspector = crate::inspector::Inspector::new(log_rx, verbosity, format);
    let inspector_task = tokio::spawn(inspector.run());

    let result = crate::reconnect::run_until_shutdown(
        &cfg.endpoint,
        &cfg.token,
        subdomain,
        target,
        host_header,
        options,
        crate::reconnect::Hooks {
            inspection,
            cancel_first_session: None,
            log_tx: Some(log_tx),
        },
        shutdown_signal(),
    )
    .await;
    if let Some(service) = service {
        service.shutdown().await;
    }
    inspector_task.abort();
    let _ = inspector_task.await;
    result
}

async fn shutdown_signal() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result?,
            signal = terminate.recv() => {
                signal.ok_or_else(|| anyhow::anyhow!("termination signal stream closed"))?;
            }
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    println!("\nStopping tunnel…");
    Ok(())
}
