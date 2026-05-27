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
        /// Suppress the per-request log (default emits a one-line summary per request).
        #[arg(long, conflicts_with_all = ["verbose", "very_verbose"])]
        quiet: bool,
        /// More detail per request line.
        #[arg(short = 'v', long, conflicts_with = "very_verbose")]
        verbose: bool,
        /// Even more detail (request body preview etc).
        #[arg(short = 'V', long = "very-verbose")]
        very_verbose: bool,
        /// Emit each log line as JSON for piping into jq.
        #[arg(long)]
        json: bool,
    },
    /// List active tunnels for the configured bearer.
    Status {
        /// Show all tunnels on the server (admin scope; v0.1.0-beta = any bearer).
        #[arg(long)]
        all: bool,
        /// Emit JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Close a tunnel by subdomain.
    Stop {
        /// Subdomain to close, e.g. `foo` (not the full hostname).
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
        Cmd::Http {
            port,
            subdomain,
            quiet,
            verbose,
            very_verbose,
            json,
        } => {
            let verbosity = if quiet {
                tnl::inspector::Verbosity::Quiet
            } else if very_verbose {
                tnl::inspector::Verbosity::VeryVerbose
            } else if verbose {
                tnl::inspector::Verbosity::Verbose
            } else {
                tnl::inspector::Verbosity::Default
            };
            let format = if json {
                tnl::inspector::Format::Json
            } else {
                tnl::inspector::Format::Text
            };
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(tnl::commands::http::run(
                port,
                subdomain.as_deref(),
                verbosity,
                format,
            ))
        }
        Cmd::Status { all, json } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(tnl::commands::status::run(all, json))
        }
        Cmd::Stop { subdomain } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(tnl::commands::stop::run(&subdomain))
        }
    }
}
