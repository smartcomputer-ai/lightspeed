//! Inbound API keys: callers authenticating against the Lightspeed gateway.
//!
//! Everything else in this crate is outbound — the agent authenticating
//! against other systems (grants, flows, the token broker). API keys point
//! the other way and live above the universe boundary: a key resolves *to* a
//! universe and principal, so records are deployment-scoped, not
//! universe-scoped, and their store hangs off the shared deployment pool
//! rather than a universe-bound store instance.
//!
//! Keys are server-generated high-entropy secrets (`lsk_<random>`). Only a
//! SHA-256 hash is persisted — no KDF (the secret is random, not a human
//! password) and no AEAD/master-key involvement (the secret never needs to be
//! recovered, only recognized). The plaintext is shown once at mint time.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{PrincipalRef, SecretValue};

/// Prefix of every Lightspeed API key secret.
pub const API_KEY_SECRET_PREFIX: &str = "lsk_";

/// Length of the stored display prefix (`lsk_` plus the first characters of
/// the random part) — enough to identify a key in listings without revealing
/// meaningful entropy.
pub const API_KEY_DISPLAY_PREFIX_LEN: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyRecord {
    /// Unique display/identification prefix of the secret (`lsk_ab12cd34`).
    /// This is the caller-facing handle for listing and revocation.
    pub key_prefix: String,
    /// Universe the key resolves to.
    pub universe_id: Uuid,
    /// Principal stamped onto grants/flows created through this key.
    /// Recorded for audit; not an authorization mechanism.
    pub principal: PrincipalRef,
    pub display_name: Option<String>,
    pub created_at_ms: u64,
    pub revoked_at_ms: Option<u64>,
    pub last_used_at_ms: Option<u64>,
}

/// A freshly minted key: the one-time plaintext secret plus its record.
#[derive(Clone, Debug)]
pub struct MintedApiKey {
    pub secret: SecretValue,
    pub key_hash: String,
    pub record: ApiKeyRecord,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateApiKey {
    pub key_hash: String,
    pub record: ApiKeyRecord,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ApiKeyError {
    #[error("api key already exists: {key_prefix}")]
    AlreadyExists { key_prefix: String },

    #[error("api key store failure: {message}")]
    Store { message: String },
}

/// Mint a new API key secret for a universe. The secret is returned exactly
/// once; only its hash and display prefix are meant to be persisted.
pub fn mint_api_key(
    universe_id: Uuid,
    principal: PrincipalRef,
    display_name: Option<String>,
    created_at_ms: u64,
) -> MintedApiKey {
    let secret = generate_api_key_secret();
    let key_hash = api_key_hash(&secret);
    let record = ApiKeyRecord {
        key_prefix: api_key_display_prefix(&secret),
        universe_id,
        principal,
        display_name,
        created_at_ms,
        revoked_at_ms: None,
        last_used_at_ms: None,
    };
    MintedApiKey {
        secret: SecretValue::new(secret),
        key_hash,
        record,
    }
}

/// Lowercase hex SHA-256 of an API key secret, the stored lookup key.
pub fn api_key_hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

pub fn api_key_display_prefix(secret: &str) -> String {
    secret.chars().take(API_KEY_DISPLAY_PREFIX_LEN).collect()
}

fn generate_api_key_secret() -> String {
    use base64::Engine as _;
    use rand::RngCore;

    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!(
        "{API_KEY_SECRET_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

/// Deployment-scoped API key persistence. Implementations sit above the
/// universe boundary (see module docs).
#[async_trait::async_trait]
pub trait ApiKeyStore: Send + Sync {
    async fn create_api_key(&self, create: CreateApiKey) -> Result<(), ApiKeyError>;

    /// Resolve an active (non-revoked) key by secret hash, recording
    /// `observed_at_ms` as its last use. Returns `None` for unknown or
    /// revoked keys — resolution never distinguishes the two.
    async fn resolve_api_key(
        &self,
        key_hash: &str,
        observed_at_ms: u64,
    ) -> Result<Option<ApiKeyRecord>, ApiKeyError>;

    async fn list_api_keys(&self) -> Result<Vec<ApiKeyRecord>, ApiKeyError>;

    /// List keys belonging to exactly one universe. Management APIs should
    /// use this scoped operation rather than loading the deployment-wide
    /// catalog and filtering in application code.
    async fn list_api_keys_for_universe(
        &self,
        universe_id: Uuid,
    ) -> Result<Vec<ApiKeyRecord>, ApiKeyError>;

    /// Revoke by display prefix. Returns `false` when no such key exists.
    /// Idempotent: revoking an already-revoked key returns `true` without
    /// moving the original revocation time.
    async fn revoke_api_key(
        &self,
        key_prefix: &str,
        revoked_at_ms: u64,
    ) -> Result<bool, ApiKeyError>;

    /// Revoke a key only when both its display prefix and owning universe
    /// match. Returns the resulting record, or `None` for unknown and
    /// foreign-universe prefixes.
    async fn revoke_api_key_for_universe(
        &self,
        universe_id: Uuid,
        key_prefix: &str,
        revoked_at_ms: u64,
    ) -> Result<Option<ApiKeyRecord>, ApiKeyError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PrincipalKind;

    #[test]
    fn minted_keys_have_prefix_hash_and_display_prefix() {
        let universe_id = Uuid::parse_str("6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f").expect("uuid");
        let minted = mint_api_key(universe_id, PrincipalRef::universe_default(), None, 1);
        let secret = minted.secret.expose().to_owned();
        assert!(secret.starts_with(API_KEY_SECRET_PREFIX));
        assert!(secret.len() > API_KEY_DISPLAY_PREFIX_LEN);
        assert_eq!(minted.key_hash, api_key_hash(&secret));
        assert_eq!(minted.record.key_prefix, api_key_display_prefix(&secret));
        assert_eq!(minted.record.universe_id, universe_id);
        assert_eq!(minted.record.principal.kind, PrincipalKind::UniverseDefault);
    }

    #[test]
    fn minted_secrets_are_unique_and_high_entropy() {
        let universe_id = Uuid::nil();
        let first = mint_api_key(universe_id, PrincipalRef::universe_default(), None, 1);
        let second = mint_api_key(universe_id, PrincipalRef::universe_default(), None, 1);
        assert_ne!(first.secret.expose(), second.secret.expose());
        assert_ne!(first.key_hash, second.key_hash);
        // 32 random bytes base64url-encoded: 43 chars after the prefix.
        assert_eq!(
            first.secret.expose().len(),
            API_KEY_SECRET_PREFIX.len() + 43
        );
    }

    #[test]
    fn api_key_hash_is_hex_sha256_of_the_secret() {
        assert_eq!(
            api_key_hash("lsk_test"),
            hex::encode(Sha256::digest(b"lsk_test"))
        );
    }
}
