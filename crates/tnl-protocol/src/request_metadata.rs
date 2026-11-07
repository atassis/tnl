//! Authenticated control-upgrade negotiation and bounded data-substream prelude.
//! Public HTTP headers are never a source of authoritative request metadata.

use std::net::IpAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const DATA_STREAM_PROTOCOL_HEADER: &str = "x-tnl-data-stream-protocol";
pub const HTTP1_METADATA_V1: &str = "http1-metadata-v1";
pub const MAX_REQUEST_METADATA_BYTES: usize = 1024;
pub const REQUEST_METADATA_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DataStreamProtocol {
    #[default]
    RawHttp1,
    Http1MetadataV1,
}

impl DataStreamProtocol {
    /// Unsupported or ambiguous offers safely select legacy raw HTTP.
    pub fn from_offer(headers: &http::HeaderMap) -> Self {
        let mut values = headers.get_all(DATA_STREAM_PROTOCOL_HEADER).iter();
        match (values.next(), values.next()) {
            (Some(value), None) if value == HTTP1_METADATA_V1 => Self::Http1MetadataV1,
            _ => Self::RawHttp1,
        }
    }

    /// A missing echo is legacy; an invalid echo is not a safe format selection.
    pub fn from_echo(headers: &http::HeaderMap) -> Result<Self, RequestMetadataError> {
        if !headers.contains_key(DATA_STREAM_PROTOCOL_HEADER) {
            return Ok(Self::RawHttp1);
        }
        match Self::from_offer(headers) {
            Self::Http1MetadataV1 => Ok(Self::Http1MetadataV1),
            Self::RawHttp1 => Err(RequestMetadataError::Negotiation),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VisitorIpSource {
    Peer,
    XForwardedFor,
    PeerMissingForwarding,
    PeerInvalidForwarding,
    PeerAllTrusted,
}

impl VisitorIpSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Peer => "peer",
            Self::XForwardedFor => "x_forwarded_for",
            Self::PeerMissingForwarding => "peer_missing_forwarding",
            Self::PeerInvalidForwarding => "peer_invalid_forwarding",
            Self::PeerAllTrusted => "peer_all_trusted",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VisitorIdentity {
    pub visitor_ip: IpAddr,
    pub peer_ip: IpAddr,
    pub source: VisitorIpSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestMetadataV1 {
    /// The existing daemon handler request ID, not a second generated ID.
    pub request_id: ulid::Ulid,
    pub visitor: VisitorIdentity,
}

#[derive(Debug, thiserror::Error)]
pub enum RequestMetadataError {
    #[error("request metadata I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid request metadata length: {0}")]
    Length(usize),
    #[error("invalid request metadata JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid data stream protocol echo")]
    Negotiation,
}

pub async fn read_request_metadata<R: AsyncRead + Unpin + ?Sized>(
    reader: &mut R,
) -> Result<RequestMetadataV1, RequestMetadataError> {
    let n = reader.read_u32().await? as usize;
    validate_length(n)?;
    let mut bytes = vec![0; n];
    reader.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub async fn write_request_metadata<W: AsyncWrite + Unpin + ?Sized>(
    writer: &mut W,
    metadata: &RequestMetadataV1,
) -> Result<(), RequestMetadataError> {
    let bytes = serde_json::to_vec(metadata)?;
    validate_length(bytes.len())?;
    let n = u32::try_from(bytes.len()).map_err(|_| RequestMetadataError::Length(bytes.len()))?;
    writer.write_u32(n).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

fn validate_length(n: usize) -> Result<(), RequestMetadataError> {
    if (1..=MAX_REQUEST_METADATA_BYTES).contains(&n) {
        Ok(())
    } else {
        Err(RequestMetadataError::Length(n))
    }
}
