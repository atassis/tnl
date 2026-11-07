use tnl_protocol::ControlMsg;

#[test]
fn protected_variants_have_strict_policy_only_shapes_and_redacted_debug() {
    let input = r#"{"type":"CreateProtectedTunnel","subdomain":null,"access":{"version":1,"basic":null,"allow_ips":[],"rate_limit":null,"max_concurrent":1}}"#;
    let msg: ControlMsg = serde_json::from_str(input).unwrap();
    assert_eq!(serde_json::to_string(&msg).unwrap(), input);
    let phc = "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY";
    let secret_input = input.replace(
        "\"basic\":null",
        &format!(r#""basic":{{"username":"USER_SENTINEL","password_hash":"{phc}"}}"#),
    );
    let secret_msg: ControlMsg = serde_json::from_str(&secret_input).unwrap();
    let debug = format!("{secret_msg:?}");
    assert!(!debug.contains(phc));
    assert!(!debug.contains("USER_SENTINEL"));
    for invalid in [
        input.replace(
            "\"access\":{",
            "\"data_protocol\":\"metadata-v1\",\"access\":{",
        ),
        input.replace(
            "\"subdomain\":null",
            "\"subdomain\":null,\"subdomain\":null",
        ),
        input.replace("\"access\":{", "\"access\":null,\"extra\":{"),
    ] {
        assert!(serde_json::from_str::<ControlMsg>(&invalid).is_err());
    }
    let missing_policy = r#"{"type":"CreateProtectedTunnel","subdomain":null}"#;
    assert!(serde_json::from_str::<ControlMsg>(missing_policy).is_err());
}

#[test]
fn protected_reattach_and_ack_require_exactly_shaped_nonnullable_fingerprints() {
    let fingerprint = "a".repeat(64);
    for input in [
        format!(
            r#"{{"type":"ReattachProtectedTunnel","tunnel_id":"00000000000000000000000000","subdomain":"demo","policy_fingerprint":"{fingerprint}"}}"#
        ),
        format!(
            r#"{{"type":"ProtectedTunnelCreated","tunnel_id":"00000000000000000000000000","hostname":"demo.t.x","subdomain":"demo","policy_fingerprint":"{fingerprint}"}}"#
        ),
    ] {
        assert!(serde_json::from_str::<ControlMsg>(&input).is_ok());
        for bad in [
            input.replace(&fingerprint, "short"),
            input.replace(&fingerprint, &"A".repeat(64)),
            input.replace(&format!("\"{fingerprint}\""), "null"),
            input.replace(",\"policy_fingerprint\":", ",\"unknown\":"),
            input.replace(
                "\"subdomain\":\"demo\"",
                "\"subdomain\":\"demo\",\"extra_protection\":true",
            ),
        ] {
            assert!(serde_json::from_str::<ControlMsg>(&bad).is_err());
        }
    }
}
