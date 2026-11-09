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

/// Classify an `anyhow::Error` chain into a sysexits-style code using typed
/// errors first, with legacy rendered-message heuristics for other failures.
pub fn classify(e: &anyhow::Error) -> i32 {
    if e.downcast_ref::<crate::access::AccessInputError>()
        .is_some()
    {
        return EX_USAGE;
    }
    if let Some(error) = e.downcast_ref::<crate::client::ControlError>() {
        return match error {
            crate::client::ControlError::Server(tnl_protocol::ErrorCode::Unauthorized) => {
                EX_TOKEN_REJECTED
            }
            crate::client::ControlError::Server(tnl_protocol::ErrorCode::SubdomainTaken) => {
                EX_SUBDOMAIN_CONFLICT
            }
            crate::client::ControlError::Server(_) => EX_SERVER_ERROR,
            crate::client::ControlError::Framing
            | crate::client::ControlError::Schema
            | crate::client::ControlError::Acknowledgment => EX_GENERIC,
        };
    }
    if let Some(error) = e.downcast_ref::<crate::client::ProtectedControlError>() {
        return match error {
            crate::client::ProtectedControlError::Endpoint => EX_USAGE,
            crate::client::ProtectedControlError::Server(tnl_protocol::ErrorCode::Unauthorized) => {
                EX_TOKEN_REJECTED
            }
            crate::client::ProtectedControlError::Server(
                tnl_protocol::ErrorCode::SubdomainTaken,
            ) => EX_SUBDOMAIN_CONFLICT,
            _ => EX_SERVER_ERROR,
        };
    }
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

#[cfg(test)]
mod public_control_tests {
    use crate::client::ControlError;
    use tnl_protocol::ErrorCode;

    use super::*;

    const SERVER_EXIT_CODES: [(ErrorCode, i32); 11] = [
        (ErrorCode::Internal, EX_SERVER_ERROR),
        (ErrorCode::Unauthorized, EX_TOKEN_REJECTED),
        (ErrorCode::SubdomainTaken, EX_SUBDOMAIN_CONFLICT),
        (ErrorCode::InvalidSubdomain, EX_SERVER_ERROR),
        (ErrorCode::TunnelNotFound, EX_SERVER_ERROR),
        (ErrorCode::TunnelLost, EX_SERVER_ERROR),
        (ErrorCode::TunnelAlreadyActive, EX_SERVER_ERROR),
        (ErrorCode::InvalidAccessPolicy, EX_SERVER_ERROR),
        (ErrorCode::AccessPolicyMismatch, EX_SERVER_ERROR),
        (ErrorCode::UnsupportedProtocol, EX_SERVER_ERROR),
        (ErrorCode::AccessUnavailable, EX_SERVER_ERROR),
    ];

    #[test]
    fn public_server_errors_preserve_exit_codes() {
        for (code, expected) in SERVER_EXIT_CODES {
            let error = anyhow::Error::new(ControlError::Server(code));
            assert_eq!(classify(&error), expected, "{code:?}");
        }
    }

    #[test]
    fn context_wrapped_public_server_errors_use_codes_not_context_text() {
        for (code, expected) in SERVER_EXIT_CODES {
            for context in [
                "create public tunnel",
                "not authenticated; already in use; server error",
            ] {
                let error = anyhow::Error::new(ControlError::Server(code)).context(context);
                assert_eq!(classify(&error), expected, "{code:?}: {context}");
            }
        }
    }

    #[test]
    fn malformed_public_control_responses_remain_generic_protocol_failures() {
        for error in [
            ControlError::Framing,
            ControlError::Schema,
            ControlError::Acknowledgment,
        ] {
            let error = anyhow::Error::new(error);
            assert_eq!(classify(&error), EX_GENERIC);
            let error = error.context("not authenticated; already in use; server error");
            assert_eq!(classify(&error), EX_GENERIC);
        }
    }
}

#[cfg(test)]
mod access_tests {
    use super::*;
    #[test]
    fn protected_control_failure_uses_server_error_exit_code() {
        assert_eq!(
            classify(&anyhow::Error::new(
                crate::client::ProtectedControlError::Acknowledgment
            )),
            EX_SERVER_ERROR
        );
        assert_eq!(
            classify(&anyhow::Error::new(
                crate::access::AccessInputError::Endpoint
            )),
            EX_USAGE
        );
    }
}
