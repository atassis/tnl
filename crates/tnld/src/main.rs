use clap::{Parser, Subcommand};

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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::HashPassword { plaintext } => tnld::hash_password::run(&plaintext),
        Cmd::Serve { config } => {
            anyhow::bail!("`serve` not yet implemented (config path: {config})")
        }
    }
}
