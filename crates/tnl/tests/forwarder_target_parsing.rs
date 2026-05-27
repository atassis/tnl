use std::str::FromStr;

use tnl::target::Target;

#[test]
fn port_only() {
    let t = Target::from_str("5173").expect("ok");
    assert!(matches!(t, Target::LocalhostPort(5173)));
}

#[test]
fn ipv4_host_port() {
    let t = Target::from_str("127.0.0.1:5173").expect("ok");
    match t {
        Target::Explicit(a) => {
            assert_eq!(a.to_string(), "127.0.0.1:5173");
        }
        Target::LocalhostPort(_) => panic!("expected explicit"),
    }
}

#[test]
fn ipv6_host_port() {
    let t = Target::from_str("[::1]:8080").expect("ok");
    match t {
        Target::Explicit(a) => assert_eq!(a.to_string(), "[::1]:8080"),
        Target::LocalhostPort(_) => panic!("expected explicit"),
    }
}

#[test]
fn lan_host_port() {
    let t = Target::from_str("192.168.1.50:80").expect("ok");
    match t {
        Target::Explicit(a) => assert_eq!(a.to_string(), "192.168.1.50:80"),
        Target::LocalhostPort(_) => panic!("expected explicit"),
    }
}

#[test]
fn invalid_inputs() {
    assert!(Target::from_str("foo:bar").is_err());
    assert!(Target::from_str("").is_err());
    assert!(Target::from_str("0").is_err()); // port 0 not allowed
    assert!(Target::from_str("hostname.local:8080").is_err()); // hostnames deferred to beta
    assert!(Target::from_str("127.0.0.1").is_err()); // host without port
}

#[test]
fn display_round_trips() {
    let cases = ["5173", "127.0.0.1:5173", "[::1]:8080", "192.168.1.50:80"];
    for s in cases {
        let t = Target::from_str(s).expect("ok");
        let again = Target::from_str(&t.to_string()).expect("ok");
        assert_eq!(t.to_string(), again.to_string(), "round-trip for {s}");
    }
}
