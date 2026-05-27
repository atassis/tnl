//! Sysexits-style exit codes for `tnld`. Symmetric to crates/tnl/src/exit.rs.

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

pub fn classify(e: &anyhow::Error) -> i32 {
    let s = format!("{e:#}").to_lowercase();
    if s.contains("config not found")
        || (s.contains("no such file or directory") && s.contains("config"))
    {
        return EX_NOT_AUTH;
    }
    if s.contains("token rejected") || s.contains("unauthorized") {
        return EX_TOKEN_REJECTED;
    }
    if s.contains("address already in use") {
        return EX_SERVER_ERROR;
    }
    if s.contains("connection refused") {
        return EX_SERVER_UNREACHABLE;
    }
    if s.contains("/healthz returned") {
        return EX_SERVER_ERROR;
    }
    EX_GENERIC
}
