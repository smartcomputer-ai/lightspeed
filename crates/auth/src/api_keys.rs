//! Inbound API keys: callers authenticating against the Lightspeed gateway.
//!
//! A key names what it reaches (one universe, or the deployment), the method
//! groups it may call, and whether it may assert the actor a request acts
//! for. That is its whole authority. What it reaches and whether it asserts
//! actors are fixed for its life; its groups are settings an administrator
//! may change. Persistence belongs to the deployment store, before universe
//! resolution.
//!
//! A key is identified by its key id, which is unique among the keys of its
//! scope and survives a change of secret. The secret is high-entropy random
//! data (`lsk_<random>`), generated here or supplied in the same form by
//! whoever declares the key. Only a SHA-256 hash is persisted — no KDF (the
//! secret is random, not a human password) and no AEAD/master-key involvement
//! (the secret never needs to be recovered, only recognized). A generated
//! plaintext is shown once at mint time.

use std::collections::BTreeSet;

use api::{AccessScope, ApiKeyGroups, Attribution, MethodGroup};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    SecretValue, generate_prefixed_secret, is_prefixed_secret, secret_display_prefix,
    secret_sha256_hex,
};

/// Prefix of every Lightspeed API key secret.
pub const API_KEY_SECRET_PREFIX: &str = api::API_KEY_SECRET_PREFIX;

/// Length of the stored display prefix (`lsk_` plus the first characters of
/// the random part) — enough to identify a key in listings without revealing
/// meaningful entropy.
pub const API_KEY_DISPLAY_PREFIX_LEN: usize = 12;

/// Longest key id.
pub const API_KEY_ID_MAX_LEN: usize = 128;

/// Prefix of a key id the server allocated because the creator named none.
const ALLOCATED_API_KEY_ID_PREFIX: &str = "api_key_";

/// A key id for a key whose creator named none.
pub fn allocate_api_key_id() -> String {
    crate::random_auth_id(ALLOCATED_API_KEY_ID_PREFIX)
}

/// Whether `key_id` has the form of a server-allocated id: the key was never
/// given a name of its creator's choosing.
pub fn is_allocated_api_key_id(key_id: &str) -> bool {
    key_id
        .strip_prefix(ALLOCATED_API_KEY_ID_PREFIX)
        .is_some_and(|rest| {
            rest.len() == 32
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        })
}

/// A key id is 1-128 ASCII letters, digits, underscores, hyphens, dots or
/// colons, starting with a letter or digit.
pub fn validate_api_key_id(key_id: &str) -> Result<(), ApiKeyError> {
    let bytes = key_id.as_bytes();
    let valid = !bytes.is_empty()
        && bytes.len() <= API_KEY_ID_MAX_LEN
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(byte));
    if valid {
        Ok(())
    } else {
        Err(ApiKeyError::Invalid {
            message: "a key id is 1-128 ASCII letters, digits, underscores, hyphens, dots or colons, starting with a letter or digit".into(),
        })
    }
}

/// How stale `last_used_at_ms` may become. Resolution is a read; the usage
/// stamp is refreshed at most this often so a busy key is not a hot row.
pub const API_KEY_LAST_USED_RESOLUTION_MS: u64 = 60_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyRecord {
    /// The key's name among the keys of its scope. It addresses the key, is
    /// kept when the secret is replaced, and is what the key's own requests
    /// are attributed to.
    pub key_id: String,
    /// Display prefix of the current secret (`lsk_ab12cd34`): enough to
    /// recognize which secret a key holds. It changes with the secret, so no
    /// record names a key by it.
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

/// What a new key is called and may do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiKeySpec {
    /// `None` has the server allocate an id.
    pub key_id: Option<String>,
    pub scope: AccessScope,
    /// The groups the key is given. There is no default: whoever makes a
    /// key states them, and "every group the scope allows" is the explicit
    /// [`ApiKeyGroups::All`], resolved when the key is built so the stored
    /// set is always concrete.
    pub groups: ApiKeyGroups,
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
    /// Another key's secret shares this display prefix. A generated secret
    /// is simply generated again.
    #[error("api key already exists: {key_prefix}")]
    AlreadyExists { key_prefix: String },

    #[error("api key id is already in use: {key_id}")]
    KeyIdInUse { key_id: String },

    /// The request is well formed but the key's present state refuses it.
    #[error("api key conflict: {message}")]
    Conflict { message: String },

    #[error("invalid api key: {message}")]
    Invalid { message: String },

    #[error("api key store failure: {message}")]
    Store { message: String },
}

/// Mint a new key secret. The secret is returned exactly once; only its hash
/// and display prefix are meant to be persisted. Groups must be allowed in
/// the key's scope: only a deployment key holds deployment groups. The
/// record holds the concrete groups, with `all` resolved for the scope as
/// this release defines it.
pub fn mint_api_key(spec: ApiKeySpec, created_at_ms: u64) -> Result<MintedApiKey, ApiKeyError> {
    import_api_key(
        spec,
        created_at_ms,
        &generate_prefixed_secret(API_KEY_SECRET_PREFIX),
    )
}

/// Build a key around a supplied secret, in the same 32-byte base64url
/// representation generated keys use.
pub fn import_api_key(
    spec: ApiKeySpec,
    created_at_ms: u64,
    secret: &str,
) -> Result<MintedApiKey, ApiKeyError> {
    if !is_prefixed_secret(API_KEY_SECRET_PREFIX, secret) {
        return Err(ApiKeyError::Invalid {
            message:
                "expected lsk_ followed by 32 random bytes encoded as base64url without padding"
                    .into(),
        });
    }
    let allowed = MethodGroup::allowed_in(spec.scope);
    let groups: BTreeSet<MethodGroup> = spec.groups.resolve(spec.scope);
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
    let key_id = match spec.key_id {
        Some(key_id) => {
            validate_api_key_id(&key_id)?;
            key_id
        }
        None => allocate_api_key_id(),
    };
    let key_hash = api_key_hash(secret);
    let record = ApiKeyRecord {
        key_id,
        key_prefix: api_key_display_prefix(secret),
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
        secret: SecretValue::new(secret.to_owned()),
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

    /// `None` asks for every group the scope allows.
    fn spec(scope: AccessScope, groups: Option<&[MethodGroup]>) -> ApiKeySpec {
        ApiKeySpec {
            key_id: None,
            scope,
            groups: groups.map_or(ApiKeyGroups::All, |groups| groups.iter().copied().collect()),
            assert_actor: false,
            created_by: Attribution::Local,
            display_name: None,
        }
    }

    #[test]
    fn a_key_takes_the_id_it_is_given_or_an_allocated_one() {
        let allocated = mint_api_key(spec(AccessScope::Deployment, None), 1).unwrap();
        assert!(is_allocated_api_key_id(&allocated.record.key_id));
        let named = mint_api_key(
            ApiKeySpec {
                key_id: Some("platform".into()),
                ..spec(AccessScope::Deployment, None)
            },
            1,
        )
        .unwrap();
        assert_eq!(named.record.key_id, "platform");
        assert!(!is_allocated_api_key_id("platform"));
        assert!(!is_allocated_api_key_id("api_key_short"));
        for invalid in ["", "-edge", "with space", "a/b", &"a".repeat(129)] {
            assert!(
                matches!(
                    mint_api_key(
                        ApiKeySpec {
                            key_id: Some(invalid.to_owned()),
                            ..spec(AccessScope::Deployment, None)
                        },
                        1,
                    ),
                    Err(ApiKeyError::Invalid { .. })
                ),
                "{invalid}"
            );
        }
        assert!(validate_api_key_id(&"a".repeat(128)).is_ok());
    }

    fn universe() -> AccessScope {
        AccessScope::Universe {
            universe_id: Uuid::from_u128(7),
        }
    }

    #[test]
    fn supplied_keys_use_the_same_representation_and_hash() {
        let generated = mint_api_key(spec(AccessScope::Deployment, None), 1).unwrap();
        let imported = import_api_key(
            spec(AccessScope::Deployment, None),
            2,
            generated.secret.expose(),
        )
        .unwrap();
        assert_eq!(generated.key_hash, imported.key_hash);
        assert_eq!(generated.record.key_prefix, imported.record.key_prefix);
        for invalid in ["", "lsk_password", "other_abcdefghijklmnopqrstuvwxyz"] {
            assert!(matches!(
                import_api_key(spec(AccessScope::Deployment, None), 1, invalid),
                Err(ApiKeyError::Invalid { .. })
            ));
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
    fn all_is_stored_as_every_group_the_scope_allows() {
        let universe_key = mint_api_key(spec(universe(), None), 1).expect("mint");
        assert_eq!(
            universe_key.record.groups,
            MethodGroup::allowed_in(universe())
        );
        assert!(
            universe_key
                .record
                .groups
                .iter()
                .all(|group| !group.is_deployment())
        );
        let deployment_key = mint_api_key(spec(AccessScope::Deployment, None), 1).expect("mint");
        assert_eq!(
            deployment_key.record.groups,
            MethodGroup::ALL.into_iter().collect()
        );
    }

    #[test]
    fn listed_groups_are_stored_exactly() {
        let key = mint_api_key(
            spec(
                AccessScope::Deployment,
                Some(&[MethodGroup::Session, MethodGroup::Models]),
            ),
            1,
        )
        .expect("mint");
        assert_eq!(
            key.record.groups,
            BTreeSet::from([MethodGroup::Session, MethodGroup::Models])
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
