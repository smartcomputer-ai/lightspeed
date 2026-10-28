//! The registration channel: the control connection an envd dials to the
//! gateway at [`CONNECT_PATH`](crate::routes::CONNECT_PATH) to register and
//! to be asked for reverse-dialed streams. It runs the shared JSON-RPC
//! envelope; the daemon is the client (it dialed), the gateway the server.
//! Both directions carry requests and notifications. It never carries
//! data-plane messages: every stream is a separate socket the daemon dials
//! on request.
//!
//! Sequence:
//!
//! 1. Gateway → daemon, notification [`registration/challenge`]: always the
//!    first frame (see [`ChallengeParams`]).
//! 2. Daemon → gateway, request `registration/register`: signs the nonce
//!    bound to the gateway origin. The response carries the receipt and
//!    `heartbeatIntervalMs`; an error refuses admission and the gateway
//!    closes the socket.
//! 3. Gateway → daemon, request `registration/openStream`, any number of
//!    times: the daemon dials
//!    [`gateway_stream_url`](crate::routes::gateway_stream_url) with the
//!    token and answers once the stream socket is upgraded, or with an
//!    error.
//! 4. Daemon → gateway, notification `registration/goodbye` before a
//!    deliberate shutdown; gateway → daemon, notification
//!    `registration/closed` before the gateway ends the session for good.
//!
//! **Frame rules.**
//!
//! - Text frames only. The gateway answers a binary frame as the envelope
//!   says (`invalidRequest`, null id); the daemon closes the connection on
//!   one.
//! - An unknown request is answered with `methodNotFound`; an unknown
//!   notification is ignored and logged. A response to an id the receiver
//!   never sent is ignored and logged.
//! - Before admission the gateway accepts only `registration/register`:
//!   any other request is answered with `notInitialized` and the gateway
//!   closes the connection; any notification closes it without an answer.
//!   A `registration/register` after admission is `invalidRequest`, and
//!   the connection stays open.
//! - Before it has the register response, the daemon accepts only the
//!   challenge (first) and that response. Any request or other
//!   notification from the gateway in that window is a protocol violation:
//!   the daemon closes the connection and reconnects with backoff. The
//!   gateway sends `registration/openStream` and `registration/closed` only
//!   after it sent the register response.
//!
//! **Liveness.** The gateway pings every `heartbeatIntervalMs`; the daemon
//! answers. A peer that receives no frame for
//! [`HEARTBEAT_MISSED_INTERVALS`](crate::limits::HEARTBEAT_MISSED_INTERVALS)
//! intervals treats the connection as dead and closes it; the daemon then
//! reconnects with backoff.
//!
//! [`registration/challenge`]: CHALLENGE_METHOD

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::{
    bytes::{SecretString, hex_value},
    error::ProtocolError,
    handshake::ImplementationInfo,
    ids::{ControlConnectionId, DaemonId, EnvironmentId},
    jsonrpc::decode_params,
    metadata::{
        DescriptiveText, DisplayName, Metadata, validate_caller_metadata, validate_display_name,
    },
    routes::CanonicalOrigin,
};

/// Method name of the frozen first frame.
pub const CHALLENGE_METHOD: &str = "registration/challenge";

/// Domain separator of the signed registration message. Its suffix numbers
/// the signed-message layout, not the protocol.
pub const REGISTRATION_SIGNATURE_DOMAIN: &[u8] = b"lightspeed-envd-registration/v2";

hex_value!(
    /// `nonce`: the gateway's 32 random bytes for one control connection.
    ChallengeNonce,
    32,
    "^[0-9a-f]{64}$"
);
hex_value!(
    /// `daemonPublicKey`: a raw Ed25519 public key.
    DaemonPublicKey,
    32,
    "^[0-9a-f]{64}$"
);
hex_value!(
    /// `signature`: an Ed25519 signature over
    /// [`signed_registration_message`].
    Ed25519Signature,
    64,
    "^[0-9a-f]{128}$"
);

/// `registration/challenge` params.
///
/// **Frozen.** This is the first frame of every control connection and the
/// only frame a daemon of any version must parse to learn it is stale: it
/// only ever gains fields, is decoded tolerantly (unknown members are
/// ignored), and a receiver reads `protocolVersion` with
/// [`challenge_protocol_version`] before decoding anything else. The whole
/// frame is
/// `{"jsonrpc":"2.0","method":"registration/challenge","params":{"protocolVersion":N,"nonce":"<64 hex>"}}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ChallengeParams {
    /// The gateway's `ENVD_PROTOCOL_VERSION`.
    pub protocol_version: u32,
    pub nonce: ChallengeNonce,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum BootstrapError {
    #[error("first frame is not JSON")]
    NotJson,
    #[error("first frame is not a registration/challenge notification")]
    NotChallenge,
    #[error("challenge carries no integer protocolVersion")]
    NoVersion,
}

/// Read the gateway's protocol version from a first frame without
/// decoding anything else, so a daemon of any version can tell it is stale
/// before it fails on fields it does not know.
pub fn challenge_protocol_version(frame: &str) -> Result<u32, BootstrapError> {
    let value: Value = serde_json::from_str(frame).map_err(|_| BootstrapError::NotJson)?;
    if value.get("method").and_then(Value::as_str) != Some(CHALLENGE_METHOD) {
        return Err(BootstrapError::NotChallenge);
    }
    value
        .get("params")
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .ok_or(BootstrapError::NoVersion)
}

/// `registration/register` params. `registrationKey`, `displayName` and
/// `metadata` are read only at a daemon identity's first admission; a
/// daemon sends the key until it is admitted once and never afterwards.
///
/// The gateway decodes them with [`RegisterParams::decode`]: build facts,
/// a display name or metadata out of bounds refuse admission with
/// `invalidRequest`; anything else that does not decode is
/// `invalidParams`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisterParams {
    /// The daemon's `ENVD_PROTOCOL_VERSION`.
    pub protocol_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_key: Option<SecretString>,
    pub daemon_public_key: DaemonPublicKey,
    /// Over [`signed_registration_message`] of the challenge nonce and the
    /// canonical origin of the URL the daemon dialed.
    pub signature: Ed25519Signature,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<DisplayName>,
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub metadata: Metadata,
    pub implementation: ImplementationInfo,
}

impl RegisterParams {
    /// Decode `registration/register` params the way the gateway answers
    /// them. Descriptive values that have the right shape but break their
    /// bounds (`implementation`, `displayName`, `metadata`) are
    /// `invalidRequest`, naming the member in the message: admission is
    /// refused, never granted with the value dropped. Every other decode
    /// failure is `invalidParams` naming the field.
    pub fn decode(params: &Value) -> Result<Self, ProtocolError> {
        if let Some(implementation) = params.get("implementation")
            && let Some(error) = ImplementationInfo::bounds_violation(implementation)
        {
            return Err(ProtocolError::invalid_request(format!(
                "implementation.{error}"
            )));
        }
        if let Some(Value::String(name)) = params.get("displayName")
            && let Err(error) = validate_display_name(name)
        {
            return Err(ProtocolError::invalid_request(format!(
                "displayName: {error}"
            )));
        }
        if let Some(metadata) = params.get("metadata")
            && let Ok(entries) = BTreeMap::<String, String>::deserialize(metadata)
            && let Err(error) = validate_caller_metadata(&entries)
        {
            return Err(ProtocolError::invalid_request(format!("metadata: {error}")));
        }
        decode_params(params)
    }
}

/// `registration/register` response: the daemon is admitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisterResponse {
    pub receipt: RegistrationReceipt,
    /// How often the gateway pings; see the liveness rule.
    pub heartbeat_interval_ms: u64,
}

/// What the daemon learns about the environment it serves. Written to the
/// receipt file as is; contains no secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistrationReceipt {
    pub environment_id: EnvironmentId,
    pub daemon_id: DaemonId,
    /// The gateway's id for this control connection.
    pub control_connection_id: ControlConnectionId,
    pub identity_mode: RegistrationIdentityMode,
    pub admission: Admission,
}

/// Registration-key policy for the daemon's environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum RegistrationIdentityMode {
    /// The environment outlives disconnects.
    Persistent,
    /// The environment closes after it stays disconnected past its grace.
    Ephemeral,
}

/// Whether this admission created the environment or rejoined it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum Admission {
    /// First admission of this daemon identity: a new environment.
    Created,
    /// A known identity reconnected to its environment.
    Reconnected,
}

/// `registration/openStream` params: the gateway asks the daemon to dial
/// one stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenStreamParams {
    /// One-time bearer token for the stream dial; short-lived.
    pub token: SecretString,
    pub stream: StreamSpec,
}

/// What the dialed stream carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum StreamSpec {
    /// The data plane, served by the daemon as on any data connection.
    DataProtocol {},
}

/// `registration/openStream` response, sent once the stream socket is
/// upgraded and served. A failed dial answers `unavailable`; the gateway
/// fails the waiting route at once.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenStreamResponse {}

/// `registration/goodbye` params: the daemon is stopping deliberately.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoodbyeParams {
    pub reason: GoodbyeReason,
}

/// Why the daemon is leaving.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum GoodbyeReason {
    /// `SIGTERM` or `SIGINT`.
    Shutdown,
}

/// `registration/closed` params: the gateway ends the session for good and
/// closes the socket after this frame. The daemon exits or parks, as
/// configured, and does not reconnect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClosedParams {
    pub reason: ClosedReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<DescriptiveText>,
}

/// Why the gateway ended the session for good.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ClosedReason {
    /// Another process with this daemon identity connected; the newest
    /// connection wins and this daemon exits with its own status naming the
    /// shared identity.
    Superseded,
    /// The identity's environment was closed; the identity is spent.
    EnvironmentClosed,
}

/// The bytes a daemon signs for one challenge:
/// [`REGISTRATION_SIGNATURE_DOMAIN`] ‖ nonce ‖ sha256(origin), where
/// `origin` is the [`canonical_origin`](crate::routes::canonical_origin) of
/// the gateway URL the daemon dialed; the gateway verifies against the
/// canonical origin of each public URL it is configured with. Binding the
/// origin stops a gateway the daemon was pointed at from relaying another
/// gateway's nonce.
pub fn signed_registration_message(nonce: &ChallengeNonce, origin: &CanonicalOrigin) -> Vec<u8> {
    let origin_hash = Sha256::digest(origin.as_str().as_bytes());
    let mut message = Vec::with_capacity(REGISTRATION_SIGNATURE_DOMAIN.len() + 32 + 32);
    message.extend_from_slice(REGISTRATION_SIGNATURE_DOMAIN);
    message.extend_from_slice(nonce.as_bytes());
    message.extend_from_slice(&origin_hash);
    message
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_challenge_version_is_read_before_anything_else() {
        let future = json!({
            "jsonrpc": "2.0",
            "method": CHALLENGE_METHOD,
            "params": {"protocolVersion": 9, "nonce": 7, "newField": {"x": 1}},
        });
        assert_eq!(challenge_protocol_version(&future.to_string()), Ok(9));
        assert_eq!(
            challenge_protocol_version("{\"method\":\"other\"}"),
            Err(BootstrapError::NotChallenge)
        );
        assert_eq!(
            challenge_protocol_version("garbage"),
            Err(BootstrapError::NotJson)
        );
    }

    #[test]
    fn the_challenge_decodes_tolerantly() {
        let params: ChallengeParams = serde_json::from_value(json!({
            "protocolVersion": 4,
            "nonce": "00".repeat(32),
            "addedLater": true,
        }))
        .unwrap();
        assert_eq!(params.protocol_version, 4);
    }

    #[test]
    fn crypto_values_are_canonical_hex() {
        let key = format!("\"{}\"", "ab".repeat(32));
        assert!(serde_json::from_str::<DaemonPublicKey>(&key).is_ok());
        for bad in [
            format!("\"{}\"", "AB".repeat(32)),
            format!("\"{}\"", "ab".repeat(31)),
            format!("\"{}\u{e9}\"", "a".repeat(62)),
        ] {
            assert!(
                serde_json::from_str::<DaemonPublicKey>(&bad).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_signed_message_binds_the_origin() {
        let nonce = ChallengeNonce::from_bytes([9; 32]);
        let origin = |url| crate::routes::canonical_origin(url).unwrap();
        let first = signed_registration_message(&nonce, &origin("wss://a.example/x"));
        let second = signed_registration_message(&nonce, &origin("wss://b.example"));
        assert_eq!(
            first,
            signed_registration_message(&nonce, &origin("WSS://A.example:443/y"))
        );
        assert!(first.starts_with(REGISTRATION_SIGNATURE_DOMAIN));
        assert_ne!(first, second);
        assert_eq!(first.len(), REGISTRATION_SIGNATURE_DOMAIN.len() + 64);
    }

    #[test]
    fn stream_tokens_are_redacted_in_debug_output() {
        let params = OpenStreamParams {
            token: SecretString::new("secret-token"),
            stream: StreamSpec::DataProtocol {},
        };
        assert!(!format!("{params:?}").contains("secret-token"));
        assert_eq!(
            serde_json::to_value(&params).unwrap(),
            json!({"token": "secret-token", "stream": {"kind": "dataProtocol"}})
        );
    }

    fn register_params() -> Value {
        json!({
            "protocolVersion": 4,
            "daemonPublicKey": "ab".repeat(32),
            "signature": "cd".repeat(64),
            "implementation": {"name": "lightspeed-envd", "version": "0.5.0"},
        })
    }

    #[test]
    fn out_of_bounds_descriptive_values_refuse_admission() {
        assert!(RegisterParams::decode(&register_params()).is_ok());
        let mut implementation = register_params();
        implementation["implementation"]["version"] = json!("v\u{7}");
        let mut name = register_params();
        name["displayName"] = json!("x".repeat(200));
        let mut metadata = register_params();
        metadata["metadata"] = json!({"lightspeed.daemon": "spoofed"});
        for params in [implementation, name, metadata] {
            assert_eq!(
                RegisterParams::decode(&params).unwrap_err().kind(),
                crate::error::ProtocolErrorKind::InvalidRequest,
                "{params}"
            );
        }
    }

    #[test]
    fn other_register_decode_failures_are_invalid_params() {
        let mut key = register_params();
        key["daemonPublicKey"] = json!("AB".repeat(32));
        let mut extra = register_params();
        extra["dataUrl"] = json!("wss://elsewhere");
        let mut shape = register_params();
        shape["implementation"] = json!({"name": 7});
        for params in [key, extra, shape] {
            assert_eq!(
                RegisterParams::decode(&params).unwrap_err().kind(),
                crate::error::ProtocolErrorKind::InvalidParams,
                "{params}"
            );
        }
    }

    #[test]
    fn fieldless_stream_kinds_reject_unknown_members() {
        assert!(serde_json::from_value::<StreamSpec>(json!({"kind": "dataProtocol"})).is_ok());
        assert!(
            serde_json::from_value::<StreamSpec>(json!({"kind": "dataProtocol", "x": 1})).is_err()
        );
    }
}
