//! Environment-gateway authentication and route identity.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

use crate::shared::{CURRENT_PROTOCOL_VERSION, HostCapabilities};

pub const DIRECT_DAEMON_PATH: &str = "/environment-gateway/daemon";
pub const ROUTE_PATH_PREFIX: &str = "/environment-gateway/routes";
pub const DEFAULT_LEASE_MS: u64 = 45_000;
const ENROLLMENT_TOKEN_PREFIX: &str = "lse1.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollmentTokenClaims {
    pub universe_id: String,
    pub environment_id: String,
    pub incarnation_id: String,
    pub secret: String,
}

pub fn encode_enrollment_token(
    universe_id: &str,
    environment_id: &str,
    incarnation_id: &str,
    secret: &[u8],
) -> Result<String, serde_json::Error> {
    let claims = EnrollmentTokenClaims {
        universe_id: universe_id.to_owned(),
        environment_id: environment_id.to_owned(),
        incarnation_id: incarnation_id.to_owned(),
        secret: URL_SAFE_NO_PAD.encode(secret),
    };
    Ok(format!(
        "{ENROLLMENT_TOKEN_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?)
    ))
}

pub fn decode_enrollment_token(token: &str) -> Result<EnrollmentTokenClaims, String> {
    let encoded = token
        .strip_prefix(ENROLLMENT_TOKEN_PREFIX)
        .ok_or_else(|| "enrollment token has an unsupported format".to_owned())?;
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| "enrollment token is not valid base64url".to_owned())?;
    let claims: EnrollmentTokenClaims = serde_json::from_slice(&bytes)
        .map_err(|_| "enrollment token payload is invalid".to_owned())?;
    if claims.universe_id.is_empty()
        || claims.environment_id.is_empty()
        || claims.incarnation_id.is_empty()
        || claims.secret.is_empty()
    {
        return Err("enrollment token payload is incomplete".to_owned());
    }
    URL_SAFE_NO_PAD
        .decode(&claims.secret)
        .map_err(|_| "enrollment token secret is invalid".to_owned())?;
    Ok(claims)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayChallenge {
    pub protocol_version: u32,
    pub nonce: String,
    pub lease_ms: u64,
}

impl GatewayChallenge {
    pub fn new(nonce: String, lease_ms: u64) -> Self {
        Self {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            nonce,
            lease_ms,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectDaemonAuthenticate {
    pub protocol_version: u32,
    pub universe_id: String,
    pub environment_id: String,
    pub incarnation_id: String,
    pub daemon_id: String,
    pub public_key: String,
    pub signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrollment_token: Option<String>,
    pub capabilities: HostCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_cwd: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayAccepted {
    pub connection_id: String,
    pub lease_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRejected {
    pub message: String,
}

/// Canonical bytes signed by direct daemons. Length prefixes prevent
/// ambiguity without relying on a JSON serializer's formatting.
pub fn direct_auth_message(
    nonce: &str,
    universe_id: &str,
    environment_id: &str,
    incarnation_id: &str,
    daemon_id: &str,
) -> Vec<u8> {
    let mut message = b"lightspeed.environment-gateway.direct.v1".to_vec();
    for value in [
        nonce,
        universe_id,
        environment_id,
        incarnation_id,
        daemon_id,
    ] {
        let bytes = value.as_bytes();
        message.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        message.extend_from_slice(bytes);
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrollment_token_carries_coordinates_and_rejects_damage() {
        let token =
            encode_enrollment_token("universe-a", "environment-a", "incarnation-a", &[7; 32])
                .expect("encode");
        let claims = decode_enrollment_token(&token).expect("decode");
        assert_eq!(claims.universe_id, "universe-a");
        assert_eq!(claims.environment_id, "environment-a");
        assert_eq!(claims.incarnation_id, "incarnation-a");
        assert!(decode_enrollment_token(&format!("{token}x")).is_err());
    }
}
