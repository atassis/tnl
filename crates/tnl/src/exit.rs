//! Sysexits-style exit codes for `tnl`.
//!
//! Documented as a stability surface: scripts should rely on these for
//! programmatic decisions. New codes may be added in minor releases;
//! existing codes never change meaning.

pub const EX_OK: i32 = 0;
pub const EX_GENERIC: i32 = 1;
pub const EX_USAGE: i32 = 2;
pub const EX_NOT_AUTH: i32 = 64;
pub const EX_TOKEN_REJECTED: i32 = 65;
pub const EX_SUBDOMAIN_CONFLICT: i32 = 66;
pub const EX_LOCAL_UNREACHABLE: i32 = 67;
pub const EX_SERVER_UNREACHABLE: i32 = 68;
pub const EX_SERVER_ERROR: i32 = 70;
pub const EX_PAIRING_FAILURE: i32 = 75;

/// Classify an `anyhow::Error` chain into a sysexits-style code by
/// inspecting the rendered error string. Coarse but predictable for v0.1.0-beta;
/// structured error variants land in 1.0.
pub fn classify(e: &anyhow::Error) -> i32 {
    let s = format!("{e:#}").to_lowercase();
    if s.contains("config not found") || s.contains("not authenticated") {
        return EX_NOT_AUTH;
    }
    if s.contains("token rejected") || s.contains("unauthorized") || s.contains("invalid token") {
        return EX_TOKEN_REJECTED;
    }
    if s.contains("subdomaintaken") || s.contains("already in use") {
        return EX_SUBDOMAIN_CONFLICT;
    }
    if s.contains("connection refused") && s.contains("127.0.0.1") {
        return EX_LOCAL_UNREACHABLE;
    }
    if s.contains("dns error") || s.contains("tls handshake") || s.contains("connect to") {
        return EX_SERVER_UNREACHABLE;
    }
    if s.contains("/healthz returned") || s.contains("server error") {
        return EX_SERVER_ERROR;
    }
    if s.contains("pair_") || s.contains("redeem failed") {
        return EX_PAIRING_FAILURE;
    }
    EX_GENERIC
}
