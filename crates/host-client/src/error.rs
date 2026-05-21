//! Client-side host protocol errors.

use host_protocol::error::HostError;
use thiserror::Error;

pub type HostClientResult<T> = Result<T, HostClientError>;

#[derive(Debug, Error)]
pub enum HostClientError {
    #[error("failed to serialize JSON-RPC message: {0}")]
    Serialize(#[from] serde_json::Error),

    #[error("transport closed")]
    TransportClosed,

    #[error("transport error: {0}")]
    Transport(String),

    #[error("invalid JSON-RPC message: {0}")]
    InvalidMessage(String),

    #[error("host protocol error: {0:?}")]
    Host(HostError),
}
