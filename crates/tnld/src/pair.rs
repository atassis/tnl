//! In-memory pairing registry. 5-min TTL, IP rate limit, attempt counter,
//! 16 concurrently-live codes max, one-time-use semantics.

use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use dashmap::DashMap;
use governor::clock::DefaultClock;
use governor::state::keyed::DashMapStateStore;
use governor::{Quota, RateLimiter};

const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const CODE_LEN: usize = 6;
pub const MAX_CODES: usize = 16;
pub const MAX_ATTEMPTS_PER_CODE: u8 = 5;

#[derive(Debug)]
pub struct PairEntry {
    pub code: String,
    pub name: String,
    pub created_at: Instant,
    pub expires_at: Instant,
    pub attempts: AtomicU8,
}

#[derive(Debug, thiserror::Error)]
pub enum RedeemErr {
    #[error("pair_not_found")]
    NotFound,
    #[error("pair_expired")]
    Expired,
    #[error("pair_too_many_attempts")]
    TooManyAttempts,
    #[error("rate_limited; retry after {retry_after_sec}s")]
    RateLimited { retry_after_sec: u32 },
}

#[derive(Debug)]
pub struct PairRegistry {
    entries: DashMap<String, Arc<PairEntry>>,
    by_ip: RateLimiter<IpAddr, DashMapStateStore<IpAddr>, DefaultClock>,
}

impl Default for PairRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PairRegistry {
    pub fn new() -> Self {
        let quota = Quota::per_minute(NonZeroU32::new(3).unwrap());
        Self {
            entries: DashMap::new(),
            by_ip: RateLimiter::dashmap(quota),
        }
    }

    pub fn create(&self, name: String, ttl: Duration) -> Result<(String, SystemTime)> {
        if self.entries.len() >= MAX_CODES {
            anyhow::bail!("pair registry full (max {MAX_CODES} concurrent codes)");
        }
        let raw = random_code();
        let entry = Arc::new(PairEntry {
            code: raw.clone(),
            name,
            created_at: Instant::now(),
            expires_at: Instant::now() + ttl,
            attempts: AtomicU8::new(0),
        });
        self.entries.insert(raw.clone(), entry);
        Ok((format_code(&raw), SystemTime::now() + ttl))
    }

    pub fn redeem(&self, code: &str, from_ip: IpAddr) -> Result<String, RedeemErr> {
        if self.by_ip.check_key(&from_ip).is_err() {
            return Err(RedeemErr::RateLimited {
                retry_after_sec: 30,
            });
        }
        let normalised = normalise(code);
        let entry = self.entries.get(&normalised).ok_or(RedeemErr::NotFound)?;
        if Instant::now() > entry.expires_at {
            drop(entry);
            self.entries.remove(&normalised);
            return Err(RedeemErr::Expired);
        }
        let attempts = entry.attempts.fetch_add(1, Ordering::AcqRel);
        if attempts >= MAX_ATTEMPTS_PER_CODE {
            drop(entry);
            self.entries.remove(&normalised);
            return Err(RedeemErr::TooManyAttempts);
        }
        let name = entry.name.clone();
        drop(entry);
        self.entries.remove(&normalised);
        Ok(name)
    }

    pub fn cleanup(&self) {
        let now = Instant::now();
        self.entries.retain(|_, e| now <= e.expires_at);
    }

    pub fn list(&self) -> Vec<(String, String, SystemTime)> {
        let now_inst = Instant::now();
        let now_sys = SystemTime::now();
        self.entries
            .iter()
            .map(|kv| {
                let e = kv.value();
                (
                    format_code(&e.code),
                    e.name.clone(),
                    now_sys + e.expires_at.saturating_duration_since(now_inst),
                )
            })
            .collect()
    }
}

fn random_code() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..CODE_LEN)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

fn format_code(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    for (i, c) in raw.chars().enumerate() {
        if i > 0 && i % 2 == 0 {
            out.push('-');
        }
        out.push(c);
    }
    out
}

pub fn normalise(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_then_redeem_returns_name() {
        let reg = PairRegistry::new();
        let (code, _) = reg
            .create("laptop".into(), Duration::from_secs(60))
            .unwrap();
        let name = reg.redeem(&code, "1.2.3.4".parse().unwrap()).unwrap();
        assert_eq!(name, "laptop");
    }

    #[test]
    fn second_redeem_is_not_found() {
        let reg = PairRegistry::new();
        let (code, _) = reg.create("x".into(), Duration::from_secs(60)).unwrap();
        let ip = "1.2.3.4".parse().unwrap();
        reg.redeem(&code, ip).unwrap();
        assert!(matches!(reg.redeem(&code, ip), Err(RedeemErr::NotFound)));
    }

    #[test]
    fn expired_returns_expired() {
        let reg = PairRegistry::new();
        let (code, _) = reg.create("x".into(), Duration::from_millis(1)).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        assert!(matches!(
            reg.redeem(&code, "1.1.1.1".parse().unwrap()),
            Err(RedeemErr::Expired)
        ));
    }

    #[test]
    fn registry_full_errors() {
        let reg = PairRegistry::new();
        for i in 0..MAX_CODES {
            reg.create(format!("n{i}"), Duration::from_secs(60))
                .unwrap();
        }
        assert!(reg
            .create("overflow".into(), Duration::from_secs(60))
            .is_err());
    }

    #[test]
    fn normalise_strips_dashes() {
        assert_eq!(normalise("ab-12-cd"), "AB12CD");
        assert_eq!(normalise(" ab 12 cd "), "AB12CD");
    }

    #[test]
    fn cleanup_removes_expired() {
        let reg = PairRegistry::new();
        reg.create("x".into(), Duration::from_millis(1)).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        reg.cleanup();
        assert_eq!(reg.list().len(), 0);
    }
}
