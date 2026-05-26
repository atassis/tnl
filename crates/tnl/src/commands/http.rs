use anyhow::Context;

use crate::client::{connect_and_create, run_accept_loop};
use crate::commands::config::resolve_config_path;
use crate::config::Config;

pub async fn run(port: u16, subdomain: &str) -> anyhow::Result<()> {
    let cfg_path = resolve_config_path()?;
    let cfg = Config::load_from(&cfg_path)?;
    let session = connect_and_create(&cfg.endpoint, &cfg.token, subdomain).await?;
    println!("┌─ tnl ─────────────────────────────────────────");
    println!("│ Tunnel:    https://{}", session.hostname);
    println!("│ Forward:   127.0.0.1:{port}");
    println!("│ Press Ctrl-C to stop.");
    println!("└────────────────────────────────────────────────");

    let session_box = session.session;
    let _ctrl_keep = session.control;
    let accept_handle = tokio::spawn(run_accept_loop(session_box, port));

    tokio::select! {
        res = accept_handle => {
            res.context("accept loop join")?
        }
        _ = tokio::signal::ctrl_c() => {
            println!("\n✓ stopping tunnel");
            Ok(())
        }
    }
}
