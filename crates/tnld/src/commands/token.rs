//! `tnld token add/list/revoke` — admin CLI for the tokens file.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Subcommand;
use serde::Serialize;

use crate::auth::TokensFile;

#[derive(Subcommand, Debug)]
pub enum TokenCmd {
    /// List all tokens (name + hash prefix, no plaintext).
    List {
        /// Path to tokens.toml. Defaults to /etc/tnld/tokens.toml.
        #[arg(long, default_value = "/etc/tnld/tokens.toml")]
        tokens_file: PathBuf,
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Serialize)]
struct TokenSummary<'a> {
    name: &'a str,
    hash_prefix: String,
}

pub fn run(cmd: TokenCmd) -> Result<()> {
    match cmd {
        TokenCmd::List { tokens_file, json } => {
            let raw = std::fs::read_to_string(&tokens_file)
                .with_context(|| format!("read tokens file at {}", tokens_file.display()))?;
            let tf: TokensFile = toml::from_str(&raw).context("parse tokens file")?;
            if json {
                let out: Vec<TokenSummary<'_>> = tf
                    .tokens
                    .iter()
                    .map(|t| TokenSummary {
                        name: &t.name,
                        hash_prefix: hash_prefix(&t.hash),
                    })
                    .collect();
                println!("{}", serde_json::to_string(&out)?);
            } else {
                println!("{:<20} {:<30}", "NAME", "HASH PREFIX");
                for t in &tf.tokens {
                    println!("{:<20} {}", t.name, hash_prefix(&t.hash));
                }
            }
            Ok(())
        }
    }
}

fn hash_prefix(h: &str) -> String {
    // argon2id encoded hashes start with "$argon2id$v=19$m=...,t=...,p=...$<salt>$<hash>".
    // Show enough that operators can tell parameters apart, but not the salt/hash.
    h.split('$').take(4).collect::<Vec<_>>().join("$")
}
