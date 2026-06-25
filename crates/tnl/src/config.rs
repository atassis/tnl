use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub endpoint: String,
    pub token: String,
}

impl Config {
    pub fn default_path() -> anyhow::Result<PathBuf> {
        let home = std::env::var("HOME").context("HOME env not set")?;
        Ok(PathBuf::from(home).join(".config/tnl/config.toml"))
    }

    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "config not found at {}; run `tnl auth login` first",
                path.display()
            )
        })?;
        let cfg: Self = toml::from_str(&text).context("parse config")?;
        Ok(cfg)
    }

    /// Resolve config from an optional file plus `TNL_ENDPOINT` / `TNL_TOKEN`
    /// env vars (env takes precedence over the file). The file is optional, so
    /// the client works in CI/containers with env only. Returns warnings for
    /// any key set in BOTH the file and the env. `env` is injected for testing.
    pub fn resolve(
        file: Option<&Path>,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> anyhow::Result<(Self, Vec<String>)> {
        let mut endpoint: Option<String> = None;
        let mut token: Option<String> = None;
        let mut file_has_endpoint = false;
        let mut file_has_token = false;
        let mut where_ = String::new();

        if let Some(p) = file {
            if p.exists() {
                let text =
                    std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?;
                let table: toml::Table = toml::from_str(&text).context("parse config")?;
                file_has_endpoint = table.contains_key("endpoint");
                file_has_token = table.contains_key("token");
                endpoint = table
                    .get("endpoint")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                token = table
                    .get("token")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                where_ = p.display().to_string();
            }
        }

        let mut warnings = Vec::new();
        if let Some(v) = env("TNL_ENDPOINT") {
            if file_has_endpoint {
                warnings.push(format!(
                    "'endpoint' set in both {where_} and $TNL_ENDPOINT; using env (env overrides file)"
                ));
            }
            endpoint = Some(v);
        }
        if let Some(v) = env("TNL_TOKEN") {
            if file_has_token {
                warnings.push(format!(
                    "'token' set in both {where_} and $TNL_TOKEN; using env (env overrides file)"
                ));
            }
            token = Some(v);
        }

        let endpoint = endpoint
            .context("no endpoint configured; set $TNL_ENDPOINT or run `tnl auth login`")?;
        let token = token.context("no token configured; set $TNL_TOKEN or run `tnl auth login`")?;
        Ok((Self { endpoint, token }, warnings))
    }

    /// Resolve config from `path` + real env, printing overlap warnings to stderr.
    pub fn load_resolved(path: &Path) -> anyhow::Result<Self> {
        let (cfg, warnings) = Self::resolve(Some(path), &|k| std::env::var(k).ok())?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        Ok(cfg)
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string(self)?;
        std::fs::write(path, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    pub fn masked_token(&self) -> String {
        let t = &self.token;
        if t.len() <= 12 {
            return "*".repeat(t.len());
        }
        format!("{}...{}", &t[..4], &t[t.len() - 4..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_roundtrip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let cfg = Config {
            endpoint: "http://localhost:7777".into(),
            token: "tnl_DEMO".into(),
        };
        cfg.save_to(tmp.path()).unwrap();
        let back = Config::load_from(tmp.path()).unwrap();
        assert_eq!(back.endpoint, cfg.endpoint);
        assert_eq!(back.token, cfg.token);
    }

    #[test]
    fn masked_token_format() {
        let cfg = Config {
            endpoint: "x".into(),
            token: "tnl_K7H3MQ9R2VTBNX5WPYZF8DJCEA".into(),
        };
        assert_eq!(cfg.masked_token(), "tnl_...JCEA");
    }

    #[test]
    fn resolve_from_env_without_file() {
        let missing = std::env::temp_dir().join("tnl-no-such-config-xyz.toml");
        let _ = std::fs::remove_file(&missing);
        let env = |k: &str| match k {
            "TNL_ENDPOINT" => Some("https://api.tnl.example.com".to_string()),
            "TNL_TOKEN" => Some("tnl_abc".to_string()),
            _ => None,
        };
        let (cfg, warnings) = Config::resolve(Some(&missing), &env).unwrap();
        assert_eq!(cfg.endpoint, "https://api.tnl.example.com");
        assert_eq!(cfg.token, "tnl_abc");
        assert!(warnings.is_empty());
    }

    #[test]
    fn resolve_file_plus_env_overlap_warns() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        Config {
            endpoint: "https://file.example.com".into(),
            token: "tnl_file".into(),
        }
        .save_to(tmp.path())
        .unwrap();
        let env = |k: &str| (k == "TNL_ENDPOINT").then(|| "https://env.example.com".to_string());
        let (cfg, warnings) = Config::resolve(Some(tmp.path()), &env).unwrap();
        assert_eq!(cfg.endpoint, "https://env.example.com");
        assert_eq!(cfg.token, "tnl_file");
        assert!(warnings
            .iter()
            .any(|w| w.contains("endpoint") && w.contains("TNL_ENDPOINT")));
    }

    #[test]
    fn resolve_errors_when_nothing_configured() {
        let res = Config::resolve(None, &|_| None);
        assert!(res.is_err());
    }
}
