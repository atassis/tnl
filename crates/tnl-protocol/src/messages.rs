use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::time::SystemTime;

/// Wire format: serde's internally-tagged enum representation (`tag = "type"`).
///
/// Newtype-struct payloads (`CreateTunnel(CreateTunnelReq)`) flatten their
/// fields next to `"type"`. Do not add tuple-newtype variants over primitives
/// — `serde_json` cannot flatten them and will panic at serialize time.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum ControlMsg {
    CreateTunnel(CreateTunnelReq),
    ReattachTunnel(ReattachReq),
    TunnelCreated(TunnelCreatedResp),
    Heartbeat,
    HeartbeatAck,
    Close,
    Closing { reason: String },
    Error { code: ErrorCode, message: String },
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct CreateTunnelReq {
    /// `None` means "server picks a random subdomain". Alpha clients always sent a String;
    /// `#[serde(default)]` keeps the wire backward-compatible.
    #[serde(default)]
    pub subdomain: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TunnelCreatedResp {
    pub tunnel_id: ulid::Ulid,
    pub hostname: String,
    /// Echoes the subdomain actually assigned (may be server-generated).
    pub subdomain: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidSubdomain,
    SubdomainTaken,
    TunnelNotFound,
    TunnelLost,
    Unauthorized,
    Internal,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LogLine {
    pub timestamp_unix_ms: u64,
    pub method: String,
    pub path: String,
    pub status: Option<u16>,
    pub duration_ms: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub remote_ip: Option<IpAddr>,
}

impl LogLine {
    /// Current wall-clock time as UNIX milliseconds. Saturates to 0 if the
    /// system clock is set before `UNIX_EPOCH`.
    #[allow(clippy::cast_possible_truncation)]
    pub fn now_unix_ms() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ReattachReq {
    pub tunnel_id: ulid::Ulid,
    pub subdomain: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PairCreateReq {
    pub name: String,
    /// Requested lifetime in seconds. Servers SHOULD clamp this to [60, 900].
    pub expires_in_sec: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PairCreateResp {
    /// Human-formatted: e.g. "AB-12-CD".
    pub code: String,
    /// UNIX timestamp (seconds since epoch) at which this code expires.
    pub expires_at_unix: u64,
    /// Shareable URL embedding endpoint + code. UX device, not a server route.
    pub invite_url: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PairRedeemReq {
    /// Caller-normalised before construction: uppercase, dashes and spaces stripped, e.g. "AB12CD".
    pub code: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PairRedeemResp {
    pub token: String,
    pub endpoint: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TunnelInfo {
    pub subdomain: String,
    pub hostname: String,
    pub owner_token: String,
    /// UNIX timestamp (seconds since epoch) when this tunnel was created.
    pub created_at_unix: u64,
    pub requests: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    /// True if the daemon currently has a live control session for this tunnel.
    pub active: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_msg_roundtrip_create_tunnel() {
        let msg = ControlMsg::CreateTunnel(CreateTunnelReq {
            subdomain: Some("foo".into()),
        });
        let s = serde_json::to_string(&msg).unwrap();
        let back: ControlMsg = serde_json::from_str(&s).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn control_msg_roundtrip_tunnel_created() {
        let msg = ControlMsg::TunnelCreated(TunnelCreatedResp {
            tunnel_id: ulid::Ulid::nil(),
            hostname: "foo.t.example.com".into(),
            subdomain: "foo".into(),
        });
        let s = serde_json::to_string(&msg).unwrap();
        assert!(s.contains(r#""type":"TunnelCreated""#));
        let back: ControlMsg = serde_json::from_str(&s).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn control_msg_roundtrip_error() {
        let msg = ControlMsg::Error {
            code: ErrorCode::SubdomainTaken,
            message: "already taken".into(),
        };
        let back: ControlMsg = serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn control_msg_roundtrip_simple_variants() {
        for msg in [
            ControlMsg::Heartbeat,
            ControlMsg::HeartbeatAck,
            ControlMsg::Close,
        ] {
            let s = serde_json::to_string(&msg).unwrap();
            let back: ControlMsg = serde_json::from_str(&s).unwrap();
            assert_eq!(msg, back);
        }
    }

    #[test]
    fn log_line_roundtrip() {
        let log = LogLine {
            timestamp_unix_ms: 1_700_000_000_000,
            method: "GET".into(),
            path: "/api/me".into(),
            status: Some(200),
            duration_ms: 23,
            bytes_in: 320,
            bytes_out: 1024,
            remote_ip: Some("1.2.3.4".parse().unwrap()),
        };
        let s = serde_json::to_string(&log).unwrap();
        let back: LogLine = serde_json::from_str(&s).unwrap();
        assert_eq!(log, back);
    }

    #[test]
    fn control_msg_roundtrip_closing() {
        let msg = ControlMsg::Closing {
            reason: "server restart".into(),
        };
        let back: ControlMsg = serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn control_msg_wire_format_pinned() {
        // Pin the on-wire tag so an accidental switch to `untagged` or
        // `rename_all = "snake_case"` is caught by tests.
        let s = serde_json::to_string(&ControlMsg::Heartbeat).unwrap();
        assert_eq!(s, r#"{"type":"Heartbeat"}"#);
    }

    #[test]
    fn pair_create_req_round_trip_json() {
        let req = PairCreateReq {
            name: "laptop".to_string(),
            expires_in_sec: 300,
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: PairCreateReq = serde_json::from_str(&s).unwrap();
        assert_eq!(back.name, "laptop");
        assert_eq!(back.expires_in_sec, 300);
    }

    #[test]
    fn pair_create_resp_round_trip_json() {
        let resp = PairCreateResp {
            code: "AB-12-CD".to_string(),
            expires_at_unix: 1_900_000_000,
            invite_url: "https://x.example.com/invite/AB-12-CD".to_string(),
        };
        let s = serde_json::to_string(&resp).unwrap();
        let back: PairCreateResp = serde_json::from_str(&s).unwrap();
        assert_eq!(back.code, "AB-12-CD");
        assert_eq!(back.expires_at_unix, 1_900_000_000);
        assert_eq!(back.invite_url, resp.invite_url);
    }

    #[test]
    fn pair_redeem_req_round_trip_json() {
        let req = PairRedeemReq {
            code: "AB12CD".to_string(),
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: PairRedeemReq = serde_json::from_str(&s).unwrap();
        assert_eq!(back.code, "AB12CD");
    }

    #[test]
    fn pair_redeem_resp_round_trip_json() {
        let resp = PairRedeemResp {
            token: "tnl_abc".to_string(),
            endpoint: "https://x.example.com".to_string(),
            name: "laptop".to_string(),
        };
        let s = serde_json::to_string(&resp).unwrap();
        let back: PairRedeemResp = serde_json::from_str(&s).unwrap();
        assert_eq!(back.token, "tnl_abc");
    }

    #[test]
    fn reattach_req_serialises_under_control_msg_tag() {
        let msg = ControlMsg::ReattachTunnel(ReattachReq {
            tunnel_id: ulid::Ulid::nil(),
            subdomain: "demo".to_string(),
        });
        let s = serde_json::to_string(&msg).unwrap();
        assert!(s.contains("\"type\":\"ReattachTunnel\""), "got: {s}");
        let back: ControlMsg = serde_json::from_str(&s).unwrap();
        match back {
            ControlMsg::ReattachTunnel(r) => {
                assert_eq!(r.subdomain, "demo");
                assert_eq!(r.tunnel_id, ulid::Ulid::nil());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn create_tunnel_req_accepts_omitted_subdomain() {
        // Alpha-style: subdomain always present.
        let alpha = r#"{"subdomain":"demo"}"#;
        let v: CreateTunnelReq = serde_json::from_str(alpha).unwrap();
        assert_eq!(v.subdomain, Some("demo".to_string()));

        // Beta-style: subdomain omitted entirely.
        let beta = "{}";
        let v: CreateTunnelReq = serde_json::from_str(beta).unwrap();
        assert_eq!(v.subdomain, None);

        // Beta-style: subdomain explicitly null.
        let null_form = r#"{"subdomain":null}"#;
        let v: CreateTunnelReq = serde_json::from_str(null_form).unwrap();
        assert_eq!(v.subdomain, None);
    }

    #[test]
    fn tunnel_created_resp_echoes_subdomain() {
        let resp = TunnelCreatedResp {
            hostname: "happy-otter-12.t.example.com".to_string(),
            subdomain: "happy-otter-12".to_string(),
            tunnel_id: ulid::Ulid::nil(),
        };
        let s = serde_json::to_string(&resp).unwrap();
        let back: TunnelCreatedResp = serde_json::from_str(&s).unwrap();
        assert_eq!(back.subdomain, "happy-otter-12");
    }

    #[test]
    fn tunnel_info_round_trip_json() {
        let info = TunnelInfo {
            subdomain: "foo".into(),
            hostname: "foo.t.example.com".into(),
            owner_token: "laptop".into(),
            created_at_unix: 1_700_000_000,
            requests: 5,
            bytes_in: 1024,
            bytes_out: 2048,
            active: true,
        };
        let s = serde_json::to_string(&info).unwrap();
        let back: TunnelInfo = serde_json::from_str(&s).unwrap();
        assert_eq!(info, back);
    }
}
