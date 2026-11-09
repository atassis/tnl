//! Control-channel wire contract shared by the client and daemon.

/// Exact HTTP header used to select the control protocol.
///
/// Runtime negotiation/enforcement is deliberately owned by the control
/// lifecycle implementation, not this wire-contract module.
pub const CONTROL_PROTOCOL_HEADER: &str = "x-tnl-control-protocol";

/// The only supported control protocol version.
pub const CONTROL_PROTOCOL_VERSION: &str = "heartbeat-v1";

/// Largest permitted length-prefixed control message payload.
pub const MAX_CONTROL_MESSAGE_BYTES: usize = 1024 * 1024;

/// How a typed server error affects a reattach attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReattachErrorDisposition {
    /// The old tunnel no longer exists; recreation with retained settings is allowed.
    Recreate,
    /// The original owner is still active; retry without taking it over.
    Retry,
    /// Authentication, policy, protocol, and all other errors stop the run.
    Terminal,
}

/// Exact, single-valued offer/echo; comma lists and duplicates are incompatible.
pub fn has_control_protocol(headers: &http::HeaderMap) -> bool {
    let mut values = headers.get_all(CONTROL_PROTOCOL_HEADER).iter();
    values
        .next()
        .is_some_and(|value| value == CONTROL_PROTOCOL_VERSION)
        && values.next().is_none()
}

#[derive(Debug, thiserror::Error)]
#[error("incompatible control protocol; both client and daemon must use heartbeat-v1")]
pub struct ControlProtocolIncompatible;
