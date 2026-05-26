use std::path::PathBuf;

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

    pub fn load_from(path: &std::path::Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "config not found at {}; run `tnl auth login` first",
                path.display()
            )
        })?;
        let cfg: Self = toml::from_str(&text).context("parse config")?;
        Ok(cfg)
    }

    pub fn save_to(&self, path: &std::path::Path) -> anyhow::Result<()> {
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
}
