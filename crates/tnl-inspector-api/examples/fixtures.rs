use serde_json::json;
use tnl_inspector_api::{
    inspection::{
        CaptureId, GenerationState, HeadersState, InspectionRecord, Message, Origin, PreviewMode,
        RequestBoundary, ResponseSource, State, Summary, TunnelGeneration, TunnelStatus, Visitor,
    },
    replay::{
        AttemptState, AvailabilityState, Delivery, ReplayAvailability, ReplayError, ReplayOutcome,
        ReplayPreview,
    },
    AvailabilityReply, DetailReply, ListReply, OutcomeReply, PreviewReply, ReplayDisplay,
    ReplayErrorReply, StatsReply,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let id = ulid::Ulid::from(0_u128);
    let generation = TunnelGeneration::from(id);
    let mut request = Message::new(PreviewMode::Off);
    request.headers_state = HeadersState::Complete;
    let mut response = Message::new(PreviewMode::Off);
    response.headers_state = HeadersState::Complete;
    let record = InspectionRecord {
        schema_version: 1,
        tunnel: "demo".into(),
        tunnel_id: Some(id),
        origin: Origin::External,
        started_at_ms: 1_700_000_000_000,
        duration_ms: 42,
        failure_kind: None,
        display_target: "localhost:3000".into(),
        resolved_addr: Some("127.0.0.1:3000".parse()?),
        response_source: ResponseSource::Backend,
        request_boundary: RequestBoundary::EffectiveBackend,
        bytes_to_backend: 40,
        bytes_from_backend: 100,
        bytes_to_client: 100,
        byte_count_semantics: "successful_local_writes_not_delivery",
        omission_reasons: vec!["disabled"],
        visitor: Visitor {
            available: false,
            source: None,
            display_ip: None,
        },
        capture_id: CaptureId(id),
        generation,
        request_id: Some(id),
        visitor_source: None,
        sequence: 1,
        method: "GET".into(),
        request_target: "/api/orders".into(),
        status: Some(200),
        state: State::Complete,
        connection: GenerationState::Connected,
        request,
        response,
        observation_incomplete: false,
        timing: fixture_timing(),
    };
    let availability = ReplayAvailability {
        state: AvailabilityState::Disabled,
        reason: Some(ReplayError::Disabled),
        request_body_bytes: None,
        raw_expires_at_unix_ms: None,
    };
    let replay = ReplayDisplay::from(availability.clone());
    let summary = Summary {
        started_at_ms: record.started_at_ms,
        duration_ms: record.duration_ms,
        response_source: record.response_source,
        request_boundary: record.request_boundary,
        failure_kind: None,
        bytes_to_backend: 40,
        bytes_from_backend: 100,
        capture_id: record.capture_id,
        generation,
        request_id: Some(id),
        sequence: 1,
        method: record.method.clone(),
        request_target: record.request_target.clone(),
        status: Some(200),
        state: State::Complete,
        connection: GenerationState::Connected,
    };
    let fixtures = json!({
        "stats": StatsReply {schema_version:1,records:1,tunnel:TunnelStatus {generation:Some(generation),connected:true}},
        "list": ListReply {schema_version:1,items:&[summary],next_before:Some(1)},
        "detail": DetailReply {record:&record,replay:&replay,last_replay_outcome:&None},
        "availability": AvailabilityReply {schema_version:1,availability},
        "preview": PreviewReply {schema_version:1,method:"GET".into(),request_target:"/api/orders".into(),preview:ReplayPreview {
            capture_id:CaptureId(id),tunnel_generation:generation,replay_id:id,confirmation_token:"0".repeat(64),
            confirmation_expires_at_unix_ms:1_700_000_000_100,raw_expires_at_unix_ms:1_700_000_000_200,
            request_body_bytes:0,target:"[::1]:3000".parse()?,host_policy:"original",warnings:["One attempt", "Side effects", "Original secrets"]}},
        "outcome": OutcomeReply {schema_version:1,outcome:Some(ReplayOutcome {
            replay_id:id,parent_capture_id:CaptureId(id),tunnel_generation:generation,state:AttemptState::Complete,
            delivery:Delivery::ResponseReceived,status:Some(200),duration_ms:5,request_body_bytes:0,
            response_body_bytes:0,response_complete:true,error_code:None})},
        "replay_error": ReplayErrorReply {schema_version:1,error:ReplayError::Expired},
        "prepare_request":{"schema_version":1},
        "confirm_request":{"schema_version":1,"replay_id":id,"confirmation_token":"0".repeat(64),"confirm_side_effects":true}
    });
    serde_json::to_writer_pretty(std::io::stdout().lock(), &fixtures)?;
    Ok(())
}

fn fixture_timing() -> tnl_inspector_api::timing::Timing {
    use tnl_inspector_api::timing::{Interval, Intervals, Outcome, Snapshot, Timing};
    let done = |start, end| Interval {
        outcome: Outcome::Completed,
        start_offset_us: Some(start),
        end_offset_us: Some(end),
    };
    let skipped = Interval {
        outcome: Outcome::Skipped,
        start_offset_us: None,
        end_offset_us: None,
    };
    Timing::from_snapshot(Snapshot {
        as_of_offset_us: 11,
        terminal: true,
        intervals: Intervals {
            metadata: skipped,
            request_head: done(0, 1),
            resolve: skipped,
            connect: done(1, 4),
            request_write: done(4, 6),
            response_read: done(5, 10),
            response_write: done(7, 11),
            upgraded: Interval::not_started(),
        },
        backend_first_byte_offset_us: Some(5),
        backend_final_headers_offset_us: Some(7),
        request_framing_end_offset_us: Some(5),
        response_framing_end_offset_us: Some(9),
        request_framing_valid: true,
        response_framing_valid: true,
    })
}
