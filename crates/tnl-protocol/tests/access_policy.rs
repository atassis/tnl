use tnl_protocol::access::{PolicyError, VisitorAccessPolicyV1, MAX_POLICY_BYTES};

const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY";

fn policy_fixture() -> String {
    format!(
        r#"{{"version":1,"basic":{{"username":"a\\\"b","password_hash":"{PHC}"}},"allow_ips":["192.0.2.99/24","::ffff:192.0.2.1/120","192.0.2.0/24","2001:db8::1"],"rate_limit":null,"max_concurrent":null}}"#
    )
}

#[test]
fn canonical_policy_bytes_and_fingerprint_are_pinned() {
    let policy = VisitorAccessPolicyV1::from_json_slice(policy_fixture().as_bytes()).unwrap();
    let canonical = format!(
        r#"{{"version":1,"basic":{{"username":"a\\\"b","password_hash":"{PHC}"}},"allow_ips":["192.0.2.0/24","2001:db8::1/128"],"rate_limit":null,"max_concurrent":null}}"#
    );
    assert_eq!(&*policy.canonical_json().unwrap(), canonical.as_bytes());
    // Independent SHA-256 test vector, computed outside the implementation.
    assert_eq!(
        policy.fingerprint().unwrap(),
        "4f585378482c912b568b24402f0d918225ff704d8cefdcbd85d4cfaeeaa8f468"
    );
    assert!(policy.allows_ip("::ffff:192.0.2.4".parse().unwrap()));
    assert!(!policy.allows_ip("192.0.3.1".parse().unwrap()));
    assert!(!format!("{policy:?}").contains(PHC));
    assert!(!format!("{policy:?}").contains("a\\\"b"));
}

#[test]
fn strict_policy_decode_rejects_missing_unknown_duplicate_and_empty_controls() {
    let public =
        r#"{"version":1,"basic":null,"allow_ips":[],"rate_limit":null,"max_concurrent":null}"#;
    assert!(VisitorAccessPolicyV1::from_json_slice(public.as_bytes()).is_err());
    let valid = public.replace("\"max_concurrent\":null", "\"max_concurrent\":1");
    assert!(VisitorAccessPolicyV1::from_json_slice(valid.as_bytes()).is_ok());
    for bad in [
        valid.replace("\"basic\":null,", ""),
        valid.replace("\"rate_limit\":null,", ""),
        valid.replace(
            "\"max_concurrent\":1",
            "\"max_concurrent\":1,\"extra_protection\":true",
        ),
        valid.replace("\"version\":1", "\"version\":1,\"version\":1"),
        valid.replace("\"version\":1", "\"version\":2"),
        valid.replace("\"max_concurrent\":1", "\"max_concurrent\":0"),
        valid.replace("\"max_concurrent\":1", "\"max_concurrent\":10001"),
        policy_fixture().replace("\"username\":", "\"extra\":1,\"username\":"),
        policy_fixture().replace("\"username\":", "\"username\":\"duplicate\",\"username\":"),
    ] {
        assert!(VisitorAccessPolicyV1::from_json_slice(bad.as_bytes()).is_err());
    }
    assert_eq!(
        VisitorAccessPolicyV1::from_json_slice(&vec![b' '; MAX_POLICY_BYTES + 1]).unwrap_err(),
        PolicyError::TooLarge
    );
}

#[test]
fn fixed_cost_verifiers_only_and_errors_are_redacted() {
    for hash in [
        PHC.replace("argon2id", "argon2i"),
        PHC.replace("v=19", "v=16"),
        PHC.replace("m=19456", "m=999999999"),
        PHC.replace("t=2", "t=3"),
        PHC.replace("p=1", "p=2"),
        PHC.replace("p=1", "p=1,keyid=YWJj"),
        PHC.replace("MDEyMzQ1Njc4OWFiY2RlZg$", "YWJj$"),
        PHC.replace("MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY", "YWJj"),
        "SECRET_SENTINEL".repeat(30),
    ] {
        let input = policy_fixture().replace(PHC, &hash);
        let err = VisitorAccessPolicyV1::from_json_slice(input.as_bytes()).unwrap_err();
        assert!(!format!("{err:?} {err}").contains(&hash));
    }
}

#[test]
fn independent_controls_and_canonical_address_families() {
    for controls in [
        r#""basic":null,"allow_ips":["127.0.0.1"],"rate_limit":null,"max_concurrent":null"#,
        r#""basic":null,"allow_ips":[],"rate_limit":{"requests_per_second":2,"burst":1},"max_concurrent":null"#,
        r#""basic":null,"allow_ips":[],"rate_limit":null,"max_concurrent":1"#,
    ] {
        let input = format!("{{\"version\":1,{controls}}}");
        assert!(VisitorAccessPolicyV1::from_json_slice(input.as_bytes()).is_ok());
    }
    let basic_only = policy_fixture().replace(
        "[\"192.0.2.99/24\",\"::ffff:192.0.2.1/120\",\"192.0.2.0/24\",\"2001:db8::1\"]",
        "[]",
    );
    let policy = VisitorAccessPolicyV1::from_json_slice(basic_only.as_bytes()).unwrap();
    assert!(policy.allows_ip("203.0.113.1".parse().unwrap()));
    for network in [
        "::ffff:192.0.2.1/95",
        "localhost",
        "192.0.2.1:80",
        "fe80::1%eth0",
        "",
    ] {
        let input = format!(
            r#"{{"version":1,"basic":null,"allow_ips":["{network}"],"rate_limit":null,"max_concurrent":null}}"#
        );
        assert!(VisitorAccessPolicyV1::from_json_slice(input.as_bytes()).is_err());
    }
    let many = vec!["127.0.0.1"; 129];
    let input = format!(
        r#"{{"version":1,"basic":null,"allow_ips":{},"rate_limit":null,"max_concurrent":null}}"#,
        serde_json::to_string(&many).unwrap()
    );
    assert!(VisitorAccessPolicyV1::from_json_slice(input.as_bytes()).is_err());
}

#[test]
fn null_fields_rate_field_order_and_second_digest_vector() {
    let canonical = r#"{"version":1,"basic":null,"allow_ips":[],"rate_limit":{"requests_per_second":2,"burst":3},"max_concurrent":null}"#;
    let policy = VisitorAccessPolicyV1::from_json_slice(canonical.as_bytes()).unwrap();
    assert_eq!(&*policy.canonical_json().unwrap(), canonical.as_bytes());
    assert_eq!(
        policy.fingerprint().unwrap(),
        "f90b74343b0035bc02854a6833755cec72aa6b0b99ca447727690d72284bc7c9"
    );
    let fingerprint = policy.fingerprint().unwrap();
    for changed in [
        canonical.replace("\"requests_per_second\":2", "\"requests_per_second\":3"),
        canonical.replace("\"burst\":3", "\"burst\":4"),
        canonical.replace("\"max_concurrent\":null", "\"max_concurrent\":1"),
        canonical.replace("\"allow_ips\":[]", "\"allow_ips\":[\"127.0.0.1\"]"),
    ] {
        assert_ne!(
            VisitorAccessPolicyV1::from_json_slice(changed.as_bytes())
                .unwrap()
                .fingerprint()
                .unwrap(),
            fingerprint
        );
    }
    let basic = VisitorAccessPolicyV1::from_json_slice(policy_fixture().as_bytes()).unwrap();
    for changed in [
        policy_fixture().replace("a\\\\\\\"b", "different-user"),
        policy_fixture().replace("$MDEyMzQ1Njc4OWFiY2RlZg$", "$NDEyMzQ1Njc4OWFiY2RlZg$"),
    ] {
        assert_ne!(
            VisitorAccessPolicyV1::from_json_slice(changed.as_bytes())
                .unwrap()
                .fingerprint()
                .unwrap(),
            basic.fingerprint().unwrap()
        );
    }
}

#[test]
fn nested_unknown_duplicate_missing_nullable_and_limit_errors_are_safe() {
    let valid = r#"{"version":1,"basic":null,"allow_ips":[],"rate_limit":{"requests_per_second":1,"burst":1},"max_concurrent":null}"#;
    for bad in [
        valid.replace("\"max_concurrent\":null", "\"SECRET_SENTINEL\":null"),
        valid.replace("\"allow_ips\":[],", ""),
        valid.replace("\"version\":1,", ""),
        valid.replace("\"burst\":1", "\"burst\":1,\"SECRET_SENTINEL\":true"),
        valid.replace("\"burst\":1", "\"burst\":1,\"burst\":1"),
        valid.replace("\"burst\":1", "\"burst\":null"),
        valid.replace("\"burst\":1", "\"burst\":10001"),
        valid.replace("\"burst\":1", "\"burst\":0"),
        valid.replace("\"requests_per_second\":1", "\"requests_per_second\":0"),
        valid.replace("\"requests_per_second\":1", "\"requests_per_second\":10001"),
        policy_fixture().replace("m=19456,t=2,p=1", "m=19456,t=2,p=1,m=19456"),
    ] {
        let err = serde_json::from_slice::<VisitorAccessPolicyV1>(bad.as_bytes()).unwrap_err();
        assert!(!err.to_string().contains("SECRET_SENTINEL"));
        assert!(!err.to_string().contains(PHC));
    }
}

#[test]
fn username_bounds_and_mapped_prefix_edges() {
    use tnl_protocol::access::{normalize_allow_ips, validate_username};
    for username in ["", "a:b", "a b", "a\nb", "é"] {
        assert!(validate_username(username).is_err());
    }
    assert!(validate_username(&"x".repeat(64)).is_ok());
    assert!(validate_username(&"x".repeat(65)).is_err());
    assert_eq!(
        normalize_allow_ips(&["::ffff:192.0.2.1/96".into()]).unwrap(),
        ["0.0.0.0/0"]
    );
    assert_eq!(
        normalize_allow_ips(&["::ffff:192.0.2.1/128".into()]).unwrap(),
        ["192.0.2.1/32"]
    );
    assert_eq!(
        normalize_allow_ips(&["::192.0.2.1".into()]).unwrap(),
        ["::c000:201/128"]
    );
}
