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
