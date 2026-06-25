use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use tnld::commands::pair::PairCmd;
use tnld::commands::token::TokenCmd;

#[derive(Parser)]
#[command(name = "tnld", version, about = "tnl tunneling daemon")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the daemon (not yet wired in v0.1.0-alpha early tasks).
    Serve {
        #[arg(short, long, default_value = "/etc/tnld/config.toml")]
        config: String,
    },
    /// Print an argon2id hash for the given plaintext token.
    HashPassword { plaintext: String },
    /// Token administration (list / add / revoke).
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
    /// Probe the local daemon's /healthz; exit 0 on 2xx, nonzero otherwise.
    Healthcheck {
        #[arg(short, long, default_value = "/etc/tnld/config.toml")]
        config: std::path::PathBuf,
    },
    /// Pairing administration: mint and list invite codes.
    Pair {
        #[command(subcommand)]
        cmd: PairCmd,
    },
    /// Print shell-completion script for the given shell on stdout.
    Completion {
        /// Shell name (bash, zsh, fish, elvish, powershell).
        shell: Shell,
    },
    /// First-run server wizard. Writes config.toml; optionally mints an initial token.
    Init {
        /// Output path for config.toml.
        #[arg(short = 'c', long, default_value = "/etc/tnld/config.toml")]
        config: std::path::PathBuf,
        #[arg(long)]
        listen: Option<String>,
        #[arg(long)]
        public_url: Option<String>,
        #[arg(long)]
        hostname_root: Option<String>,
        #[arg(long)]
        tokens_file: Option<std::path::PathBuf>,
        /// If set, generate an initial token of this name and print the plaintext.
        #[arg(long)]
        admin_token_name: Option<String>,
        /// Supply a known token value instead of generating one (for CI/provisioning).
        #[arg(long)]
        admin_token: Option<String>,
        /// Override the default 30s grace window.
        #[arg(long)]
        session_grace_sec: Option<u32>,
        /// Overwrite an existing config.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Emit a JSON summary instead of human text.
        #[arg(long)]
        json: bool,
    },
}

fn main() {
    let result = real_main();
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(tnld::exit::classify(&e));
    }
}

fn real_main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::HashPassword { plaintext } => tnld::hash_password::run(&plaintext),
        Cmd::Token { cmd } => tnld::commands::token::run(cmd),
        Cmd::Healthcheck { config } => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(tnld::commands::healthcheck::run(config))
        }
        Cmd::Pair { cmd } => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(tnld::commands::pair::run(cmd))
        }
        Cmd::Init {
            config,
            listen,
            public_url,
            hostname_root,
            tokens_file,
            admin_token_name,
            admin_token,
            session_grace_sec,
            yes,
            json,
        } => tnld::commands::init::run(tnld::commands::init::InitArgs {
            config,
            listen,
            public_url,
            hostname_root,
            tokens_file,
            admin_token_name,
            admin_token,
            session_grace_sec,
            yes,
            json,
        }),
        Cmd::Completion { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "tnld", &mut std::io::stdout());
            Ok(())
        }
        Cmd::Serve { config } => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(async move {
                let cfg = tnld::config::Config::load(std::path::Path::new(&config))?;
                let handle = tnld::serve::spawn_server(cfg).await?;
                eprintln!("tnld listening on http://{}", handle.local_addr);
                handle.join.await?;
                Ok::<_, anyhow::Error>(())
            })
        }
    }
}
