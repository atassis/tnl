use std::sync::Arc;

use dashmap::DashMap;
use thiserror::Error;
use ulid::Ulid;

pub type TunnelId = String;
pub type SessionId = String;

#[derive(Debug)]
pub struct Tunnel {
    pub id: TunnelId,
    pub subdomain: String,
    pub hostname: String,
    pub session_id: SessionId,
    pub created_by: String, // token name
}

#[derive(Debug)]
pub struct SessionState {
    pub id: SessionId,
    pub token_name: String,
}

#[derive(Debug, Default)]
pub struct Registry {
    by_subdomain: DashMap<String, Arc<Tunnel>>,
    by_id: DashMap<TunnelId, Arc<Tunnel>>,
    sessions: DashMap<SessionId, Arc<SessionState>>,
    pub hostname_root: String,
}

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("subdomain '{0}' is already taken")]
    SubdomainTaken(String),
    #[error("invalid subdomain '{0}'")]
    InvalidSubdomain(String),
    #[error("session not found")]
    SessionMissing,
}

impl Registry {
    pub fn new(hostname_root: impl Into<String>) -> Self {
        Self {
            hostname_root: hostname_root.into(),
            ..Default::default()
        }
    }

    pub fn register_session(&self, token_name: impl Into<String>) -> Arc<SessionState> {
        let id = Ulid::new().to_string();
        let s = Arc::new(SessionState {
            id: id.clone(),
            token_name: token_name.into(),
        });
        self.sessions.insert(id, s.clone());
        s
    }

    pub fn drop_session(&self, session_id: &str) {
        self.sessions.remove(session_id);
        let mut to_remove = Vec::new();
        for kv in &self.by_id {
            if kv.value().session_id == session_id {
                to_remove.push(kv.key().clone());
            }
        }
        for id in to_remove {
            if let Some((_, t)) = self.by_id.remove(&id) {
                self.by_subdomain.remove(&t.subdomain);
            }
        }
    }

    pub fn create_tunnel(
        &self,
        subdomain: &str,
        session_id: &str,
        token_name: &str,
    ) -> Result<Arc<Tunnel>, RegistryError> {
        if !valid_subdomain(subdomain) {
            return Err(RegistryError::InvalidSubdomain(subdomain.into()));
        }
        if !self.sessions.contains_key(session_id) {
            return Err(RegistryError::SessionMissing);
        }
        if self.by_subdomain.contains_key(subdomain) {
            return Err(RegistryError::SubdomainTaken(subdomain.into()));
        }
        let hostname = format!("{subdomain}.{}", self.hostname_root);
        let tunnel = Arc::new(Tunnel {
            id: Ulid::new().to_string(),
            subdomain: subdomain.into(),
            hostname,
            session_id: session_id.into(),
            created_by: token_name.into(),
        });
        self.by_id.insert(tunnel.id.clone(), tunnel.clone());
        self.by_subdomain
            .insert(tunnel.subdomain.clone(), tunnel.clone());
        Ok(tunnel)
    }

    pub fn find_by_hostname(&self, host: &str) -> Option<Arc<Tunnel>> {
        let subdomain = host.strip_suffix(&format!(".{}", self.hostname_root))?;
        self.by_subdomain.get(subdomain).map(|t| t.clone())
    }
}

pub fn valid_subdomain(s: &str) -> bool {
    let len = s.len();
    if !(1..=31).contains(&len) {
        return false;
    }
    let bytes = s.as_bytes();
    if bytes[0] == b'-' || bytes[len - 1] == b'-' {
        return false;
    }
    s.bytes()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subdomain_validation() {
        assert!(valid_subdomain("foo"));
        assert!(valid_subdomain("foo-bar"));
        assert!(valid_subdomain("a1b2c3"));
        assert!(!valid_subdomain(""));
        assert!(!valid_subdomain("-foo"));
        assert!(!valid_subdomain("foo-"));
        assert!(!valid_subdomain("Foo")); // uppercase
        assert!(!valid_subdomain("foo.bar")); // dot
        assert!(!valid_subdomain(&"x".repeat(32)));
    }

    #[test]
    fn create_lookup_drop_lifecycle() {
        let reg = Registry::new("t.example.com");
        let sess = reg.register_session("laptop");
        let tunnel = reg.create_tunnel("foo", &sess.id, "laptop").unwrap();
        assert_eq!(tunnel.hostname, "foo.t.example.com");
        let by_host = reg.find_by_hostname("foo.t.example.com").unwrap();
        assert_eq!(by_host.id, tunnel.id);
        // duplicate subdomain rejected
        let err = reg.create_tunnel("foo", &sess.id, "laptop").unwrap_err();
        matches!(err, RegistryError::SubdomainTaken(_));
        // drop session removes tunnel
        reg.drop_session(&sess.id);
        assert!(reg.find_by_hostname("foo.t.example.com").is_none());
    }

    #[test]
    fn invalid_subdomain_rejected_at_create() {
        let reg = Registry::new("t.example.com");
        let sess = reg.register_session("laptop");
        let err = reg.create_tunnel("BAD!", &sess.id, "laptop").unwrap_err();
        matches!(err, RegistryError::InvalidSubdomain(_));
    }
}
