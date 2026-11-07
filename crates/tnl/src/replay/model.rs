use crate::inspection::{CaptureId, TunnelGeneration};
use serde::Serialize;
use std::net::SocketAddr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityState {
    Disabled,
    Pending,
    Eligible,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayError {
    Disabled,
    LocalTargetRequired,
    NotRetained,
    Incomplete,
    InvalidRequest,
    TunnelUnavailable,
    WrongGeneration,
    Expired,
    ConfirmationRequired,
    InvalidConfirmation,
    Busy,
    Overloaded,
    RuntimeUnavailable,
}
impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ReplayError {}
#[derive(Clone, Debug, Serialize)]
pub struct ReplayAvailability {
    pub state: AvailabilityState,
    pub reason: Option<ReplayError>,
    pub request_body_bytes: Option<usize>,
    pub raw_expires_at_unix_ms: Option<u64>,
}
/// Contains an intentionally sensitive one-use ticket, never raw request data.
#[derive(Serialize)]
pub struct ReplayPreview {
    pub capture_id: CaptureId,
    pub tunnel_generation: TunnelGeneration,
    pub replay_id: ulid::Ulid,
    pub confirmation_token: String,
    pub confirmation_expires_at_unix_ms: u64,
    pub raw_expires_at_unix_ms: u64,
    pub request_body_bytes: usize,
    pub target: SocketAddr,
    pub host_policy: &'static str,
    pub warnings: [&'static str; 3],
}
impl std::fmt::Debug for ReplayPreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplayPreview")
            .field("capture_id", &self.capture_id)
            .field("replay_id", &self.replay_id)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Running,
    Complete,
    Failed,
    Cancelled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Unknown,
    NotSent,
    PossiblySent,
    ResponseReceived,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineError {
    ConnectFailed,
    WriteFailed,
    ResponseTimeout,
    ResponseInvalid,
    ResponseTooLarge,
    Cancelled,
}
#[derive(Clone, Debug, Serialize)]
pub struct ReplayOutcome {
    pub replay_id: ulid::Ulid,
    pub parent_capture_id: CaptureId,
    pub tunnel_generation: TunnelGeneration,
    pub state: AttemptState,
    pub delivery: Delivery,
    pub status: Option<u16>,
    pub duration_ms: u64,
    pub request_body_bytes: usize,
    pub response_body_bytes: u64,
    pub response_complete: bool,
    pub error_code: Option<EngineError>,
}
