//! Bounded per-run policy representation only. Admission and the protected
//! control lifecycle must be implemented before these types provide protection.

use std::{fmt, net::IpAddr};

use argon2::password_hash::PasswordHash;
use ipnet::IpNet;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub const MAX_POLICY_BYTES: usize = 16 * 1024;
pub const MAX_ALLOW_IPS: usize = 128;
pub const MAX_LIMIT: u32 = 10_000;
pub const ARGON2_MEMORY_KIB: u32 = 19_456;
pub const ARGON2_TIME: u32 = 2;
pub const ARGON2_PARALLELISM: u32 = 1;
pub const ARGON2_OUTPUT_BYTES: usize = 32;
pub const ARGON2_SALT_BYTES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("access policy exceeds size bound")]
    TooLarge,
    #[error("invalid access policy encoding")]
    Encoding,
    #[error("unsupported access policy version")]
    Version,
    #[error("access policy must enable at least one control")]
    Empty,
    #[error("invalid Basic username")]
    Username,
    #[error("invalid fixed-cost Basic verifier")]
    Verifier,
    #[error("invalid allowed IP network")]
    Network,
    #[error("too many allowed IP networks")]
    NetworkCount,
    #[error("access limit must be between 1 and 10000")]
    Limit,
}

/// Sensitive offline-guessable verifier.
///
/// Serialization is for the private authenticated control path, never an
/// inspector/status DTO. Zeroization is best effort: it cannot erase serializer
/// copies, allocator history or dumps.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretVerifier(Zeroizing<String>);

impl SecretVerifier {
    pub fn new(value: String) -> Result<Self, PolicyError> {
        let value = Zeroizing::new(value);
        validate_verifier(&value)?;
        Ok(Self(value))
    }

    pub fn expose_for_verification(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretVerifier([REDACTED])")
    }
}

impl Serialize for SecretVerifier {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SecretVerifier {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?)
            .map_err(|_| serde::de::Error::custom("invalid fixed-cost verifier"))
    }
}

fn validate_verifier(value: &str) -> Result<(), PolicyError> {
    if value.len() > 256 || !value.is_ascii() {
        return Err(PolicyError::Verifier);
    }
    let hash = PasswordHash::new(value).map_err(|_| PolicyError::Verifier)?;
    if hash.algorithm.as_str() != "argon2id"
        || hash.version != Some(19)
        || hash.params.iter().count() != 3
        || hash.params.get_decimal("m") != Some(ARGON2_MEMORY_KIB)
        || hash.params.get_decimal("t") != Some(ARGON2_TIME)
        || hash.params.get_decimal("p") != Some(ARGON2_PARALLELISM)
        || hash
            .hash
            .is_none_or(|output| output.len() != ARGON2_OUTPUT_BYTES)
    {
        return Err(PolicyError::Verifier);
    }
    let mut salt_bytes = [0; 64];
    let salt = hash.salt.ok_or(PolicyError::Verifier)?;
    if salt
        .decode_b64(&mut salt_bytes)
        .map_err(|_| PolicyError::Verifier)?
        .len()
        != ARGON2_SALT_BYTES
    {
        return Err(PolicyError::Verifier);
    }
    Ok(())
}

pub fn validate_username(username: &str) -> Result<(), PolicyError> {
    if username.is_empty()
        || username.len() > 64
        || !username.bytes().all(|b| b.is_ascii_graphic() && b != b':')
    {
        return Err(PolicyError::Username);
    }
    Ok(())
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BasicVerifier {
    username: String,
    password_hash: SecretVerifier,
}

impl BasicVerifier {
    pub fn new(username: String, password_hash: SecretVerifier) -> Result<Self, PolicyError> {
        validate_username(&username)?;
        Ok(Self {
            username,
            password_hash,
        })
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub const fn password_hash(&self) -> &SecretVerifier {
        &self.password_hash
    }
}

impl fmt::Debug for BasicVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BasicVerifier([REDACTED])")
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VisitorRateLimit {
    pub requests_per_second: u32,
    pub burst: u32,
}

/// Canonical, validated, immutable policy. All named fields serialize in this
/// order, with explicit nulls; this is the fingerprint's versioned byte contract.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VisitorAccessPolicyV1 {
    version: u16,
    basic: Option<BasicVerifier>,
    allow_ips: Vec<String>,
    rate_limit: Option<VisitorRateLimit>,
    max_concurrent: Option<u32>,
}

// deserialize_with suppresses serde's implicit default for Option fields: a
// present null is allowed, but omitting a nullable protection is not allowed.
fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyWire {
    version: u16,
    #[serde(deserialize_with = "required_nullable")]
    basic: Option<BasicVerifier>,
    allow_ips: Vec<String>,
    #[serde(deserialize_with = "required_nullable")]
    rate_limit: Option<VisitorRateLimit>,
    #[serde(deserialize_with = "required_nullable")]
    max_concurrent: Option<u32>,
}

impl<'de> Deserialize<'de> for VisitorAccessPolicyV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = PolicyWire::deserialize(deserializer)
            .map_err(|_| serde::de::Error::custom("invalid access policy encoding"))?;
        if wire.version != 1 {
            return Err(serde::de::Error::custom(
                "unsupported access policy version",
            ));
        }
        Self::new(
            wire.basic,
            &wire.allow_ips,
            wire.rate_limit,
            wire.max_concurrent,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl VisitorAccessPolicyV1 {
    pub fn new(
        basic: Option<BasicVerifier>,
        allow_ips: &[String],
        rate_limit: Option<VisitorRateLimit>,
        max_concurrent: Option<u32>,
    ) -> Result<Self, PolicyError> {
        if let Some(basic) = &basic {
            validate_username(basic.username())?;
        }
        if let Some(rate) = rate_limit {
            validate_limit(rate.requests_per_second)?;
            validate_limit(rate.burst)?;
        }
        if let Some(max) = max_concurrent {
            validate_limit(max)?;
        }
        let allow_ips = normalize_allow_ips(allow_ips)?;
        if basic.is_none()
            && allow_ips.is_empty()
            && rate_limit.is_none()
            && max_concurrent.is_none()
        {
            return Err(PolicyError::Empty);
        }
        let policy = Self {
            version: 1,
            basic,
            allow_ips,
            rate_limit,
            max_concurrent,
        };
        policy.canonical_json()?;
        Ok(policy)
    }

    /// Call at the bounded private-control decoding boundary. Errors deliberately
    /// discard serde text, which can contain malicious secret-bearing tokens.
    pub fn from_json_slice(bytes: &[u8]) -> Result<Self, PolicyError> {
        if bytes.len() > MAX_POLICY_BYTES {
            return Err(PolicyError::TooLarge);
        }
        serde_json::from_slice(bytes).map_err(|_| PolicyError::Encoding)
    }

    pub fn canonical_json(&self) -> Result<Zeroizing<Vec<u8>>, PolicyError> {
        let bytes = Zeroizing::new(serde_json::to_vec(self).map_err(|_| PolicyError::Encoding)?);
        if bytes.len() > MAX_POLICY_BYTES {
            return Err(PolicyError::TooLarge);
        }
        Ok(bytes)
    }

    pub fn fingerprint(&self) -> Result<String, PolicyError> {
        let mut digest = Sha256::new();
        digest.update(b"tnl.visitor-access.v1\0");
        digest.update(&*self.canonical_json()?);
        Ok(format!("{:x}", digest.finalize()))
    }

    pub const fn basic(&self) -> Option<&BasicVerifier> {
        self.basic.as_ref()
    }

    pub fn allow_ips(&self) -> &[String] {
        &self.allow_ips
    }

    pub const fn rate_limit(&self) -> Option<VisitorRateLimit> {
        self.rate_limit
    }

    pub const fn max_concurrent(&self) -> Option<u32> {
        self.max_concurrent
    }

    /// Match a trusted typed resolver result, never a raw forwarding header.
    /// No allowlist means no identity requirement. Phase2 compiles once for use
    /// in admission, rather than reparsing networks on each request.
    pub fn allows_ip(&self, address: IpAddr) -> bool {
        self.allow_ips.is_empty()
            || self.allow_ips.iter().any(|network| {
                network
                    .parse::<IpNet>()
                    .is_ok_and(|net| net.contains(&address.to_canonical()))
            })
    }
}

pub fn validate_limit(value: u32) -> Result<(), PolicyError> {
    if !(1..=MAX_LIMIT).contains(&value) {
        return Err(PolicyError::Limit);
    }
    Ok(())
}

pub fn normalize_allow_ips(entries: &[String]) -> Result<Vec<String>, PolicyError> {
    if entries.len() > MAX_ALLOW_IPS {
        return Err(PolicyError::NetworkCount);
    }
    let mut networks = entries
        .iter()
        .map(|s| canonical_network(s))
        .collect::<Result<Vec<_>, _>>()?;
    networks.sort_unstable();
    networks.dedup();
    Ok(networks)
}

fn canonical_network(text: &str) -> Result<String, PolicyError> {
    // Bound before IP parsing and allocation. Longest supported IPv6 CIDRs are
    // well under 64 ASCII bytes; zones/whitespace/hostnames are not accepted.
    if text.len() > 64 {
        return Err(PolicyError::Network);
    }
    let network = if text.contains('/') {
        text.parse::<IpNet>().map_err(|_| PolicyError::Network)?
    } else {
        IpNet::from(text.parse::<IpAddr>().map_err(|_| PolicyError::Network)?)
    };
    if let IpNet::V6(v6) = network {
        if let Some(v4) = v6.addr().to_ipv4_mapped() {
            let prefix = v6
                .prefix_len()
                .checked_sub(96)
                .ok_or(PolicyError::Network)?;
            return Ok(IpNet::new(v4.into(), prefix)
                .map_err(|_| PolicyError::Network)?
                .trunc()
                .to_string());
        }
    }
    Ok(network.trunc().to_string())
}
