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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum PreviewMode {
    Off,
    Json,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum State {
    Active,
    Complete,
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
    pub text: Option<String>,
    pub reason: Option<&'static str>,
    pub observed_payload_bytes: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Message {
    pub headers: Vec<Header>,
    pub body: BodyPreview,
}
impl Message {
    pub(crate) fn new(mode: PreviewMode) -> Self {
        Self {
            headers: Vec::new(),
            body: BodyPreview {
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
    pub capture_id: CaptureId,
    pub generation: TunnelGeneration,
    pub request_id: Option<ulid::Ulid>,
    pub visitor_source: Option<&'static str>,
    pub sequence: u64,
    pub method: String,
    pub request_target: String,
    pub status: Option<u16>,
    pub state: State,
    pub request: Message,
    pub response: Message,
    pub observation_incomplete: bool,
}
