use axum::http::{HeaderMap, HeaderValue};
use tnl_protocol::request_metadata::{VisitorIdentity, VisitorIpSource};
use tnld::visitor_ip::TrustedProxies;

fn trust(cidrs: &[&str]) -> TrustedProxies {
    TrustedProxies::from_cidrs(&cidrs.iter().map(ToString::to_string).collect::<Vec<_>>()).unwrap()
}
fn headers(value: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert("x-forwarded-for", value.parse().unwrap());
    h
}
fn check(
    trusted: &TrustedProxies,
    peer: &str,
    h: &HeaderMap,
    visitor: &str,
    source: VisitorIpSource,
) {
    assert_eq!(
        trusted.resolve(peer.parse().unwrap(), h),
        VisitorIdentity {
            visitor_ip: visitor.parse().unwrap(),
            peer_ip: peer.parse::<std::net::IpAddr>().unwrap().to_canonical(),
            source,
        }
    );
}

#[test]
fn visitor_ip_untrusted_peer_ignores_all_public_claims() {
    let mut h = headers("forged, invalid");
    h.insert("forwarded", "for=203.0.113.1".parse().unwrap());
    h.insert("x-real-ip", "203.0.113.1".parse().unwrap());
    check(
        &trust(&[]),
        "198.51.100.7",
        &h,
        "198.51.100.7",
        VisitorIpSource::Peer,
    );
    check(
        &trust(&[]),
        "127.0.0.1",
        &headers("198.51.100.7"),
        "127.0.0.1",
        VisitorIpSource::Peer,
    );
}

#[test]
fn visitor_ip_trusted_chain_stops_at_rightmost_untrusted_boundary() {
    let t = trust(&["127.0.0.1/32", "192.0.2.0/24", "2001:db8:1::/48"]);
    for value in [
        "198.51.100.7",
        "198.51.100.7, 192.0.2.9",
        "203.0.113.66, 198.51.100.7, 192.0.2.9",
        "\t198.51.100.7 \t, 192.0.2.9",
    ] {
        check(
            &t,
            "127.0.0.1",
            &headers(value),
            "198.51.100.7",
            VisitorIpSource::XForwardedFor,
        );
    }
    check(
        &t,
        "127.0.0.1",
        &HeaderMap::new(),
        "127.0.0.1",
        VisitorIpSource::PeerMissingForwarding,
    );
    check(
        &t,
        "127.0.0.1",
        &headers("192.0.2.9, 192.0.2.9"),
        "127.0.0.1",
        VisitorIpSource::PeerAllTrusted,
    );
}

#[test]
fn visitor_ip_invalid_complete_chain_falls_back_without_partial_attribution() {
    let t = trust(&["127.0.0.1/32"]);
    for value in [
        "",
        ",198.51.100.7",
        "198.51.100.7,",
        "unknown, 198.51.100.7",
        "_hidden",
        "\"198.51.100.7\"",
        "198.51.100.7:80",
        "[2001:db8::1]",
        "fe80::1%eth0",
        "198.51.100.7,,192.0.2.1",
    ] {
        check(
            &t,
            "127.0.0.1",
            &headers(value),
            "127.0.0.1",
            VisitorIpSource::PeerInvalidForwarding,
        );
    }
    let mut h = headers("198.51.100.7");
    h.append("X-Forwarded-For", "198.51.100.7".parse().unwrap());
    check(
        &t,
        "127.0.0.1",
        &h,
        "127.0.0.1",
        VisitorIpSource::PeerInvalidForwarding,
    );
    h = HeaderMap::new();
    h.insert("x-forwarded-for", HeaderValue::from_bytes(&[0xff]).unwrap());
    check(
        &t,
        "127.0.0.1",
        &h,
        "127.0.0.1",
        VisitorIpSource::PeerInvalidForwarding,
    );
}

#[test]
fn visitor_ip_exact_byte_and_token_limits() {
    let t = trust(&["127.0.0.1/32"]);
    let value = format!("198.51.100.7{}", " ".repeat(4096 - "198.51.100.7".len()));
    check(
        &t,
        "127.0.0.1",
        &headers(&value),
        "198.51.100.7",
        VisitorIpSource::XForwardedFor,
    );
    check(
        &t,
        "127.0.0.1",
        &headers(&(value + " ")),
        "127.0.0.1",
        VisitorIpSource::PeerInvalidForwarding,
    );
    check(
        &t,
        "127.0.0.1",
        &headers(&["198.51.100.7"; 32].join(",")),
        "198.51.100.7",
        VisitorIpSource::XForwardedFor,
    );
    check(
        &t,
        "127.0.0.1",
        &headers(&["198.51.100.7"; 33].join(",")),
        "127.0.0.1",
        VisitorIpSource::PeerInvalidForwarding,
    );
}

#[test]
fn visitor_ip_ipv6_mapped_and_header_family_policy() {
    let t = trust(&["127.0.0.1/32", "2001:db8:1::/48"]);
    check(
        &t,
        "2001:db8:1::5",
        &headers("2001:db8::7, 2001:db8:1::9"),
        "2001:db8::7",
        VisitorIpSource::XForwardedFor,
    );
    check(
        &t,
        "::ffff:127.0.0.1",
        &headers("::ffff:198.51.100.7"),
        "198.51.100.7",
        VisitorIpSource::XForwardedFor,
    );
    check(
        &trust(&["::/0"]),
        "::ffff:127.0.0.1",
        &headers("198.51.100.7"),
        "127.0.0.1",
        VisitorIpSource::Peer,
    );
    let mut h = headers("fd00::7");
    h.insert("forwarded", "for=203.0.113.66".parse().unwrap());
    h.insert("x-real-ip", "203.0.113.66".parse().unwrap());
    check(
        &t,
        "127.0.0.1",
        &h,
        "fd00::7",
        VisitorIpSource::XForwardedFor,
    );
    h.remove("x-forwarded-for");
    check(
        &t,
        "127.0.0.1",
        &h,
        "127.0.0.1",
        VisitorIpSource::PeerMissingForwarding,
    );
    check(
        &t,
        "127.0.0.1",
        &headers("10.0.0.7"),
        "10.0.0.7",
        VisitorIpSource::XForwardedFor,
    );
}

#[test]
fn visitor_ip_cidrs_are_explicit_and_canonical() {
    for value in [
        "127.0.0.1",
        "192.0.2.7/24",
        "localhost/32",
        "::1/129",
        "fe80::1%lo/128",
    ] {
        assert!(
            TrustedProxies::from_cidrs(&[value.into()]).is_err(),
            "{value}"
        );
    }
    assert!(TrustedProxies::from_cidrs(&["0.0.0.0/0".into(), "::/0".into()]).is_ok());
}

#[tokio::test]
async fn visitor_ip_programmatic_config_rejects_bad_cidr_before_bind() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let cfg = tnld::config::Config {
        listen: "invalid-listen".into(),
        public_url: "http://x".into(),
        hostname_root: "t.x".into(),
        tokens_file: file.path().to_string_lossy().into_owned(),
        session_grace_sec: 30,
        trusted_proxies: vec!["192.0.2.7/24".into()],
    };
    let error = tnld::serve::spawn_server_with_state(cfg).await.unwrap_err();
    assert!(error.to_string().contains("trusted_proxies"), "{error:#}");
}
