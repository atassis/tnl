//! Bounded local sanitized inspection; never an executable replay snapshot.
mod assets;
mod history;
mod model;
mod observe;
mod redact;
mod server;
pub(crate) mod timing;
pub use history::Inspection;
pub use model::{
    BodyPreview, BodyState, CaptureId, GenerationState, Header, HeadersState, InspectionRecord,
    Message, Origin, PreviewMode, RequestBoundary, ResponseSource, State, Summary,
    TunnelGeneration, TunnelStatus, Visitor,
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

pub(crate) use model::{wall_ms, TransportCounters};

pub(crate) fn safe_tunnel(value: &str) -> String {
    redact::text(value, 64)
}
