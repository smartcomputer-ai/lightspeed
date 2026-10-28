//! Environment-compute domain contracts.
//!
//! Physical providers are deployment-scoped; provider bindings, registration
//! keys and environments are universe-scoped. One `environments` row covers
//! three kinds: provisioned (a provider target), external (an `envd`
//! Lightspeed dials at a fixed endpoint) and registered (an `envd` that
//! dialed the gateway). Lightspeed owns a five-state lifecycle and the
//! decisions to create and to close; everything a provider does while a
//! target runs (power, idle, wake, ingress) is the provider's, and the row
//! only caches what the provider last reported, for display.
//!
//! Every write is a request type with a pure `apply` that the stores share:
//! a lifecycle write whose expected `from` state no longer holds is skipped
//! and leaves the record as it is, so a close accepted while a create was in
//! flight wins. Nothing that only reads a row writes to it; whether a row
//! may be used is [`EnvironmentRecord::use_gate`].

use std::{collections::BTreeMap, fmt, str::FromStr};

use async_trait::async_trait;
use auth::{AuthGrantId, AuthProviderId, SecretId, SecretValue};
pub use environment_protocol::{
    control::target::{PowerState, TargetPhase},
    idle::IdlePolicy,
};
use environment_protocol::{
    control::template::EnvironmentTemplate,
    handshake::ImplementationInfo,
    ids::ProviderTargetId,
    metadata::{DescriptiveText, validate_caller_metadata, validate_descriptive_text},
    registration::DaemonPublicKey,
    routes::{DataRouteBase, DataRouteRefusal},
};
pub use harness::EnvironmentId;
use harness::{StringIdError, validate_general_string_id};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;
use uuid::Uuid;

macro_rules! registry_string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                let value = value.into();
                Self::try_new(value)
                    .unwrap_or_else(|error| panic!("invalid {}: {error}", stringify!($name)))
            }

            pub fn try_new(value: impl Into<String>) -> Result<Self, StringIdError> {
                let value = value.into();
                validate_general_string_id(stringify!($name), &value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = StringIdError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = StringIdError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl FromStr for $name {
            type Err = StringIdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::try_new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::try_new(value).map_err(de::Error::custom)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

registry_string_id!(EnvironmentProviderId);
registry_string_id!(EnvironmentProvisionRequestId);
registry_string_id!(EnvironmentTemplateId);
registry_string_id!(EnvironmentJobGroupId);
registry_string_id!(EnvironmentRegistrationKeyId);
registry_string_id!(EnvironmentDaemonId);

/// Prefix of every daemon id derived from an `envd` public key.
pub const DAEMON_ID_PREFIX: &str = "daemon_";

impl EnvironmentDaemonId {
    /// The daemon id Lightspeed assigns to an `envd` public key: `daemon_`
    /// followed by the lowercase hex SHA-256 of the raw key bytes. The key is
    /// the identity; the id is its stable, bounded, log-safe handle.
    pub fn from_public_key(public_key: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        Self::new(format!(
            "{DAEMON_ID_PREFIX}{}",
            hex_lower(&Sha256::digest(public_key))
        ))
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EnvironmentRegistryError {
    #[error("environment registry {kind} already exists: {id}")]
    AlreadyExists { kind: &'static str, id: String },

    #[error("environment registry {kind} not found: {id}")]
    NotFound { kind: &'static str, id: String },

    #[error("invalid environment registry request: {message}")]
    InvalidInput { message: String },

    /// The registration key exists but no longer admits new daemon
    /// identities (revoked or past its expiry), the secret an admission
    /// presented was retired by a rotation (`rotated`), or a revoked key
    /// was asked to rotate.
    #[error("environment registration key {registration_key_id} is {reason}")]
    RegistrationKeyUnavailable {
        registration_key_id: String,
        reason: &'static str,
    },

    /// The registration key's active-environment limit is reached; the
    /// refused registration left no rows behind.
    #[error(
        "environment registration key {registration_key_id} has reached its active environment limit of {limit}"
    )]
    RegistrationCapacityExhausted {
        registration_key_id: String,
        limit: u32,
    },

    #[error("environment registry store failure: {message}")]
    Store { message: String },
}

// ---------------------------------------------------------------------------
// Timing constants
// ---------------------------------------------------------------------------

/// Default disconnect grace for ephemeral registered environments whose key
/// does not set one: long enough for a pod restart or a brief network
/// interruption, short enough that leaked benchmark sandboxes disappear.
pub const DEFAULT_EPHEMERAL_DISCONNECT_GRACE_MS: u64 = 5 * 60 * 1_000;

/// How long a caller waits for the disconnected daemon of a persistent
/// registered environment to reconnect before the environment counts as
/// away. Covers a rolling restart without hiding a dead pod. Ephemeral
/// daemons get no wait.
pub const REGISTERED_RECONNECT_WAIT_MS: i64 = 2 * 60 * 1_000;

/// Default re-check interval of a [`EnvironmentUse::Wait`] when no
/// `Retry-After` says otherwise.
pub const WAIT_RETRY_AFTER_MS: u64 = 2_000;

/// A connected registered environment's `last_seen_at_ms` is written at
/// most this often; connects and disconnects are always written.
pub const REGISTERED_LIVENESS_STAMP_MS: i64 = 5 * 60 * 1_000;

/// The registered sweep marks a row disconnected when it reads connected but
/// its `last_seen_at_ms` is older than this: the gateway that held the
/// connection stopped without recording the disconnect.
pub const REGISTERED_STALE_AFTER_MS: i64 = 15 * 60 * 1_000;

/// Longest stored `state_message`, in bytes.
pub const STATE_MESSAGE_MAX_BYTES: usize = 1024;

/// Default page size of [`ListEnvironments`].
pub const LIST_ENVIRONMENTS_DEFAULT_LIMIT: u32 = 100;

/// Largest page size of [`ListEnvironments`].
pub const LIST_ENVIRONMENTS_MAX_LIMIT: u32 = 500;

// ---------------------------------------------------------------------------
// Providers and bindings
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentProviderRecord {
    pub provider_id: EnvironmentProviderId,
    pub display_name: Option<String>,
    /// Where Lightspeed dials the provider's controller.
    pub controller_endpoint: EnvironmentEndpoint,
    /// Operator-configured base of the provider's data routes; `None`
    /// derives it from the controller endpoint
    /// ([`DataRouteBase::derived_from_controller`]).
    pub data_route_base: Option<DataRouteBase>,
    /// A bearer credential is stored; without one no `Authorization` header
    /// is sent on the controller or data routes.
    pub has_credential: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl EnvironmentProviderRecord {
    /// The data route base the gateway and the reconciler dial: the
    /// configured one, or the one derived from a `ws`/`wss` controller
    /// endpoint. A `unix://` controller has no derivable base.
    pub fn effective_data_route_base(&self) -> Result<DataRouteBase, EnvironmentRegistryError> {
        if let Some(base) = &self.data_route_base {
            return Ok(base.clone());
        }
        if self.controller_endpoint.scheme() == EndpointScheme::Unix {
            return invalid(
                "a provider with a unix:// controller endpoint needs a configured data route base",
            );
        }
        DataRouteBase::derived_from_controller(self.controller_endpoint.as_str())
            .map_err(|error| invalid_error(format!("data route base: {error}")))
    }

    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        validate_nonempty_optional("display_name", self.display_name.as_deref())?;
        validate_timestamps(self.created_at_ms, self.updated_at_ms)?;
        self.effective_data_route_base()?;
        if let Some(base) = &self.data_route_base
            && self.controller_endpoint.scheme() != EndpointScheme::Unix
        {
            base.check_for_controller(self.controller_endpoint.as_str())
                .map_err(|error| invalid_error(format!("data route base: {error}")))?;
        }
        Ok(())
    }
}

/// What a provider put does with the stored bearer credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderCredentialChange {
    /// Keep the stored credential, or none for a new provider.
    Keep,
    /// Store this credential, replacing any stored one.
    Set(SecretValue),
    /// Remove the stored credential.
    Clear,
}

/// Register or replace a provider. The provider credential is write-only:
/// the store keeps it sealed and hands it out only through
/// [`EnvironmentProviderStore::read_provider_credential`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PutEnvironmentProvider {
    pub provider_id: EnvironmentProviderId,
    pub display_name: Option<String>,
    pub controller_endpoint: EnvironmentEndpoint,
    pub data_route_base: Option<DataRouteBase>,
    pub credential: ProviderCredentialChange,
    pub updated_at_ms: i64,
}

impl PutEnvironmentProvider {
    /// The record this put writes for a provider whose current record is
    /// `existing`: the creation time is kept, and `has_credential` follows
    /// the credential change. A new provider needs no credential.
    pub fn into_record(
        self,
        existing: Option<&EnvironmentProviderRecord>,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(self.updated_at_ms, "updated_at_ms")?;
        let has_credential = match &self.credential {
            ProviderCredentialChange::Keep => existing.is_some_and(|record| record.has_credential),
            ProviderCredentialChange::Set(credential) => {
                validate_provider_credential(credential)?;
                true
            }
            ProviderCredentialChange::Clear => false,
        };
        let created_at_ms = existing.map_or(self.updated_at_ms, |record| record.created_at_ms);
        let record = EnvironmentProviderRecord {
            provider_id: self.provider_id,
            display_name: self.display_name,
            controller_endpoint: self.controller_endpoint,
            data_route_base: self.data_route_base,
            has_credential,
            created_at_ms,
            updated_at_ms: self.updated_at_ms.max(created_at_ms),
        };
        record.validate()?;
        Ok(record)
    }
}

/// Longest accepted provider credential, in bytes.
pub const PROVIDER_CREDENTIAL_MAX_BYTES: usize = 4096;

/// A provider credential travels as an HTTP bearer token: 1 to
/// [`PROVIDER_CREDENTIAL_MAX_BYTES`] bytes of visible ASCII.
pub fn validate_provider_credential(
    credential: &SecretValue,
) -> Result<(), EnvironmentRegistryError> {
    let value = credential.expose();
    if value.is_empty()
        || value.len() > PROVIDER_CREDENTIAL_MAX_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return invalid(format!(
            "provider credential must be 1..={PROVIDER_CREDENTIAL_MAX_BYTES} bytes of visible ASCII"
        ));
    }
    Ok(())
}

/// A universe's use of a provider. At most one per `(universe, provider)`;
/// `enabled` admits creates, template listing and data routes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentProviderBindingRecord {
    pub universe_id: Uuid,
    pub provider_id: EnvironmentProviderId,
    pub enabled: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl EnvironmentProviderBindingRecord {
    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        validate_timestamps(self.created_at_ms, self.updated_at_ms)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutEnvironmentProviderBinding {
    pub universe_id: Uuid,
    pub provider_id: EnvironmentProviderId,
    pub enabled: bool,
    pub updated_at_ms: i64,
}

impl PutEnvironmentProviderBinding {
    /// The record this put writes over `existing`, keeping its creation
    /// time.
    pub fn into_record(
        self,
        existing: Option<&EnvironmentProviderBindingRecord>,
    ) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(self.updated_at_ms, "updated_at_ms")?;
        let created_at_ms = existing.map_or(self.updated_at_ms, |record| record.created_at_ms);
        let record = EnvironmentProviderBindingRecord {
            universe_id: self.universe_id,
            provider_id: self.provider_id,
            enabled: self.enabled,
            created_at_ms,
            updated_at_ms: self.updated_at_ms.max(created_at_ms),
        };
        record.validate()?;
        Ok(record)
    }
}

// ---------------------------------------------------------------------------
// Registration keys
// ---------------------------------------------------------------------------

/// What Lightspeed does with a registered environment while its daemon is
/// disconnected. A property of the registration key, read from the key for
/// every environment it admitted; `envd` neither knows nor chooses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegisteredIdentityMode {
    /// The environment stays open while its daemon is away: callers wait up
    /// to [`REGISTERED_RECONNECT_WAIT_MS`] for a reconnect, and the key's
    /// `offline_close_after_ms` may close it eventually.
    Persistent,
    /// The environment is closed once its daemon has been away longer than
    /// the key's disconnect grace; callers never wait for a reconnect.
    Ephemeral,
}

impl RegisteredIdentityMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Persistent => "persistent",
            Self::Ephemeral => "ephemeral",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "persistent" => Some(Self::Persistent),
            "ephemeral" => Some(Self::Ephemeral),
            _ => None,
        }
    }
}

/// Prefix of every environment registration key secret.
pub const REGISTRATION_KEY_SECRET_PREFIX: &str = "lsrk_";

/// Length of the stored display prefix of a registration key secret.
pub const REGISTRATION_KEY_DISPLAY_PREFIX_LEN: usize = 12;

/// Longest accepted registration-key display name, in bytes.
pub const REGISTRATION_KEY_DISPLAY_NAME_MAX_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentRegistrationKeyStatus {
    Active,
    Revoked,
    Expired,
}

/// One reusable, universe-scoped admission policy for outbound `envd`
/// registration, and the group of the environments it admitted. The row
/// stores no counters: registration and active counts derive from
/// environment rows carrying the key id. Its secrets live beside it, so a
/// rotation keeps the group, the allowlists naming the key and its identity
/// mode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentRegistrationKeyRecord {
    pub registration_key_id: EnvironmentRegistrationKeyId,
    /// Required: the group name shown wherever registered environments are
    /// listed.
    pub display_name: String,
    /// Display prefix of the key's live secret.
    pub key_prefix: String,
    /// Immutable.
    pub identity_mode: RegisteredIdentityMode,
    /// Non-closed environments this key may have at once; absent means
    /// unlimited.
    pub max_active_environments: Option<u32>,
    /// Absent means [`DEFAULT_EPHEMERAL_DISCONNECT_GRACE_MS`].
    pub ephemeral_disconnect_grace_ms: Option<u64>,
    /// Persistent keys only: close an environment whose daemon has been
    /// away longer than this.
    pub offline_close_after_ms: Option<u64>,
    pub expires_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub revoked_at_ms: Option<i64>,
}

impl EnvironmentRegistrationKeyRecord {
    pub fn status(&self, now_ms: i64) -> EnvironmentRegistrationKeyStatus {
        if self.revoked_at_ms.is_some() {
            EnvironmentRegistrationKeyStatus::Revoked
        } else if self.expires_at_ms.is_some_and(|expires| expires <= now_ms) {
            EnvironmentRegistrationKeyStatus::Expired
        } else {
            EnvironmentRegistrationKeyStatus::Active
        }
    }

    /// Typed refusal when the key no longer admits new daemon identities.
    pub fn check_admits(&self, now_ms: i64) -> Result<(), EnvironmentRegistryError> {
        match self.status(now_ms) {
            EnvironmentRegistrationKeyStatus::Active => Ok(()),
            EnvironmentRegistrationKeyStatus::Revoked => Err(self.unavailable("revoked")),
            EnvironmentRegistrationKeyStatus::Expired => Err(self.unavailable("expired")),
        }
    }

    fn unavailable(&self, reason: &'static str) -> EnvironmentRegistryError {
        EnvironmentRegistryError::RegistrationKeyUnavailable {
            registration_key_id: self.registration_key_id.to_string(),
            reason,
        }
    }

    pub fn ephemeral_disconnect_grace_ms(&self) -> u64 {
        self.ephemeral_disconnect_grace_ms
            .unwrap_or(DEFAULT_EPHEMERAL_DISCONNECT_GRACE_MS)
    }

    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        validate_registration_display_name(&self.display_name)?;
        validate_nonempty_string("key_prefix", &self.key_prefix)?;
        if self.max_active_environments == Some(0) {
            return invalid("max_active_environments must be positive when set");
        }
        if self.ephemeral_disconnect_grace_ms == Some(0) {
            return invalid("ephemeral_disconnect_grace_ms must be positive when set");
        }
        if self.offline_close_after_ms == Some(0) {
            return invalid("offline_close_after_ms must be positive when set");
        }
        if self.offline_close_after_ms.is_some()
            && self.identity_mode != RegisteredIdentityMode::Persistent
        {
            return invalid("offline_close_after_ms applies only to persistent keys");
        }
        validate_timestamps(self.created_at_ms, self.updated_at_ms)?;
        if let Some(expires) = self.expires_at_ms {
            validate_nonnegative_i64(expires, "expires_at_ms")?;
        }
        if let Some(revoked) = self.revoked_at_ms
            && revoked < self.created_at_ms
        {
            return invalid("revoked_at_ms must be >= created_at_ms");
        }
        Ok(())
    }
}

/// Policy fields an operator chooses when minting a registration key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistrationKeyPolicy {
    pub display_name: String,
    pub identity_mode: RegisteredIdentityMode,
    pub max_active_environments: Option<u32>,
    pub ephemeral_disconnect_grace_ms: Option<u64>,
    pub offline_close_after_ms: Option<u64>,
    pub expires_at_ms: Option<i64>,
}

/// The editable part of a key's policy; a full replace (absent means
/// unset). The identity mode is not part of it: it never changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistrationKeySettings {
    pub display_name: String,
    pub max_active_environments: Option<u32>,
    pub ephemeral_disconnect_grace_ms: Option<u64>,
    pub offline_close_after_ms: Option<u64>,
    pub expires_at_ms: Option<i64>,
}

/// A freshly minted registration secret: the one-time plaintext plus the
/// hash and display prefix to persist. The secret is structurally not
/// persistable.
#[derive(Clone, Debug)]
pub struct MintedRegistrationSecret {
    pub secret: SecretValue,
    pub secret_hash: String,
    pub key_prefix: String,
}

/// Mint a registration secret. Only its SHA-256 hash and display prefix are
/// ever persisted.
pub fn mint_registration_secret() -> MintedRegistrationSecret {
    let secret = auth::generate_prefixed_secret(REGISTRATION_KEY_SECRET_PREFIX);
    MintedRegistrationSecret {
        secret_hash: auth::secret_sha256_hex(&secret),
        key_prefix: auth::secret_display_prefix(&secret, REGISTRATION_KEY_DISPLAY_PREFIX_LEN),
        secret: SecretValue::new(secret),
    }
}

/// A freshly minted registration key: its first secret plus the record to
/// persist.
#[derive(Clone, Debug)]
pub struct MintedRegistrationKey {
    pub secret: MintedRegistrationSecret,
    pub record: EnvironmentRegistrationKeyRecord,
}

/// Mint a registration key. The secret is returned exactly once.
pub fn mint_registration_key(
    registration_key_id: EnvironmentRegistrationKeyId,
    policy: RegistrationKeyPolicy,
    created_at_ms: i64,
) -> Result<MintedRegistrationKey, EnvironmentRegistryError> {
    let secret = mint_registration_secret();
    let record = EnvironmentRegistrationKeyRecord {
        registration_key_id,
        display_name: policy.display_name,
        key_prefix: secret.key_prefix.clone(),
        identity_mode: policy.identity_mode,
        max_active_environments: policy.max_active_environments,
        ephemeral_disconnect_grace_ms: policy.ephemeral_disconnect_grace_ms,
        offline_close_after_ms: policy.offline_close_after_ms,
        expires_at_ms: policy.expires_at_ms,
        created_at_ms,
        updated_at_ms: created_at_ms,
        revoked_at_ms: None,
    };
    record.validate()?;
    Ok(MintedRegistrationKey { secret, record })
}

/// Lowercase hex SHA-256 of a presented registration secret: the lookup key.
pub fn registration_key_hash(secret: &str) -> String {
    auth::secret_sha256_hex(secret)
}

/// Create a key with its first live secret.
#[derive(Clone, Debug)]
pub struct CreateEnvironmentRegistrationKey {
    pub secret_hash: String,
    pub record: EnvironmentRegistrationKeyRecord,
}

/// Replace the editable policy of a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateEnvironmentRegistrationKey {
    pub registration_key_id: EnvironmentRegistrationKeyId,
    pub settings: RegistrationKeySettings,
    pub updated_at_ms: i64,
}

impl UpdateEnvironmentRegistrationKey {
    /// Apply the settings to `record`; the result is validated, so an
    /// `offline_close_after_ms` on an ephemeral key is refused.
    pub fn apply(
        &self,
        record: &mut EnvironmentRegistrationKeyRecord,
    ) -> Result<(), EnvironmentRegistryError> {
        validate_nonnegative_i64(self.updated_at_ms, "updated_at_ms")?;
        let settings = self.settings.clone();
        let mut candidate = EnvironmentRegistrationKeyRecord {
            display_name: settings.display_name,
            max_active_environments: settings.max_active_environments,
            ephemeral_disconnect_grace_ms: settings.ephemeral_disconnect_grace_ms,
            offline_close_after_ms: settings.offline_close_after_ms,
            expires_at_ms: settings.expires_at_ms,
            ..record.clone()
        };
        candidate.updated_at_ms = candidate.updated_at_ms.max(self.updated_at_ms);
        candidate.validate()?;
        *record = candidate;
        Ok(())
    }
}

/// Retire every live secret of the key and make this one its only live
/// secret. Daemons the key already admitted are unaffected: they never
/// present the key again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RotateEnvironmentRegistrationKey {
    pub registration_key_id: EnvironmentRegistrationKeyId,
    pub secret_hash: String,
    pub key_prefix: String,
    pub rotated_at_ms: i64,
}

impl RotateEnvironmentRegistrationKey {
    /// Apply the rotation to the key record: the new display prefix and
    /// update time. A revoked key refuses with `RegistrationKeyUnavailable`.
    pub fn apply(
        &self,
        record: &mut EnvironmentRegistrationKeyRecord,
    ) -> Result<(), EnvironmentRegistryError> {
        validate_nonnegative_i64(self.rotated_at_ms, "rotated_at_ms")?;
        validate_secret_hash(&self.secret_hash)?;
        if record.revoked_at_ms.is_some() {
            return Err(record.unavailable("revoked"));
        }
        let mut candidate = record.clone();
        candidate.key_prefix = self.key_prefix.clone();
        candidate.updated_at_ms = candidate.updated_at_ms.max(self.rotated_at_ms);
        candidate.validate()?;
        *record = candidate;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevokeEnvironmentRegistrationKey {
    pub registration_key_id: EnvironmentRegistrationKeyId,
    pub revoked_at_ms: i64,
}

/// Derived per-key counts from environment rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegistrationKeyUsage {
    /// Environments ever admitted by the key, closed included.
    pub registered: u64,
    /// Non-closed environments admitted by the key.
    pub active: u64,
    pub last_registered_at_ms: Option<i64>,
}

#[async_trait]
pub trait EnvironmentRegistrationKeyStore: Send + Sync {
    async fn create_registration_key(
        &self,
        request: CreateEnvironmentRegistrationKey,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError>;
    async fn read_registration_key(
        &self,
        registration_key_id: &EnvironmentRegistrationKeyId,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError>;
    async fn list_registration_keys(
        &self,
    ) -> Result<Vec<EnvironmentRegistrationKeyRecord>, EnvironmentRegistryError>;
    async fn update_registration_key(
        &self,
        request: UpdateEnvironmentRegistrationKey,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError>;
    /// A revoked key refuses with `RegistrationKeyUnavailable { reason:
    /// "revoked" }`.
    async fn rotate_registration_key(
        &self,
        request: RotateEnvironmentRegistrationKey,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError>;
    /// Idempotent: revoking a revoked key keeps the original revocation time.
    async fn revoke_registration_key(
        &self,
        request: RevokeEnvironmentRegistrationKey,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError>;
    /// Resolve a presented secret by hash among live (not retired) secrets,
    /// regardless of the key's status; the caller turns revoked or expired
    /// into typed refusals.
    async fn resolve_registration_key(
        &self,
        secret_hash: &str,
    ) -> Result<Option<EnvironmentRegistrationKeyRecord>, EnvironmentRegistryError>;
    async fn registration_key_usage(
        &self,
        registration_key_id: &EnvironmentRegistrationKeyId,
    ) -> Result<RegistrationKeyUsage, EnvironmentRegistryError>;
}

// ---------------------------------------------------------------------------
// Environment records
// ---------------------------------------------------------------------------

/// The lifecycle Lightspeed owns. External and registered environments are
/// only ever `open` or `closed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentState {
    /// A provisioned environment whose target is being created and has not
    /// passed its open handshake yet.
    Provisioning,
    Open,
    /// A provisioned environment whose target is being closed.
    Closing,
    Closed,
    /// A provisioned environment broken beyond recovery; see
    /// `state_message`.
    Failed,
}

impl EnvironmentState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provisioning => "provisioning",
            Self::Open => "open",
            Self::Closing => "closing",
            Self::Closed => "closed",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "provisioning" => Some(Self::Provisioning),
            "open" => Some(Self::Open),
            "closing" => Some(Self::Closing),
            "closed" => Some(Self::Closed),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

impl fmt::Display for EnvironmentState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why an environment is closing or closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentCloseReason {
    /// A caller closed it.
    Requested,
    /// Its registration key was revoked with its environments.
    KeyRevoked,
    /// Its ephemeral daemon stayed away past the key's disconnect grace.
    EphemeralGrace,
    /// Its ephemeral daemon said goodbye.
    Departed,
    /// Its persistent daemon stayed away past the key's
    /// `offline_close_after_ms`.
    OfflineTimeout,
    /// The provider reported its target closed or gone.
    ProviderEnded,
}

impl EnvironmentCloseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::KeyRevoked => "key_revoked",
            Self::EphemeralGrace => "ephemeral_grace",
            Self::Departed => "departed",
            Self::OfflineTimeout => "offline_timeout",
            Self::ProviderEnded => "provider_ended",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "requested" => Some(Self::Requested),
            "key_revoked" => Some(Self::KeyRevoked),
            "ephemeral_grace" => Some(Self::EphemeralGrace),
            "departed" => Some(Self::Departed),
            "offline_timeout" => Some(Self::OfflineTimeout),
            "provider_ended" => Some(Self::ProviderEnded),
            _ => None,
        }
    }
}

impl fmt::Display for EnvironmentCloseReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Display status derived from the state and the per-kind observations;
/// never stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentDisplayStatus {
    Provisioning,
    Ready,
    Paused,
    Suspended,
    Stopped,
    /// The provider's last report gives no usable phase.
    Unknown,
    /// A registered daemon is disconnected.
    Away,
    Closing,
    Closed,
    Failed,
}

impl EnvironmentDisplayStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provisioning => "provisioning",
            Self::Ready => "ready",
            Self::Paused => "paused",
            Self::Suspended => "suspended",
            Self::Stopped => "stopped",
            Self::Unknown => "unknown",
            Self::Away => "away",
            Self::Closing => "closing",
            Self::Closed => "closed",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for EnvironmentDisplayStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// What Lightspeed learned about the daemon serving an environment: from
/// `registration/register` for a registered daemon, from the data-plane
/// `initialize` of the open handshake for a provisioned one. Recorded so an
/// operator sees which build serves it, and which protocol version a
/// mismatched daemon spoke. Never consulted for admission: the protocol
/// version alone decides that.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonBuild {
    pub version: Option<String>,
    pub git_sha: Option<String>,
    pub protocol_version: u32,
}

impl DaemonBuild {
    pub fn from_implementation(implementation: &ImplementationInfo, protocol_version: u32) -> Self {
        Self {
            version: implementation.version().map(str::to_owned),
            git_sha: implementation.git_sha().map(str::to_owned),
            protocol_version,
        }
    }

    fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        validate_nonempty_optional("daemon version", self.version.as_deref())?;
        validate_nonempty_optional("daemon git sha", self.git_sha.as_deref())
    }
}

/// What the provider last reported about a provisioned environment's
/// target. Display only: no loop selects on it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedTarget {
    pub phase: TargetPhase,
    /// Present only when `phase` is `active`.
    pub power: Option<PowerState>,
    pub message: Option<String>,
    pub observed_at_ms: i64,
}

impl ObservedTarget {
    fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        validate_nonnegative_i64(self.observed_at_ms, "observed_at_ms")?;
        if self.power.is_some() && self.phase != TargetPhase::Active {
            return invalid("an observed power state requires the active phase");
        }
        if let Some(message) = &self.message {
            validate_descriptive_text(message)
                .map_err(|error| invalid_error(format!("observed message: {error}")))?;
        }
        Ok(())
    }
}

/// The per-kind facts of a provisioned environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionedSource {
    pub provider_id: EnvironmentProviderId,
    pub template_id: EnvironmentTemplateId,
    /// Create-time idle override, sent with every `target/create` retry.
    /// Never interpreted by Lightspeed.
    pub idle: Option<IdlePolicy>,
    /// The provider's handle, once `target/create` answered with one.
    pub provider_target_id: Option<ProviderTargetId>,
    /// Display only; no loop selects on it.
    pub observed: Option<ObservedTarget>,
    /// The public endpoint the provider last reported.
    pub public_endpoint: Option<String>,
    /// Work schedule; `Some` exactly while `provisioning` or `closing`.
    pub next_attempt_at_ms: Option<i64>,
    /// Consecutive failed attempts of the current work; drives backoff.
    pub attempts: u32,
}

/// The per-kind facts of a registered environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredSource {
    pub registration_key_id: EnvironmentRegistrationKeyId,
    /// Lowercase hex of the daemon's raw Ed25519 public key: the identity.
    /// One key maps to at most one environment in the deployment, ever, so
    /// a closed environment's daemon can never register again.
    pub daemon_public_key: String,
    /// Read from the key; not stored on the row.
    pub identity_mode: RegisteredIdentityMode,
    /// The gateway holds a control connection, as far as durable state
    /// knows.
    pub connected: bool,
    /// When `connected` last changed (or the gateway restarted).
    pub connection_changed_at_ms: i64,
    /// Coarse liveness: written at connect and disconnect, and at most every
    /// [`REGISTERED_LIVENESS_STAMP_MS`] while connected.
    pub last_seen_at_ms: i64,
}

impl RegisteredSource {
    /// The daemon id derived from the public key.
    pub fn daemon_id(&self) -> EnvironmentDaemonId {
        match DaemonPublicKey::parse(&self.daemon_public_key) {
            Ok(key) => EnvironmentDaemonId::from_public_key(key.as_bytes()),
            // A record that passed validation always parses; an unvalidated
            // one still gets a stable id.
            Err(_) => EnvironmentDaemonId::from_public_key(self.daemon_public_key.as_bytes()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EnvironmentSource {
    /// A provider target Lightspeed asked a provider to create.
    Provisioned(ProvisionedSource),
    /// An `envd` Lightspeed dials directly at a fixed endpoint.
    External { endpoint: EnvironmentEndpoint },
    /// An `envd` that dialed the gateway outbound and was admitted by a
    /// registration key.
    Registered(RegisteredSource),
}

impl EnvironmentSource {
    pub fn provider_id(&self) -> Option<&EnvironmentProviderId> {
        match self {
            Self::Provisioned(source) => Some(&source.provider_id),
            Self::External { .. } | Self::Registered(_) => None,
        }
    }

    pub fn registration_key_id(&self) -> Option<&EnvironmentRegistrationKeyId> {
        match self {
            Self::Registered(source) => Some(&source.registration_key_id),
            Self::Provisioned(_) | Self::External { .. } => None,
        }
    }

    /// The stored `source_kind`: `provisioned`, `external` or `registered`.
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Provisioned(_) => "provisioned",
            Self::External { .. } => "external",
            Self::Registered(_) => "registered",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentRecord {
    pub environment_id: EnvironmentId,
    /// Caller retry key; `None` for registered environments, whose daemon
    /// public key makes a retried registration converge.
    pub request_id: Option<EnvironmentProvisionRequestId>,
    pub source: EnvironmentSource,
    pub display_name: Option<String>,
    /// Caller metadata only.
    pub metadata: BTreeMap<String, String>,
    pub state: EnvironmentState,
    /// Why the row failed, or what provisioning or closing waits on. At most
    /// [`STATE_MESSAGE_MAX_BYTES`].
    pub state_message: Option<String>,
    /// Set exactly while `closing` or `closed`.
    pub close_reason: Option<EnvironmentCloseReason>,
    pub state_changed_at_ms: i64,
    /// The daemon that serves it: from `registration/register`, or the open
    /// handshake.
    pub daemon_build: Option<DaemonBuild>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub closed_at_ms: Option<i64>,
}

/// Why an environment cannot be used yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentWaitReason {
    /// Still provisioning.
    Provisioning,
    /// The registered daemon is not connected.
    Disconnected,
    /// The target is resuming from a powered-down state.
    Waking,
    /// The target runs but `envd` is not answering yet.
    Booting,
}

impl EnvironmentWaitReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provisioning => "provisioning",
            Self::Disconnected => "disconnected",
            Self::Waking => "waking",
            Self::Booting => "booting",
        }
    }
}

impl fmt::Display for EnvironmentWaitReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why an environment cannot be used at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentGoneReason {
    Closing,
    Closed,
    Failed,
    /// The registered daemon has been away past its reconnect bound.
    Away,
}

impl EnvironmentGoneReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closing => "closing",
            Self::Closed => "closed",
            Self::Failed => "failed",
            Self::Away => "away",
        }
    }
}

impl fmt::Display for EnvironmentGoneReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The single answer to "may I use this environment now".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvironmentUse {
    /// Dial it.
    Use,
    /// Not usable yet: re-check after `retry_after_ms`; `until_ms` bounds
    /// the wait.
    Wait {
        reason: EnvironmentWaitReason,
        retry_after_ms: u64,
        until_ms: Option<i64>,
    },
    /// Not usable: closing, closed, failed, or a daemon away past its bound.
    Gone {
        reason: EnvironmentGoneReason,
        message: Option<String>,
    },
}

impl EnvironmentUse {
    /// What a typed data-route refusal means for the caller. Callers that
    /// hold the record use [`EnvironmentRecord::bound_route_refusal`].
    pub fn from_route_refusal(refusal: &DataRouteRefusal, retry_after_ms: Option<u64>) -> Self {
        let wait = |reason| Self::Wait {
            reason,
            retry_after_ms: retry_after_ms.unwrap_or(WAIT_RETRY_AFTER_MS),
            until_ms: None,
        };
        match refusal {
            DataRouteRefusal::Waking {} => wait(EnvironmentWaitReason::Waking),
            DataRouteRefusal::Booting {} => wait(EnvironmentWaitReason::Booting),
            DataRouteRefusal::Provisioning {} => wait(EnvironmentWaitReason::Provisioning),
            DataRouteRefusal::Disconnected {} => wait(EnvironmentWaitReason::Disconnected),
            DataRouteRefusal::Away {} => Self::Gone {
                reason: EnvironmentGoneReason::Away,
                message: None,
            },
            DataRouteRefusal::Gone {} => Self::Gone {
                reason: EnvironmentGoneReason::Closed,
                message: None,
            },
            DataRouteRefusal::Failed { message } => Self::Gone {
                reason: EnvironmentGoneReason::Failed,
                message: Some(message.as_str().to_owned()),
            },
        }
    }
}

impl EnvironmentRecord {
    pub fn is_registered(&self) -> bool {
        matches!(self.source, EnvironmentSource::Registered(_))
    }

    pub fn provider_id(&self) -> Option<&EnvironmentProviderId> {
        self.source.provider_id()
    }

    pub fn registration_key_id(&self) -> Option<&EnvironmentRegistrationKeyId> {
        self.source.registration_key_id()
    }

    pub fn provisioned(&self) -> Option<&ProvisionedSource> {
        match &self.source {
            EnvironmentSource::Provisioned(source) => Some(source),
            EnvironmentSource::External { .. } | EnvironmentSource::Registered(_) => None,
        }
    }

    pub fn registered(&self) -> Option<&RegisteredSource> {
        match &self.source {
            EnvironmentSource::Registered(source) => Some(source),
            EnvironmentSource::Provisioned(_) | EnvironmentSource::External { .. } => None,
        }
    }

    /// The display status: the state, refined on an open row by the
    /// registered connection or the provider's last observation.
    pub fn display_status(&self) -> EnvironmentDisplayStatus {
        match self.state {
            EnvironmentState::Provisioning => EnvironmentDisplayStatus::Provisioning,
            EnvironmentState::Closing => EnvironmentDisplayStatus::Closing,
            EnvironmentState::Closed => EnvironmentDisplayStatus::Closed,
            EnvironmentState::Failed => EnvironmentDisplayStatus::Failed,
            EnvironmentState::Open => match &self.source {
                EnvironmentSource::External { .. } => EnvironmentDisplayStatus::Ready,
                EnvironmentSource::Registered(source) => {
                    if source.connected {
                        EnvironmentDisplayStatus::Ready
                    } else {
                        EnvironmentDisplayStatus::Away
                    }
                }
                EnvironmentSource::Provisioned(source) => match &source.observed {
                    None => EnvironmentDisplayStatus::Ready,
                    Some(observed) => match observed.phase {
                        TargetPhase::Active => match observed.power {
                            None | Some(PowerState::Running) => EnvironmentDisplayStatus::Ready,
                            Some(PowerState::Paused) => EnvironmentDisplayStatus::Paused,
                            Some(PowerState::Suspended) => EnvironmentDisplayStatus::Suspended,
                            Some(PowerState::Stopped) => EnvironmentDisplayStatus::Stopped,
                        },
                        TargetPhase::Provisioning => EnvironmentDisplayStatus::Provisioning,
                        TargetPhase::Closing => EnvironmentDisplayStatus::Closing,
                        TargetPhase::Unknown | TargetPhase::Failed | TargetPhase::Closed => {
                            EnvironmentDisplayStatus::Unknown
                        }
                    },
                },
            },
        }
    }

    /// The use gate every consumer shares: selection, the resolver, job
    /// activities, discovery and the gateway route. It only reads.
    pub fn use_gate(&self, now_ms: i64) -> EnvironmentUse {
        match self.state {
            EnvironmentState::Closing => gone(EnvironmentGoneReason::Closing, None),
            EnvironmentState::Closed => gone(EnvironmentGoneReason::Closed, None),
            EnvironmentState::Failed => {
                gone(EnvironmentGoneReason::Failed, self.state_message.clone())
            }
            EnvironmentState::Provisioning => EnvironmentUse::Wait {
                reason: EnvironmentWaitReason::Provisioning,
                retry_after_ms: WAIT_RETRY_AFTER_MS,
                until_ms: None,
            },
            EnvironmentState::Open => match &self.source {
                EnvironmentSource::Registered(source) if !source.connected => {
                    let away = || {
                        gone(
                            EnvironmentGoneReason::Away,
                            Some(format!(
                                "daemon away for {}",
                                compact_duration(
                                    now_ms.saturating_sub(source.connection_changed_at_ms)
                                )
                            )),
                        )
                    };
                    match source.identity_mode {
                        RegisteredIdentityMode::Ephemeral => away(),
                        RegisteredIdentityMode::Persistent => {
                            let until = source
                                .connection_changed_at_ms
                                .saturating_add(REGISTERED_RECONNECT_WAIT_MS);
                            if now_ms < until {
                                EnvironmentUse::Wait {
                                    reason: EnvironmentWaitReason::Disconnected,
                                    retry_after_ms: WAIT_RETRY_AFTER_MS,
                                    until_ms: Some(until),
                                }
                            } else {
                                away()
                            }
                        }
                    }
                }
                _ => EnvironmentUse::Use,
            },
        }
    }

    /// What a typed data-route refusal means for this environment:
    /// [`EnvironmentUse::from_route_refusal`], except that a `disconnected`
    /// refusal of a registered environment follows its identity mode. The
    /// row read connected, but the gateway held no control handle: the
    /// moment between a dropped connection and its disconnect write. An
    /// ephemeral daemon is then away at once; a persistent one is waited
    /// for, unbounded here, so the caller bounds consecutive waits by
    /// [`REGISTERED_RECONNECT_WAIT_MS`] until the row records the
    /// disconnect and the gate's own bound applies.
    pub fn bound_route_refusal(
        &self,
        refusal: &DataRouteRefusal,
        retry_after_ms: Option<u64>,
    ) -> EnvironmentUse {
        let answer = EnvironmentUse::from_route_refusal(refusal, retry_after_ms);
        match (&self.source, refusal) {
            (EnvironmentSource::Registered(source), DataRouteRefusal::Disconnected {})
                if source.identity_mode == RegisteredIdentityMode::Ephemeral =>
            {
                gone(
                    EnvironmentGoneReason::Away,
                    Some("daemon is not connected".to_owned()),
                )
            }
            _ => answer,
        }
    }

    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        validate_nonempty_optional("display_name", self.display_name.as_deref())?;
        validate_caller_metadata(&self.metadata)
            .map_err(|error| invalid_error(error.to_string()))?;
        validate_timestamps(self.created_at_ms, self.updated_at_ms)?;
        if self.state_changed_at_ms < self.created_at_ms {
            return invalid("state_changed_at_ms must be >= created_at_ms");
        }
        if let Some(message) = &self.state_message {
            validate_descriptive_text(message)
                .map_err(|error| invalid_error(format!("state message: {error}")))?;
        }
        if let Some(build) = &self.daemon_build {
            build.validate()?;
        }
        let closing_or_closed = matches!(
            self.state,
            EnvironmentState::Closing | EnvironmentState::Closed
        );
        if self.close_reason.is_some() != closing_or_closed {
            return invalid("a close reason is recorded exactly while closing or closed");
        }
        match self.closed_at_ms {
            Some(closed_at) if self.state == EnvironmentState::Closed => {
                if closed_at < self.created_at_ms {
                    return invalid("closed_at_ms must be >= created_at_ms");
                }
            }
            None if self.state != EnvironmentState::Closed => {}
            _ => return invalid("closed_at_ms is recorded exactly while closed"),
        }
        if self.request_id.is_some() == self.is_registered() {
            return invalid(
                "provisioned and external environments carry a request id; registered ones do not",
            );
        }
        match &self.source {
            EnvironmentSource::Provisioned(source) => {
                let scheduled = matches!(
                    self.state,
                    EnvironmentState::Provisioning | EnvironmentState::Closing
                );
                if source.next_attempt_at_ms.is_some() != scheduled {
                    return invalid(
                        "a provisioned environment is scheduled exactly while provisioning or closing",
                    );
                }
                if let Some(policy) = &source.idle {
                    policy
                        .validate()
                        .map_err(|error| invalid_error(error.to_string()))?;
                }
                if let Some(observed) = &source.observed {
                    observed.validate()?;
                }
                if let Some(endpoint) = &source.public_endpoint {
                    if endpoint.is_empty() || endpoint.len() > MAX_ENDPOINT_BYTES {
                        return invalid(format!(
                            "public_endpoint must be 1..={MAX_ENDPOINT_BYTES} bytes"
                        ));
                    }
                    if self.state == EnvironmentState::Closed {
                        return invalid("a closed environment has no public_endpoint");
                    }
                }
            }
            EnvironmentSource::External { .. } => self.validate_open_or_closed("external")?,
            EnvironmentSource::Registered(source) => {
                self.validate_open_or_closed("registered")?;
                parse_daemon_public_key(&source.daemon_public_key)?;
                validate_nonnegative_i64(
                    source.connection_changed_at_ms,
                    "connection_changed_at_ms",
                )?;
                validate_nonnegative_i64(source.last_seen_at_ms, "last_seen_at_ms")?;
            }
        }
        Ok(())
    }

    fn validate_open_or_closed(&self, kind: &str) -> Result<(), EnvironmentRegistryError> {
        if !matches!(
            self.state,
            EnvironmentState::Open | EnvironmentState::Closed
        ) {
            return invalid(format!("{kind} environments are only open or closed"));
        }
        Ok(())
    }

    fn provisioned_mut(&mut self) -> Option<&mut ProvisionedSource> {
        match &mut self.source {
            EnvironmentSource::Provisioned(source) => Some(source),
            EnvironmentSource::External { .. } | EnvironmentSource::Registered(_) => None,
        }
    }

    fn registered_mut(&mut self) -> Option<&mut RegisteredSource> {
        match &mut self.source {
            EnvironmentSource::Registered(source) => Some(source),
            EnvironmentSource::Provisioned(_) | EnvironmentSource::External { .. } => None,
        }
    }

    /// Move to `state` at `at_ms`, clamped so the row's times stay ordered.
    /// Returns the clamped time.
    fn transition(&mut self, state: EnvironmentState, at_ms: i64) -> i64 {
        let at = at_ms.max(self.created_at_ms).max(self.state_changed_at_ms);
        self.state = state;
        self.state_changed_at_ms = at;
        self.touch(at);
        at
    }

    fn touch(&mut self, at_ms: i64) {
        self.updated_at_ms = self.updated_at_ms.max(at_ms);
    }

    /// Clear the work schedule of a provisioned row.
    fn clear_schedule(&mut self) {
        if let Some(source) = self.provisioned_mut() {
            source.next_attempt_at_ms = None;
            source.attempts = 0;
        }
    }

    fn close(&mut self, reason: EnvironmentCloseReason, at_ms: i64) {
        let at = self.transition(EnvironmentState::Closed, at_ms);
        self.close_reason = Some(reason);
        self.closed_at_ms = Some(at);
        self.state_message = None;
        self.clear_schedule();
        // A closed target serves nothing.
        if let Some(source) = self.provisioned_mut() {
            source.public_endpoint = None;
        }
    }
}

fn gone(reason: EnvironmentGoneReason, message: Option<String>) -> EnvironmentUse {
    EnvironmentUse::Gone { reason, message }
}

/// A `state_message` built from any text: control characters cleaned and
/// cut to [`STATE_MESSAGE_MAX_BYTES`]. `None` when nothing is left.
pub fn clean_state_message(text: &str) -> Option<String> {
    DescriptiveText::lossy(text).map(DescriptiveText::into_string)
}

// ---------------------------------------------------------------------------
// Creates
// ---------------------------------------------------------------------------

/// Create a provisioned environment. The store checks that the universe's
/// binding of `provider_id` exists (`NotFound`) and is enabled
/// (`InvalidInput`). The same `request_id` with identical inputs returns the
/// stored row; with different inputs it is `AlreadyExists`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateEnvironment {
    pub request_id: EnvironmentProvisionRequestId,
    pub environment_id: EnvironmentId,
    pub provider_id: EnvironmentProviderId,
    pub template_id: EnvironmentTemplateId,
    pub idle: Option<IdlePolicy>,
    pub display_name: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub created_at_ms: i64,
}

impl CreateEnvironment {
    /// The validated record this create inserts: `provisioning`, due at
    /// once.
    pub fn record(&self) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let record = new_record(
            self.environment_id.clone(),
            Some(self.request_id.clone()),
            EnvironmentSource::Provisioned(ProvisionedSource {
                provider_id: self.provider_id.clone(),
                template_id: self.template_id.clone(),
                idle: self.idle.clone(),
                provider_target_id: None,
                observed: None,
                public_endpoint: None,
                next_attempt_at_ms: Some(self.created_at_ms),
                attempts: 0,
            }),
            EnvironmentState::Provisioning,
            self.display_name.clone(),
            self.metadata.clone(),
            None,
            self.created_at_ms,
        );
        record.validate()?;
        Ok(record)
    }

    /// True when `record` is what this create made: a retry with the same
    /// inputs. The environment id and the time of a retry are not inputs.
    pub fn matches(&self, record: &EnvironmentRecord) -> bool {
        record.request_id.as_ref() == Some(&self.request_id)
            && record.display_name == self.display_name
            && record.metadata == self.metadata
            && record.provisioned().is_some_and(|source| {
                source.provider_id == self.provider_id
                    && source.template_id == self.template_id
                    && source.idle == self.idle
            })
    }
}

/// Create an external environment, `open` at once. The same `request_id`
/// with identical inputs returns the stored row; with different inputs it is
/// `AlreadyExists`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateExternalEnvironment {
    pub request_id: EnvironmentProvisionRequestId,
    pub environment_id: EnvironmentId,
    pub endpoint: EnvironmentEndpoint,
    pub display_name: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub created_at_ms: i64,
}

impl CreateExternalEnvironment {
    /// The validated record this create inserts.
    pub fn record(&self) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let record = new_record(
            self.environment_id.clone(),
            Some(self.request_id.clone()),
            EnvironmentSource::External {
                endpoint: self.endpoint.clone(),
            },
            EnvironmentState::Open,
            self.display_name.clone(),
            self.metadata.clone(),
            None,
            self.created_at_ms,
        );
        record.validate()?;
        Ok(record)
    }

    /// True when `record` is what this create made.
    pub fn matches(&self, record: &EnvironmentRecord) -> bool {
        record.request_id.as_ref() == Some(&self.request_id)
            && record.display_name == self.display_name
            && record.metadata == self.metadata
            && matches!(&record.source, EnvironmentSource::External { endpoint } if endpoint == &self.endpoint)
    }
}

/// Admission of a first-seen daemon identity. The store locks the key,
/// re-checks that it still admits and that the presented secret is still
/// its live one, counts the key's non-closed environments against its
/// limit, and inserts the environment `open` and connected, all in one
/// transaction. Rotation takes the same lock, so a secret it retired admits
/// nothing once the rotation commits. A retry with the same daemon public
/// key returns the row it admitted (closed included: the identity is
/// spent).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateRegisteredEnvironment {
    pub registration_key_id: EnvironmentRegistrationKeyId,
    /// [`registration_key_hash`] of the secret the daemon presented.
    pub secret_hash: String,
    pub environment_id: EnvironmentId,
    /// Lowercase hex of the raw Ed25519 public key.
    pub daemon_public_key: String,
    pub display_name: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub daemon_build: DaemonBuild,
    pub created_at_ms: i64,
}

impl CreateRegisteredEnvironment {
    /// The refusal when the presented secret is no longer the key's live
    /// one: a rotation retired it after the caller resolved the key.
    pub fn secret_retired(&self) -> EnvironmentRegistryError {
        EnvironmentRegistryError::RegistrationKeyUnavailable {
            registration_key_id: self.registration_key_id.to_string(),
            reason: "rotated",
        }
    }

    pub fn daemon_id(&self) -> Result<EnvironmentDaemonId, EnvironmentRegistryError> {
        let public_key = parse_daemon_public_key(&self.daemon_public_key)?;
        Ok(EnvironmentDaemonId::from_public_key(public_key.as_bytes()))
    }

    /// The validated record this admission inserts under `key`.
    pub fn record(
        &self,
        key: &EnvironmentRegistrationKeyRecord,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        if key.registration_key_id != self.registration_key_id {
            return invalid("registered environment admitted under another key");
        }
        let record = new_record(
            self.environment_id.clone(),
            None,
            EnvironmentSource::Registered(RegisteredSource {
                registration_key_id: key.registration_key_id.clone(),
                daemon_public_key: self.daemon_public_key.clone(),
                identity_mode: key.identity_mode,
                connected: true,
                connection_changed_at_ms: self.created_at_ms,
                last_seen_at_ms: self.created_at_ms,
            }),
            EnvironmentState::Open,
            self.display_name.clone(),
            self.metadata.clone(),
            Some(self.daemon_build.clone()),
            self.created_at_ms,
        );
        record.validate()?;
        Ok(record)
    }
}

#[allow(clippy::too_many_arguments)]
fn new_record(
    environment_id: EnvironmentId,
    request_id: Option<EnvironmentProvisionRequestId>,
    source: EnvironmentSource,
    state: EnvironmentState,
    display_name: Option<String>,
    metadata: BTreeMap<String, String>,
    daemon_build: Option<DaemonBuild>,
    created_at_ms: i64,
) -> EnvironmentRecord {
    EnvironmentRecord {
        environment_id,
        request_id,
        source,
        display_name,
        metadata,
        state,
        state_message: None,
        close_reason: None,
        state_changed_at_ms: created_at_ms,
        daemon_build,
        created_at_ms,
        updated_at_ms: created_at_ms,
        closed_at_ms: None,
    }
}

// ---------------------------------------------------------------------------
// Writes
//
// Each `apply` changes `record` in place and returns whether it changed
// anything; a write whose expected state no longer holds returns `false` and
// leaves the record as it was. The stores validate the result before they
// keep it.
// ---------------------------------------------------------------------------

/// Replace the display name and caller metadata. Refused for a closed
/// environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateEnvironment {
    pub environment_id: EnvironmentId,
    pub display_name: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub updated_at_ms: i64,
}

impl UpdateEnvironment {
    pub fn apply(&self, record: &mut EnvironmentRecord) -> Result<bool, EnvironmentRegistryError> {
        validate_nonnegative_i64(self.updated_at_ms, "updated_at_ms")?;
        if record.state == EnvironmentState::Closed {
            return invalid("environment closed");
        }
        record.display_name = self.display_name.clone();
        record.metadata = self.metadata.clone();
        record.touch(self.updated_at_ms);
        Ok(true)
    }
}

/// A known registered daemon reconnected: connected, both connection stamps
/// set, and its build replaced (it may be a newer one). Refused with
/// `InvalidInput` when the environment is closed: the identity is spent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkRegisteredConnected {
    pub environment_id: EnvironmentId,
    pub daemon_build: DaemonBuild,
    pub at_ms: i64,
}

impl MarkRegisteredConnected {
    pub fn apply(&self, record: &mut EnvironmentRecord) -> Result<bool, EnvironmentRegistryError> {
        validate_nonnegative_i64(self.at_ms, "at_ms")?;
        if record.state == EnvironmentState::Closed {
            return invalid("environment closed");
        }
        let Some(source) = record.registered_mut() else {
            return invalid("connection writes apply only to registered environments");
        };
        source.connected = true;
        source.connection_changed_at_ms = self.at_ms;
        source.last_seen_at_ms = self.at_ms;
        record.daemon_build = Some(self.daemon_build.clone());
        record.touch(self.at_ms);
        Ok(true)
    }
}

/// The current control connection of a registered daemon ended. Written
/// only while the row reads connected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkRegisteredDisconnected {
    pub environment_id: EnvironmentId,
    pub at_ms: i64,
}

impl MarkRegisteredDisconnected {
    pub fn apply(&self, record: &mut EnvironmentRecord) -> Result<bool, EnvironmentRegistryError> {
        validate_nonnegative_i64(self.at_ms, "at_ms")?;
        let Some(source) = record.registered_mut() else {
            return invalid("connection writes apply only to registered environments");
        };
        if !source.connected {
            return Ok(false);
        }
        source.connected = false;
        source.connection_changed_at_ms = self.at_ms;
        record.touch(self.at_ms);
        Ok(true)
    }
}

/// The coarse liveness stamp of a connected registered daemon: written only
/// while open and connected, and only once the last stamp is at least
/// [`REGISTERED_LIVENESS_STAMP_MS`] old.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StampRegisteredSeen {
    pub environment_id: EnvironmentId,
    pub at_ms: i64,
}

impl StampRegisteredSeen {
    pub fn apply(&self, record: &mut EnvironmentRecord) -> Result<bool, EnvironmentRegistryError> {
        validate_nonnegative_i64(self.at_ms, "at_ms")?;
        let open = record.state == EnvironmentState::Open;
        let Some(source) = record.registered_mut() else {
            return invalid("connection writes apply only to registered environments");
        };
        if !open
            || !source.connected
            || source.last_seen_at_ms > self.at_ms.saturating_sub(REGISTERED_LIVENESS_STAMP_MS)
        {
            return Ok(false);
        }
        source.last_seen_at_ms = self.at_ms;
        record.touch(self.at_ms);
        Ok(true)
    }
}

/// What one registered sweep changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegisteredSweep {
    /// Rows that read connected with a stale liveness stamp, now
    /// disconnected as of that stamp.
    pub disconnected: Vec<EnvironmentId>,
    /// Rows the sweep closed, with the reason.
    pub closed: Vec<(EnvironmentId, EnvironmentCloseReason)>,
}

/// One registered sweep step for an open registered row under its key, at
/// `now_ms`. In order:
///
/// 1. connected with `last_seen_at_ms < now - REGISTERED_STALE_AFTER_MS` →
///    disconnected, `connection_changed_at_ms = last_seen_at_ms`;
/// 2. disconnected, ephemeral, away longer than the key's grace → closed
///    (`ephemeral_grace`);
/// 3. disconnected, persistent, the key sets `offline_close_after_ms` and
///    the daemon is away longer → closed (`offline_timeout`).
///
/// Returns whether step 1 applied and the close reason of step 2 or 3.
pub fn sweep_registered_environment(
    record: &mut EnvironmentRecord,
    key: &EnvironmentRegistrationKeyRecord,
    now_ms: i64,
) -> (bool, Option<EnvironmentCloseReason>) {
    if record.state != EnvironmentState::Open {
        return (false, None);
    }
    let Some(source) = record.registered_mut() else {
        return (false, None);
    };
    let mut disconnected = false;
    if source.connected && source.last_seen_at_ms < now_ms.saturating_sub(REGISTERED_STALE_AFTER_MS)
    {
        source.connected = false;
        source.connection_changed_at_ms = source.last_seen_at_ms;
        disconnected = true;
    }
    let close = if source.connected {
        None
    } else {
        let away_ms = now_ms.saturating_sub(source.connection_changed_at_ms);
        match source.identity_mode {
            RegisteredIdentityMode::Ephemeral
                if away_ms > duration_ms(key.ephemeral_disconnect_grace_ms()) =>
            {
                Some(EnvironmentCloseReason::EphemeralGrace)
            }
            RegisteredIdentityMode::Persistent
                if key
                    .offline_close_after_ms
                    .is_some_and(|after| away_ms > duration_ms(after)) =>
            {
                Some(EnvironmentCloseReason::OfflineTimeout)
            }
            _ => None,
        }
    };
    if disconnected {
        record.touch(now_ms);
    }
    if let Some(reason) = close {
        record.close(reason, now_ms);
    }
    (disconnected, close)
}

fn duration_ms(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// A span for people to read, such as `45s`, `3m12s` or `1h5m`. Negative
/// spans read as `0s`.
fn compact_duration(ms: i64) -> String {
    let seconds = ms.max(0) / 1_000;
    let (hours, minutes, seconds) = (seconds / 3_600, seconds / 60 % 60, seconds % 60);
    match (hours, minutes, seconds) {
        (0, 0, seconds) => format!("{seconds}s"),
        (0, minutes, 0) => format!("{minutes}m"),
        (0, minutes, seconds) => format!("{minutes}m{seconds}s"),
        (hours, 0, _) => format!("{hours}h"),
        (hours, minutes, _) => format!("{hours}h{minutes}m"),
    }
}

/// What the provider reported about a provisioned environment's target, in
/// any state. The target id is recorded (a different id than the recorded
/// one is refused); the observation and public endpoint replace the stored
/// ones unless an observation stamped later is already stored. The stored
/// observation is stamped with `observed_at_ms`. A closed row keeps no
/// public endpoint. Never changes the state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordTargetObservation {
    pub environment_id: EnvironmentId,
    /// Absent while an asynchronous provider has not assigned one yet; the
    /// recorded id is kept.
    pub provider_target_id: Option<ProviderTargetId>,
    pub observed: ObservedTarget,
    pub public_endpoint: Option<String>,
    pub observed_at_ms: i64,
}

impl RecordTargetObservation {
    pub fn apply(&self, record: &mut EnvironmentRecord) -> Result<bool, EnvironmentRegistryError> {
        validate_nonnegative_i64(self.observed_at_ms, "observed_at_ms")?;
        let closed = record.state == EnvironmentState::Closed;
        let Some(source) = record.provisioned_mut() else {
            return invalid("provider observations apply only to provisioned environments");
        };
        if let (Some(recorded), Some(observed)) = (
            source.provider_target_id.as_ref(),
            self.provider_target_id.as_ref(),
        ) && recorded != observed
        {
            return invalid("provider target conflicts with the recorded target");
        }
        let mut changed = false;
        if source.provider_target_id.is_none() && self.provider_target_id.is_some() {
            source.provider_target_id = self.provider_target_id.clone();
            changed = true;
        }
        let fresh = source
            .observed
            .as_ref()
            .is_none_or(|previous| self.observed_at_ms >= previous.observed_at_ms);
        if fresh {
            source.observed = Some(ObservedTarget {
                observed_at_ms: self.observed_at_ms,
                ..self.observed.clone()
            });
            if !closed {
                source.public_endpoint = self.public_endpoint.clone();
            }
            changed = true;
        }
        if changed {
            record.touch(self.observed_at_ms);
        }
        Ok(changed)
    }
}

/// Reschedule the work of a provisioning or closing row: the next attempt,
/// the attempt count and what it waits on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleEnvironmentAttempt {
    pub environment_id: EnvironmentId,
    /// The state of the row the step read, `provisioning` or `closing`. The
    /// write is skipped when the row has moved on, so a close that lands
    /// while a provision step is in flight keeps its own schedule.
    pub expected_state: EnvironmentState,
    pub next_attempt_at_ms: i64,
    pub attempts: u32,
    /// Cleaned and cut by [`clean_state_message`].
    pub state_message: Option<String>,
    pub at_ms: i64,
}

impl ScheduleEnvironmentAttempt {
    pub fn apply(&self, record: &mut EnvironmentRecord) -> Result<bool, EnvironmentRegistryError> {
        validate_nonnegative_i64(self.at_ms, "at_ms")?;
        validate_nonnegative_i64(self.next_attempt_at_ms, "next_attempt_at_ms")?;
        if record.state != self.expected_state
            || !matches!(
                record.state,
                EnvironmentState::Provisioning | EnvironmentState::Closing
            )
        {
            return Ok(false);
        }
        let Some(source) = record.provisioned_mut() else {
            return Ok(false);
        };
        source.next_attempt_at_ms = Some(self.next_attempt_at_ms);
        source.attempts = self.attempts;
        record.state_message = self.state_message.as_deref().and_then(clean_state_message);
        record.touch(self.at_ms);
        Ok(true)
    }
}

/// The open handshake of a provisioning row succeeded: `open`, schedule and
/// message cleared, the daemon's build recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenEnvironment {
    pub environment_id: EnvironmentId,
    pub daemon_build: DaemonBuild,
    pub at_ms: i64,
}

impl OpenEnvironment {
    pub fn apply(&self, record: &mut EnvironmentRecord) -> Result<bool, EnvironmentRegistryError> {
        validate_nonnegative_i64(self.at_ms, "at_ms")?;
        if record.provisioned().is_none() || record.state != EnvironmentState::Provisioning {
            return Ok(false);
        }
        record.transition(EnvironmentState::Open, self.at_ms);
        record.clear_schedule();
        record.state_message = None;
        record.daemon_build = Some(self.daemon_build.clone());
        Ok(true)
    }
}

/// A provisioning or open provisioned row is broken: `failed` with
/// `message`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailEnvironment {
    pub environment_id: EnvironmentId,
    pub message: String,
    /// The daemon whose build made the environment unusable, such as one
    /// that speaks another protocol version.
    pub daemon_build: Option<DaemonBuild>,
    pub at_ms: i64,
}

impl FailEnvironment {
    pub fn apply(&self, record: &mut EnvironmentRecord) -> Result<bool, EnvironmentRegistryError> {
        validate_nonnegative_i64(self.at_ms, "at_ms")?;
        if record.provisioned().is_none() {
            return invalid("only provisioned environments fail");
        }
        if !matches!(
            record.state,
            EnvironmentState::Provisioning | EnvironmentState::Open
        ) {
            return Ok(false);
        }
        record.transition(EnvironmentState::Failed, self.at_ms);
        record.clear_schedule();
        record.state_message = Some(
            clean_state_message(&self.message).unwrap_or_else(|| "unspecified failure".to_owned()),
        );
        if let Some(build) = &self.daemon_build {
            record.daemon_build = Some(build.clone());
        }
        Ok(true)
    }
}

/// Begin closing. A provisioned row moves from provisioning, open or failed
/// to `closing`, due at once; an external or registered row moves from open
/// straight to `closed`. A closing or closed row is left as it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BeginCloseEnvironment {
    pub environment_id: EnvironmentId,
    pub reason: EnvironmentCloseReason,
    pub at_ms: i64,
}

impl BeginCloseEnvironment {
    pub fn apply(&self, record: &mut EnvironmentRecord) -> Result<bool, EnvironmentRegistryError> {
        validate_nonnegative_i64(self.at_ms, "at_ms")?;
        if matches!(
            record.state,
            EnvironmentState::Closing | EnvironmentState::Closed
        ) {
            return Ok(false);
        }
        if record.provisioned().is_some() {
            let at = record.transition(EnvironmentState::Closing, self.at_ms);
            record.close_reason = Some(self.reason);
            record.state_message = None;
            if let Some(source) = record.provisioned_mut() {
                source.next_attempt_at_ms = Some(at);
                source.attempts = 0;
            }
        } else {
            record.close(self.reason, self.at_ms);
        }
        Ok(true)
    }
}

/// The provider confirmed a closing row's target gone: `closed`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinishCloseEnvironment {
    pub environment_id: EnvironmentId,
    pub at_ms: i64,
}

impl FinishCloseEnvironment {
    pub fn apply(&self, record: &mut EnvironmentRecord) -> Result<bool, EnvironmentRegistryError> {
        validate_nonnegative_i64(self.at_ms, "at_ms")?;
        if record.state != EnvironmentState::Closing {
            return Ok(false);
        }
        let reason = record
            .close_reason
            .unwrap_or(EnvironmentCloseReason::Requested);
        record.close(reason, self.at_ms);
        Ok(true)
    }
}

/// The provider reported an open or failed row's target closed or not
/// found: `closed` with `provider_ended`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndProvisionedEnvironment {
    pub environment_id: EnvironmentId,
    pub at_ms: i64,
}

impl EndProvisionedEnvironment {
    pub fn apply(&self, record: &mut EnvironmentRecord) -> Result<bool, EnvironmentRegistryError> {
        validate_nonnegative_i64(self.at_ms, "at_ms")?;
        if record.provisioned().is_none()
            || !matches!(
                record.state,
                EnvironmentState::Open | EnvironmentState::Failed
            )
        {
            return Ok(false);
        }
        record.close(EnvironmentCloseReason::ProviderEnded, self.at_ms);
        Ok(true)
    }
}

/// One page of environments, ordered by environment id. Closed rows are
/// listed only with `include_closed` or `state == Closed`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListEnvironments {
    pub provider_id: Option<EnvironmentProviderId>,
    /// Only environments admitted by this registration key.
    pub registration_key_id: Option<EnvironmentRegistrationKeyId>,
    pub state: Option<EnvironmentState>,
    pub include_closed: bool,
    /// Only environments carrying every listed metadata pair (containment).
    pub metadata: BTreeMap<String, String>,
    /// 1 to [`LIST_ENVIRONMENTS_MAX_LIMIT`].
    pub limit: u32,
    /// Continue after this environment id (exclusive).
    pub after: Option<EnvironmentId>,
}

impl Default for ListEnvironments {
    fn default() -> Self {
        Self {
            provider_id: None,
            registration_key_id: None,
            state: None,
            include_closed: false,
            metadata: BTreeMap::new(),
            limit: LIST_ENVIRONMENTS_DEFAULT_LIMIT,
            after: None,
        }
    }
}

impl ListEnvironments {
    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        if !(1..=LIST_ENVIRONMENTS_MAX_LIMIT).contains(&self.limit) {
            return invalid(format!(
                "list limit must be 1..={LIST_ENVIRONMENTS_MAX_LIMIT}"
            ));
        }
        Ok(())
    }

    /// Whether `record` passes every filter except the page bounds.
    pub fn matches(&self, record: &EnvironmentRecord) -> bool {
        let closed_listed = self.include_closed || self.state == Some(EnvironmentState::Closed);
        (record.state != EnvironmentState::Closed || closed_listed)
            && self.state.is_none_or(|state| state == record.state)
            && self
                .provider_id
                .as_ref()
                .is_none_or(|id| record.provider_id() == Some(id))
            && self
                .registration_key_id
                .as_ref()
                .is_none_or(|id| record.registration_key_id() == Some(id))
            && harness::storage::metadata_matches(&record.metadata, &self.metadata)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvironmentPage {
    pub environments: Vec<EnvironmentRecord>,
    /// Pass as `after` for the next page; `None` on the last page.
    pub next_after: Option<EnvironmentId>,
}

// ---------------------------------------------------------------------------
// Environment credentials
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentCredentialRecord {
    pub environment_id: EnvironmentId,
    pub env_name: String,
    pub source: EnvironmentCredentialSource,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl EnvironmentCredentialRecord {
    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        validate_env_name(&self.env_name)?;
        validate_timestamps(self.created_at_ms, self.updated_at_ms)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutEnvironmentCredential {
    pub environment_id: EnvironmentId,
    pub env_name: String,
    pub source: EnvironmentCredentialSource,
    pub created_at_ms: i64,
}

impl PutEnvironmentCredential {
    pub fn into_record(self) -> EnvironmentCredentialRecord {
        EnvironmentCredentialRecord {
            environment_id: self.environment_id,
            env_name: self.env_name,
            source: self.source,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.created_at_ms,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EnvironmentCredentialSource {
    AuthGrant { grant_id: AuthGrantId },
    AuthProviderCredential { provider_id: AuthProviderId },
    DirectSecret { secret_id: SecretId },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListEnvironmentCredentials {
    pub environment_id: EnvironmentId,
}

// ---------------------------------------------------------------------------
// Store traits
// ---------------------------------------------------------------------------

/// Deployment-scoped providers.
#[async_trait]
pub trait EnvironmentProviderStore: Send + Sync {
    async fn put_provider(
        &self,
        request: PutEnvironmentProvider,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError>;
    async fn read_provider(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError>;
    /// The provider's bearer credential, unsealed; `None` when it has none.
    async fn read_provider_credential(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<Option<SecretValue>, EnvironmentRegistryError>;
    async fn list_providers(
        &self,
    ) -> Result<Vec<EnvironmentProviderRecord>, EnvironmentRegistryError>;
    /// Refused while bindings or non-closed environments reference it.
    async fn delete_provider(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError>;
}

/// A universe's provider bindings, one per provider.
#[async_trait]
pub trait EnvironmentProviderBindingStore: Send + Sync {
    /// The provider must exist.
    async fn put_provider_binding(
        &self,
        request: PutEnvironmentProviderBinding,
    ) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError>;
    async fn read_provider_binding(
        &self,
        universe_id: Uuid,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError>;
    async fn list_provider_bindings(
        &self,
        universe_id: Uuid,
    ) -> Result<Vec<EnvironmentProviderBindingRecord>, EnvironmentRegistryError>;
    /// Refused while the universe has non-closed environments of the
    /// provider.
    async fn delete_provider_binding(
        &self,
        universe_id: Uuid,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError>;
}

/// A universe's environments. Lifecycle writes return the current record,
/// whether they applied or were skipped.
#[async_trait]
pub trait EnvironmentStore: Send + Sync {
    async fn create_environment(
        &self,
        request: CreateEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn create_external_environment(
        &self,
        request: CreateExternalEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn create_registered_environment(
        &self,
        request: CreateRegisteredEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn read_environment(
        &self,
        environment_id: &EnvironmentId,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    /// The one environment (closed included) bound to a daemon public key in
    /// this universe.
    async fn read_environment_by_daemon_public_key(
        &self,
        daemon_public_key: &str,
    ) -> Result<Option<EnvironmentRecord>, EnvironmentRegistryError>;
    /// Rows that no longer decode are left out and logged by the store, so
    /// one bad row never hides the others; every list method does the same.
    async fn list_environments(
        &self,
        request: ListEnvironments,
    ) -> Result<EnvironmentPage, EnvironmentRegistryError>;
    async fn update_environment(
        &self,
        request: UpdateEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn begin_close_environment(
        &self,
        request: BeginCloseEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn finish_close_environment(
        &self,
        request: FinishCloseEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;

    async fn mark_registered_connected(
        &self,
        request: MarkRegisteredConnected,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn mark_registered_disconnected(
        &self,
        request: MarkRegisteredDisconnected,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    /// Whether the stamp was written.
    async fn stamp_registered_seen(
        &self,
        request: StampRegisteredSeen,
    ) -> Result<bool, EnvironmentRegistryError>;
    /// Apply [`sweep_registered_environment`] to every open registered row
    /// under its key.
    async fn sweep_registered_environments(
        &self,
        now_ms: i64,
    ) -> Result<RegisteredSweep, EnvironmentRegistryError>;
    /// After a gateway restart: every connected row becomes disconnected
    /// with `connection_changed_at_ms = now_ms`, since the connections the
    /// gateway held ended now, and persistent rows get their full reconnect
    /// wait. `last_seen_at_ms` is kept. Returns the rows it changed.
    async fn repair_registered_connections(
        &self,
        now_ms: i64,
    ) -> Result<u64, EnvironmentRegistryError>;

    /// Provisioned rows whose next attempt is due at `now_ms`, earliest
    /// first.
    async fn list_environment_work(
        &self,
        now_ms: i64,
        limit: u32,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError>;
    /// Open provisioned rows: the display sweep's candidates.
    async fn list_open_provisioned_environments(
        &self,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError>;
    async fn record_target_observation(
        &self,
        request: RecordTargetObservation,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn schedule_environment_attempt(
        &self,
        request: ScheduleEnvironmentAttempt,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn open_environment(
        &self,
        request: OpenEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn fail_environment(
        &self,
        request: FailEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn end_provisioned_environment(
        &self,
        request: EndProvisionedEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
}

#[async_trait]
pub trait EnvironmentCredentialStore: Send + Sync {
    async fn bind_credential(
        &self,
        record: PutEnvironmentCredential,
    ) -> Result<EnvironmentCredentialRecord, EnvironmentRegistryError>;
    async fn list_credentials(
        &self,
        request: ListEnvironmentCredentials,
    ) -> Result<Vec<EnvironmentCredentialRecord>, EnvironmentRegistryError>;
    async fn unbind_credential(
        &self,
        environment_id: &EnvironmentId,
        env_name: &str,
    ) -> Result<EnvironmentCredentialRecord, EnvironmentRegistryError>;
}

pub fn template_record(
    value: &EnvironmentTemplate,
) -> Result<EnvironmentTemplateId, EnvironmentRegistryError> {
    let template_id =
        EnvironmentTemplateId::try_new(value.template_id.as_str()).map_err(|error| {
            EnvironmentRegistryError::InvalidInput {
                message: format!("invalid template id: {error}"),
            }
        })?;
    Ok(template_id)
}

pub mod endpoint;
pub use endpoint::{EndpointError, EndpointScheme, EnvironmentEndpoint, MAX_ENDPOINT_BYTES};
mod memory;
pub use memory::InMemoryEnvironmentRegistryStore;

fn invalid<T>(message: impl Into<String>) -> Result<T, EnvironmentRegistryError> {
    Err(invalid_error(message))
}

fn invalid_error(message: impl Into<String>) -> EnvironmentRegistryError {
    EnvironmentRegistryError::InvalidInput {
        message: message.into(),
    }
}

fn validate_timestamps(
    created_at_ms: i64,
    updated_at_ms: i64,
) -> Result<(), EnvironmentRegistryError> {
    validate_nonnegative_i64(created_at_ms, "created_at_ms")?;
    validate_nonnegative_i64(updated_at_ms, "updated_at_ms")?;
    if updated_at_ms < created_at_ms {
        return invalid("updated_at_ms must be >= created_at_ms");
    }
    Ok(())
}

fn validate_registration_display_name(value: &str) -> Result<(), EnvironmentRegistryError> {
    validate_nonempty_string("registration key display_name", value)?;
    if value.len() > REGISTRATION_KEY_DISPLAY_NAME_MAX_BYTES
        || value.chars().any(char::is_control)
        || value.trim() != value
    {
        return invalid(format!(
            "registration key display_name must be at most {REGISTRATION_KEY_DISPLAY_NAME_MAX_BYTES} bytes, trimmed, and contain no control characters"
        ));
    }
    Ok(())
}

/// A registration secret hash is lowercase hex SHA-256.
fn validate_secret_hash(value: &str) -> Result<(), EnvironmentRegistryError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return invalid("registration secret hash must be 64 lowercase hex characters");
    }
    Ok(())
}

/// A daemon public key is the raw 32-byte Ed25519 key as lowercase hex.
pub fn validate_daemon_public_key(value: &str) -> Result<(), EnvironmentRegistryError> {
    parse_daemon_public_key(value).map(|_| ())
}

fn parse_daemon_public_key(value: &str) -> Result<DaemonPublicKey, EnvironmentRegistryError> {
    DaemonPublicKey::parse(value)
        .map_err(|_| invalid_error("daemon public key must be 64 lowercase hex characters"))
}

fn validate_nonempty_optional(
    name: &'static str,
    value: Option<&str>,
) -> Result<(), EnvironmentRegistryError> {
    if value.is_some_and(str::is_empty) {
        return invalid(format!("{name} must not be empty"));
    }
    Ok(())
}

fn validate_nonempty_string(
    name: &'static str,
    value: &str,
) -> Result<(), EnvironmentRegistryError> {
    if value.is_empty() {
        return invalid(format!("{name} must not be empty"));
    }
    Ok(())
}

pub(crate) fn validate_nonnegative_i64(
    value: i64,
    name: &'static str,
) -> Result<(), EnvironmentRegistryError> {
    if value < 0 {
        return invalid(format!("{name} must be nonnegative"));
    }
    Ok(())
}

fn validate_env_name(value: &str) -> Result<(), EnvironmentRegistryError> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return invalid("credential env_name must not be empty");
    };
    if !(first == '_' || first.is_ascii_alphabetic())
        || chars.any(|value| !(value == '_' || value.is_ascii_alphanumeric()))
    {
        return invalid("credential env_name must match [A-Za-z_][A-Za-z0-9_]*");
    }
    Ok(())
}

pub(crate) fn not_found(kind: &'static str, id: &impl ToString) -> EnvironmentRegistryError {
    EnvironmentRegistryError::NotFound {
        kind,
        id: id.to_string(),
    }
}

#[cfg(test)]
mod tests;
