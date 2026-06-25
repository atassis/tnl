use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

#[derive(Deserialize, Debug, Clone)]
pub struct Config {
    pub listen: String,
    pub public_url: String,
    pub hostname_root: String,
    pub tokens_file: String,
    /// Seconds a tunnel remains reserved after the control WS closes, before GC
    /// removes it and the subdomain becomes available again. Default: 30.
    /// Set to 0 to disable the grace window and remove immediately on disconnect.
    #[serde(default = "default_session_grace_sec")]
    pub session_grace_sec: u32,
}

const fn default_session_grace_sec() -> u32 {
    30
}

impl Config {
    /// Load config from `path`, then apply `TNLD_*` environment overrides
    /// (env takes precedence over the file). Returns the resolved config plus a
    /// list of warnings for keys set in BOTH the file and the environment.
    /// `env` is injected so the resolution logic is unit-testable.
    pub fn load_with_env(
        path: &Path,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> anyhow::Result<(Self, Vec<String>)> {
        let text = std::fs::read_to_string(path)?;
        let mut cfg: Self = toml::from_str(&text)?;
        let table: toml::Table = toml::from_str(&text)?;
        let where_ = path.display().to_string();
        let mut warnings = Vec::new();

        if let Some(v) = env("TNLD_LISTEN") {
            note_overlap(&mut warnings, &table, "listen", "TNLD_LISTEN", &where_);
            cfg.listen = v;
        }
        if let Some(v) = env("TNLD_PUBLIC_URL") {
            note_overlap(&mut warnings, &table, "public_url", "TNLD_PUBLIC_URL", &where_);
            cfg.public_url = v;
        }
        if let Some(v) = env("TNLD_HOSTNAME_ROOT") {
            note_overlap(
                &mut warnings,
                &table,
                "hostname_root",
                "TNLD_HOSTNAME_ROOT",
                &where_,
            );
            cfg.hostname_root = v;
        }
        if let Some(v) = env("TNLD_TOKENS_FILE") {
            note_overlap(
                &mut warnings,
                &table,
                "tokens_file",
                "TNLD_TOKENS_FILE",
                &where_,
            );
            cfg.tokens_file = v;
        }
        if let Some(v) = env("TNLD_SESSION_GRACE_SEC") {
            note_overlap(
                &mut warnings,
                &table,
                "session_grace_sec",
                "TNLD_SESSION_GRACE_SEC",
                &where_,
            );
            cfg.session_grace_sec = v
                .parse()
                .context("TNLD_SESSION_GRACE_SEC must be a non-negative integer")?;
        }
        Ok((cfg, warnings))
    }

    /// Load config, applying `TNLD_*` env overrides and printing any
    /// file/env overlap warnings to stderr.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let (cfg, warnings) = Self::load_with_env(path, &|k| std::env::var(k).ok())?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        Ok(cfg)
    }
}

/// Record a warning if `key` is present in both the config file and the env.
fn note_overlap(
    warnings: &mut Vec<String>,
    table: &toml::Table,
    key: &str,
    envkey: &str,
    where_: &str,
) {
    if table.contains_key(key) {
        warnings.push(format!(
            "'{key}' set in both {where_} and ${envkey}; using env (env overrides file)"
        ));
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn write_cfg() -> tempfile::NamedTempFile {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            tmp,
            r#"
listen        = "127.0.0.1:7777"
public_url    = "https://tnl-api.t.example.com"
hostname_root = "t.example.com"
tokens_file   = "/etc/tnld/tokens.toml"
"#
        )
        .unwrap();
        tmp
    }

    #[test]
    fn loads_minimal_config() {
        let tmp = write_cfg();
        let cfg = Config::load(tmp.path()).unwrap();
        assert_eq!(cfg.listen, "127.0.0.1:7777");
        assert_eq!(cfg.hostname_root, "t.example.com");
        assert_eq!(cfg.session_grace_sec, 30);
    }

    #[test]
    fn env_overrides_file_value_and_warns() {
        let tmp = write_cfg();
        let env = |k: &str| (k == "TNLD_HOSTNAME_ROOT").then(|| "override.example.com".to_string());
        let (cfg, warnings) = Config::load_with_env(tmp.path(), &env).unwrap();
        assert_eq!(cfg.hostname_root, "override.example.com");
        assert!(warnings
            .iter()
            .any(|w| w.contains("hostname_root") && w.contains("TNLD_HOSTNAME_ROOT")));
    }

    #[test]
    fn env_only_optional_key_does_not_warn() {
        // session_grace_sec is absent from the file, so setting it via env is
        // not an overlap and must not warn.
        let tmp = write_cfg();
        let env = |k: &str| (k == "TNLD_SESSION_GRACE_SEC").then(|| "5".to_string());
        let (cfg, warnings) = Config::load_with_env(tmp.path(), &env).unwrap();
        assert_eq!(cfg.session_grace_sec, 5);
        assert!(warnings.is_empty());
    }

    #[test]
    fn no_env_leaves_file_values_and_no_warnings() {
        let tmp = write_cfg();
        let (cfg, warnings) = Config::load_with_env(tmp.path(), &|_| None).unwrap();
        assert_eq!(cfg.hostname_root, "t.example.com");
        assert!(warnings.is_empty());
    }
}
