//! Environment-compute domain contracts.
//!
//! Physical providers are deployment-scoped, provider bindings and
//! environments are universe-scoped, and provider target facts live on an
//! environment incarnation rather than on stable environment identity.

use std::{collections::BTreeMap, fmt, str::FromStr};

use async_trait::async_trait;
use auth::{AuthGrantId, AuthProviderId, SecretId};
pub use engine::EnvironmentId;
use engine::{StringIdError, validate_general_string_id};
use host_protocol::{
    control::targets::EnvironmentTemplate,
    shared::{HostTargetId, HostTransport},
};
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

    #[error("environment registry store failure: {message}")]
    Store { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentConnectionSpec {
    pub endpoint: String,
    pub transport: HostTransport,
}

impl EnvironmentConnectionSpec {
    pub fn new(endpoint: impl Into<String>, transport: HostTransport) -> Self {
        Self {
            endpoint: endpoint.into(),
            transport,
        }
    }

    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        validate_endpoint("host controller endpoint", &self.endpoint)
    }
}

pub type HostControllerConnectionSpec = EnvironmentConnectionSpec;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentProviderRecord {
    pub provider_id: EnvironmentProviderId,
    pub display_name: Option<String>,
    pub controller_connection: HostControllerConnectionSpec,
    pub metadata: BTreeMap<String, String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl EnvironmentProviderRecord {
    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        validate_nonempty_optional("display_name", self.display_name.as_deref())?;
        self.controller_connection.validate()?;
        validate_metadata(&self.metadata)?;
        validate_timestamps(self.created_at_ms, self.updated_at_ms)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutEnvironmentProvider {
    pub provider_id: EnvironmentProviderId,
    pub display_name: Option<String>,
    pub controller_connection: HostControllerConnectionSpec,
    pub metadata: BTreeMap<String, String>,
    pub updated_at_ms: i64,
}

impl PutEnvironmentProvider {
    pub fn into_record(self) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(self.updated_at_ms, "updated_at_ms")?;
        let record = EnvironmentProviderRecord {
            provider_id: self.provider_id,
            display_name: self.display_name,
            controller_connection: self.controller_connection,
            metadata: self.metadata,
            created_at_ms: self.updated_at_ms,
            updated_at_ms: self.updated_at_ms,
        };
        record.validate()?;
        Ok(record)
    }
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EnvironmentSource {
    Provisioned {
        provider_id: EnvironmentProviderId,
        binding_id: EnvironmentProviderBindingId,
    },
    External {
        connection: EnvironmentConnectionSpec,
    },
}

impl EnvironmentSource {
    pub fn provider_id(&self) -> Option<&EnvironmentProviderId> {
        match self {
            Self::Provisioned { provider_id, .. } => Some(provider_id),
            Self::External { .. } => None,
        }
    }

    pub fn binding_id(&self) -> Option<&EnvironmentProviderBindingId> {
        match self {
            Self::Provisioned { binding_id, .. } => Some(binding_id),
            Self::External { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentStatus {
    Provisioning,
    Booting,
    Ready,
    Offline,
    Closing,
    Closed,
    Failed,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentIncarnationRecord {
    pub incarnation_id: EnvironmentIncarnationId,
    pub provision_request_id: Option<EnvironmentProvisionRequestId>,
    pub provider_target_id: Option<HostTargetId>,
    pub template_id: Option<EnvironmentTemplateId>,
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
    pub incarnation: EnvironmentIncarnationRecord,
    pub public_ingress_enabled: bool,
    pub public_endpoint: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl EnvironmentRecord {
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
        if self.public_ingress_enabled != self.public_endpoint.is_some() {
            return invalid("public ingress requires exactly one public endpoint when enabled");
        }
        if self.public_ingress_enabled
            && !matches!(self.source, EnvironmentSource::Provisioned { .. })
        {
            return invalid("external environments cannot have provider-managed ingress");
        }
        if let Some(target_id) = &self.incarnation.provider_target_id {
            validate_host_target_id(target_id)?;
        }
        match &self.source {
            EnvironmentSource::Provisioned { .. } => {
                if self.incarnation.provision_request_id.is_none()
                    || self.incarnation.template_id.is_none()
                {
                    return invalid(
                        "provisioned incarnation requires provision request and template ids",
                    );
                }
            }
            EnvironmentSource::External { connection } => {
                connection.validate()?;
                if self.incarnation.provision_request_id.is_some()
                    || self.incarnation.provider_target_id.is_some()
                    || self.incarnation.template_id.is_some()
                {
                    return invalid("external incarnation must not have provider linkage");
                }
            }
        }
        Ok(())
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
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateExternalEnvironment {
    pub request_id: EnvironmentProvisionRequestId,
    pub environment_id: EnvironmentId,
    pub incarnation_id: EnvironmentIncarnationId,
    pub connection: EnvironmentConnectionSpec,
    pub display_name: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListEnvironments {
    pub provider_id: Option<EnvironmentProviderId>,
    pub binding_id: Option<EnvironmentProviderBindingId>,
    pub status: Option<EnvironmentStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObserveProvisionedEnvironment {
    pub environment_id: EnvironmentId,
    pub provider_target_id: HostTargetId,
    pub status: EnvironmentStatus,
    pub observed_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailEnvironmentLifecycle {
    pub environment_id: EnvironmentId,
    pub message: String,
    pub observed_at_ms: i64,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetEnvironmentIngress {
    pub environment_id: EnvironmentId,
    pub enabled: bool,
    pub public_endpoint: Option<String>,
    pub updated_at_ms: i64,
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
    async fn create_external_environment(
        &self,
        request: CreateExternalEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn read_environment(
        &self,
        environment_id: &EnvironmentId,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn read_environment_by_request_id(
        &self,
        request_id: &EnvironmentProvisionRequestId,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn list_environments(
        &self,
        request: ListEnvironments,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError>;
    async fn list_environments_needing_reconcile(
        &self,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError>;
    async fn observe_provisioned_environment(
        &self,
        request: ObserveProvisionedEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError>;
    async fn fail_environment_lifecycle(
        &self,
        request: FailEnvironmentLifecycle,
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
        EnvironmentTemplateId::try_new(value.template_id.clone()).map_err(|error| {
            EnvironmentRegistryError::InvalidInput {
                message: format!("invalid template id: {error}"),
            }
        })?;
    Ok(template_id)
}

mod memory;
pub use memory::InMemoryEnvironmentRegistryStore;

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

fn validate_endpoint(name: &'static str, value: &str) -> Result<(), EnvironmentRegistryError> {
    validate_nonempty_string(name, value)?;
    if value.chars().any(char::is_whitespace) {
        return invalid(format!("{name} must not contain whitespace"));
    }
    Ok(())
}

fn validate_host_target_id(value: &HostTargetId) -> Result<(), EnvironmentRegistryError> {
    validate_general_string_id("target_id", value.as_str()).map_err(|error| {
        EnvironmentRegistryError::InvalidInput {
            message: error.to_string(),
        }
    })
}

fn validate_metadata(value: &BTreeMap<String, String>) -> Result<(), EnvironmentRegistryError> {
    for (key, value) in value {
        validate_nonempty_string("metadata key", key)?;
        validate_nonempty_string("metadata value", value)?;
    }
    Ok(())
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
