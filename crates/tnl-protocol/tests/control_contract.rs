use tnl_protocol::{
    control::{
        ReattachErrorDisposition, CONTROL_PROTOCOL_HEADER, CONTROL_PROTOCOL_VERSION,
        MAX_CONTROL_MESSAGE_BYTES,
    },
    ClosingReason, ControlMsg, ErrorCode,
};

#[test]
fn heartbeat_and_ack_wire_format_pin_identity_and_sequence() {
    let tunnel_id = ulid::Ulid::nil();
    let heartbeat = ControlMsg::Heartbeat {
        tunnel_id,
        sequence: 42,
    };
    let ack = ControlMsg::HeartbeatAck {
        tunnel_id,
        sequence: 42,
    };

    assert_eq!(
        serde_json::to_string(&heartbeat).unwrap(),
        r#"{"type":"Heartbeat","tunnel_id":"00000000000000000000000000","sequence":42}"#
    );
    assert_eq!(
        serde_json::to_string(&ack).unwrap(),
        r#"{"type":"HeartbeatAck","tunnel_id":"00000000000000000000000000","sequence":42}"#
    );
    assert_eq!(
        serde_json::from_str::<ControlMsg>(&serde_json::to_string(&ack).unwrap()).unwrap(),
        ack
    );
}

#[test]
fn heartbeat_fields_are_strict_and_protocol_constants_are_exact() {
    for invalid in [
        r#"{"type":"Heartbeat","tunnel_id":"00000000000000000000000000"}"#,
        r#"{"type":"Heartbeat","tunnel_id":"not-a-ulid","sequence":1}"#,
        r#"{"type":"Heartbeat","tunnel_id":"00000000000000000000000000","sequence":"1"}"#,
        r#"{"type":"Heartbeat","tunnel_id":"00000000000000000000000000","sequence":1,"unexpected":true}"#,
    ] {
        assert!(
            serde_json::from_str::<ControlMsg>(invalid).is_err(),
            "accepted {invalid}"
        );
    }
    assert_eq!(CONTROL_PROTOCOL_HEADER, "x-tnl-control-protocol");
    assert_eq!(CONTROL_PROTOCOL_VERSION, "heartbeat-v1");
    assert_eq!(MAX_CONTROL_MESSAGE_BYTES, 1024 * 1024);
}

#[test]
fn closing_reason_and_lifecycle_error_are_typed_and_strict() {
    let closing = ControlMsg::Closing {
        reason: ClosingReason::ClientShutdown,
    };
    assert_eq!(
        serde_json::to_string(&closing).unwrap(),
        r#"{"type":"Closing","reason":"ClientShutdown"}"#
    );
    assert_eq!(
        serde_json::from_str::<ControlMsg>(r#"{"type":"Closing","reason":"RemoteStop"}"#).unwrap(),
        ControlMsg::Closing {
            reason: ClosingReason::RemoteStop,
        }
    );
    for invalid in [
        r#"{"type":"Closing","reason":"client_shutdown"}"#,
        r#"{"type":"Closing","reason":"Other"}"#,
        r#"{"type":"Closing","reason":"RemoteStop","unexpected":true}"#,
    ] {
        assert!(
            serde_json::from_str::<ControlMsg>(invalid).is_err(),
            "accepted {invalid}"
        );
    }

    let error = ControlMsg::Error {
        code: ErrorCode::TunnelAlreadyActive,
        message: "sanitized server diagnostic".into(),
    };
    assert_eq!(
        serde_json::to_string(&error).unwrap(),
        r#"{"type":"Error","code":"TunnelAlreadyActive","message":"sanitized server diagnostic"}"#
    );
}

#[test]
fn only_lost_tunnels_recreate_and_active_owners_retry() {
    assert_eq!(
        ErrorCode::TunnelLost.reattach_disposition(),
        ReattachErrorDisposition::Recreate
    );
    assert_eq!(
        ErrorCode::TunnelAlreadyActive.reattach_disposition(),
        ReattachErrorDisposition::Retry
    );
    for error in [
        ErrorCode::Unauthorized,
        ErrorCode::AccessPolicyMismatch,
        ErrorCode::UnsupportedProtocol,
        ErrorCode::SubdomainTaken,
    ] {
        assert_eq!(
            error.reattach_disposition(),
            ReattachErrorDisposition::Terminal
        );
    }
}
