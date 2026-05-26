use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum ControlMsg {
    CreateTunnel(CreateTunnelReq),
    TunnelCreated(TunnelCreatedResp),
    Heartbeat,
    HeartbeatAck,
    Close,
    Closing { reason: String },
    Error { code: ErrorCode, message: String },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CreateTunnelReq {
    pub subdomain: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TunnelCreatedResp {
    pub tunnel_id: String,
    pub hostname: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidSubdomain,
    SubdomainTaken,
    TunnelNotFound,
    Unauthorized,
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_msg_roundtrip_create_tunnel() {
        let msg = ControlMsg::CreateTunnel(CreateTunnelReq {
            subdomain: "foo".into(),
        });
        let s = serde_json::to_string(&msg).unwrap();
        let back: ControlMsg = serde_json::from_str(&s).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn control_msg_roundtrip_tunnel_created() {
        let msg = ControlMsg::TunnelCreated(TunnelCreatedResp {
            tunnel_id: "01JCMR5XYZ".into(),
            hostname: "foo.t.example.com".into(),
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
        for msg in [ControlMsg::Heartbeat, ControlMsg::HeartbeatAck, ControlMsg::Close] {
            let s = serde_json::to_string(&msg).unwrap();
            let back: ControlMsg = serde_json::from_str(&s).unwrap();
            assert_eq!(msg, back);
        }
    }
}
