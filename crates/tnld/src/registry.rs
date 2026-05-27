use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use thiserror::Error;
use ulid::Ulid;

pub type TunnelId = String;
pub type SessionId = String;

/// Per-tunnel request/byte counters. Use atomics for lock-free data-plane updates.
#[derive(Debug, Default)]
pub struct TunnelStats {
    pub requests: AtomicU64,
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
}

/// Immutable tunnel descriptor — everything set at creation time.
#[derive(Debug)]
pub struct Tunnel {
    pub id: TunnelId,
    pub subdomain: String,
    pub hostname: String,
    /// Token name of the session that originally created this tunnel.
    pub created_by: String,
    /// UNIX timestamp (seconds since epoch) when this tunnel was created.
    pub created_at_unix: u64,
    pub stats: TunnelStats,
}

/// Connection state of a tunnel (stored separately from the immutable Tunnel).
#[derive(Debug)]
pub enum TunnelState {
    Active,
    Disconnected { since: Instant },
}

/// Mutable per-tunnel binding: which session currently owns it, and its state.
#[derive(Debug)]
pub struct TunnelBinding {
    pub session_id: SessionId,
    pub state: TunnelState,
}

/// Session record (existing — do NOT rename; referenced by control.rs).
#[derive(Debug)]
pub struct SessionState {
    pub id: SessionId,
    pub token_name: String,
}

#[derive(Debug, Default)]
pub struct Registry {
    by_subdomain: DashMap<String, Arc<Tunnel>>,
    by_id: DashMap<TunnelId, Arc<Tunnel>>,
    /// Mutable per-tunnel binding (`session_id` + lifecycle state).
    tunnel_state: DashMap<TunnelId, TunnelBinding>,
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

    /// Mark all tunnels owned by `session_id` as `Disconnected`.
    ///
    /// When `grace_sec == 0`, tunnels are removed immediately (backward-compatible).
    /// Otherwise GC will clean them up after the grace window expires.
    pub fn drop_session(&self, session_id: &str, grace_sec: u32) {
        self.sessions.remove(session_id);

        if grace_sec == 0 {
            // Immediate removal: collect tunnel ids first, then remove.
            let mut to_remove = Vec::new();
            for kv in &self.tunnel_state {
                if kv.value().session_id == session_id {
                    to_remove.push(kv.key().clone());
                }
            }
            for id in to_remove {
                self.tunnel_state.remove(&id);
                if let Some((_, t)) = self.by_id.remove(&id) {
                    self.by_subdomain.remove(&t.subdomain);
                }
            }
        } else {
            // Grace window: mark Disconnected, let GC clean up later.
            let now = Instant::now();
            let mut to_mark = Vec::new();
            for kv in &self.tunnel_state {
                if kv.value().session_id == session_id {
                    to_mark.push(kv.key().clone());
                }
            }
            for id in to_mark {
                if let Some(mut binding) = self.tunnel_state.get_mut(&id) {
                    if matches!(binding.state, TunnelState::Active) {
                        binding.state = TunnelState::Disconnected { since: now };
                    }
                }
            }
        }
    }

    /// Remove all tunnels whose grace window has expired.
    pub fn gc_disconnected(&self, grace: std::time::Duration) {
        let now = Instant::now();
        let mut to_remove = Vec::new();
        for kv in &self.tunnel_state {
            if let TunnelState::Disconnected { since } = &kv.value().state {
                if now.duration_since(*since) > grace {
                    to_remove.push(kv.key().clone());
                }
            }
        }
        for id in to_remove {
            self.tunnel_state.remove(&id);
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
        let created_at_unix = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let tunnel = Arc::new(Tunnel {
            id: Ulid::new().to_string(),
            subdomain: subdomain.into(),
            hostname,
            created_by: token_name.into(),
            created_at_unix,
            stats: TunnelStats::default(),
        });
        self.by_id.insert(tunnel.id.clone(), tunnel.clone());
        self.by_subdomain
            .insert(tunnel.subdomain.clone(), tunnel.clone());
        self.tunnel_state.insert(
            tunnel.id.clone(),
            TunnelBinding {
                session_id: session_id.into(),
                state: TunnelState::Active,
            },
        );
        Ok(tunnel)
    }

    /// Returns the current (possibly re-attached) session id for a tunnel.
    /// Returns `None` if the tunnel doesn't exist or has no binding.
    pub fn current_session_id(&self, tunnel_id: &str) -> Option<SessionId> {
        self.tunnel_state
            .get(tunnel_id)
            .map(|b| b.session_id.clone())
    }

    /// Attempt to reattach an existing (Disconnected) tunnel to a new session.
    ///
    /// Checks:
    /// - tunnel exists with matching `tunnel_id` and `subdomain`
    /// - tunnel was created by `owner_token`
    /// - tunnel is currently Disconnected (not Active)
    /// - `new_session_id` is a registered session
    ///
    /// On success, swaps the `session_id` and marks the tunnel Active.
    pub fn try_reattach(
        &self,
        tunnel_id: &str,
        subdomain: &str,
        owner_token: &str,
        new_session_id: &str,
    ) -> Result<Arc<Tunnel>, &'static str> {
        // Clone Arc immediately so we don't hold the DashMap guard.
        let tunnel = self.by_id.get(tunnel_id).ok_or("not_found")?.clone();

        if tunnel.subdomain != subdomain {
            return Err("subdomain_mismatch");
        }
        if tunnel.created_by != owner_token {
            return Err("not_owner");
        }
        if !self.sessions.contains_key(new_session_id) {
            return Err("session_missing");
        }

        // Now mutate the binding.
        let mut binding = self.tunnel_state.get_mut(tunnel_id).ok_or("not_found")?;

        if matches!(binding.state, TunnelState::Active) {
            return Err("already_active");
        }

        binding.session_id = new_session_id.into();
        binding.state = TunnelState::Active;
        drop(binding);

        Ok(tunnel)
    }

    pub fn find_by_hostname(&self, host: &str) -> Option<Arc<Tunnel>> {
        let subdomain = host.strip_suffix(&format!(".{}", self.hostname_root))?;
        self.by_subdomain.get(subdomain).map(|t| t.clone())
    }

    pub fn find_by_subdomain(&self, subdomain: &str) -> Option<Arc<Tunnel>> {
        self.by_subdomain.get(subdomain).map(|t| t.clone())
    }

    /// Snapshot all tunnels as `TunnelInfo` values for the REST list endpoint.
    pub fn snapshot_infos(&self) -> Vec<tnl_protocol::messages::TunnelInfo> {
        self.by_id
            .iter()
            .map(|kv| {
                let t = kv.value();
                let active = self
                    .tunnel_state
                    .get(&t.id)
                    .is_some_and(|b| matches!(b.state, TunnelState::Active));
                tnl_protocol::messages::TunnelInfo {
                    subdomain: t.subdomain.clone(),
                    hostname: t.hostname.clone(),
                    owner_token: t.created_by.clone(),
                    created_at_unix: t.created_at_unix,
                    requests: t.stats.requests.load(Ordering::Relaxed),
                    bytes_in: t.stats.bytes_in.load(Ordering::Relaxed),
                    bytes_out: t.stats.bytes_out.load(Ordering::Relaxed),
                    active,
                }
            })
            .collect()
    }

    /// Remove a tunnel from the registry by subdomain.
    ///
    /// Returns `Err("not_found")` if no such tunnel exists, `Err("not_owner")`
    /// if the caller's token doesn't match. Sessions linger; clients discover
    /// the closure naturally on their next request.
    pub fn close_by_subdomain(&self, subdomain: &str, owner: &str) -> Result<(), &'static str> {
        // Clone Arc so we don't hold the DashMap guard while mutating.
        let tunnel = self.by_subdomain.get(subdomain).ok_or("not_found")?.clone();
        if tunnel.created_by != owner {
            return Err("not_owner");
        }
        let id = tunnel.id.clone();
        drop(tunnel);
        self.by_subdomain.remove(subdomain);
        self.by_id.remove(&id);
        self.tunnel_state.remove(&id);
        Ok(())
    }
}

impl crate::random_subdomain::Reserved for Registry {
    fn contains(&self, s: &str) -> bool {
        self.find_by_subdomain(s).is_some()
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
        // drop session with grace=0 removes tunnel immediately
        reg.drop_session(&sess.id, 0);
        assert!(reg.find_by_hostname("foo.t.example.com").is_none());
    }

    #[test]
    fn invalid_subdomain_rejected_at_create() {
        let reg = Registry::new("t.example.com");
        let sess = reg.register_session("laptop");
        let err = reg.create_tunnel("BAD!", &sess.id, "laptop").unwrap_err();
        matches!(err, RegistryError::InvalidSubdomain(_));
    }

    #[test]
    fn drop_session_with_grace_marks_disconnected() {
        let reg = Registry::new("t.example.com");
        let sess = reg.register_session("laptop");
        let tunnel = reg.create_tunnel("foo", &sess.id, "laptop").unwrap();
        // drop with 30s grace — tunnel stays in by_id/by_subdomain but Disconnected
        reg.drop_session(&sess.id, 30);
        assert!(reg.find_by_hostname("foo.t.example.com").is_some());
        // current_session_id still returns something
        assert!(reg.current_session_id(&tunnel.id).is_some());
    }

    #[test]
    fn gc_removes_expired_tunnels() {
        let reg = Registry::new("t.example.com");
        let sess = reg.register_session("laptop");
        let tunnel = reg.create_tunnel("foo", &sess.id, "laptop").unwrap();
        reg.drop_session(&sess.id, 30);
        // GC with 0s grace should remove immediately (since disconnected since "now")
        reg.gc_disconnected(std::time::Duration::ZERO);
        assert!(reg.find_by_hostname("foo.t.example.com").is_none());
        assert!(reg.current_session_id(&tunnel.id).is_none());
    }

    #[test]
    fn try_reattach_success() {
        let reg = Registry::new("t.example.com");
        let sess1 = reg.register_session("user");
        let tunnel = reg.create_tunnel("bar", &sess1.id, "user").unwrap();
        // Disconnect the first session
        reg.drop_session(&sess1.id, 30);
        // Register a new session
        let sess2 = reg.register_session("user");
        // Reattach
        let reattached = reg
            .try_reattach(&tunnel.id, "bar", "user", &sess2.id)
            .unwrap();
        assert_eq!(reattached.id, tunnel.id);
        assert_eq!(reg.current_session_id(&tunnel.id).unwrap(), sess2.id);
    }

    #[test]
    fn try_reattach_already_active_fails() {
        let reg = Registry::new("t.example.com");
        let sess1 = reg.register_session("user");
        let tunnel = reg.create_tunnel("baz", &sess1.id, "user").unwrap();
        let sess2 = reg.register_session("user");
        // Tunnel is still Active — reattach should fail
        let err = reg
            .try_reattach(&tunnel.id, "baz", "user", &sess2.id)
            .unwrap_err();
        assert_eq!(err, "already_active");
    }
}
