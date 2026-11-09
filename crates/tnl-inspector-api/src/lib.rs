//! Sanitized inspector wire DTOs. No client, transport, capture buffers or authority.
pub mod inspection;
pub mod replay;
pub mod timing;

use schemars::{generate::SchemaSettings, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub fn id_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({"type":"string","pattern":"^[0-7][0-9A-HJKMNP-TV-Z]{25}$"})
}
pub fn optional_id_schema(generator: &mut SchemaGenerator) -> Schema {
    let id = id_schema(generator);
    schemars::json_schema!({"anyOf":[id,{"type":"null"}]})
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct StatsReply {
    #[schemars(extend("const" = 1))]
    pub schema_version: u8,
    pub records: usize,
    pub tunnel: inspection::TunnelStatus,
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct ListReply<'a> {
    #[schemars(extend("const" = 1))]
    pub schema_version: u8,
    pub items: &'a [inspection::Summary],
    pub next_before: Option<u64>,
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct ReplayDisplay {
    pub eligible: bool,
    #[serde(flatten)]
    pub availability: replay::ReplayAvailability,
}
impl From<replay::ReplayAvailability> for ReplayDisplay {
    fn from(availability: replay::ReplayAvailability) -> Self {
        Self {
            eligible: availability.state == replay::AvailabilityState::Eligible,
            availability,
        }
    }
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct DetailReply<'a> {
    #[serde(flatten)]
    pub record: &'a inspection::InspectionRecord,
    pub replay: &'a ReplayDisplay,
    pub last_replay_outcome: &'a Option<replay::ReplayOutcome>,
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct AvailabilityReply {
    #[schemars(extend("const" = 1))]
    pub schema_version: u8,
    pub availability: replay::ReplayAvailability,
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct PreviewReply {
    #[schemars(extend("const" = 1))]
    pub schema_version: u8,
    pub method: String,
    pub request_target: String,
    pub preview: replay::ReplayPreview,
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct OutcomeReply {
    #[schemars(extend("const" = 1))]
    pub schema_version: u8,
    pub outcome: Option<replay::ReplayOutcome>,
}
#[derive(Debug, Serialize, JsonSchema)]
pub struct ReplayErrorReply {
    #[schemars(extend("const" = 1))]
    pub schema_version: u8,
    pub error: replay::ReplayError,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrepareRequest {
    #[schemars(extend("const" = 1))]
    pub schema_version: u8,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfirmRequest {
    #[schemars(extend("const" = 1))]
    pub schema_version: u8,
    #[schemars(schema_with = "crate::id_schema")]
    pub replay_id: ulid::Ulid,
    pub confirmation_token: String,
    pub confirm_side_effects: bool,
}
impl std::fmt::Debug for ConfirmRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfirmRequest")
            .field("replay_id", &self.replay_id)
            .finish_non_exhaustive()
    }
}

fn schema<T: JsonSchema>(serialize: bool) -> serde_json::Value {
    let settings = SchemaSettings::draft07();
    let settings = if serialize {
        settings.for_serialize()
    } else {
        settings.for_deserialize()
    };
    let mut value: serde_json::Value = settings.into_generator().into_root_schema_for::<T>().into();
    normalize_integers(&mut value);
    value
}
// Rust integer formats are annotations outside standard Draft-07. Preserve
// their numeric constraints explicitly so standalone validators need no eval
// or nonstandard format callbacks.
fn normalize_integers(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            let maximum = match map.get("format").and_then(serde_json::Value::as_str) {
                Some("uint8") => Some(u64::from(u8::MAX)),
                Some("uint16") => Some(u64::from(u16::MAX)),
                Some("uint32") => Some(u64::from(u32::MAX)),
                Some("uint64" | "uint") => Some(u64::MAX),
                _ => None,
            };
            if let Some(maximum) = maximum {
                map.remove("format");
                map.entry("maximum").or_insert(maximum.into());
            }
            for child in map.values_mut() {
                normalize_integers(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                normalize_integers(item);
            }
        }
        _ => {}
    }
}
pub fn schemas() -> BTreeMap<&'static str, serde_json::Value> {
    BTreeMap::from([
        ("stats", schema::<StatsReply>(true)),
        ("list", schema::<ListReply<'static>>(true)),
        ("detail", schema::<DetailReply<'static>>(true)),
        ("availability", schema::<AvailabilityReply>(true)),
        ("preview", schema::<PreviewReply>(true)),
        ("outcome", schema::<OutcomeReply>(true)),
        ("replay_error", schema::<ReplayErrorReply>(true)),
        ("prepare_request", schema::<PrepareRequest>(false)),
        ("confirm_request", schema::<ConfirmRequest>(false)),
    ])
}
