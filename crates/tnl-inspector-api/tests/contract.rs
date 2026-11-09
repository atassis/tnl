#[test]
fn every_response_and_mutation_has_an_independent_schema() {
    let schemas = tnl_inspector_api::schemas();
    for name in [
        "stats",
        "list",
        "detail",
        "availability",
        "preview",
        "outcome",
        "replay_error",
        "prepare_request",
        "confirm_request",
    ] {
        assert!(schemas.contains_key(name), "missing {name}");
    }
}

#[test]
fn response_nulls_are_required_but_requests_reject_unknown_keys() {
    let schemas = tnl_inspector_api::schemas();
    assert_eq!(schemas["stats"]["properties"]["schema_version"]["const"], 1);
    assert_eq!(
        schemas["stats"]["properties"]["records"]["maximum"],
        u64::MAX
    );
    let body = &schemas["detail"]["definitions"]["BodyPreview"];
    for field in ["text", "reason"] {
        assert!(body["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == field));
    }
    assert_eq!(schemas["confirm_request"]["additionalProperties"], false);
    assert!(serde_json::from_value::<tnl_inspector_api::PrepareRequest>(
        serde_json::json!({"schema_version":1,"extra":true})
    )
    .is_err());
    assert!(serde_json::from_value::<tnl_inspector_api::ConfirmRequest>(
        serde_json::json!({"schema_version":1,"replay_id":"invalid","confirmation_token":"private","confirm_side_effects":true})).is_err());
}

#[test]
fn confirmation_debug_never_contains_ticket() {
    let value = serde_json::json!({"schema_version":1,"replay_id":"00000000000000000000000000",
        "confirmation_token":"PRIVATE_TICKET_CANARY","confirm_side_effects":true});
    let request: tnl_inspector_api::ConfirmRequest = serde_json::from_value(value).unwrap();
    assert!(!format!("{request:?}").contains("PRIVATE_TICKET_CANARY"));
}
