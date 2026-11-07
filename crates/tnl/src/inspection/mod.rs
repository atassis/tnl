//! Bounded local sanitized inspection; never an executable replay snapshot.
mod history;
mod model;
mod observe;
mod redact;
mod server;
pub use history::Inspection;
pub use model::{
    BodyPreview, CaptureId, GenerationState, Header, InspectionRecord, Message, PreviewMode, State,
    Summary, TunnelGeneration, TunnelStatus,
};
pub use observe::{
    Capture, CaptureContext, ConsumerFactory, Direction, Observation, ObservationConsumer,
};
pub use server::{BoundInspector, InspectorService};

pub(crate) fn safe_target(value: &str) -> String {
    redact::target(value)
}
pub(crate) fn safe_method(value: &str) -> String {
    redact::text(value, 32)
}
