//! Phase1 per-run input helpers.
//!
//! Not wired into `tnl http` until the daemon's protected lifecycle and
//! enforcement are available. Call preparation before
//! starting the runtime (Argon2 is intentionally synchronous and runs once).

use std::{
    fmt,
    fs::OpenOptions,
    io::{IsTerminal, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

use argon2::{
    password_hash::{rand_core::OsRng, SaltString},
    Algorithm, Argon2, Params, PasswordHasher, Version,
};
use clap::Args;
use tnl_protocol::access::{
    normalize_allow_ips, validate_limit, validate_username, BasicVerifier, PolicyError,
    SecretVerifier, VisitorAccessPolicyV1, VisitorRateLimit, ARGON2_MEMORY_KIB,
    ARGON2_OUTPUT_BYTES, ARGON2_PARALLELISM, ARGON2_TIME,
};
use zeroize::Zeroizing;

#[derive(Args, Default)]
pub struct AccessArgs {
    #[arg(long, value_name = "USER")]
    pub basic_auth: Option<String>,
    #[arg(long, value_name = "PATH", requires = "basic_auth")]
    pub basic_auth_password_file: Option<PathBuf>,
    #[arg(long = "allow", value_name = "IP_OR_CIDR")]
    pub allow_ips: Vec<String>,
    #[arg(long, value_name = "REQUESTS_PER_SECOND", value_parser = clap::value_parser!(u32).range(1..=10_000))]
    pub rate_limit: Option<u32>,
    #[arg(long, value_name = "BURST", requires = "rate_limit", value_parser = clap::value_parser!(u32).range(1..=10_000))]
    pub rate_burst: Option<u32>,
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=10_000))]
    pub max_concurrent: Option<u32>,
}

impl fmt::Debug for AccessArgs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessArgs")
            .field("basic", &self.basic_auth.is_some())
            .field("allow_count", &self.allow_ips.len())
            .field("rate", &self.rate_limit.is_some())
            .field("concurrency", &self.max_concurrent.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AccessInputError {
    #[error("invalid access option dependency")]
    Dependency,
    #[error("protected tunnels require an https endpoint without credentials, query or fragment")]
    Endpoint,
    #[error(transparent)]
    Policy(#[from] PolicyError),
    #[error("password must be 1-1024 UTF-8 bytes without control characters")]
    Password,
    #[error("unable to read password input")]
    Input,
    #[error("password file must be a regular file with no group/other permissions")]
    FilePermissions,
    #[error("password requires a hidden terminal prompt or --basic-auth-password-file")]
    Terminal,
    #[error("unable to derive password verifier")]
    Hash,
}

impl AccessArgs {
    pub fn prepare(
        &self,
        endpoint: &str,
    ) -> Result<Option<Arc<VisitorAccessPolicyV1>>, AccessInputError> {
        self.prepare_with_prompt(endpoint, hidden_password_prompt)
    }

    /// Input injection keeps hidden-prompt behavior separate from hashing and
    /// ensures all endpoint/options validation precedes any secret input.
    pub fn prepare_with_prompt<F>(
        &self,
        endpoint: &str,
        prompt: F,
    ) -> Result<Option<Arc<VisitorAccessPolicyV1>>, AccessInputError>
    where
        F: FnOnce() -> Result<Zeroizing<String>, AccessInputError>,
    {
        if (self.basic_auth_password_file.is_some() && self.basic_auth.is_none())
            || (self.rate_burst.is_some() && self.rate_limit.is_none())
        {
            return Err(AccessInputError::Dependency);
        }
        let protected = self.basic_auth.is_some()
            || !self.allow_ips.is_empty()
            || self.rate_limit.is_some()
            || self.max_concurrent.is_some();
        if !protected {
            return Ok(None);
        }
        validate_protected_endpoint(endpoint)?;
        if let Some(username) = &self.basic_auth {
            validate_username(username)?;
        }
        let networks = normalize_allow_ips(&self.allow_ips)?;
        for value in [self.rate_limit, self.rate_burst, self.max_concurrent]
            .into_iter()
            .flatten()
        {
            validate_limit(value)?;
        }
        let rate = self.rate_limit.map(|requests_per_second| VisitorRateLimit {
            requests_per_second,
            burst: self.rate_burst.unwrap_or(requests_per_second),
        });
        let basic = if let Some(username) = &self.basic_auth {
            let password = match &self.basic_auth_password_file {
                Some(path) => read_password_file(path)?,
                None => prompt()?,
            };
            validate_password(&password)?;
            let verifier = hash_password(&password)?;
            drop(password);
            Some(BasicVerifier::new(username.clone(), verifier)?)
        } else {
            None
        };
        Ok(Some(Arc::new(VisitorAccessPolicyV1::new(
            basic,
            &networks,
            rate,
            self.max_concurrent,
        )?)))
    }
}

pub(crate) fn validate_protected_endpoint(endpoint: &str) -> Result<(), AccessInputError> {
    let url = url::Url::parse(endpoint).map_err(|_| AccessInputError::Endpoint)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AccessInputError::Endpoint);
    }
    Ok(())
}

fn validate_password(password: &str) -> Result<(), AccessInputError> {
    if password.is_empty() || password.len() > 1024 || password.chars().any(char::is_control) {
        return Err(AccessInputError::Password);
    }
    Ok(())
}

/// Opens once and checks the opened target (including symlinks), not the path.
///
/// Path/directories/ownership/ACLs must be trusted by the caller. On non-Unix
/// systems, restrictive file ACLs remain the user's responsibility.
pub fn read_password_file(path: &Path) -> Result<Zeroizing<String>, AccessInputError> {
    let mut options = OpenOptions::new();
    options.read(true);
    // Inspect FIFOs/devices without blocking on open before rejecting them.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| AccessInputError::Input)?;
    let metadata = file.metadata().map_err(|_| AccessInputError::Input)?;
    if !metadata.is_file() {
        return Err(AccessInputError::FilePermissions);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(AccessInputError::FilePermissions);
        }
    }
    if metadata.len() > 1026 {
        return Err(AccessInputError::Password);
    }
    read_password_contents(file)
}

fn read_password_contents(reader: impl Read) -> Result<Zeroizing<String>, AccessInputError> {
    // Metadata can be stale or underreport length. One overflow byte
    // distinguishes actual bounded EOF from a truncated password prefix.
    let mut bytes = Zeroizing::new(Vec::with_capacity(1027));
    reader
        .take(1027)
        .read_to_end(&mut bytes)
        .map_err(|_| AccessInputError::Input)?;
    if bytes.len() > 1026 {
        return Err(AccessInputError::Password);
    }
    if bytes.ends_with(b"\n") {
        bytes.pop();
        if bytes.ends_with(b"\r") {
            bytes.pop();
        }
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| AccessInputError::Password)?;
    validate_password(text)?;
    Ok(Zeroizing::new(text.to_owned()))
}

fn hidden_password_prompt() -> Result<Zeroizing<String>, AccessInputError> {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Err(AccessInputError::Terminal);
    }
    dialoguer::Password::new()
        .with_prompt("Visitor password")
        .with_confirmation("Confirm visitor password", "Passwords do not match")
        .interact()
        .map(Zeroizing::new)
        .map_err(|_| AccessInputError::Input)
}

fn hash_password(password: &str) -> Result<SecretVerifier, AccessInputError> {
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_TIME,
        ARGON2_PARALLELISM,
        Some(ARGON2_OUTPUT_BYTES),
    )
    .map_err(|_| AccessInputError::Hash)?;
    let salt = SaltString::generate(&mut OsRng);
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let hash = argon
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| AccessInputError::Hash)?;
    Ok(SecretVerifier::new(hash.to_string())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    #[test]
    fn password_file_rejects_growth_after_metadata_check_without_echoing_secret() {
        let mut file = tempfile::tempfile().unwrap();
        let mut password = b"SECRET_SENTINEL".to_vec();
        password.resize(1024, b'x');
        password.extend_from_slice(b"\r\n");
        file.write_all(&password).unwrap();
        let checked_metadata = file.metadata().unwrap();
        assert!(checked_metadata.is_file());
        assert_eq!(checked_metadata.len(), 1026);

        // Deterministically grow the same regular file after the caller's size
        // check, before its content read. No path is reopened or replaced.
        file.write_all(&[b'y'; 4096]).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        match read_password_contents(&mut file) {
            Err(err) => {
                assert!(matches!(err, AccessInputError::Password));
                assert!(!format!("{err:?} {err}").contains("SECRET_SENTINEL"));
            }
            Ok(_) => panic!("oversized password file was silently truncated"),
        }
        assert_eq!(file.stream_position().unwrap(), 1027);
    }
}
