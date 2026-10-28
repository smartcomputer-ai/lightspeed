//! Client-side errors.

use environment_protocol::{
    error::{ProtocolError, ProtocolErrorKind},
    handshake::Plane,
    routes::DataRouteRefusal,
};
use thiserror::Error;

/// Why a connect or call failed.
///
/// A server's error answer is [`ClientError::Protocol`] with the server's
/// error kind intact; everything else is a fact about the connection or a
/// contract breach the client detected itself. The client never
/// synthesizes a protocol error kind.
#[derive(Clone, Debug, Error)]
pub enum ClientError {
    /// The connection settings cannot be used: an endpoint that is not
    /// `ws://`, `wss://` or `unix:///path`, or a header that is not valid
    /// HTTP. Retrying does not help.
    #[error("invalid connection settings for {endpoint}: {message}")]
    Configuration { endpoint: String, message: String },

    /// TCP or Unix connect, TLS or the WebSocket upgrade failed below HTTP.
    #[error("connect to {endpoint}: {message}")]
    Connect { endpoint: String, message: String },

    /// Connect, TLS and upgrade did not finish within
    /// [`ConnectOptions::connect_timeout`](crate::ConnectOptions::connect_timeout).
    #[error("connect to {endpoint} timed out")]
    ConnectTimeout { endpoint: String },

    /// The server answered the upgrade with an HTTP status instead of
    /// switching protocols: 401 for a missing or wrong credential, or a
    /// data route's 503, 410 or 409. `refusal` is the response body when it
    /// decodes as a [`DataRouteRefusal`], else the refusal the status alone
    /// names (410 `gone`, 409 `failed`, a 503 with `Retry-After` `booting`),
    /// so a body split from the headers by the transport still types the
    /// refusal; `retry_after_ms` is the
    /// `Retry-After` header in milliseconds, clamped to
    /// [`RETRY_AFTER_MAX_SECS`](environment_protocol::limits::RETRY_AFTER_MAX_SECS).
    #[error("upgrade refused with HTTP {status}{}", describe_refusal(refusal.as_ref()))]
    UpgradeRejected {
        status: u16,
        refusal: Option<DataRouteRefusal>,
        retry_after_ms: Option<u64>,
    },

    /// The server speaks another protocol version on this plane: it
    /// answered `initialize` with `versionMismatch`, or with a different
    /// `protocolVersion`. `local` is this client's version.
    #[error("{plane} protocol mismatch: this client speaks {local}, the server {remote}")]
    VersionMismatch {
        plane: Plane,
        local: u32,
        remote: u32,
    },

    /// The server's `initialize` response names another plane: the
    /// endpoint serves something else.
    #[error("expected the {expected} plane, the server serves {actual}")]
    WrongPlane { expected: Plane, actual: Plane },

    /// The server answered the call with this error.
    #[error("{0}")]
    Protocol(ProtocolError),

    /// The call's deadline passed before its response arrived. On the data
    /// plane an abortable call was also sent `$/cancelRequest`.
    #[error("deadline exceeded after {elapsed_ms} ms")]
    DeadlineExceeded { elapsed_ms: u64 },

    /// The connection is closed: by the server (with its close code and
    /// reason when it sent a close frame), by the transport ending, or by
    /// [`close`](crate::DataClient::close).
    #[error("connection closed{}", describe_close(*code, reason.as_deref()))]
    Closed {
        code: Option<u16>,
        reason: Option<String>,
    },

    /// The encoded request is larger than the protocol's message limit; it
    /// was not sent.
    #[error("request of {actual_bytes} bytes exceeds the {limit_bytes}-byte limit")]
    PayloadTooLarge { limit_bytes: u64, actual_bytes: u64 },

    /// The request could not be encoded.
    #[error("encode request: {0}")]
    Encode(String),

    /// The server's result does not decode as the method's response type:
    /// the server broke the contract.
    #[error("decode response: {0}")]
    Decode(String),

    /// The server sent something no client accepts on this plane: a
    /// request or notification, a binary frame, a frame over the message
    /// limit, or a frame that is not a valid response. The client closed
    /// the connection and failed every pending call with this error.
    #[error("invalid message from server: {0}")]
    InvalidMessage(String),

    /// The socket failed after the connection was established.
    #[error("transport: {0}")]
    Transport(String),
}

impl ClientError {
    /// The server's error kind: the kind of a [`Protocol`](Self::Protocol)
    /// error, and `versionMismatch` for
    /// [`VersionMismatch`](Self::VersionMismatch). `None` for failures the
    /// client observed itself.
    pub fn protocol_kind(&self) -> Option<ProtocolErrorKind> {
        match self {
            Self::Protocol(error) => Some(error.kind()),
            Self::VersionMismatch { .. } => Some(ProtocolErrorKind::VersionMismatch),
            _ => None,
        }
    }

    /// The server's error, for [`Protocol`](Self::Protocol).
    pub fn protocol_error(&self) -> Option<&ProtocolError> {
        match self {
            Self::Protocol(error) => Some(error),
            _ => None,
        }
    }

    /// The data route's typed refusal, for an
    /// [`UpgradeRejected`](Self::UpgradeRejected) whose body or status
    /// names one.
    pub fn data_route_refusal(&self) -> Option<&DataRouteRefusal> {
        match self {
            Self::UpgradeRejected { refusal, .. } => refusal.as_ref(),
            _ => None,
        }
    }

    /// The refused upgrade's `Retry-After`, in milliseconds.
    pub fn retry_after_ms(&self) -> Option<u64> {
        match self {
            Self::UpgradeRejected { retry_after_ms, .. } => *retry_after_ms,
            _ => None,
        }
    }

    /// True when the same call may succeed later without any change: a
    /// retryable protocol kind (`busy`, `unavailable`, `aborted`), a
    /// connection failure, a closed connection, a deadline, or an upgrade
    /// refused with 429 or a 5xx status. Version and plane mismatches,
    /// contract breaches and refusals are not.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Protocol(error) => error.is_retryable(),
            Self::Connect { .. }
            | Self::ConnectTimeout { .. }
            | Self::Closed { .. }
            | Self::Transport(_)
            | Self::DeadlineExceeded { .. } => true,
            Self::UpgradeRejected { status, .. } => *status == 429 || (500..600).contains(status),
            Self::Configuration { .. }
            | Self::VersionMismatch { .. }
            | Self::WrongPlane { .. }
            | Self::PayloadTooLarge { .. }
            | Self::Encode(_)
            | Self::Decode(_)
            | Self::InvalidMessage(_) => false,
        }
    }
}

fn describe_refusal(refusal: Option<&DataRouteRefusal>) -> String {
    refusal.map_or_else(String::new, |refusal| format!(": {refusal}"))
}

fn describe_close(code: Option<u16>, reason: Option<&str>) -> String {
    match (code, reason) {
        (Some(code), Some(reason)) if !reason.is_empty() => format!(" ({code}: {reason})"),
        (Some(code), _) => format!(" ({code})"),
        (None, _) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use environment_protocol::error::{AbortReason, ProtocolError};

    use super::*;

    #[test]
    fn protocol_errors_keep_their_kind_and_retryability() {
        let aborted = ClientError::Protocol(ProtocolError::aborted(
            Some(AbortReason::Cancelled),
            "cancelled",
        ));
        assert_eq!(aborted.protocol_kind(), Some(ProtocolErrorKind::Aborted));
        assert!(aborted.is_retryable());
        let missing = ClientError::Protocol(ProtocolError::not_found("no such process"));
        assert_eq!(missing.protocol_kind(), Some(ProtocolErrorKind::NotFound));
        assert!(!missing.is_retryable());
        assert_eq!(missing.to_string(), "notFound: no such process");
    }

    #[test]
    fn connection_failures_are_retryable_and_breaches_are_not() {
        assert!(
            ClientError::Closed {
                code: None,
                reason: None
            }
            .is_retryable()
        );
        assert!(ClientError::DeadlineExceeded { elapsed_ms: 10 }.is_retryable());
        assert!(rejected(503, None).is_retryable());
        assert!(rejected(429, None).is_retryable());
        assert!(!rejected(401, None).is_retryable());
        assert!(!rejected(410, Some(DataRouteRefusal::Gone {})).is_retryable());
        assert!(!ClientError::Decode("x".to_owned()).is_retryable());
        assert!(
            !ClientError::VersionMismatch {
                plane: Plane::Data,
                local: 4,
                remote: 3
            }
            .is_retryable()
        );
        assert_eq!(
            ClientError::VersionMismatch {
                plane: Plane::Controller,
                local: 4,
                remote: 5
            }
            .protocol_kind(),
            Some(ProtocolErrorKind::VersionMismatch)
        );
    }

    fn rejected(status: u16, refusal: Option<DataRouteRefusal>) -> ClientError {
        ClientError::UpgradeRejected {
            status,
            refusal,
            retry_after_ms: None,
        }
    }

    #[test]
    fn upgrade_refusals_display_their_state() {
        assert_eq!(
            rejected(401, None).to_string(),
            "upgrade refused with HTTP 401"
        );
        let waking = ClientError::UpgradeRejected {
            status: 503,
            refusal: Some(DataRouteRefusal::Waking {}),
            retry_after_ms: Some(3_000),
        };
        assert_eq!(waking.to_string(), "upgrade refused with HTTP 503: waking");
        assert_eq!(
            waking.data_route_refusal(),
            Some(&DataRouteRefusal::Waking {})
        );
        assert_eq!(waking.retry_after_ms(), Some(3_000));
        assert_eq!(rejected(401, None).data_route_refusal(), None);
    }

    #[test]
    fn close_display_names_code_and_reason() {
        assert_eq!(
            ClientError::Closed {
                code: Some(1008),
                reason: Some("route closed".to_owned())
            }
            .to_string(),
            "connection closed (1008: route closed)"
        );
        assert_eq!(
            ClientError::Closed {
                code: None,
                reason: None
            }
            .to_string(),
            "connection closed"
        );
    }
}
