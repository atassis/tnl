use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::SystemTime;

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
    path: PathBuf,
    inner: RwLock<TokenStoreInner>,
}

#[derive(Debug)]
struct TokenStoreInner {
    by_hash: HashMap<String, TokenEntry>,
    mtime: Option<SystemTime>,
}

impl TokenStore {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let inner = read_inner(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            inner: RwLock::new(inner),
        })
    }

    /// Look up which token (if any) matches the given plaintext.
    /// Re-reads tokens.toml first if its mtime has advanced.
    pub fn verify(&self, plaintext: &str) -> Option<String> {
        // Best-effort reload; if stat fails we keep the cached state.
        let _ = self.reload_if_changed();
        // Collect entries while holding the lock, then drop the lock before
        // doing the expensive argon2 verification.
        let entries: Vec<(String, String)> = self
            .inner
            .read()
            .ok()?
            .by_hash
            .iter()
            .map(|(h, e)| (h.clone(), e.name.clone()))
            .collect();
        for (hash_str, name) in &entries {
            let Ok(parsed) = PasswordHash::new(hash_str) else {
                continue;
            };
            if Argon2::default()
                .verify_password(plaintext.as_bytes(), &parsed)
                .is_ok()
            {
                return Some(name.clone());
            }
        }
        None
    }

    pub fn is_empty(&self) -> bool {
        self.inner
            .read()
            .map(|g| g.by_hash.is_empty())
            .unwrap_or(true)
    }

    pub fn append(&self, entry: TokenEntry) -> anyhow::Result<()> {
        // Read current file (or treat empty as empty list).
        let raw = std::fs::read_to_string(&self.path).unwrap_or_default();
        let mut tf: TokensFile = if raw.trim().is_empty() {
            TokensFile::default()
        } else {
            toml::from_str(&raw).context("parse tokens.toml")?
        };
        tf.tokens.push(entry);

        // Atomic rewrite using the helper from commands::token.
        crate::commands::token::write_tokens_file_atomic(&self.path, &tf)?;

        // Refresh in-memory state.
        let mut by_hash = HashMap::new();
        for tok in tf.tokens {
            by_hash.insert(tok.hash.clone(), tok);
        }
        let mtime = std::fs::metadata(&self.path)
            .ok()
            .and_then(|m| m.modified().ok());
        if let Ok(mut guard) = self.inner.write() {
            *guard = TokenStoreInner { by_hash, mtime };
        }
        Ok(())
    }

    fn reload_if_changed(&self) -> anyhow::Result<()> {
        let meta = std::fs::metadata(&self.path)
            .with_context(|| format!("stat {}", self.path.display()))?;
        let new_mtime = meta.modified().ok();
        let cached_mtime = self.inner.read().ok().and_then(|g| g.mtime);
        if new_mtime > cached_mtime {
            let fresh = read_inner(&self.path)?;
            if let Ok(mut guard) = self.inner.write() {
                *guard = fresh;
            }
        }
        Ok(())
    }
}

fn read_inner(path: &Path) -> anyhow::Result<TokenStoreInner> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read tokens file at {}", path.display()))?;
    let file: TokensFile = if text.trim().is_empty() {
        TokensFile::default()
    } else {
        toml::from_str(&text).context("parse tokens.toml")?
    };
    let mut by_hash = HashMap::new();
    for tok in file.tokens {
        by_hash.insert(tok.hash.clone(), tok);
    }
    let mtime = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());
    Ok(TokenStoreInner { by_hash, mtime })
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
        assert_eq!(store.verify("tnl_TESTSECRET"), Some("smoke".to_string()));
        assert_eq!(store.verify("wrong"), None);
    }
}
