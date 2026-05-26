//! Shared wire types and transport traits used by `tnl` and `tnld`.

pub mod messages;
pub use messages::{ControlMsg, CreateTunnelReq, ErrorCode, LogLine, TunnelCreatedResp};
