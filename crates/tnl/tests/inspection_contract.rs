use serde_json::{json, Value};
use tnl::inspection::{Inspection, PreviewMode, TunnelGeneration};

#[test]
fn sanitized_record_wire_shape_is_independent_of_runtime_state() {
    let history = Inspection::new(PreviewMode::Off);
    let mut capture = history.admit(TunnelGeneration::new(), None).unwrap();
    capture.request(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
    capture.response(b"HTTP/1.1 204 No Content\r\n\r\n");
    capture.finish();
    let record = history.get(capture.id()).unwrap();
    let mut value = serde_json::to_value(record).unwrap();
    let object = value.as_object_mut().unwrap();
    for key in ["started_at_ms", "duration_ms", "capture_id", "generation"] {
        assert!(object.remove(key).is_some(), "missing volatile field {key}");
    }
    let empty_body = json!({"mode":"off","state":"disabled","retained_bytes":0,
        "redacted":false,"text":null,"reason":"disabled","observed_payload_bytes":0});
    assert_eq!(
        value,
        json!({
            "schema_version":1,"tunnel":"","tunnel_id":null,"origin":"external",
            "failure_kind":null,"display_target":"","resolved_addr":null,
            "response_source":"backend","request_boundary":"effective_backend",
            "bytes_to_backend":0,"bytes_from_backend":0,"bytes_to_client":0,
            "byte_count_semantics":"successful_local_writes_not_delivery",
            "omission_reasons":["disabled"],"visitor":{"available":false,"source":null,"display_ip":null},
            "request_id":null,"visitor_source":null,"sequence":1,"method":"GET",
            "request_target":"/","status":204,"state":"complete","connection":"untracked",
            "request":{"headers_state":"complete","headers":[{"name":"host","value":"localhost","redacted":false}],"body":empty_body},
            "response":{"headers_state":"complete","headers":[],"body":empty_body},
            "observation_incomplete":false,
            "timing":{"schema_version":1,"scope":"client_backend","availability":"unavailable","reason":"not_observed","snapshot":null}
        })
    );
    assert_eq!(value.get("started"), None::<&Value>);
}
