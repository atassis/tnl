use schemars::JsonSchema;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CaptureId(#[schemars(schema_with = "crate::id_schema")] pub ulid::Ulid);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TunnelGeneration(#[schemars(schema_with = "crate::id_schema")] ulid::Ulid);
impl TunnelGeneration {
    pub fn new() -> Self {
        Self(ulid::Ulid::new())
    }
}
impl From<ulid::Ulid> for TunnelGeneration {
    fn from(id: ulid::Ulid) -> Self {
        Self(id)
    }
}
impl Default for TunnelGeneration {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PreviewMode {
    Off,
    Json,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Active,
    Complete,
    Failed,
    Incomplete,
    Upgraded,
}
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct Header {
    pub name: String,
    pub value: String,
    pub redacted: bool,
}
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct BodyPreview {
    pub mode: PreviewMode,
    pub state: BodyState,
    pub retained_bytes: u32,
    pub redacted: bool,
    pub text: Option<String>,
    pub reason: Option<&'static str>,
    pub observed_payload_bytes: u64,
}
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct Message {
    pub headers_state: HeadersState,
    pub headers: Vec<Header>,
    pub body: BodyPreview,
}
impl Message {
    pub fn new(mode: PreviewMode) -> Self {
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
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct InspectionRecord {
    #[schemars(extend("const" = 1))]
    pub schema_version: u8,
    pub tunnel: String,
    #[schemars(schema_with = "crate::optional_id_schema")]
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
    pub capture_id: CaptureId,
    pub generation: TunnelGeneration,
    #[schemars(schema_with = "crate::optional_id_schema")]
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
    pub timing: crate::timing::Timing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GenerationState {
    Untracked,
    Connected,
    Disconnected,
    Superseded,
}
#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
pub struct TunnelStatus {
    pub generation: Option<TunnelGeneration>,
    pub connected: bool,
}
#[derive(Clone, Debug, Serialize, JsonSchema)]
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
    #[schemars(schema_with = "crate::optional_id_schema")]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    External,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResponseSource {
    Backend,
    ClientSynth,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequestBoundary {
    EffectiveBackend,
    PreconnectUnavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HeadersState {
    Complete,
    Capped,
    Malformed,
    Incomplete,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BodyState {
    Disabled,
    Pending,
    Available,
    Empty,
    Omitted,
}
#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
pub struct Visitor {
    pub available: bool,
    pub source: Option<&'static str>,
    pub display_ip: Option<&'static str>,
}
