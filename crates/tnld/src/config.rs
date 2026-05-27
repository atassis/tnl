use std::path::Path;

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
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let cfg: Self = toml::from_str(&text)?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn loads_minimal_config() {
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
        let cfg = Config::load(tmp.path()).unwrap();
        assert_eq!(cfg.listen, "127.0.0.1:7777");
        assert_eq!(cfg.hostname_root, "t.example.com");
    }
}
