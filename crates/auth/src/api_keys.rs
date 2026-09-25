//! Inbound API keys: callers authenticating against the Lightspeed gateway.
//!
//! A key names what it reaches (one universe, or the deployment), the method
//! groups it may call, and whether it may assert the actor a request acts
//! for. That is its whole authority. Keys are immutable apart from their
//! display name and revocation: changing what a key may do is revoking it and
//! minting another, so a running process never gains or loses rights
//! silently. Persistence belongs to the deployment store, before universe
//! resolution.
//!
//! Keys are server-generated high-entropy secrets (`lsk_<random>`). Only a
//! SHA-256 hash is persisted — no KDF (the secret is random, not a human
//! password) and no AEAD/master-key involvement (the secret never needs to be
//! recovered, only recognized). The plaintext is shown once at mint time.

use std::collections::BTreeSet;

use api::{AccessScope, Attribution, MethodGroup};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{SecretValue, generate_prefixed_secret, secret_display_prefix, secret_sha256_hex};

/// Prefix of every Lightspeed API key secret.
pub const API_KEY_SECRET_PREFIX: &str = "lsk_";

/// Length of the stored display prefix (`lsk_` plus the first characters of
/// the random part) — enough to identify a key in listings without revealing
/// meaningful entropy.
pub const API_KEY_DISPLAY_PREFIX_LEN: usize = 12;

/// How stale `last_used_at_ms` may become. Resolution is a read; the usage
/// stamp is refreshed at most this often so a busy key is not a hot row.
pub const API_KEY_LAST_USED_RESOLUTION_MS: u64 = 60_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyRecord {
    /// Unique display/identification prefix of the secret (`lsk_ab12cd34`).
    /// This is the caller-facing handle for listing and revocation.
    pub key_prefix: String,
    /// What the key reaches. A deployment key addresses a universe by header.
    pub scope: AccessScope,
    /// The method groups the key may call.
    pub groups: BTreeSet<MethodGroup>,
    /// Whether the key may name the actor a request acts for.
    pub assert_actor: bool,
    /// Who minted it; attribution only.
    pub created_by: Attribution,
    pub display_name: Option<String>,
    pub created_at_ms: u64,
    pub revoked_at_ms: Option<u64>,
    pub last_used_at_ms: Option<u64>,
}

/// What a new key may do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiKeySpec {
    pub scope: AccessScope,
    /// `None` grants every group the scope allows.
    pub groups: Option<BTreeSet<MethodGroup>>,
    pub assert_actor: bool,
    pub created_by: Attribution,
    pub display_name: Option<String>,
}

/// A freshly minted key: the one-time plaintext secret plus its record.
#[derive(Clone, Debug)]
pub struct MintedApiKey {
    pub secret: SecretValue,
    pub key_hash: String,
    pub record: ApiKeyRecord,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ApiKeyError {
    #[error("api key already exists: {key_prefix}")]
    AlreadyExists { key_prefix: String },

    #[error("invalid api key: {message}")]
    Invalid { message: String },

    #[error("api key store failure: {message}")]
    Store { message: String },
}

/// Mint a new key secret. The secret is returned exactly once; only its hash
/// and display prefix are meant to be persisted. Groups must be allowed in
/// the key's scope: only a deployment key holds deployment groups.
pub fn mint_api_key(spec: ApiKeySpec, created_at_ms: u64) -> Result<MintedApiKey, ApiKeyError> {
    let allowed = MethodGroup::allowed_in(spec.scope);
    let groups = spec.groups.unwrap_or_else(|| allowed.clone());
    if groups.is_empty() {
        return Err(ApiKeyError::Invalid {
            message: "a key needs at least one method group".into(),
        });
    }
    if let Some(group) = groups.iter().find(|group| !allowed.contains(group)) {
        return Err(ApiKeyError::Invalid {
            message: format!("a universe key cannot hold {}", group.as_str()),
        });
    }
    let secret = generate_prefixed_secret(API_KEY_SECRET_PREFIX);
    let key_hash = api_key_hash(&secret);
    let record = ApiKeyRecord {
        key_prefix: api_key_display_prefix(&secret),
        scope: spec.scope,
        groups,
        assert_actor: spec.assert_actor,
        created_by: spec.created_by,
        display_name: spec.display_name,
        created_at_ms,
        revoked_at_ms: None,
        last_used_at_ms: None,
    };
    Ok(MintedApiKey {
        secret: SecretValue::new(secret),
        key_hash,
        record,
    })
}

/// Lowercase hex SHA-256 of an API key secret, the stored lookup key.
pub fn api_key_hash(secret: &str) -> String {
    secret_sha256_hex(secret)
}

pub fn api_key_display_prefix(secret: &str) -> String {
    secret_display_prefix(secret, API_KEY_DISPLAY_PREFIX_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn spec(scope: AccessScope, groups: Option<&[MethodGroup]>) -> ApiKeySpec {
        ApiKeySpec {
            scope,
            groups: groups.map(|groups| groups.iter().copied().collect()),
            assert_actor: false,
            created_by: Attribution::Local,
            display_name: None,
        }
    }

    fn universe() -> AccessScope {
        AccessScope::Universe {
            universe_id: Uuid::from_u128(7),
        }
    }

    #[test]
    fn minted_keys_have_prefix_hash_and_display_prefix() {
        let minted = mint_api_key(spec(universe(), None), 1).expect("mint");
        let secret = minted.secret.expose().to_owned();
        assert!(secret.starts_with(API_KEY_SECRET_PREFIX));
        assert!(secret.len() > API_KEY_DISPLAY_PREFIX_LEN);
        assert_eq!(minted.key_hash, api_key_hash(&secret));
        assert_eq!(minted.record.key_prefix, api_key_display_prefix(&secret));
        assert_eq!(minted.record.scope, universe());
    }

    #[test]
    fn keys_without_groups_hold_every_group_their_scope_allows() {
        let universe_key = mint_api_key(spec(universe(), None), 1).expect("mint");
        assert_eq!(
            universe_key.record.groups,
            MethodGroup::allowed_in(universe())
        );
        let deployment_key = mint_api_key(spec(AccessScope::Deployment, None), 1).expect("mint");
        assert!(
            deployment_key
                .record
                .groups
                .contains(&MethodGroup::DeploymentApiKeys)
        );
    }

    #[test]
    fn a_universe_key_never_holds_a_deployment_group_or_nothing() {
        assert!(matches!(
            mint_api_key(
                spec(universe(), Some(&[MethodGroup::DeploymentUniverses])),
                1
            ),
            Err(ApiKeyError::Invalid { .. })
        ));
        assert!(matches!(
            mint_api_key(spec(universe(), Some(&[])), 1),
            Err(ApiKeyError::Invalid { .. })
        ));
        let connector = mint_api_key(
            spec(
                AccessScope::Deployment,
                Some(&[
                    MethodGroup::ChannelsInbound,
                    MethodGroup::DeploymentChannels,
                ]),
            ),
            1,
        )
        .expect("mint");
        assert_eq!(connector.record.groups.len(), 2);
    }

    #[test]
    fn minted_secrets_are_unique_and_high_entropy() {
        let first = mint_api_key(spec(universe(), None), 1).expect("mint");
        let second = mint_api_key(spec(universe(), None), 1).expect("mint");
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
        use sha2::{Digest, Sha256};
        assert_eq!(
            api_key_hash("lsk_test"),
            hex::encode(Sha256::digest(b"lsk_test"))
        );
    }
}
