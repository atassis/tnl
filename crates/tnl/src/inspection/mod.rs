//! Bounded process-local observation. No listener, raw replay retention, or
//! executable reconstruction from sanitized records is provided here.
mod history;
mod model;
mod observe;
mod redact;
pub use history::Inspection;
pub use model::{
    BodyPreview, CaptureId, Header, InspectionRecord, Message, PreviewMode, State, TunnelGeneration,
};
pub use observe::{
    Capture, CaptureContext, ConsumerFactory, Direction, Observation, ObservationConsumer,
};

pub(crate) fn safe_target(value: &str) -> String {
    redact::target(value)
}
pub(crate) fn safe_method(value: &str) -> String {
    redact::text(value, 32)
}
