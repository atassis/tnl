//! `tnld token add/list/revoke` — admin CLI for the tokens file.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Subcommand;
use serde::Serialize;

use crate::auth::{hash_plaintext, TokenEntry, TokensFile};

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
    /// Add a new token. Prints the plaintext value on stdout exactly once.
    Add {
        /// Token name. Must be unique within the tokens file.
        name: String,
        #[arg(long, default_value = "/etc/tnld/tokens.toml")]
        tokens_file: PathBuf,
        /// If a token with this name already exists, overwrite it.
        #[arg(long)]
        replace: bool,
    },
    /// Remove a token by name.
    Revoke {
        name: String,
        #[arg(long, default_value = "/etc/tnld/tokens.toml")]
        tokens_file: PathBuf,
        /// Skip the "are you sure?" confirmation.
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

#[derive(Serialize)]
struct TokenSummary<'a> {
    name: &'a str,
    hash_prefix: String,
}

pub fn run(cmd: TokenCmd) -> Result<()> {
    match cmd {
        TokenCmd::List { tokens_file, json } => list(&tokens_file, json),
        TokenCmd::Add {
            name,
            tokens_file,
            replace,
        } => add(&name, &tokens_file, replace),
        TokenCmd::Revoke {
            name,
            tokens_file,
            yes,
        } => revoke(&name, &tokens_file, yes),
    }
}

fn list(tokens_file: &PathBuf, json: bool) -> Result<()> {
    let raw = std::fs::read_to_string(tokens_file)
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

fn add(name: &str, tokens_file: &PathBuf, replace: bool) -> Result<()> {
    let mut tf: TokensFile = match std::fs::read_to_string(tokens_file) {
        Ok(raw) if !raw.trim().is_empty() => toml::from_str(&raw).context("parse tokens file")?,
        _ => TokensFile { tokens: vec![] },
    };

    let existing_idx = tf.tokens.iter().position(|t| t.name == name);
    if existing_idx.is_some() && !replace {
        anyhow::bail!("token {name:?} already exists; use --replace or choose another name");
    }

    let plaintext = format!("tnl_{}", random_token_suffix(26));
    let hash = hash_plaintext(&plaintext).context("argon2 hash")?;
    let entry = TokenEntry {
        name: name.to_string(),
        hash,
    };

    if let Some(i) = existing_idx {
        tf.tokens[i] = entry;
    } else {
        tf.tokens.push(entry);
    }

    write_tokens_file_atomic(tokens_file, &tf)?;

    eprintln!("✓ Token {:?} written to {}", name, tokens_file.display());
    println!("{plaintext}");
    Ok(())
}

fn revoke(name: &str, tokens_file: &PathBuf, yes: bool) -> Result<()> {
    let raw = std::fs::read_to_string(tokens_file)
        .with_context(|| format!("read tokens file at {}", tokens_file.display()))?;
    let mut tf: TokensFile = if raw.trim().is_empty() {
        TokensFile { tokens: vec![] }
    } else {
        toml::from_str(&raw).context("parse tokens file")?
    };
    let before = tf.tokens.len();
    tf.tokens.retain(|t| t.name != name);
    if tf.tokens.len() == before {
        anyhow::bail!("no such token: {name:?}");
    }
    if !yes {
        use std::io::{stdin, BufRead};
        eprint!("Revoke token {name:?}? Active sessions will be terminated. (y/N) ");
        let mut line = String::new();
        stdin()
            .lock()
            .read_line(&mut line)
            .context("read confirmation")?;
        if !matches!(line.trim().to_lowercase().as_str(), "y" | "yes") {
            anyhow::bail!("aborted");
        }
    }
    write_tokens_file_atomic(tokens_file, &tf)?;
    eprintln!("✓ Token {name:?} revoked.");
    Ok(())
}

fn random_token_suffix(len: usize) -> String {
    use rand::distributions::Slice;
    use rand::Rng;
    // Exclude visually ambiguous chars: 0/O, 1/l/I.
    const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";
    let mut rng = rand::thread_rng();
    let dist = Slice::new(ALPHABET).expect("alphabet non-empty");
    (0..len).map(|_| *rng.sample(dist) as char).collect()
}

fn write_tokens_file_atomic(path: &std::path::Path, tf: &TokensFile) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    let serialised = toml::to_string(tf).context("serialise tokens file")?;
    tmp.write_all(serialised.as_bytes())?;
    tmp.as_file_mut().sync_all()?;
    let perms = std::fs::Permissions::from_mode(0o644);
    std::fs::set_permissions(tmp.path(), perms)?;
    tmp.persist(path).map_err(|e| anyhow::anyhow!(e.error))?;
    Ok(())
}

fn hash_prefix(h: &str) -> String {
    // argon2id encoded hashes start with "$argon2id$v=19$m=...,t=...,p=...$<salt>$<hash>".
    // Show enough that operators can tell parameters apart, but not the salt/hash.
    h.split('$').take(4).collect::<Vec<_>>().join("$")
}
