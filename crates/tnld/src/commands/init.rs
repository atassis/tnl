//! `tnld init` — server-side first-run wizard.
//!
//! Writes config.toml to the requested path (default /etc/tnld/config.toml)
//! and, if asked, mints an initial admin token into tokens.toml.

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input};

use crate::auth::{hash_plaintext, TokenEntry, TokensFile};
use crate::commands::token::{random_token_suffix, write_tokens_file_atomic};

#[derive(Debug, Default)]
pub struct InitArgs {
    pub config: PathBuf,
    pub listen: Option<String>,
    pub public_url: Option<String>,
    pub hostname_root: Option<String>,
    pub tokens_file: Option<PathBuf>,
    pub admin_token_name: Option<String>,
    /// Supply a known plaintext token instead of generating one (for CI).
    pub admin_token: Option<String>,
    pub session_grace_sec: Option<u32>,
    pub yes: bool,
    pub json: bool,
}

/// Resolve the fields that require interactive prompts when not supplied as flags.
struct Resolved {
    listen: String,
    public_url: String,
    hostname_root: String,
    tokens_file: PathBuf,
}

fn resolve_fields(args: &mut InitArgs, interactive: bool) -> Result<Resolved> {
    let theme = ColorfulTheme::default();

    let listen = match args.listen.take() {
        Some(v) => v,
        None if interactive => Input::with_theme(&theme)
            .with_prompt("listen address")
            .default("127.0.0.1:7777".into())
            .interact_text()
            .context("read listen")?,
        None => "127.0.0.1:7777".into(),
    };

    let public_url = match args.public_url.take() {
        Some(v) => v,
        None if interactive => Input::with_theme(&theme)
            .with_prompt("public URL (e.g. https://tnl-api.example.com)")
            .interact_text()
            .context("read public_url")?,
        None => anyhow::bail!("--public-url is required in non-interactive mode"),
    };

    let hostname_root = match args.hostname_root.take() {
        Some(v) => v,
        None if interactive => {
            let default = derive_hostname_root(&public_url);
            Input::with_theme(&theme)
                .with_prompt("hostname root (wildcard suffix, e.g. t.example.com)")
                .default(default)
                .interact_text()
                .context("read hostname_root")?
        }
        None => anyhow::bail!("--hostname-root is required in non-interactive mode"),
    };

    let tokens_file = match args.tokens_file.take() {
        Some(p) => p,
        None if interactive => {
            let s: String = Input::with_theme(&theme)
                .with_prompt("tokens file path")
                .default("/etc/tnld/tokens.toml".into())
                .interact_text()
                .context("read tokens_file")?;
            PathBuf::from(s)
        }
        None => PathBuf::from("/etc/tnld/tokens.toml"),
    };

    Ok(Resolved {
        listen,
        public_url,
        hostname_root,
        tokens_file,
    })
}

/// Prompt for (or skip) minting a first token; returns the token name if wanted.
fn resolve_token_name(args: &InitArgs, interactive: bool) -> Result<Option<String>> {
    if let Some(name) = args.admin_token_name.as_deref() {
        return Ok(Some(name.to_string()));
    }
    // A supplied token with no explicit name still gets minted, named "admin".
    if args.admin_token.is_some() {
        return Ok(Some("admin".to_string()));
    }
    if !interactive {
        return Ok(None);
    }
    let theme = ColorfulTheme::default();
    let want = Confirm::with_theme(&theme)
        .with_prompt("create an initial admin token now?")
        .default(true)
        .interact()
        .context("read confirm")?;
    if !want {
        return Ok(None);
    }
    let name: String = Input::with_theme(&theme)
        .with_prompt("token name")
        .default("admin".into())
        .interact_text()
        .context("read token name")?;
    Ok(Some(name))
}

/// Mint or store a token, append it to the tokens file, print the plaintext to
/// stdout, and return it. If `supplied` is given it is used verbatim (CI path);
/// otherwise a fresh random token is generated.
fn mint_token(name: &str, supplied: Option<&str>, tokens_file: &PathBuf) -> Result<String> {
    let plaintext = supplied.map_or_else(
        || format!("tnl_{}", random_token_suffix(26)),
        ToString::to_string,
    );
    let hash = hash_plaintext(&plaintext).context("argon2 hash")?;
    let entry = TokenEntry {
        name: name.to_string(),
        hash,
    };

    let mut tf: TokensFile = match std::fs::read_to_string(tokens_file) {
        Ok(raw) if !raw.trim().is_empty() => toml::from_str(&raw).context("parse tokens file")?,
        _ => TokensFile { tokens: vec![] },
    };
    tf.tokens.push(entry);

    if let Some(parent) = tokens_file.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create tokens dir {}", parent.display()))?;
        }
    }
    write_tokens_file_atomic(tokens_file, &tf)?;

    eprintln!("✓ token {name:?} written to {}", tokens_file.display());
    println!("{plaintext}");
    Ok(plaintext)
}

pub fn run(mut args: InitArgs) -> Result<()> {
    // Already-exists guard.
    if args.config.exists() && !args.yes {
        anyhow::bail!(
            "{} already exists; pass -y to overwrite",
            args.config.display()
        );
    }

    let interactive = std::io::stdin().is_terminal();
    let r = resolve_fields(&mut args, interactive)?;
    let session_grace_sec = args.session_grace_sec.unwrap_or(30);

    // Render and write config.toml.
    let toml_text = format!(
        r#"listen           = "{}"
public_url       = "{}"
hostname_root    = "{}"
tokens_file      = "{}"
session_grace_sec = {session_grace_sec}
"#,
        r.listen,
        r.public_url,
        r.hostname_root,
        r.tokens_file.display()
    );

    if let Some(parent) = args.config.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create config dir {}", parent.display()))?;
        }
    }
    std::fs::write(&args.config, toml_text)
        .with_context(|| format!("write {}", args.config.display()))?;
    eprintln!("✓ wrote config to {}", args.config.display());

    // Optionally mint a first token.
    let minted = if let Some(name) = resolve_token_name(&args, interactive)? {
        Some(mint_token(
            &name,
            args.admin_token.as_deref(),
            &r.tokens_file,
        )?)
    } else {
        None
    };

    if args.json {
        let summary = serde_json::json!({
            "config":             args.config.display().to_string(),
            "tokens_file":        r.tokens_file.display().to_string(),
            "listen":             r.listen,
            "public_url":         r.public_url,
            "hostname_root":      r.hostname_root,
            "session_grace_sec":  session_grace_sec,
        });
        println!("{summary}");
    } else {
        eprintln!();
        eprintln!("next steps:");
        eprintln!("  1. configure your reverse proxy (Caddy/nginx/Traefik) to route");
        eprintln!("     *.{}  →  {}", r.hostname_root, r.listen);
        eprintln!("  2. point DNS A/AAAA *.{} at this host", r.hostname_root);
        eprintln!("  3. obtain a wildcard TLS cert for *.{}", r.hostname_root);
        eprintln!(
            "  4. start tnld: tnld serve --config {}",
            args.config.display()
        );
        if let Some(token) = &minted {
            eprintln!();
            eprintln!("connect a client:");
            eprintln!(
                "  tnl auth login --endpoint {} --token {token}",
                r.public_url
            );
            eprintln!();
            eprintln!("example Caddy site block (TLS via your DNS provider):");
            eprintln!("  *.{} {{", r.hostname_root);
            eprintln!("      tls {{ dns <provider> {{env.YOUR_DNS_TOKEN}} }}");
            eprintln!("      reverse_proxy {}", r.listen);
            eprintln!("  }}");
        }
    }
    Ok(())
}

fn derive_hostname_root(public_url: &str) -> String {
    let s = public_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("tnl-api.")
        .to_string();
    s.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_hostname_root_strips_prefix() {
        assert_eq!(
            derive_hostname_root("https://tnl-api.example.com"),
            "example.com"
        );
        assert_eq!(
            derive_hostname_root("https://api.example.com"),
            "api.example.com"
        );
    }
}
