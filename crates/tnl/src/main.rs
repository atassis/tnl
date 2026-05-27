use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tnl", version, about = "tnl tunneling client")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print version and exit.
    Version,
    /// Authenticate with a tnld endpoint and store credentials locally.
    #[command(subcommand)]
    Auth(AuthCmd),
    /// Inspect or modify local CLI config.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Open an HTTP tunnel forwarding to a local port.
    Http {
        /// Local port to forward (e.g. 3000).
        port: u16,
        /// Subdomain under `<hostname_root>` to claim. If omitted, the daemon
        /// picks a random adjective-noun-N name like "happy-otter-12".
        subdomain: Option<String>,
    },
}

#[derive(Subcommand)]
enum AuthCmd {
    /// Validate a token against the daemon and write it to ~/.config/tnl/config.toml.
    Login {
        #[arg(long)]
        endpoint: String,
        #[arg(long, env = "TNL_TOKEN")]
        token: String,
    },
    /// Redeem an invite URL (https://<endpoint>/invite/<code>) and save token.
    Pair {
        /// Full invite URL.
        invite_url: String,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print the current config (token masked).
    Show,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Version => {
            tnl::commands::version::run();
            Ok(())
        }
        Cmd::Config(ConfigCmd::Show) => tnl::commands::config::run_show(),
        Cmd::Auth(AuthCmd::Login { endpoint, token }) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(tnl::commands::auth::run_login(&endpoint, &token))
        }
        Cmd::Auth(AuthCmd::Pair { invite_url }) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(tnl::commands::auth::run_pair(&invite_url))
        }
        Cmd::Http { port, subdomain } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(tnl::commands::http::run(port, subdomain.as_deref()))
        }
    }
}
