use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct CaptureId(pub(crate) ulid::Ulid);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct TunnelGeneration(ulid::Ulid);
impl TunnelGeneration {
    pub fn new() -> Self {
        Self(ulid::Ulid::new())
    }
}
impl Default for TunnelGeneration {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum PreviewMode {
    Off,
    Json,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Active,
    Complete,
    Failed,
    Incomplete,
    Upgraded,
}
#[derive(Clone, Debug, Serialize)]
pub struct Header {
    pub name: String,
    pub value: String,
    pub redacted: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct BodyPreview {
    pub mode: PreviewMode,
    pub state: BodyState,
    pub retained_bytes: u32,
    pub redacted: bool,
    pub text: Option<String>,
    pub reason: Option<&'static str>,
    pub observed_payload_bytes: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Message {
    pub headers_state: HeadersState,
    pub headers: Vec<Header>,
    pub body: BodyPreview,
}
impl Message {
    pub(crate) fn new(mode: PreviewMode) -> Self {
        Self {
            headers: Vec::new(),
            headers_state: HeadersState::Incomplete,
            body: BodyPreview {
                mode,
                state: if mode == PreviewMode::Off {
                    BodyState::Disabled
                } else {
                    BodyState::Pending
                },
                retained_bytes: 0,
                redacted: false,
                text: None,
                reason: Some(if mode == PreviewMode::Off {
                    "disabled"
                } else {
                    "incomplete"
                }),
                observed_payload_bytes: 0,
            },
        }
    }
}
/// Sanitized display data only. Never an executable request or raw snapshot.
#[derive(Clone, Debug, Serialize)]
pub struct InspectionRecord {
    pub schema_version: u8,
    pub tunnel: String,
    pub tunnel_id: Option<ulid::Ulid>,
    pub origin: Origin,
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub failure_kind: Option<&'static str>,
    pub display_target: String,
    pub resolved_addr: Option<std::net::SocketAddr>,
    pub response_source: ResponseSource,
    pub request_boundary: RequestBoundary,
    /// Counts bytes accepted by successful local `poll_write` calls, including
    /// HTTP framing. These are not backend-application/browser delivery claims.
    pub bytes_to_backend: u64,
    pub bytes_from_backend: u64,
    pub bytes_to_client: u64,
    pub byte_count_semantics: &'static str,
    pub omission_reasons: Vec<&'static str>,
    pub visitor: Visitor,
    #[serde(skip)]
    pub(crate) started: std::time::Instant,
    #[serde(skip)]
    pub(crate) terminal: bool,
    #[serde(skip)]
    pub(crate) counters: Option<TransportCounters>,
    pub capture_id: CaptureId,
    pub generation: TunnelGeneration,
    pub request_id: Option<ulid::Ulid>,
    pub visitor_source: Option<&'static str>,
    pub sequence: u64,
    pub method: String,
    pub request_target: String,
    pub status: Option<u16>,
    pub state: State,
    pub connection: GenerationState,
    pub request: Message,
    pub response: Message,
    pub observation_incomplete: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationState {
    Untracked,
    Connected,
    Disconnected,
    Superseded,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct TunnelStatus {
    pub generation: Option<TunnelGeneration>,
    pub connected: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub response_source: ResponseSource,
    pub request_boundary: RequestBoundary,
    pub failure_kind: Option<&'static str>,
    pub bytes_to_backend: u64,
    pub bytes_from_backend: u64,
    pub capture_id: CaptureId,
    pub generation: TunnelGeneration,
    pub request_id: Option<ulid::Ulid>,
    pub sequence: u64,
    pub method: String,
    pub request_target: String,
    pub status: Option<u16>,
    pub state: State,
    pub connection: GenerationState,
}

impl State {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Incomplete => "incomplete",
            Self::Upgraded => "upgraded",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    External,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseSource {
    Backend,
    ClientSynth,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestBoundary {
    EffectiveBackend,
    PreconnectUnavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadersState {
    Complete,
    Capped,
    Malformed,
    Incomplete,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyState {
    Disabled,
    Pending,
    Available,
    Empty,
    Omitted,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Visitor {
    pub available: bool,
    pub source: Option<&'static str>,
    pub display_ip: Option<&'static str>,
}
impl InspectionRecord {
    pub(crate) fn snapshot(&self) -> Self {
        let mut record = self.clone();
        if !record.terminal {
            record.duration_ms = elapsed_ms(record.started);
        }
        if let Some(counters) = &record.counters {
            use std::sync::atomic::Ordering;
            record.bytes_to_backend = counters.to_backend.load(Ordering::Relaxed);
            record.bytes_to_client = counters.to_client.load(Ordering::Relaxed);
            record.bytes_from_backend = if record.response_source == ResponseSource::Backend {
                record.bytes_to_client
            } else {
                0
            };
        }
        record
    }
    pub(crate) fn omit(&mut self, reason: &'static str) {
        let reason = match reason {
            "head-limit" | "chunk-limit" | "trailer-limit" | "informational-limit" => {
                "capture-limit"
            }
            "malformed-head" | "ambiguous-framing" | "invalid-chunk" | "invalid-trailer"
            | "unexpected-data" => "invalid-framing",
            "cancelled" => "incomplete",
            other => other,
        };
        if self.omission_reasons.len() < 16 && !self.omission_reasons.contains(&reason) {
            self.omission_reasons.push(reason);
        }
    }
}
pub(super) fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
pub fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[derive(Clone, Debug)]
pub struct TransportCounters {
    pub to_backend: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub to_client: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

#[derive(Serialize)]
pub(super) struct Detail {
    #[serde(flatten)]
    pub record: InspectionRecord,
    pub replay: ReplayDisplay,
    pub last_replay_outcome: Option<crate::replay::ReplayOutcome>,
}

#[derive(Serialize)]
pub(super) struct ReplayDisplay {
    pub eligible: bool,
    #[serde(flatten)]
    pub availability: crate::replay::ReplayAvailability,
}
impl From<crate::replay::ReplayAvailability> for ReplayDisplay {
    fn from(availability: crate::replay::ReplayAvailability) -> Self {
        Self {
            eligible: availability.state == crate::replay::AvailabilityState::Eligible,
            availability,
        }
    }
}

#[cfg(test)]
impl InspectionRecord {
    pub(crate) fn allocated_capacity(&self) -> usize {
        let mut bytes = std::mem::size_of::<Self>()
            + self.tunnel.capacity()
            + self.display_target.capacity()
            + self.method.capacity()
            + self.request_target.capacity()
            + self.omission_reasons.capacity() * std::mem::size_of::<&str>();
        for message in [&self.request, &self.response] {
            bytes += message.headers.capacity() * std::mem::size_of::<Header>();
            bytes += message
                .headers
                .iter()
                .map(|h| h.name.capacity() + h.value.capacity())
                .sum::<usize>();
            bytes += message.body.text.as_ref().map_or(0, String::capacity);
        }
        if self.counters.is_some() {
            bytes += 2
                * (std::mem::size_of::<std::sync::atomic::AtomicU64>()
                    + 2 * std::mem::size_of::<usize>());
        }
        bytes
    }
}
