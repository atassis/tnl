use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use argon2::password_hash::{PasswordHash, PasswordVerifier, SaltString};
use argon2::{Argon2, PasswordHasher};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TokenEntry {
    pub name: String,
    pub hash: String,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct TokensFile {
    #[serde(rename = "tokens", default)]
    pub tokens: Vec<TokenEntry>,
}

#[derive(Debug)]
pub struct TokenStore {
    by_hash: HashMap<String, TokenEntry>,
}

impl TokenStore {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read tokens file at {}", path.display()))?;
        let file: TokensFile = toml::from_str(&text).context("parse tokens.toml")?;
        let mut by_hash = HashMap::new();
        for tok in file.tokens {
            by_hash.insert(tok.hash.clone(), tok);
        }
        Ok(Self { by_hash })
    }

    /// Look up which token (if any) matches the given plaintext.
    /// Returns the token name on match, `None` otherwise.
    pub fn verify(&self, plaintext: &str) -> Option<&str> {
        for (hash_str, entry) in &self.by_hash {
            let Ok(parsed) = PasswordHash::new(hash_str) else {
                continue;
            };
            if Argon2::default()
                .verify_password(plaintext.as_bytes(), &parsed)
                .is_ok()
            {
                return Some(&entry.name);
            }
        }
        None
    }

    pub fn is_empty(&self) -> bool {
        self.by_hash.is_empty()
    }
}

/// Hash a plaintext token with argon2id default parameters.
pub fn hash_plaintext(plaintext: &str) -> anyhow::Result<String> {
    use argon2::password_hash::rand_core::OsRng;

    let salt = SaltString::generate(&mut OsRng);
    let argon = Argon2::default();
    let hash = argon
        .hash_password(plaintext.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2 hash: {e}"))?;
    Ok(hash.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn hash_and_verify_roundtrip() {
        let hash = hash_plaintext("tnl_TESTSECRET").unwrap();
        let tokens = TokensFile {
            tokens: vec![TokenEntry {
                name: "smoke".into(),
                hash,
            }],
        };
        let toml_text = toml::to_string(&tokens).unwrap();

        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(toml_text.as_bytes()).unwrap();

        let store = TokenStore::load(tmp.path()).unwrap();
        assert_eq!(store.verify("tnl_TESTSECRET"), Some("smoke"));
        assert_eq!(store.verify("wrong"), None);
    }
}
