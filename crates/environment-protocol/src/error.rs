//! The one error vocabulary of every plane.
//!
//! An error travels as a JSON-RPC error object whose `code` is a fixed
//! function of its kind and whose `data` carries the kind and its typed
//! fields:
//!
//! ```json
//! {"code": -32004, "message": "unknown process id: p1", "data": {"kind": "notFound", "reason": "expired"}}
//! ```
//!
//! `data.kind` is authoritative: receivers branch on it, never on the code
//! or the message. Retryability is a property of the kind
//! ([`ProtocolErrorKind::is_retryable`]) and is never sent. The same error
//! object is embedded in results that report per-entry failures (`job/read`
//! entries, `fs/scan` roots, a failed transfer's status).

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::limits::MAX_ERROR_MESSAGE_BYTES;

/// Closed set of error kinds. Each kind has one meaning and one code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ProtocolErrorKind {
    /// The frame is not JSON. Answered with a null id.
    ParseError,
    /// The frame is JSON but not a valid request: wrong `jsonrpc`, a batch,
    /// a missing or malformed member. A second `initialize` is also
    /// `invalidRequest`.
    InvalidRequest,
    /// No method of that name exists on this plane.
    MethodNotFound,
    /// The params do not decode, or a value is out of range or above an
    /// advertised ceiling.
    InvalidParams,
    /// A method other than `initialize` arrived before a successful
    /// `initialize`.
    NotInitialized,
    /// The peer stated another protocol version.
    VersionMismatch,
    /// The named handle, path or target does not exist.
    NotFound,
    /// The thing to create already exists.
    AlreadyExists,
    /// The request is well-formed but the current state forbids it; `reason`
    /// says which state.
    FailedPrecondition,
    /// The caller may not do this: confinement, credentials, signatures.
    PermissionDenied,
    /// The endpoint lacks the capability the request needs; an endpoint
    /// never silently ignores a parameter it cannot honour.
    CapabilityUnavailable,
    /// A limit on concurrent work was reached; retry later.
    Busy,
    /// A server-side duration bound ran out.
    DeadlineExceeded,
    /// A credential or token is past its validity.
    Expired,
    /// The operation stopped before completing: cancelled, or its input
    /// changed underneath it. Retrying from the start may succeed.
    Aborted,
    /// A message, payload or result is larger than allowed.
    PayloadTooLarge,
    /// A process could not be started or supervised.
    ProcessFailed,
    /// A dependency of the server is temporarily unreachable.
    Unavailable,
    /// A defect or unexpected failure inside the server.
    Internal,
}

impl ProtocolErrorKind {
    /// The JSON-RPC `code` of this kind. Framing kinds use the reserved
    /// codes; domain kinds reuse the public API's numbers where the meaning
    /// matches (`permissionDenied`, `notFound`, `alreadyExists`,
    /// `payloadTooLarge`).
    pub fn code(self) -> i32 {
        match self {
            Self::ParseError => -32700,
            Self::InvalidRequest => -32600,
            Self::MethodNotFound => -32601,
            Self::InvalidParams => -32602,
            Self::Internal => -32603,
            Self::NotInitialized => -32002,
            Self::PermissionDenied => -32003,
            Self::NotFound => -32004,
            Self::VersionMismatch => -32005,
            Self::CapabilityUnavailable => -32006,
            Self::FailedPrecondition => -32007,
            Self::Busy => -32008,
            Self::AlreadyExists => -32009,
            Self::PayloadTooLarge => -32013,
            Self::DeadlineExceeded => -32015,
            Self::Expired => -32016,
            Self::Aborted => -32017,
            Self::ProcessFailed => -32018,
            Self::Unavailable => -32019,
        }
    }

    /// The kind a code stands for, for error objects that carry no `data`
    /// (a non-conforming peer or an intermediary).
    pub fn from_code(code: i64) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| i64::from(kind.code()) == code)
    }

    /// True when retrying the same request later can succeed without any
    /// change: `busy`, `unavailable` and `aborted`.
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::Busy | Self::Unavailable | Self::Aborted)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ParseError => "parseError",
            Self::InvalidRequest => "invalidRequest",
            Self::MethodNotFound => "methodNotFound",
            Self::InvalidParams => "invalidParams",
            Self::NotInitialized => "notInitialized",
            Self::VersionMismatch => "versionMismatch",
            Self::NotFound => "notFound",
            Self::AlreadyExists => "alreadyExists",
            Self::FailedPrecondition => "failedPrecondition",
            Self::PermissionDenied => "permissionDenied",
            Self::CapabilityUnavailable => "capabilityUnavailable",
            Self::Busy => "busy",
            Self::DeadlineExceeded => "deadlineExceeded",
            Self::Expired => "expired",
            Self::Aborted => "aborted",
            Self::PayloadTooLarge => "payloadTooLarge",
            Self::ProcessFailed => "processFailed",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }

    pub const ALL: [Self; 19] = [
        Self::ParseError,
        Self::InvalidRequest,
        Self::MethodNotFound,
        Self::InvalidParams,
        Self::NotInitialized,
        Self::VersionMismatch,
        Self::NotFound,
        Self::AlreadyExists,
        Self::FailedPrecondition,
        Self::PermissionDenied,
        Self::CapabilityUnavailable,
        Self::Busy,
        Self::DeadlineExceeded,
        Self::Expired,
        Self::Aborted,
        Self::PayloadTooLarge,
        Self::ProcessFailed,
        Self::Unavailable,
        Self::Internal,
    ];
}

impl fmt::Display for ProtocolErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why a handle, path or identity was not found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum NotFoundReason {
    /// Its retention window ended; a tombstone remembers it.
    Expired,
    /// It belonged to an earlier daemon instance (see `daemonInstanceId`).
    DaemonRestarted,
    /// A daemon identity the gateway has never admitted presented no
    /// registration key.
    UnknownDaemon,
}

/// The state that forbids a `failedPrecondition` request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum PreconditionReason {
    /// The process has no open input: started without `pipe` or `pty`,
    /// input already closed, or exited.
    InputClosed,
    /// The file's current `version` differs from `ifMatch`.
    IfMatchFailed,
    /// `create: ifAbsent` met an existing file, or `create: ifPresent` a
    /// missing one.
    CreateCondition,
    /// The directory is not empty and `recursive` was not set.
    NotEmpty,
    /// The path names a directory where a file is required.
    IsDirectory,
    /// A path component or the path names a non-directory where a directory
    /// is required.
    NotDirectory,
    /// The path names a device, FIFO, socket or other non-regular file.
    NotRegularFile,
    /// A symlink chain loops.
    SymlinkLoop,
    /// A capture met a symlink that leaves the selection, or one the
    /// selection's symlink policy rejects.
    SymlinkRejected,
    /// The filesystem root itself cannot be replaced or removed.
    FilesystemRoot,
    /// Rename across filesystems.
    CrossDevice,
    /// No space left on the device.
    NoSpace,
    /// A job id is in use by a job with another `requestId` or spec; choose
    /// a new job id.
    JobIdInUse,
    /// A transfer `operationId` is in use for different input.
    OperationIdInUse,
    /// The transfer action is not valid in the operation's current phase.
    TransferPhase,
    /// A transfer write's `offset` is not the digest's next expected offset.
    TransferOffset,
    /// Uploaded bytes do not hash to their digest.
    DigestMismatch,
    /// The environment is closed; its identity or target is spent.
    EnvironmentClosed,
    /// The daemon identity is bound to another universe.
    IdentityInUse,
    /// The target belongs to another owner.
    OwnerMismatch,
    /// The template is deprecated and creates no new targets.
    TemplateDeprecated,
    /// `ingress/ensure` on a target whose template has no public ingress.
    IngressUnsupported,
    /// A power change on a target whose `phase` is not `active`
    /// (provisioning, closing, closed or failed).
    TargetNotActive,
}

/// Why a `permissionDenied` request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum PermissionReason {
    /// The path, or a symlink it passes through, resolves outside the
    /// daemon's filesystem root.
    OutsideRoot,
    /// The path names or contains the daemon's state directory.
    DaemonState,
    /// The operating system refused access.
    FilePermission,
    /// The registration signature does not verify.
    InvalidSignature,
    /// The registration key is unknown.
    InvalidRegistrationKey,
    /// The registration key was revoked.
    RegistrationKeyRevoked,
}

/// What limit a `busy` request hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum BusyReason {
    /// The connection's concurrent request limit.
    RequestLimit,
    /// The daemon's limit on active transfer operations.
    TransferCapacity,
    /// A rate limit on the caller's identity.
    RateLimited,
    /// A registration key's limit on active environments.
    CapacityExhausted,
}

/// Why an operation was `aborted`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum AbortReason {
    /// `$/cancelRequest` named the request.
    Cancelled,
    /// A file or directory changed while it was being read.
    SourceChanged,
}

/// A capability a request needed and the endpoint lacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum Capability {
    /// The `filesystem` family.
    Filesystem,
    /// `capabilities.filesystem.write`: the filesystem is writable.
    FilesystemWrite,
    /// The `process` family.
    Process,
    /// `capabilities.process.pty`.
    ProcessPty,
    /// The `job` family.
    Job,
    /// The controller's `power` capability.
    Power,
    /// The controller's `ingress` capability.
    Ingress,
}

/// The `data` member of an error object: the kind and its typed fields.
///
/// Kinds without fields are empty struct variants rather than unit
/// variants: serde does not apply `deny_unknown_fields` to the unit
/// variants of an internally tagged enum, and every kind must refuse
/// unknown members.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ErrorData {
    ParseError {},
    InvalidRequest {},
    MethodNotFound {},
    InvalidParams {
        /// Wire path of the offending field, such as `limits.maxDurationMs`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
        /// The advertised ceiling the value exceeded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ceiling: Option<u64>,
    },
    NotInitialized {},
    VersionMismatch {
        /// The version of the peer that sent this error.
        expected: u32,
        /// The version the requester stated.
        received: u32,
    },
    NotFound {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<NotFoundReason>,
    },
    AlreadyExists {},
    FailedPrecondition {
        reason: PreconditionReason,
    },
    PermissionDenied {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<PermissionReason>,
    },
    CapabilityUnavailable {
        capability: Capability,
    },
    Busy {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<BusyReason>,
        /// Earliest useful retry, when the server knows it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_ms: Option<u64>,
    },
    DeadlineExceeded {},
    Expired {},
    Aborted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<AbortReason>,
    },
    PayloadTooLarge {
        /// The byte limit the payload exceeded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit_bytes: Option<u64>,
        /// The entry-count limit the result exceeded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit_entries: Option<u32>,
    },
    ProcessFailed {},
    Unavailable {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_ms: Option<u64>,
    },
    Internal {},
}

impl ErrorData {
    pub fn kind(&self) -> ProtocolErrorKind {
        match self {
            Self::ParseError {} => ProtocolErrorKind::ParseError,
            Self::InvalidRequest {} => ProtocolErrorKind::InvalidRequest,
            Self::MethodNotFound {} => ProtocolErrorKind::MethodNotFound,
            Self::InvalidParams { .. } => ProtocolErrorKind::InvalidParams,
            Self::NotInitialized {} => ProtocolErrorKind::NotInitialized,
            Self::VersionMismatch { .. } => ProtocolErrorKind::VersionMismatch,
            Self::NotFound { .. } => ProtocolErrorKind::NotFound,
            Self::AlreadyExists {} => ProtocolErrorKind::AlreadyExists,
            Self::FailedPrecondition { .. } => ProtocolErrorKind::FailedPrecondition,
            Self::PermissionDenied { .. } => ProtocolErrorKind::PermissionDenied,
            Self::CapabilityUnavailable { .. } => ProtocolErrorKind::CapabilityUnavailable,
            Self::Busy { .. } => ProtocolErrorKind::Busy,
            Self::DeadlineExceeded {} => ProtocolErrorKind::DeadlineExceeded,
            Self::Expired {} => ProtocolErrorKind::Expired,
            Self::Aborted { .. } => ProtocolErrorKind::Aborted,
            Self::PayloadTooLarge { .. } => ProtocolErrorKind::PayloadTooLarge,
            Self::ProcessFailed {} => ProtocolErrorKind::ProcessFailed,
            Self::Unavailable { .. } => ProtocolErrorKind::Unavailable,
            Self::Internal {} => ProtocolErrorKind::Internal,
        }
    }

    /// The fieldless data of a kind, for kinds whose fields are all
    /// optional. Kinds with required fields get their typed constructor on
    /// [`ProtocolError`] instead.
    fn bare(kind: ProtocolErrorKind) -> Option<Self> {
        Some(match kind {
            ProtocolErrorKind::ParseError => Self::ParseError {},
            ProtocolErrorKind::InvalidRequest => Self::InvalidRequest {},
            ProtocolErrorKind::MethodNotFound => Self::MethodNotFound {},
            ProtocolErrorKind::InvalidParams => Self::InvalidParams {
                field: None,
                ceiling: None,
            },
            ProtocolErrorKind::NotInitialized => Self::NotInitialized {},
            ProtocolErrorKind::NotFound => Self::NotFound { reason: None },
            ProtocolErrorKind::AlreadyExists => Self::AlreadyExists {},
            ProtocolErrorKind::PermissionDenied => Self::PermissionDenied { reason: None },
            ProtocolErrorKind::Busy => Self::Busy {
                reason: None,
                retry_after_ms: None,
            },
            ProtocolErrorKind::DeadlineExceeded => Self::DeadlineExceeded {},
            ProtocolErrorKind::Expired => Self::Expired {},
            ProtocolErrorKind::Aborted => Self::Aborted { reason: None },
            ProtocolErrorKind::ProcessFailed => Self::ProcessFailed {},
            ProtocolErrorKind::Unavailable => Self::Unavailable {
                retry_after_ms: None,
            },
            ProtocolErrorKind::Internal => Self::Internal {},
            ProtocolErrorKind::PayloadTooLarge => Self::PayloadTooLarge {
                limit_bytes: None,
                limit_entries: None,
            },
            ProtocolErrorKind::VersionMismatch
            | ProtocolErrorKind::FailedPrecondition
            | ProtocolErrorKind::CapabilityUnavailable => return None,
        })
    }
}

/// A JSON-RPC error object as it appears on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ErrorObject {
    /// Fixed by `data.kind`; see [`ProtocolErrorKind::code`].
    pub code: i32,
    /// Human-readable description. Never parsed.
    pub message: String,
    pub data: ErrorData,
}

/// A protocol error: one kind, its typed data and a message.
///
/// Serializes as an [`ErrorObject`]. Displays as `{kind}: {message}`.
#[derive(Clone, Debug, PartialEq, Eq, Error, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(with = "ErrorObject"))]
#[serde(from = "ErrorObject", into = "ErrorObject")]
#[error("{}: {message}", self.kind())]
pub struct ProtocolError {
    pub data: ErrorData,
    pub message: String,
}

impl From<ErrorObject> for ProtocolError {
    /// `data.kind` is authoritative; `code` is not consulted.
    fn from(object: ErrorObject) -> Self {
        Self {
            data: object.data,
            message: object.message,
        }
    }
}

impl From<ProtocolError> for ErrorObject {
    fn from(error: ProtocolError) -> Self {
        Self {
            code: error.code(),
            message: error.message,
            data: error.data,
        }
    }
}

impl ProtocolError {
    /// An error of this kind. A message longer than
    /// [`MAX_ERROR_MESSAGE_BYTES`] is cut on a character boundary, so errors
    /// embedded in results stay bounded.
    pub fn new(data: ErrorData, message: impl Into<String>) -> Self {
        let mut message = message.into();
        let max = MAX_ERROR_MESSAGE_BYTES as usize;
        if message.len() > max {
            let mut end = max;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
        }
        Self { data, message }
    }

    pub fn kind(&self) -> ProtocolErrorKind {
        self.data.kind()
    }

    pub fn code(&self) -> i32 {
        self.kind().code()
    }

    pub fn is_retryable(&self) -> bool {
        self.kind().is_retryable()
    }

    /// Decode an error object tolerantly: a conforming object keeps its
    /// typed data; an object without valid `data` (from a non-conforming
    /// peer or an intermediary) maps its code to a kind, falling back to
    /// `internal`.
    pub fn from_error_value(value: &Value) -> Self {
        if let Ok(object) = serde_json::from_value::<ErrorObject>(value.clone()) {
            return object.into();
        }
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("JSON-RPC error")
            .to_owned();
        let data = value
            .get("code")
            .and_then(Value::as_i64)
            .and_then(ProtocolErrorKind::from_code)
            .and_then(ErrorData::bare)
            .unwrap_or(ErrorData::Internal {});
        Self { data, message }
    }

    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::new(ErrorData::ParseError {}, message)
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(ErrorData::InvalidRequest {}, message)
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            ErrorData::MethodNotFound {},
            format!("method not found: {method}"),
        )
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(
            ErrorData::InvalidParams {
                field: None,
                ceiling: None,
            },
            message,
        )
    }

    /// `invalidParams` naming the offending field by its wire path.
    pub fn invalid_field(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(
            ErrorData::InvalidParams {
                field: Some(field.into()),
                ceiling: None,
            },
            message,
        )
    }

    /// `invalidParams` for a value above an advertised ceiling.
    pub fn above_ceiling(field: impl Into<String>, value: u64, ceiling: u64) -> Self {
        let field = field.into();
        let message = format!("{field} is {value}; the ceiling is {ceiling}");
        Self::new(
            ErrorData::InvalidParams {
                field: Some(field),
                ceiling: Some(ceiling),
            },
            message,
        )
    }

    pub fn not_initialized() -> Self {
        Self::new(
            ErrorData::NotInitialized {},
            "initialize must be the first request on a connection",
        )
    }

    pub fn version_mismatch(expected: u32, received: u32) -> Self {
        Self::new(
            ErrorData::VersionMismatch { expected, received },
            format!("protocol version {received} is not supported; this peer speaks {expected}"),
        )
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorData::NotFound { reason: None }, message)
    }

    pub fn not_found_because(reason: NotFoundReason, message: impl Into<String>) -> Self {
        Self::new(
            ErrorData::NotFound {
                reason: Some(reason),
            },
            message,
        )
    }

    pub fn already_exists(message: impl Into<String>) -> Self {
        Self::new(ErrorData::AlreadyExists {}, message)
    }

    pub fn failed_precondition(reason: PreconditionReason, message: impl Into<String>) -> Self {
        Self::new(ErrorData::FailedPrecondition { reason }, message)
    }

    pub fn permission_denied(reason: Option<PermissionReason>, message: impl Into<String>) -> Self {
        Self::new(ErrorData::PermissionDenied { reason }, message)
    }

    pub fn capability_unavailable(capability: Capability, message: impl Into<String>) -> Self {
        Self::new(ErrorData::CapabilityUnavailable { capability }, message)
    }

    pub fn busy(
        reason: Option<BusyReason>,
        retry_after_ms: Option<u64>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(
            ErrorData::Busy {
                reason,
                retry_after_ms,
            },
            message,
        )
    }

    pub fn deadline_exceeded(message: impl Into<String>) -> Self {
        Self::new(ErrorData::DeadlineExceeded {}, message)
    }

    pub fn expired(message: impl Into<String>) -> Self {
        Self::new(ErrorData::Expired {}, message)
    }

    pub fn aborted(reason: Option<AbortReason>, message: impl Into<String>) -> Self {
        Self::new(ErrorData::Aborted { reason }, message)
    }

    /// `aborted` for a request named by `$/cancelRequest`.
    pub fn cancelled() -> Self {
        Self::aborted(Some(AbortReason::Cancelled), "request cancelled")
    }

    pub fn payload_too_large(limit_bytes: u64, message: impl Into<String>) -> Self {
        Self::new(
            ErrorData::PayloadTooLarge {
                limit_bytes: Some(limit_bytes),
                limit_entries: None,
            },
            message,
        )
    }

    /// `payloadTooLarge` for a result with more entries than allowed.
    pub fn too_many_entries(limit_entries: u32, message: impl Into<String>) -> Self {
        Self::new(
            ErrorData::PayloadTooLarge {
                limit_bytes: None,
                limit_entries: Some(limit_entries),
            },
            message,
        )
    }

    pub fn process_failed(message: impl Into<String>) -> Self {
        Self::new(ErrorData::ProcessFailed {}, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(
            ErrorData::Unavailable {
                retry_after_ms: None,
            },
            message,
        )
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorData::Internal {}, message)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn errors_serialize_as_json_rpc_error_objects_with_the_kind_code() {
        let error = ProtocolError::not_found_because(NotFoundReason::Expired, "unknown process");
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(
            value,
            json!({"code": -32004, "message": "unknown process", "data": {"kind": "notFound", "reason": "expired"}})
        );
        assert_eq!(
            serde_json::from_value::<ProtocolError>(value).unwrap(),
            error
        );
        assert_eq!(error.to_string(), "notFound: unknown process");
    }

    #[test]
    fn every_kind_has_a_distinct_code_that_maps_back() {
        let mut codes = std::collections::BTreeSet::new();
        for kind in ProtocolErrorKind::ALL {
            assert!(codes.insert(kind.code()), "duplicate code for {kind}");
            assert_eq!(ProtocolErrorKind::from_code(kind.code().into()), Some(kind));
        }
    }

    #[test]
    fn retryability_follows_the_kind() {
        let retryable: Vec<_> = ProtocolErrorKind::ALL
            .into_iter()
            .filter(|kind| kind.is_retryable())
            .collect();
        assert_eq!(
            retryable,
            [
                ProtocolErrorKind::Busy,
                ProtocolErrorKind::Aborted,
                ProtocolErrorKind::Unavailable
            ]
        );
    }

    #[test]
    fn foreign_error_objects_map_reserved_codes_and_fall_back_to_internal() {
        let parsed = ProtocolError::from_error_value(&json!({"code": -32601, "message": "nope"}));
        assert_eq!(parsed.kind(), ProtocolErrorKind::MethodNotFound);
        let parsed = ProtocolError::from_error_value(&json!({"code": "weird"}));
        assert_eq!(parsed.kind(), ProtocolErrorKind::Internal);
        assert_eq!(parsed.message, "JSON-RPC error");
    }

    #[test]
    fn unknown_data_members_are_rejected() {
        let value =
            json!({"code": -32004, "message": "m", "data": {"kind": "notFound", "extra": 1}});
        assert!(serde_json::from_value::<ProtocolError>(value).is_err());
    }

    /// One error of every kind, with every optional field set where the kind
    /// has one.
    fn one_of_each_kind() -> Vec<ProtocolError> {
        vec![
            ProtocolError::parse_error("m"),
            ProtocolError::invalid_request("m"),
            ProtocolError::method_not_found("x/y"),
            ProtocolError::above_ceiling("waitMs", 2, 1),
            ProtocolError::not_initialized(),
            ProtocolError::version_mismatch(3, 2),
            ProtocolError::not_found_because(NotFoundReason::DaemonRestarted, "m"),
            ProtocolError::already_exists("m"),
            ProtocolError::failed_precondition(PreconditionReason::TargetNotActive, "m"),
            ProtocolError::permission_denied(Some(PermissionReason::OutsideRoot), "m"),
            ProtocolError::capability_unavailable(Capability::ProcessPty, "m"),
            ProtocolError::busy(Some(BusyReason::RateLimited), Some(500), "m"),
            ProtocolError::deadline_exceeded("m"),
            ProtocolError::expired("m"),
            ProtocolError::aborted(Some(AbortReason::SourceChanged), "m"),
            ProtocolError::new(
                ErrorData::PayloadTooLarge {
                    limit_bytes: Some(1),
                    limit_entries: Some(2),
                },
                "m",
            ),
            ProtocolError::process_failed("m"),
            ProtocolError::new(
                ErrorData::Unavailable {
                    retry_after_ms: Some(10),
                },
                "m",
            ),
            ProtocolError::internal("m"),
        ]
    }

    #[test]
    fn every_kind_round_trips_with_its_code() {
        let errors = one_of_each_kind();
        let kinds: Vec<_> = errors.iter().map(ProtocolError::kind).collect();
        assert_eq!(kinds, ProtocolErrorKind::ALL);
        for error in errors {
            let value = serde_json::to_value(&error).unwrap();
            assert_eq!(value["code"], json!(error.code()));
            assert_eq!(value["data"]["kind"], json!(error.kind().as_str()));
            assert_eq!(
                serde_json::from_value::<ProtocolError>(value).unwrap(),
                error
            );
        }
    }

    #[test]
    fn every_kind_rejects_unknown_data_members() {
        for error in one_of_each_kind() {
            let mut value = serde_json::to_value(&error).unwrap();
            value["data"]["unexpectedMember"] = json!(1);
            assert!(
                serde_json::from_value::<ProtocolError>(value).is_err(),
                "{} accepts an unknown member",
                error.kind()
            );
        }
    }

    #[test]
    fn fieldless_kinds_serialize_as_the_kind_alone() {
        assert_eq!(
            serde_json::to_value(ErrorData::NotInitialized {}).unwrap(),
            json!({"kind": "notInitialized"})
        );
    }

    #[test]
    fn long_messages_are_cut_on_a_character_boundary() {
        let error = ProtocolError::internal("\u{e9}".repeat(MAX_ERROR_MESSAGE_BYTES as usize));
        assert!(error.message.len() <= MAX_ERROR_MESSAGE_BYTES as usize);
        assert!(error.message.len() > MAX_ERROR_MESSAGE_BYTES as usize - 2);
    }
}
