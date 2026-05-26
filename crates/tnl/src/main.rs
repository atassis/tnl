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
        /// Subdomain under `<hostname_root>` to claim (e.g. "foo" → foo.t.example.com).
        subdomain: String,
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
        Cmd::Http { port, subdomain } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(tnl::commands::http::run(port, &subdomain))
        }
    }
}
