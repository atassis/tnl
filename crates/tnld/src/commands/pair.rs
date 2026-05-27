//! `tnld pair add/list` — admin CLI that talks REST to the daemon.

use anyhow::{Context, Result};
use clap::Subcommand;
use serde_json::json;

#[derive(Subcommand, Debug)]
pub enum PairCmd {
    /// Mint a pairing code; print the invite URL.
    Add {
        /// Token name to attach to the redeemed credential.
        name: String,
        /// Daemon admin endpoint (defaults to in-container loopback).
        #[arg(long, default_value = "http://127.0.0.1:7777")]
        endpoint: String,
        /// Bearer for the admin endpoint.
        #[arg(long, env = "TNLD_ADMIN_TOKEN")]
        admin_token: String,
        /// Code TTL in seconds (server clamps to [60, 900]).
        #[arg(long, default_value_t = 300)]
        ttl_sec: u32,
    },
    /// List active pairing codes (debug aid).
    List {
        #[arg(long, default_value = "http://127.0.0.1:7777")]
        endpoint: String,
        #[arg(long, env = "TNLD_ADMIN_TOKEN")]
        admin_token: String,
        /// Emit JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(cmd: PairCmd) -> Result<()> {
    let client = reqwest::Client::new();
    match cmd {
        PairCmd::Add {
            name,
            endpoint,
            admin_token,
            ttl_sec,
        } => {
            let resp = client
                .post(format!("{}/pair", endpoint.trim_end_matches('/')))
                .bearer_auth(&admin_token)
                .json(&json!({"name": name, "expires_in_sec": ttl_sec}))
                .send()
                .await
                .context("POST /pair")?;
            if !resp.status().is_success() {
                anyhow::bail!("POST /pair: {}", resp.status());
            }
            let v: serde_json::Value = resp.json().await?;
            eprintln!("✓ Token {name:?} created.");
            println!("  Invite (expires in {ttl_sec}s):");
            println!();
            println!("      {}", v["invite_url"].as_str().unwrap_or(""));
            println!();
            Ok(())
        }
        PairCmd::List {
            endpoint,
            admin_token,
            json: as_json,
        } => {
            let resp = client
                .get(format!("{}/pair/list", endpoint.trim_end_matches('/')))
                .bearer_auth(&admin_token)
                .send()
                .await?;
            if !resp.status().is_success() {
                anyhow::bail!("GET /pair/list: {}", resp.status());
            }
            let v: serde_json::Value = resp.json().await?;
            if as_json {
                println!("{}", serde_json::to_string(&v)?);
            } else {
                println!("{:<12} {:<20} EXPIRES", "CODE", "NAME");
                if let Some(arr) = v.as_array() {
                    for e in arr {
                        println!(
                            "{:<12} {:<20} {}",
                            e["code"].as_str().unwrap_or(""),
                            e["name"].as_str().unwrap_or(""),
                            e["expires_at_unix"]
                        );
                    }
                }
            }
            Ok(())
        }
    }
}
