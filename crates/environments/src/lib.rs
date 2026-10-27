//! Environment-compute domain contracts.
//!
//! Physical providers are deployment-scoped, provider bindings and
//! environments are universe-scoped, and provider target facts live on an
//! environment incarnation rather than on stable environment identity.

use std::{collections::BTreeMap, fmt, str::FromStr};

use async_trait::async_trait;
use auth::{AuthGrantId, AuthProviderId, SecretId, SecretValue};
pub use environment_protocol::control::target::PowerState;
use environment_protocol::{
    control::template::EnvironmentTemplate,
    handshake::ImplementationInfo,
    ids::{ProviderTargetId, validate_opaque_id},
    metadata::{DescriptiveText, validate_descriptive_text, validate_stored_metadata},
    routes::DataRouteBase,
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
registry_string_id!(EnvironmentProviderBindingId);
registry_string_id!(EnvironmentIncarnationId);
registry_string_id!(EnvironmentProvisionRequestId);
registry_string_id!(EnvironmentTemplateId);
registry_string_id!(EnvironmentJobGroupId);
registry_string_id!(EnvironmentRegistrationKeyId);
registry_string_id!(EnvironmentDaemonId);

impl EnvironmentProvisionRequestId {
    /// The deterministic request id of the one environment a daemon identity
    /// may register, so a retried first registration converges on the same
    /// environment through the `(universe, request_id)` unique key.
    pub fn for_daemon(daemon_id: &EnvironmentDaemonId) -> Self {
        Self::new(format!(
            "{DAEMON_PROVISION_REQUEST_PREFIX}{}",
            daemon_id.as_str()
        ))
    }
}

/// Prefix of the request id a registered environment derives from its daemon
/// identity.
pub const DAEMON_PROVISION_REQUEST_PREFIX: &str = "daemon:";

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

    #[error(
        "environment registry revision conflict for {kind} {id}: expected {expected:?}, actual {actual:?}"
    )]
    RevisionConflict {
        kind: &'static str,
        id: String,
        expected: Option<u64>,
        actual: Option<u64>,
    },

    #[error("invalid environment registry request: {message}")]
    InvalidInput { message: String },

    /// The registration key exists but no longer admits new daemon
    /// identities: revoked or past its expiry.
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

/// Optional controller methods a provider's controller advertised in its
/// last successful `initialize`. Calls and API requests that need an absent
/// feature are refused before they reach the provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerFeatures {
    /// `target/adopt` is served.
    pub adopt_target: bool,
    /// `ingress/ensure` and `ingress/remove` are served.
    pub ingress: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentProviderRecord {
    pub provider_id: EnvironmentProviderId,
    pub display_name: Option<String>,
    /// Where Lightspeed dials the provider's controller.
    pub controller_endpoint: EnvironmentEndpoint,
    pub metadata: BTreeMap<String, String>,
    /// Recorded at the controller's last successful `initialize`, together
    /// with `data_route_base`; absent until then and after the endpoint
    /// changes.
    pub controller_features: Option<ControllerFeatures>,
    /// Base URL of the provider's data routes as its controller advertised
    /// it (`routes.data`). The gateway builds every target's data route from
    /// it; it is never derived from the controller endpoint.
    pub data_route_base: Option<DataRouteBase>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl EnvironmentProviderRecord {
    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        validate_nonempty_optional("display_name", self.display_name.as_deref())?;
        validate_metadata(&self.metadata)?;
        validate_timestamps(self.created_at_ms, self.updated_at_ms)?;
        if self.controller_features.is_some() != self.data_route_base.is_some() {
            return invalid("controller features and data route base are recorded together");
        }
        Ok(())
    }
}

/// Register or replace a provider. The provider credential is write-only:
/// the store keeps it sealed and hands it out only through
/// [`EnvironmentProviderStore::read_provider_credential`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PutEnvironmentProvider {
    pub provider_id: EnvironmentProviderId,
    pub display_name: Option<String>,
    pub controller_endpoint: EnvironmentEndpoint,
    pub metadata: BTreeMap<String, String>,
    /// The bearer credential Lightspeed presents on the controller and data
    /// routes. Required for a new provider; `None` keeps the stored one.
    pub credential: Option<SecretValue>,
    pub updated_at_ms: i64,
}

impl PutEnvironmentProvider {
    /// The record this put writes for a provider whose current record is
    /// `existing`: the creation time is kept, and the recorded controller
    /// facts survive only while the endpoint stays the same.
    pub fn into_record(
        self,
        existing: Option<&EnvironmentProviderRecord>,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(self.updated_at_ms, "updated_at_ms")?;
        if let Some(credential) = &self.credential {
            validate_provider_credential(credential)?;
        } else if existing.is_none() {
            return invalid("a new environment provider requires a credential");
        }
        let kept = existing.filter(|record| record.controller_endpoint == self.controller_endpoint);
        let created_at_ms = existing.map_or(self.updated_at_ms, |record| record.created_at_ms);
        let record = EnvironmentProviderRecord {
            provider_id: self.provider_id,
            display_name: self.display_name,
            controller_endpoint: self.controller_endpoint,
            metadata: self.metadata,
            controller_features: kept.and_then(|record| record.controller_features),
            data_route_base: kept.and_then(|record| record.data_route_base.clone()),
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

/// What a controller advertised at a successful `initialize`. Written only
/// while the provider is still registered at `controller_endpoint`, so an
/// `initialize` against an endpoint the operator has since replaced cannot
/// overwrite the new endpoint's facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordProviderController {
    pub provider_id: EnvironmentProviderId,
    pub controller_endpoint: EnvironmentEndpoint,
    pub features: ControllerFeatures,
    pub data_route_base: DataRouteBase,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListEnvironmentProviders {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentProviderBindingStatus {
    Enabled,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentProviderBindingRecord {
    pub universe_id: Uuid,
    pub binding_id: EnvironmentProviderBindingId,
    pub provider_id: EnvironmentProviderId,
    pub status: EnvironmentProviderBindingStatus,
    pub revision: u64,
    pub metadata: BTreeMap<String, String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl EnvironmentProviderBindingRecord {
    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        if self.revision == 0 {
            return invalid("provider binding revision must be positive");
        }
        validate_metadata(&self.metadata)?;
        validate_timestamps(self.created_at_ms, self.updated_at_ms)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutEnvironmentProviderBinding {
    pub universe_id: Uuid,
    pub binding_id: EnvironmentProviderBindingId,
    pub provider_id: EnvironmentProviderId,
    pub status: EnvironmentProviderBindingStatus,
    pub metadata: BTreeMap<String, String>,
    pub expected_revision: Option<u64>,
    pub updated_at_ms: i64,
}

/// What Lightspeed does with a registered environment while its daemon is
/// disconnected. A property of the registration key, copied onto every
/// environment the key admits; `envd` neither knows nor chooses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegisteredIdentityMode {
    /// The environment stays `Offline` until it is explicitly closed.
    Persistent,
    /// The environment is closed once its daemon has been away longer than
    /// the key's disconnect grace.
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EnvironmentSource {
    Provisioned {
        provider_id: EnvironmentProviderId,
        binding_id: EnvironmentProviderBindingId,
    },
    /// An `envd` Lightspeed dials directly at a fixed endpoint.
    External { endpoint: EnvironmentEndpoint },
    /// An `envd` that dialed the gateway outbound and was admitted by a
    /// registration key. The daemon public key is the identity: one key
    /// maps to at most one environment in the deployment, ever, so a closed
    /// environment's daemon can never register again without a new key.
    Registered {
        registration_key_id: EnvironmentRegistrationKeyId,
        daemon_id: EnvironmentDaemonId,
        /// Lowercase hex of the daemon's raw Ed25519 public key.
        daemon_public_key: String,
        identity_mode: RegisteredIdentityMode,
    },
}

impl EnvironmentSource {
    pub fn provider_id(&self) -> Option<&EnvironmentProviderId> {
        match self {
            Self::Provisioned { provider_id, .. } => Some(provider_id),
            Self::External { .. } | Self::Registered { .. } => None,
        }
    }

    pub fn binding_id(&self) -> Option<&EnvironmentProviderBindingId> {
        match self {
            Self::Provisioned { binding_id, .. } => Some(binding_id),
            Self::External { .. } | Self::Registered { .. } => None,
        }
    }

    pub fn registration_key_id(&self) -> Option<&EnvironmentRegistrationKeyId> {
        match self {
            Self::Registered {
                registration_key_id,
                ..
            } => Some(registration_key_id),
            Self::Provisioned { .. } | Self::External { .. } => None,
        }
    }

    pub fn identity_mode(&self) -> Option<RegisteredIdentityMode> {
        match self {
            Self::Registered { identity_mode, .. } => Some(*identity_mode),
            Self::Provisioned { .. } | Self::External { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentStatus {
    Provisioning,
    Booting,
    Ready,
    /// Execution frozen with RAM resident; wakes on use.
    Paused,
    /// Execution state saved to disk; wakes on use.
    Suspended,
    /// Powered off with disk retained; wakes on use for provisioned
    /// environments whose provider supports power control.
    Offline,
    Closing,
    Closed,
    Failed,
    Unknown,
}

impl EnvironmentStatus {
    /// The steady power state this observed status corresponds to, or `None`
    /// while the environment is transitioning, failed, or gone.
    pub fn power_state(self) -> Option<PowerState> {
        match self {
            Self::Ready => Some(PowerState::Running),
            Self::Paused => Some(PowerState::Paused),
            Self::Suspended => Some(PowerState::Suspended),
            Self::Offline => Some(PowerState::Stopped),
            Self::Provisioning
            | Self::Booting
            | Self::Closing
            | Self::Closed
            | Self::Failed
            | Self::Unknown => None,
        }
    }

    /// True for the observed states a power change can start from or wake
    /// out of.
    pub fn is_powered_down(self) -> bool {
        matches!(self, Self::Paused | Self::Suspended | Self::Offline)
    }

    /// True once a close was accepted: no observation may change the status
    /// any more except the close's own completion.
    pub fn is_closing_or_closed(self) -> bool {
        matches!(self, Self::Closing | Self::Closed)
    }
}

/// Staged idle policy: each threshold is measured against the daemon's
/// reported idle duration and must be non-decreasing in the order
/// pause → suspend → stop → close. Every stage is optional.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentIdlePolicy {
    pub pause_after_ms: Option<u64>,
    pub suspend_after_ms: Option<u64>,
    pub stop_after_ms: Option<u64>,
    pub close_after_ms: Option<u64>,
}

/// One stage of an idle policy in escalation order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleAction {
    Pause,
    Suspend,
    Stop,
    Close,
}

impl IdleAction {
    /// The power state this stage requests; `None` for close.
    pub fn power_state(self) -> Option<PowerState> {
        match self {
            Self::Pause => Some(PowerState::Paused),
            Self::Suspend => Some(PowerState::Suspended),
            Self::Stop => Some(PowerState::Stopped),
            Self::Close => None,
        }
    }
}

impl EnvironmentIdlePolicy {
    pub fn is_empty(&self) -> bool {
        self.pause_after_ms.is_none()
            && self.suspend_after_ms.is_none()
            && self.stop_after_ms.is_none()
            && self.close_after_ms.is_none()
    }

    /// Stages in escalation order with their thresholds.
    pub fn stages(&self) -> Vec<(IdleAction, u64)> {
        [
            (IdleAction::Pause, self.pause_after_ms),
            (IdleAction::Suspend, self.suspend_after_ms),
            (IdleAction::Stop, self.stop_after_ms),
            (IdleAction::Close, self.close_after_ms),
        ]
        .into_iter()
        .filter_map(|(action, threshold)| threshold.map(|threshold| (action, threshold)))
        .collect()
    }

    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        if self.is_empty() {
            return invalid("idle policy must set at least one stage");
        }
        let mut previous: Option<(IdleAction, u64)> = None;
        for (action, threshold) in self.stages() {
            if threshold == 0 {
                return invalid(format!("idle policy {action:?} threshold must be positive"));
            }
            if let Some((earlier, earlier_threshold)) = previous
                && threshold < earlier_threshold
            {
                return invalid(format!(
                    "idle policy {action:?} threshold must not be below {earlier:?}"
                ));
            }
            previous = Some((action, threshold));
        }
        Ok(())
    }

    /// The most escalated stage whose threshold `idle_for_ms` has crossed and
    /// whose power state the provider supports (`close` is always
    /// supported). Returns `None` while no stage applies.
    pub fn due_action(&self, idle_for_ms: u64, supported: &[PowerState]) -> Option<IdleAction> {
        self.stages()
            .into_iter()
            .filter(|(_, threshold)| idle_for_ms >= *threshold)
            .filter(|(action, _)| {
                action
                    .power_state()
                    .is_none_or(|state| supported.contains(&state))
            })
            .map(|(action, _)| action)
            .next_back()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentIncarnationRecord {
    pub incarnation_id: EnvironmentIncarnationId,
    pub provision_request_id: Option<EnvironmentProvisionRequestId>,
    pub provider_target_id: Option<ProviderTargetId>,
    pub template_id: Option<EnvironmentTemplateId>,
    /// Provider-native reference used only while explicitly adopting an
    /// existing target. The provider converts it into `provider_target_id`.
    pub adoption_source_target: Option<String>,
    /// Power states the provider reported this target supports; observed
    /// with the target, empty until first observation or when the provider
    /// offers no power control.
    pub power_states: Vec<PowerState>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentRecord {
    pub environment_id: EnvironmentId,
    pub request_id: EnvironmentProvisionRequestId,
    pub source: EnvironmentSource,
    pub display_name: Option<String>,
    pub status: EnvironmentStatus,
    /// Lightspeed-owned power intent. The lifecycle reconciler
    /// converges the provider target toward it; observed state is `status`.
    pub desired_power: PowerState,
    /// Optional staged idle policy the power reaper applies from the
    /// daemon's idle report.
    pub idle_policy: Option<EnvironmentIdlePolicy>,
    pub incarnation: EnvironmentIncarnationRecord,
    /// Lightspeed-owned public ingress intent. The lifecycle reconciler
    /// converges the provider target toward it while the environment is
    /// ready; the realized endpoint is `public_endpoint`.
    pub public_ingress_enabled: bool,
    /// The public endpoint the provider last reported realized. It can lag
    /// the intent in either direction while the reconciler converges.
    pub public_endpoint: Option<String>,
    pub metadata: BTreeMap<String, String>,
    /// Registered environments only: when the gateway last saw the daemon's
    /// control connection (connect, heartbeat). A stale stamp under `Ready`
    /// means the gateway stopped without recording the disconnect.
    pub last_seen_at_ms: Option<i64>,
    /// Why the environment failed, or the latest error while its lifecycle
    /// keeps retrying. Cleared once an observation reaches a steady state.
    pub lifecycle_error: Option<EnvironmentLifecycleError>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// The latest lifecycle error of an environment and how long errors have
/// persisted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentLifecycleError {
    /// 1 to 1024 bytes without control characters other than line feed and
    /// tab.
    pub message: String,
    /// When the current run of errors began: the first error recorded since
    /// the environment was last steady.
    pub first_at_ms: i64,
    /// When `message` was recorded.
    pub last_at_ms: i64,
}

impl EnvironmentLifecycleError {
    /// The error recorded at `at_ms`, continuing the run of `previous`.
    /// `message` is cleaned and cut to the stored bound.
    pub fn record(previous: Option<&Self>, message: &str, at_ms: i64) -> Self {
        let message = DescriptiveText::lossy(message)
            .map(DescriptiveText::into_string)
            .unwrap_or_else(|| "unspecified lifecycle error".to_owned());
        let first_at_ms = previous.map_or(at_ms, |previous| previous.first_at_ms.min(at_ms));
        Self {
            message,
            first_at_ms,
            last_at_ms: at_ms.max(first_at_ms),
        }
    }

    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        validate_descriptive_text(&self.message)
            .map_err(|error| invalid_error(format!("lifecycle error message: {error}")))?;
        validate_nonnegative_i64(self.first_at_ms, "lifecycle_error.first_at_ms")?;
        if self.last_at_ms < self.first_at_ms {
            return invalid("lifecycle_error.last_at_ms must be >= first_at_ms");
        }
        Ok(())
    }
}

impl EnvironmentRecord {
    pub fn registration_key_id(&self) -> Option<&EnvironmentRegistrationKeyId> {
        self.source.registration_key_id()
    }

    pub fn is_registered(&self) -> bool {
        matches!(self.source, EnvironmentSource::Registered { .. })
    }

    /// True for a registered environment whose daemon is not currently
    /// connected as far as durable state knows, including a `Ready` row
    /// whose heartbeat stamp is older than `stale_after_ms`.
    pub fn registered_daemon_absent(&self, now_ms: i64, stale_after_ms: i64) -> bool {
        if !self.is_registered() {
            return false;
        }
        match self.status {
            EnvironmentStatus::Ready => self
                .last_seen_at_ms
                .is_none_or(|seen| now_ms.saturating_sub(seen) > stale_after_ms),
            EnvironmentStatus::Offline => true,
            _ => false,
        }
    }
    /// True when the observed steady power state differs from the desired
    /// one and the environment is in a state a power change can act on.
    pub fn power_diverges(&self) -> bool {
        matches!(self.source, EnvironmentSource::Provisioned { .. })
            && self
                .status
                .power_state()
                .is_some_and(|observed| observed != self.desired_power)
    }

    /// True for a ready provisioned environment whose realized public
    /// endpoint does not match its ingress intent yet.
    pub fn ingress_diverges(&self) -> bool {
        matches!(self.source, EnvironmentSource::Provisioned { .. })
            && self.status == EnvironmentStatus::Ready
            && self.public_ingress_enabled != self.public_endpoint.is_some()
    }

    /// True for a provisioned environment at rest in a power state whose
    /// provider target is recorded: only a periodic observation notices a
    /// target that changed or vanished out of band.
    pub fn is_steady_provisioned(&self) -> bool {
        matches!(self.source, EnvironmentSource::Provisioned { .. })
            && self.status.power_state().is_some()
            && self.incarnation.provider_target_id.is_some()
    }

    /// Whether a reconcile pass of `scope` visits this environment.
    pub fn needs_reconcile(&self, scope: ReconcileScope) -> bool {
        matches!(
            self.status,
            EnvironmentStatus::Provisioning
                | EnvironmentStatus::Booting
                | EnvironmentStatus::Closing
                | EnvironmentStatus::Unknown
        ) || self.power_diverges()
            || self.ingress_diverges()
            || (scope == ReconcileScope::WithSteady && self.is_steady_provisioned())
    }

    pub fn provider_id(&self) -> Option<&EnvironmentProviderId> {
        self.source.provider_id()
    }

    pub fn binding_id(&self) -> Option<&EnvironmentProviderBindingId> {
        self.source.binding_id()
    }

    pub fn observed_at_ms(&self) -> i64 {
        self.updated_at_ms
    }

    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        validate_nonempty_optional("display_name", self.display_name.as_deref())?;
        validate_metadata(&self.metadata)?;
        validate_timestamps(self.created_at_ms, self.updated_at_ms)?;
        validate_timestamps(
            self.incarnation.created_at_ms,
            self.incarnation.updated_at_ms,
        )?;
        validate_nonempty_optional("public_endpoint", self.public_endpoint.as_deref())?;
        if let Some(policy) = &self.idle_policy {
            policy.validate()?;
        }
        if (self.public_ingress_enabled || self.public_endpoint.is_some())
            && !matches!(self.source, EnvironmentSource::Provisioned { .. })
        {
            return invalid("only provisioned environments can have provider-managed ingress");
        }
        if self.last_seen_at_ms.is_some() && !self.is_registered() {
            return invalid("only registered environments carry a last-seen stamp");
        }
        if let Some(seen) = self.last_seen_at_ms {
            validate_nonnegative_i64(seen, "last_seen_at_ms")?;
        }
        if let Some(error) = &self.lifecycle_error {
            error.validate()?;
        }
        match &self.source {
            EnvironmentSource::Provisioned { .. } => {
                if self.incarnation.provision_request_id.is_none() {
                    return invalid("provisioned incarnation requires a provision request id");
                }
                if self.incarnation.template_id.is_some()
                    == self.incarnation.adoption_source_target.is_some()
                {
                    return invalid(
                        "provisioned incarnation requires exactly one template or adoption source",
                    );
                }
                if let Some(source) = self.incarnation.adoption_source_target.as_deref() {
                    validate_adoption_source(source)?;
                }
            }
            EnvironmentSource::External { .. } => {
                self.validate_no_provider_linkage("external")?;
            }
            EnvironmentSource::Registered {
                daemon_id,
                daemon_public_key,
                ..
            } => {
                let public_key = parse_daemon_public_key(daemon_public_key)?;
                if daemon_id != &EnvironmentDaemonId::from_public_key(public_key.as_bytes()) {
                    return invalid("registered daemon id does not match its public key");
                }
                if self.request_id != EnvironmentProvisionRequestId::for_daemon(daemon_id) {
                    return invalid(
                        "registered environment request id must derive from its daemon id",
                    );
                }
                if matches!(
                    self.status,
                    EnvironmentStatus::Provisioning
                        | EnvironmentStatus::Booting
                        | EnvironmentStatus::Paused
                        | EnvironmentStatus::Suspended
                ) {
                    return invalid(
                        "registered environments are ready, offline, closing, or closed",
                    );
                }
                self.validate_no_provider_linkage("registered")?;
            }
        }
        Ok(())
    }

    fn validate_no_provider_linkage(&self, kind: &str) -> Result<(), EnvironmentRegistryError> {
        if self.incarnation.provision_request_id.is_some()
            || self.incarnation.provider_target_id.is_some()
            || self.incarnation.template_id.is_some()
            || self.incarnation.adoption_source_target.is_some()
            || !self.incarnation.power_states.is_empty()
        {
            return invalid(format!("{kind} incarnation must not have provider linkage"));
        }
        if self.desired_power != PowerState::Running || self.idle_policy.is_some() {
            return invalid(format!("{kind} environments have no power control"));
        }
        Ok(())
    }
}

/// Default disconnect grace for ephemeral registered environments whose key
/// does not set one: long enough for a pod restart or a brief network
/// interruption, short enough that leaked benchmark sandboxes disappear.
pub const DEFAULT_EPHEMERAL_DISCONNECT_GRACE_MS: u64 = 5 * 60 * 1_000;

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
/// environment rows carrying the key id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentRegistrationKeyRecord {
    pub registration_key_id: EnvironmentRegistrationKeyId,
    /// Required: the group name shown wherever registered environments are
    /// listed.
    pub display_name: String,
    pub key_prefix: String,
    pub identity_mode: RegisteredIdentityMode,
    /// Non-closed environments this key may have at once; absent means
    /// unlimited.
    pub max_active_environments: Option<u32>,
    /// Absent means [`DEFAULT_EPHEMERAL_DISCONNECT_GRACE_MS`].
    pub ephemeral_disconnect_grace_ms: Option<u64>,
    pub expires_at_ms: Option<i64>,
    pub created_at_ms: i64,
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
            EnvironmentRegistrationKeyStatus::Revoked => {
                Err(EnvironmentRegistryError::RegistrationKeyUnavailable {
                    registration_key_id: self.registration_key_id.to_string(),
                    reason: "revoked",
                })
            }
            EnvironmentRegistrationKeyStatus::Expired => {
                Err(EnvironmentRegistryError::RegistrationKeyUnavailable {
                    registration_key_id: self.registration_key_id.to_string(),
                    reason: "expired",
                })
            }
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
        validate_nonnegative_i64(self.created_at_ms, "created_at_ms")?;
        if let Some(expires) = self.expires_at_ms {
            validate_nonnegative_i64(expires, "expires_at_ms")?;
        }
        if let Some(revoked) = self.revoked_at_ms {
            validate_nonnegative_i64(revoked, "revoked_at_ms")?;
            if revoked < self.created_at_ms {
                return invalid("revoked_at_ms must be >= created_at_ms");
            }
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
    pub expires_at_ms: Option<i64>,
}

/// A freshly minted registration key: the one-time plaintext secret plus the
/// record and hash to persist. The secret is structurally not persistable.
#[derive(Clone, Debug)]
pub struct MintedRegistrationKey {
    pub secret: SecretValue,
    pub secret_hash: String,
    pub record: EnvironmentRegistrationKeyRecord,
}

/// Mint a registration key. The secret is returned exactly once; only its
/// SHA-256 hash and display prefix are persisted.
pub fn mint_registration_key(
    registration_key_id: EnvironmentRegistrationKeyId,
    policy: RegistrationKeyPolicy,
    created_at_ms: i64,
) -> Result<MintedRegistrationKey, EnvironmentRegistryError> {
    let secret = auth::generate_prefixed_secret(REGISTRATION_KEY_SECRET_PREFIX);
    let record = EnvironmentRegistrationKeyRecord {
        registration_key_id,
        display_name: policy.display_name,
        key_prefix: auth::secret_display_prefix(&secret, REGISTRATION_KEY_DISPLAY_PREFIX_LEN),
        identity_mode: policy.identity_mode,
        max_active_environments: policy.max_active_environments,
        ephemeral_disconnect_grace_ms: policy.ephemeral_disconnect_grace_ms,
        expires_at_ms: policy.expires_at_ms,
        created_at_ms,
        revoked_at_ms: None,
    };
    record.validate()?;
    Ok(MintedRegistrationKey {
        secret_hash: auth::secret_sha256_hex(&secret),
        secret: SecretValue::new(secret),
        record,
    })
}

/// Lowercase hex SHA-256 of a presented registration secret: the lookup key.
pub fn registration_key_hash(secret: &str) -> String {
    auth::secret_sha256_hex(secret)
}

#[derive(Clone, Debug)]
pub struct CreateEnvironmentRegistrationKey {
    pub secret_hash: String,
    pub record: EnvironmentRegistrationKeyRecord,
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
    /// Idempotent: revoking a revoked key keeps the original revocation time.
    async fn revoke_registration_key(
        &self,
        request: RevokeEnvironmentRegistrationKey,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError>;
    /// Resolve a presented secret by hash regardless of status; the caller
    /// turns revoked/expired into typed refusals.
    async fn resolve_registration_key(
        &self,
        secret_hash: &str,
    ) -> Result<Option<EnvironmentRegistrationKeyRecord>, EnvironmentRegistryError>;
    async fn registration_key_usage(
        &self,
        registration_key_id: &EnvironmentRegistrationKeyId,
    ) -> Result<RegistrationKeyUsage, EnvironmentRegistryError>;
}

/// Admission of a first-seen daemon identity. The store locks the key row,
/// re-checks that the key still admits, counts the key's non-closed
/// environments against its limit, and inserts the environment `Ready`, all
/// in one transaction. A retry with the same daemon converges through the
/// derived request id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateRegisteredEnvironment {
    pub registration_key_id: EnvironmentRegistrationKeyId,
    pub environment_id: EnvironmentId,
    pub incarnation_id: EnvironmentIncarnationId,
    /// Lowercase hex of the raw Ed25519 public key.
    pub daemon_public_key: String,
    pub display_name: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub created_at_ms: i64,
}

impl CreateRegisteredEnvironment {
    pub fn daemon_id(&self) -> Result<EnvironmentDaemonId, EnvironmentRegistryError> {
        let public_key = parse_daemon_public_key(&self.daemon_public_key)?;
        Ok(EnvironmentDaemonId::from_public_key(public_key.as_bytes()))
    }
}

/// A gateway observation of a registered daemon's control connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegisteredConnectionObservation {
    /// A control connection was admitted: `Ready` plus a fresh stamp.
    Connected,
    /// The connection is still alive: fresh stamp only.
    Heartbeat,
    /// The connection ended: `Offline`; the stamp keeps its last value.
    Disconnected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObserveRegisteredEnvironment {
    pub environment_id: EnvironmentId,
    pub observation: RegisteredConnectionObservation,
    pub observed_at_ms: i64,
    /// Reserved-prefix entries the gateway learned about the daemon at this
    /// admission, merged into the row so a replaced daemon shows its new
    /// build. Empty for heartbeats and disconnects. Ignored once the
    /// environment is closing or closed.
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

/// Metadata keys Lightspeed writes about the daemon serving an environment.
/// They carry the reserved prefix, so a caller cannot set them.
pub const ENVD_VERSION_METADATA_KEY: &str = "lightspeed.envd.version";
pub const ENVD_GIT_SHA_METADATA_KEY: &str = "lightspeed.envd.gitSha";
pub const ENVD_PROTOCOL_VERSION_METADATA_KEY: &str = "lightspeed.envd.protocolVersion";

/// What Lightspeed learned about the daemon serving an environment: from
/// `registration/register` for a registered daemon, from the data-plane
/// `initialize` at the Ready transition for a provisioned one. Recorded on
/// the environment row so an operator sees which build serves it, and which
/// protocol version a mismatched daemon spoke. Never consulted for
/// admission: the protocol version alone decides that.
#[derive(Clone, Debug, PartialEq, Eq)]
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

    /// The reserved entries for this build. A fact the daemon did not report
    /// is left out, so the row keeps that key's last value.
    pub fn metadata(&self) -> BTreeMap<String, String> {
        let mut metadata = BTreeMap::new();
        if let Some(version) = &self.version {
            metadata.insert(ENVD_VERSION_METADATA_KEY.to_owned(), version.clone());
        }
        if let Some(git_sha) = &self.git_sha {
            metadata.insert(ENVD_GIT_SHA_METADATA_KEY.to_owned(), git_sha.clone());
        }
        metadata.insert(
            ENVD_PROTOCOL_VERSION_METADATA_KEY.to_owned(),
            self.protocol_version.to_string(),
        );
        metadata
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateEnvironment {
    pub request_id: EnvironmentProvisionRequestId,
    pub environment_id: EnvironmentId,
    pub incarnation_id: EnvironmentIncarnationId,
    pub binding_id: EnvironmentProviderBindingId,
    pub template_id: EnvironmentTemplateId,
    pub display_name: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub idle_policy: Option<EnvironmentIdlePolicy>,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoptEnvironment {
    pub request_id: EnvironmentProvisionRequestId,
    pub environment_id: EnvironmentId,
    pub incarnation_id: EnvironmentIncarnationId,
    pub binding_id: EnvironmentProviderBindingId,
    pub source_target: String,
    pub display_name: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateExternalEnvironment {
    pub request_id: EnvironmentProvisionRequestId,
    pub environment_id: EnvironmentId,
    pub incarnation_id: EnvironmentIncarnationId,
    pub endpoint: EnvironmentEndpoint,
    pub display_name: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub created_at_ms: i64,
}

impl CreateEnvironment {
    /// The validated record this create inserts for a binding of
    /// `provider_id`.
    pub fn record(
        &self,
        provider_id: EnvironmentProviderId,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let record = new_record(
            self.environment_id.clone(),
            self.request_id.clone(),
            EnvironmentSource::Provisioned {
                provider_id,
                binding_id: self.binding_id.clone(),
            },
            self.display_name.clone(),
            EnvironmentStatus::Provisioning,
            EnvironmentIncarnationRecord {
                provision_request_id: Some(self.request_id.clone()),
                template_id: Some(self.template_id.clone()),
                ..new_incarnation(self.incarnation_id.clone(), self.created_at_ms)
            },
            self.metadata.clone(),
            self.created_at_ms,
        );
        let record = EnvironmentRecord {
            idle_policy: self.idle_policy.clone(),
            ..record
        };
        record.validate()?;
        Ok(record)
    }
}

impl AdoptEnvironment {
    /// The validated record this adoption inserts for a binding of
    /// `provider_id`.
    pub fn record(
        &self,
        provider_id: EnvironmentProviderId,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        validate_adoption_source(&self.source_target)?;
        let record = new_record(
            self.environment_id.clone(),
            self.request_id.clone(),
            EnvironmentSource::Provisioned {
                provider_id,
                binding_id: self.binding_id.clone(),
            },
            self.display_name.clone(),
            EnvironmentStatus::Provisioning,
            EnvironmentIncarnationRecord {
                provision_request_id: Some(self.request_id.clone()),
                adoption_source_target: Some(self.source_target.clone()),
                ..new_incarnation(self.incarnation_id.clone(), self.created_at_ms)
            },
            self.metadata.clone(),
            self.created_at_ms,
        );
        record.validate()?;
        Ok(record)
    }
}

impl CreateExternalEnvironment {
    /// The validated record this create inserts.
    pub fn record(&self) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let record = new_record(
            self.environment_id.clone(),
            self.request_id.clone(),
            EnvironmentSource::External {
                endpoint: self.endpoint.clone(),
            },
            self.display_name.clone(),
            EnvironmentStatus::Ready,
            new_incarnation(self.incarnation_id.clone(), self.created_at_ms),
            self.metadata.clone(),
            self.created_at_ms,
        );
        record.validate()?;
        Ok(record)
    }
}

impl CreateRegisteredEnvironment {
    /// The validated record this admission inserts under `key`.
    pub fn record(
        &self,
        key: &EnvironmentRegistrationKeyRecord,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let daemon_id = self.daemon_id()?;
        let record = new_record(
            self.environment_id.clone(),
            EnvironmentProvisionRequestId::for_daemon(&daemon_id),
            EnvironmentSource::Registered {
                registration_key_id: key.registration_key_id.clone(),
                daemon_id,
                daemon_public_key: self.daemon_public_key.clone(),
                identity_mode: key.identity_mode,
            },
            self.display_name.clone(),
            EnvironmentStatus::Ready,
            new_incarnation(self.incarnation_id.clone(), self.created_at_ms),
            self.metadata.clone(),
            self.created_at_ms,
        );
        let record = EnvironmentRecord {
            last_seen_at_ms: Some(self.created_at_ms),
            ..record
        };
        record.validate()?;
        Ok(record)
    }
}

fn new_incarnation(
    incarnation_id: EnvironmentIncarnationId,
    created_at_ms: i64,
) -> EnvironmentIncarnationRecord {
    EnvironmentIncarnationRecord {
        incarnation_id,
        provision_request_id: None,
        provider_target_id: None,
        template_id: None,
        adoption_source_target: None,
        power_states: Vec::new(),
        created_at_ms,
        updated_at_ms: created_at_ms,
    }
}

#[allow(clippy::too_many_arguments)]
fn new_record(
    environment_id: EnvironmentId,
    request_id: EnvironmentProvisionRequestId,
    source: EnvironmentSource,
    display_name: Option<String>,
    status: EnvironmentStatus,
    incarnation: EnvironmentIncarnationRecord,
    metadata: BTreeMap<String, String>,
    created_at_ms: i64,
) -> EnvironmentRecord {
    EnvironmentRecord {
        environment_id,
        request_id,
        source,
        display_name,
        status,
        desired_power: PowerState::Running,
        idle_policy: None,
        incarnation,
        public_ingress_enabled: false,
        public_endpoint: None,
        metadata,
        last_seen_at_ms: None,
        lifecycle_error: None,
        created_at_ms,
        updated_at_ms: created_at_ms,
    }
}

/// Which environments a lifecycle reconcile pass visits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconcileScope {
    /// Environments with convergence pending: provisioning, booting,
    /// closing or unknown, a power state that diverges from the intent, or
    /// a ready environment whose public endpoint does not match its
    /// ingress intent.
    Converging,
    /// Those plus every steady provisioned environment with a recorded
    /// target, observed on a slower cadence.
    WithSteady,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListEnvironments {
    pub provider_id: Option<EnvironmentProviderId>,
    pub binding_id: Option<EnvironmentProviderBindingId>,
    pub status: Option<EnvironmentStatus>,
    /// Only environments admitted by this registration key.
    pub registration_key_id: Option<EnvironmentRegistrationKeyId>,
    /// Only environments carrying every listed metadata pair (containment).
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

/// A provider observation of a provisioned environment's target. The target
/// id and power states are recorded whatever the environment's status, so a
/// close that raced the first create can still address the target. The
/// status, the daemon build and the clearing of `lifecycle_error` apply only
/// while the environment is not closing or closed: a close is never
/// overwritten by an observation that was in flight when it was accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObserveProvisionedEnvironment {
    pub environment_id: EnvironmentId,
    /// Recorded when present; absent while an asynchronous provider has not
    /// assigned one yet, which keeps the recorded id. A different id than
    /// the recorded one is refused.
    pub provider_target_id: Option<ProviderTargetId>,
    pub status: EnvironmentStatus,
    /// Provider-reported supported power states for this target.
    pub power_states: Vec<PowerState>,
    /// The daemon found by the data-plane `initialize` of a Ready
    /// transition.
    pub daemon_build: Option<DaemonBuild>,
    pub observed_at_ms: i64,
}

impl ObserveProvisionedEnvironment {
    /// Apply this observation to `record`. Stale observations (older than
    /// the incarnation's last update) leave it unchanged.
    pub fn apply(&self, record: &mut EnvironmentRecord) -> Result<(), EnvironmentRegistryError> {
        validate_nonnegative_i64(self.observed_at_ms, "observed_at_ms")?;
        if !matches!(record.source, EnvironmentSource::Provisioned { .. }) {
            return invalid("provider observations apply only to provisioned environments");
        }
        if self.observed_at_ms < record.incarnation.updated_at_ms {
            return Ok(());
        }
        if let (Some(recorded), Some(observed)) = (
            record.incarnation.provider_target_id.as_ref(),
            self.provider_target_id.as_ref(),
        ) && recorded != observed
        {
            return invalid("provider target conflicts with current incarnation");
        }
        if let Some(target_id) = &self.provider_target_id {
            record.incarnation.provider_target_id = Some(target_id.clone());
        }
        record.incarnation.power_states = self.power_states.clone();
        record.incarnation.updated_at_ms = self.observed_at_ms;
        if record.status.is_closing_or_closed() {
            return Ok(());
        }
        record.status = self.status;
        if let Some(build) = &self.daemon_build {
            record.metadata.extend(build.metadata());
        }
        if self.status.power_state().is_some() {
            record.lifecycle_error = None;
        }
        record.updated_at_ms = record.updated_at_ms.max(self.observed_at_ms);
        Ok(())
    }
}

/// Set the Lightspeed-owned power intent. Rejected for external, closing,
/// closed, and failed environments; the caller validates provider support.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetEnvironmentPower {
    pub environment_id: EnvironmentId,
    pub desired_power: PowerState,
    pub updated_at_ms: i64,
}

/// Replace (or clear) the idle policy of a provisioned environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetEnvironmentIdlePolicy {
    pub environment_id: EnvironmentId,
    pub idle_policy: Option<EnvironmentIdlePolicy>,
    pub updated_at_ms: i64,
}

/// Move an environment to `failed` with `message` as its lifecycle error.
/// A closing or closed environment is left as it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailEnvironmentLifecycle {
    pub environment_id: EnvironmentId,
    pub message: String,
    /// The daemon whose build made the environment unusable, such as one
    /// that speaks another protocol version.
    pub daemon_build: Option<DaemonBuild>,
    pub observed_at_ms: i64,
}

impl FailEnvironmentLifecycle {
    pub fn apply(&self, record: &mut EnvironmentRecord) -> Result<(), EnvironmentRegistryError> {
        validate_nonnegative_i64(self.observed_at_ms, "observed_at_ms")?;
        if record.status.is_closing_or_closed() {
            return Ok(());
        }
        record.status = EnvironmentStatus::Failed;
        record.lifecycle_error = Some(EnvironmentLifecycleError::record(
            record.lifecycle_error.as_ref(),
            &self.message,
            self.observed_at_ms,
        ));
        if let Some(build) = &self.daemon_build {
            record.metadata.extend(build.metadata());
        }
        record.incarnation.updated_at_ms =
            record.incarnation.updated_at_ms.max(self.observed_at_ms);
        record.updated_at_ms = record.updated_at_ms.max(self.observed_at_ms);
        Ok(())
    }
}

/// Record a lifecycle error the reconciler will retry past, without
/// changing the environment's status. A closed environment is left as it
/// is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordEnvironmentLifecycleError {
    pub environment_id: EnvironmentId,
    pub message: String,
    pub observed_at_ms: i64,
}

impl RecordEnvironmentLifecycleError {
    pub fn apply(&self, record: &mut EnvironmentRecord) -> Result<(), EnvironmentRegistryError> {
        validate_nonnegative_i64(self.observed_at_ms, "observed_at_ms")?;
        if record.status == EnvironmentStatus::Closed {
            return Ok(());
        }
        record.lifecycle_error = Some(EnvironmentLifecycleError::record(
            record.lifecycle_error.as_ref(),
            &self.message,
            self.observed_at_ms,
        ));
        record.updated_at_ms = record.updated_at_ms.max(self.observed_at_ms);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeginCloseEnvironment {
    pub environment_id: EnvironmentId,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishCloseEnvironment {
    pub environment_id: EnvironmentId,
    pub observed_at_ms: i64,
}

/// Record the public ingress intent of a provisioned environment; the
/// lifecycle reconciler converges the provider toward it. Enabling is
/// refused for a closing or closed environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetEnvironmentIngress {
    pub environment_id: EnvironmentId,
    pub enabled: bool,
    pub updated_at_ms: i64,
}

/// Record the public endpoint a provider observation reports realized,
/// `None` when the target has no public ingress. A closing or closed
/// environment is left as it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordEnvironmentIngress {
    pub environment_id: EnvironmentId,
    pub public_endpoint: Option<String>,
    pub observed_at_ms: i64,
}

impl RecordEnvironmentIngress {
    pub fn apply(&self, record: &mut EnvironmentRecord) -> Result<(), EnvironmentRegistryError> {
        validate_nonnegative_i64(self.observed_at_ms, "observed_at_ms")?;
        validate_nonempty_optional("public_endpoint", self.public_endpoint.as_deref())?;
        if !matches!(record.source, EnvironmentSource::Provisioned { .. }) {
            return invalid("provider-managed ingress requires a provisioned environment");
        }
        if record.status.is_closing_or_closed() {
            return Ok(());
        }
        record.public_endpoint = self.public_endpoint.clone();
        record.updated_at_ms = record.updated_at_ms.max(self.observed_at_ms);
        Ok(())
    }
}

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

#[async_trait]
pub trait EnvironmentProviderStore: Send + Sync {
    async fn put_provider(
        &self,
        record: PutEnvironmentProvider,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError>;
    async fn read_provider(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError>;
    /// The provider's bearer credential, unsealed.
    async fn read_provider_credential(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<SecretValue, EnvironmentRegistryError>;
    /// Record what the controller advertised at a successful `initialize`.
    /// Returns the record unchanged when the provider has moved to another
    /// endpoint since.
    async fn record_provider_controller(
        &self,
        request: RecordProviderController,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError>;
    async fn list_providers(
        &self,
        request: ListEnvironmentProviders,
    ) -> Result<Vec<EnvironmentProviderRecord>, EnvironmentRegistryError>;
    async fn delete_provider(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError>;
}

#[async_trait]
pub trait EnvironmentProviderBindingStore: Send + Sync {
    async fn put_provider_binding(
        &self,
        request: PutEnvironmentProviderBinding,
    ) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError>;
    async fn read_provider_binding(
        &self,
        universe_id: Uuid,
        binding_id: &EnvironmentProviderBindingId,
    ) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError>;
    async fn list_provider_bindings(
        &self,
        universe_id: Uuid,
    ) -> Result<Vec<EnvironmentProviderBindingRecord>, EnvironmentRegistryError>;
    async fn delete_provider_binding(
        &self,
        universe_id: Uuid,
        binding_id: &EnvironmentProviderBindingId,
    ) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError>;
}

#[async_trait]
pub trait EnvironmentStore: Send + Sync {
    async fn create_environment(
        &self,
        request: CreateEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn adopt_environment(
        &self,
        request: AdoptEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn create_external_environment(
        &self,
        request: CreateExternalEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn create_registered_environment(
        &self,
        request: CreateRegisteredEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    /// The one environment (closed included) bound to a daemon public key in
    /// this universe.
    async fn read_environment_by_daemon_public_key(
        &self,
        daemon_public_key: &str,
    ) -> Result<Option<EnvironmentRecord>, EnvironmentRegistryError>;
    async fn observe_registered_environment(
        &self,
        request: ObserveRegisteredEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    /// Registered environments that are not closing or closed: the
    /// lifecycle reconciler's candidates for stale repair and ephemeral
    /// cleanup.
    async fn list_open_registered_environments(
        &self,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError>;
    async fn read_environment(
        &self,
        environment_id: &EnvironmentId,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn read_environment_by_request_id(
        &self,
        request_id: &EnvironmentProvisionRequestId,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    /// Rows that no longer decode are left out and logged by the store, so
    /// one bad row never hides the others; every list method does the same.
    async fn list_environments(
        &self,
        request: ListEnvironments,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError>;
    /// The environments a reconcile pass of `scope` visits; see
    /// [`EnvironmentRecord::needs_reconcile`].
    async fn list_environments_needing_reconcile(
        &self,
        scope: ReconcileScope,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError>;
    async fn observe_provisioned_environment(
        &self,
        request: ObserveProvisionedEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn fail_environment_lifecycle(
        &self,
        request: FailEnvironmentLifecycle,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn record_environment_lifecycle_error(
        &self,
        request: RecordEnvironmentLifecycleError,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn begin_close_environment(
        &self,
        request: BeginCloseEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn finish_close_environment(
        &self,
        request: FinishCloseEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn set_environment_ingress(
        &self,
        request: SetEnvironmentIngress,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn record_environment_ingress(
        &self,
        request: RecordEnvironmentIngress,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn set_environment_power(
        &self,
        request: SetEnvironmentPower,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn set_environment_idle_policy(
        &self,
        request: SetEnvironmentIdlePolicy,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    /// Ready provisioned environments carrying an idle policy: the power
    /// reaper's candidates.
    async fn list_environments_with_idle_policy(
        &self,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError>;
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
pub use endpoint::{EndpointError, EndpointScheme, EnvironmentEndpoint};
mod memory;
pub use memory::{InMemoryEnvironmentRegistryStore, apply_registered_observation};

fn invalid<T>(message: impl Into<String>) -> Result<T, EnvironmentRegistryError> {
    Err(EnvironmentRegistryError::InvalidInput {
        message: message.into(),
    })
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

/// An adoption source is a provider-native reference under the protocol's
/// opaque rule: 1 to 255 bytes without control characters.
pub fn validate_adoption_source(value: &str) -> Result<(), EnvironmentRegistryError> {
    validate_opaque_id("adoption_source_target", value)
        .map_err(|error| invalid_error(error.to_string()))
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

/// A daemon public key is the raw 32-byte Ed25519 key as lowercase hex.
pub fn validate_daemon_public_key(value: &str) -> Result<(), EnvironmentRegistryError> {
    parse_daemon_public_key(value).map(|_| ())
}

fn parse_daemon_public_key(
    value: &str,
) -> Result<environment_protocol::registration::DaemonPublicKey, EnvironmentRegistryError> {
    environment_protocol::registration::DaemonPublicKey::parse(value)
        .map_err(|_| invalid_error("daemon public key must be 64 lowercase hex characters"))
}

fn invalid_error(message: impl Into<String>) -> EnvironmentRegistryError {
    EnvironmentRegistryError::InvalidInput {
        message: message.into(),
    }
}

/// Stored records obey the descriptive metadata bounds. The reserved-prefix
/// check applies to caller input at the API boundary, not here: Lightspeed
/// itself annotates records under that prefix, and those entries sit on top
/// of a caller's full allowance rather than eating into it.
fn validate_metadata(value: &BTreeMap<String, String>) -> Result<(), EnvironmentRegistryError> {
    validate_stored_metadata(value).map_err(|error| invalid_error(error.to_string()))
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
