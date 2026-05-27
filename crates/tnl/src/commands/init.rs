//! `tnl init` — first-run wizard.
//!
//! Three scenarios per the v0.1.0-beta spec:
//! 1. Invite link: user has https://<endpoint>/invite/<code>; redeem.
//! 2. Paste token: user has <endpoint> + <token>; validate + save.
//! 3. Existing config: prompt to test / replace / cancel.
//!
//! Non-interactive: at least one of (--invite) or (--endpoint AND --token)
//! must be present, OR stdin must be a TTY.

use std::io::IsTerminal;

use anyhow::{Context, Result};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Input, Password, Select};

use crate::commands::auth::{run_login, run_pair};
use crate::commands::config::resolve_config_path;
use crate::config::Config;

#[derive(Debug, Default)]
pub struct InitArgs {
    pub invite: Option<String>,
    pub endpoint: Option<String>,
    pub token: Option<String>,
    pub yes: bool,
    pub json: bool,
    pub no_shell_completion: bool,
}

pub async fn run(args: InitArgs) -> Result<()> {
    // 1) Detect existing config.
    let cfg_path = resolve_config_path()?;
    let existing = Config::load_from(&cfg_path).ok();

    // 2) Non-interactive paths take priority over the menu.
    if let Some(invite) = args.invite.as_deref() {
        if existing.is_some() && !args.yes {
            anyhow::bail!(
                "config already exists at {}; pass -y to overwrite",
                cfg_path.display()
            );
        }
        run_pair(invite).await?;
        offer_shell_completion(&args);
        return Ok(());
    }
    if let (Some(endpoint), Some(token)) = (args.endpoint.as_deref(), args.token.as_deref()) {
        if existing.is_some() && !args.yes {
            anyhow::bail!(
                "config already exists at {}; pass -y to overwrite",
                cfg_path.display()
            );
        }
        run_login(endpoint, token).await?;
        offer_shell_completion(&args);
        return Ok(());
    }

    // 3) From here we need a TTY.
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "non-interactive `tnl init` requires --invite OR (--endpoint AND --token); \
             stdin is not a TTY and no flags were provided"
        );
    }

    if let Some(cfg) = existing {
        let theme = ColorfulTheme::default();
        let choices = &["Keep current config", "Replace config", "Cancel"];
        let pick = Select::with_theme(&theme)
            .with_prompt(format!(
                "config exists at {}: endpoint = {}",
                cfg_path.display(),
                cfg.endpoint
            ))
            .items(choices)
            .default(0)
            .interact()
            .context("read selection")?;
        match pick {
            0 => {
                println!(
                    "keeping current config; run `tnl http <port> [subdomain]` to open a tunnel."
                );
                return Ok(());
            }
            2 => {
                println!("aborted.");
                return Ok(());
            }
            _ => {} // 1 → fall through to fresh setup
        }
    }

    let theme = ColorfulTheme::default();
    let choices = &[
        "I have an invite link (https://.../invite/...)",
        "I have an endpoint URL + token",
        "Cancel",
    ];
    let pick = Select::with_theme(&theme)
        .with_prompt("how would you like to set up your tnl client?")
        .items(choices)
        .default(0)
        .interact()
        .context("read selection")?;

    match pick {
        0 => {
            let invite: String = Input::with_theme(&theme)
                .with_prompt("paste the invite URL")
                .interact_text()
                .context("read invite URL")?;
            run_pair(invite.trim()).await?;
        }
        1 => {
            let endpoint: String = Input::with_theme(&theme)
                .with_prompt("endpoint URL (e.g. https://tnl-api.example.com)")
                .interact_text()
                .context("read endpoint")?;
            let token: String = Password::with_theme(&theme)
                .with_prompt("token (input hidden)")
                .interact()
                .context("read token")?;
            run_login(endpoint.trim(), token.trim()).await?;
        }
        _ => {
            println!("aborted.");
            return Ok(());
        }
    }

    offer_shell_completion(&args);
    Ok(())
}

const fn offer_shell_completion(args: &InitArgs) {
    if args.no_shell_completion || args.json {
        return;
    }
    // TODO(beta-task-28/32): once clap_complete is wired up, generate
    // completion scripts into the user's XDG completion dir. For now we
    // just point the user at the future `tnl completion <shell>` command.
    let _ = args;
}
