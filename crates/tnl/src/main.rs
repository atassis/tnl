use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::engine::ArgValueCompleter;
use clap_complete::env::{Bash, Elvish, EnvCompleter, Fish, Powershell, Zsh};
use clap_complete::CompleteEnv;

/// Shell names accepted by `tnl completion`.
///
/// A hand-rolled enum so we don't depend on `clap_complete::aot::Shell` for
/// the derive macro while still giving clap a `ValueEnum` for argument parsing.
#[derive(Clone, Debug, clap::ValueEnum)]
enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    Elvish,
    Powershell,
}

impl CompletionShell {
    fn env_completer(&self) -> &'static dyn EnvCompleter {
        match self {
            Self::Bash => &Bash,
            Self::Zsh => &Zsh,
            Self::Fish => &Fish,
            Self::Elvish => &Elvish,
            Self::Powershell => &Powershell,
        }
    }
}

#[derive(Parser)]
#[command(name = "tnl", version, about = "tnl tunneling client")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print version and exit.
    Version {
        #[arg(long)]
        json: bool,
    },
    /// Authenticate with a tnld endpoint and store credentials locally.
    #[command(subcommand)]
    Auth(AuthCmd),
    /// Inspect or modify local CLI config.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Open an HTTP tunnel forwarding to a local port or host:port.
    Http {
        /// Local target: a bare port (forwards to localhost dual-stack), or
        /// `IP:PORT`. Examples: `3000`, `127.0.0.1:3000`, `[::1]:8080`,
        /// `192.168.1.50:80`. Hostnames are not accepted in alpha.3.
        #[arg(value_name = "TARGET", value_parser = parse_target)]
        target: tnl::target::Target,
        /// Subdomain to claim. If omitted, the daemon assigns a random one.
        subdomain: Option<String>,
        /// `Host` sent to your backend. Default (auto): forward the real host,
        /// but if the dev server rejects it (Vite/webpack/Django/Rails host
        /// allowlists) rewrite it to the connected address for later requests.
        /// `preserve` disables that; `rewrite` always rewrites; any other value
        /// is used verbatim.
        #[arg(long, value_name = "MODE")]
        host_header: Option<String>,
        #[arg(long, conflicts_with_all = ["verbose", "very_verbose"])]
        quiet: bool,
        #[arg(short = 'v', long, conflicts_with = "very_verbose")]
        verbose: bool,
        #[arg(short = 'V', long = "very-verbose")]
        very_verbose: bool,
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
        #[arg(add = ArgValueCompleter::new(tnl::completion::complete_live_subdomains))]
        subdomain: String,
    },
    /// Self-diagnostic: check config, daemon connectivity, token validity, clock skew.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Print dynamic shell-completion script for the given shell on stdout.
    ///
    /// Source the output in your shell init file to enable TAB completion.
    /// The emitted script calls back into `tnl` on every TAB so that
    /// completions are always live (e.g. `tnl stop <TAB>` fetches real tunnel
    /// names from the daemon).
    ///
    /// Example (zsh):
    ///   echo 'source <(COMPLETE=zsh tnl --)' >> ~/.zshrc
    ///   # or use this subcommand for a one-time print:
    ///   source <(tnl completion zsh)
    Completion {
        /// Shell name (bash, zsh, fish, elvish, powershell).
        shell: CompletionShell,
    },
    /// First-run wizard (interactive in a TTY; flag-driven otherwise).
    Init {
        /// Redeem an invite URL.
        #[arg(long)]
        invite: Option<String>,
        /// Endpoint URL (used with --token).
        #[arg(long)]
        endpoint: Option<String>,
        /// Bearer token (used with --endpoint).
        #[arg(long, env = "TNL_TOKEN")]
        token: Option<String>,
        /// Overwrite an existing config without prompting.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Emit a JSON status line at the end instead of human text.
        #[arg(long)]
        json: bool,
        /// Skip the shell-completion offer.
        #[arg(long)]
        no_shell_completion: bool,
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
    Show {
        #[arg(long)]
        json: bool,
    },
}

fn parse_target(s: &str) -> Result<tnl::target::Target, String> {
    use std::str::FromStr as _;
    tnl::target::Target::from_str(s).map_err(|e| format!("{e}"))
}

fn main() {
    // Must be called before argument parsing so that completion requests
    // (COMPLETE=<shell> tnl -- …) are intercepted before any real CLI logic
    // runs.  CompleteEnv::complete() is a no-op when the COMPLETE env var is
    // absent, so it is safe to call unconditionally.
    CompleteEnv::with_factory(Cli::command).complete();

    let result = real_main();
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(tnl::exit::classify(&e));
    }
}

fn real_main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Version { json } => {
            tnl::commands::version::run(json);
            Ok(())
        }
        Cmd::Config(ConfigCmd::Show { json }) => tnl::commands::config::run_show(json),
        Cmd::Auth(AuthCmd::Login { endpoint, token }) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(tnl::commands::auth::run_login(&endpoint, &token))
        }
        Cmd::Auth(AuthCmd::Pair { invite_url }) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(tnl::commands::auth::run_pair(&invite_url))
        }
        Cmd::Http {
            target,
            subdomain,
            host_header,
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
                target,
                subdomain.as_deref(),
                tnl::host_header::HostHeader::parse(host_header.as_deref()),
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
        Cmd::Doctor { json } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(tnl::commands::doctor::run(json))
        }
        Cmd::Completion { shell } => {
            // Emit the dynamic registration script.  The emitted function
            // calls back into `tnl` on every TAB via COMPLETE=<shell>, so
            // custom completers (e.g. live subdomain list for `tnl stop`) are
            // exercised at completion time.
            let completer = shell.env_completer();
            let bin_path = std::env::current_exe()
                .map_or_else(|_| "tnl".to_owned(), |p| p.to_string_lossy().into_owned());
            completer
                .write_registration("COMPLETE", "tnl", "tnl", &bin_path, &mut std::io::stdout())
                .map_err(|e| anyhow::anyhow!("write completion script: {e}"))
        }
        Cmd::Init {
            invite,
            endpoint,
            token,
            yes,
            json,
            no_shell_completion,
        } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(tnl::commands::init::run(tnl::commands::init::InitArgs {
                invite,
                endpoint,
                token,
                yes,
                json,
                no_shell_completion,
            }))
        }
    }
}
