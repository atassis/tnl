//! Shared wire types and transport traits used by `tnl` and `tnld`.

pub mod messages;
pub mod transport;
pub mod wordlists;

pub use messages::{
    ControlMsg, CreateTunnelReq, ErrorCode, LogLine, ReattachReq, TunnelCreatedResp,
};
pub use transport::{
    client_session_from_ws, client_session_from_ws_generic, server_session_from_ws,
    server_session_from_ws_generic, Session, Stream,
};
