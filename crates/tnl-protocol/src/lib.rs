//! Shared wire types and transport traits used by `tnl` and `tnld`.

pub mod access;
pub mod control;
pub mod error_page;
pub mod messages;
pub mod request_metadata;
pub mod transport;
pub mod wordlists;

pub use messages::{
    ClosingReason, ControlMsg, CreateTunnelReq, ErrorCode, LogLine, ReattachReq, TunnelCreatedResp,
};
pub use transport::{
    client_session_from_ws, client_session_from_ws_generic, server_session_from_ws,
    server_session_from_ws_generic, OpenStreamError, Session, SessionCompletion,
    SessionTermination, Stream,
};
