//! Runtime environment provider registry contracts.
//!
//! This crate owns provider-independent records and store traits for the
//! hosted runtime's environment-provider registry. Concrete persistence
//! adapters, such as `store-pg`, implement these traits outside this crate.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    str::FromStr,
};

use async_trait::async_trait;
use auth::{AuthGrantId, AuthProviderId, SecretId};
use engine::{
    RunId, SessionId, StringIdError, ToolCallId, ToolExecutionTarget, TurnId,
    validate_general_string_id,
};
use host_protocol::{
    control::{
        handshake::ControllerCapabilities,
        targets::{HostTargetStatus, HostTargetSummary},
    },
    shared::{
        HostCapabilities, HostConnectionSpec, HostPath, HostScope, HostTargetId, HostTransport,
        ImplementationInfo, JobId,
    },
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

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

            pub fn parse(value: impl Into<String>) -> Result<Self, StringIdError> {
                Self::try_new(value)
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
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

registry_string_id!(EnvironmentProviderId);
registry_string_id!(EnvironmentId);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EnvironmentRegistryError {
    #[error("environment registry {kind} already exists: {id}")]
    AlreadyExists { kind: &'static str, id: String },

    #[error("environment registry {kind} not found: {id}")]
    NotFound { kind: &'static str, id: String },

    #[error("invalid environment registry request: {message}")]
    InvalidInput { message: String },

    #[error("environment registry store failure: {message}")]
    Store { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentProviderRecord {
    pub provider_id: EnvironmentProviderId,
    pub provider_kind: EnvironmentProviderKind,
    pub display_name: Option<String>,
    pub status: EnvironmentProviderStatus,
    pub controller_connection: HostControllerConnectionSpec,
    pub capabilities: EnvironmentProviderCapabilities,
    pub implementation: ImplementationInfo,
    pub last_seen_ms: i64,
    pub lease_expires_ms: i64,
    pub metadata: BTreeMap<String, String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl EnvironmentProviderRecord {
    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        validate_nonempty_optional("display_name", self.display_name.as_deref())?;
        self.controller_connection.validate()?;
        self.capabilities.validate()?;
        validate_implementation(&self.implementation)?;
        validate_metadata(&self.metadata)?;
        validate_nonnegative_i64(self.last_seen_ms, "last_seen_ms")?;
        validate_nonnegative_i64(self.lease_expires_ms, "lease_expires_ms")?;
        validate_nonnegative_i64(self.created_at_ms, "created_at_ms")?;
        validate_nonnegative_i64(self.updated_at_ms, "updated_at_ms")?;
        if self.updated_at_ms < self.created_at_ms {
            return Err(EnvironmentRegistryError::InvalidInput {
                message: format!(
                    "updated_at_ms {} must be >= created_at_ms {}",
                    self.updated_at_ms, self.created_at_ms
                ),
            });
        }
        if self.lease_expires_ms < self.last_seen_ms {
            return Err(EnvironmentRegistryError::InvalidInput {
                message: format!(
                    "lease_expires_ms {} must be >= last_seen_ms {}",
                    self.lease_expires_ms, self.last_seen_ms
                ),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterEnvironmentProvider {
    pub provider_id: EnvironmentProviderId,
    pub provider_kind: EnvironmentProviderKind,
    pub display_name: Option<String>,
    pub controller_connection: HostControllerConnectionSpec,
    pub capabilities: EnvironmentProviderCapabilities,
    pub implementation: ImplementationInfo,
    pub lease_ttl_ms: i64,
    pub metadata: BTreeMap<String, String>,
    pub observed_at_ms: i64,
}

impl RegisterEnvironmentProvider {
    pub fn into_record(self) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(self.observed_at_ms, "observed_at_ms")?;
        validate_positive_i64(self.lease_ttl_ms, "lease_ttl_ms")?;
        let lease_expires_ms = self
            .observed_at_ms
            .checked_add(self.lease_ttl_ms)
            .ok_or_else(|| EnvironmentRegistryError::InvalidInput {
                message: "lease expiry timestamp overflowed".to_owned(),
            })?;
        let record = EnvironmentProviderRecord {
            provider_id: self.provider_id,
            provider_kind: self.provider_kind,
            display_name: self.display_name,
            status: EnvironmentProviderStatus::Online,
            controller_connection: self.controller_connection,
            capabilities: self.capabilities,
            implementation: self.implementation,
            last_seen_ms: self.observed_at_ms,
            lease_expires_ms,
            metadata: self.metadata,
            created_at_ms: self.observed_at_ms,
            updated_at_ms: self.observed_at_ms,
        };
        record.validate()?;
        Ok(record)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentProviderHeartbeat {
    pub provider_id: EnvironmentProviderId,
    pub observed_at_ms: i64,
    pub lease_ttl_ms: Option<i64>,
    pub observed_targets: Vec<HostTargetSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateEnvironmentProviderStatus {
    pub provider_id: EnvironmentProviderId,
    pub status: EnvironmentProviderStatus,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListEnvironmentProviders {
    pub status: Option<EnvironmentProviderStatus>,
    pub provider_kind: Option<EnvironmentProviderKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentProviderKind {
    Sandbox,
    Bridge,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentProviderStatus {
    Registering,
    Online,
    Stale,
    Offline,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostControllerConnectionSpec {
    pub endpoint: String,
    pub transport: HostTransport,
}

impl HostControllerConnectionSpec {
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentProviderCapabilities {
    #[serde(default)]
    pub list_targets: bool,
    #[serde(default)]
    pub create_target: bool,
    #[serde(default)]
    pub attach_target: bool,
    #[serde(default)]
    pub get_target: bool,
    #[serde(default)]
    pub close_target: bool,
}

impl EnvironmentProviderCapabilities {
    pub fn from_controller(value: ControllerCapabilities) -> Self {
        Self {
            list_targets: value.list_targets,
            create_target: value.create_target,
            attach_target: value.attach_target,
            get_target: value.get_target,
            close_target: value.close_target,
        }
    }

    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        if !self.list_targets
            && !self.create_target
            && !self.attach_target
            && !self.get_target
            && !self.close_target
        {
            return Err(EnvironmentRegistryError::InvalidInput {
                message: "environment provider must expose at least one controller capability"
                    .to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentTargetRecord {
    pub provider_id: EnvironmentProviderId,
    pub target_id: HostTargetId,
    pub display_name: Option<String>,
    pub status: HostTargetStatus,
    pub scope: HostScope,
    pub capabilities: HostCapabilities,
    pub default_cwd: Option<HostPath>,
    pub metadata: BTreeMap<String, String>,
    pub observed_at_ms: i64,
}

impl EnvironmentTargetRecord {
    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        validate_host_target_id(&self.target_id)?;
        validate_nonempty_optional("display_name", self.display_name.as_deref())?;
        validate_metadata(&self.metadata)?;
        validate_nonnegative_i64(self.observed_at_ms, "observed_at_ms")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpsertEnvironmentTargetRecord {
    pub provider_id: EnvironmentProviderId,
    pub target_id: HostTargetId,
    pub display_name: Option<String>,
    pub status: HostTargetStatus,
    pub scope: HostScope,
    pub capabilities: HostCapabilities,
    pub default_cwd: Option<HostPath>,
    pub metadata: BTreeMap<String, String>,
    pub observed_at_ms: i64,
}

impl UpsertEnvironmentTargetRecord {
    pub fn into_record(self) -> EnvironmentTargetRecord {
        EnvironmentTargetRecord {
            provider_id: self.provider_id,
            target_id: self.target_id,
            display_name: self.display_name,
            status: self.status,
            scope: self.scope,
            capabilities: self.capabilities,
            default_cwd: self.default_cwd,
            metadata: self.metadata,
            observed_at_ms: self.observed_at_ms,
        }
    }
}

impl From<(EnvironmentProviderId, HostTargetSummary, i64)> for UpsertEnvironmentTargetRecord {
    fn from(
        (provider_id, target, observed_at_ms): (EnvironmentProviderId, HostTargetSummary, i64),
    ) -> Self {
        Self {
            provider_id,
            target_id: target.target_id,
            display_name: target.display_name,
            status: target.status,
            scope: target.scope,
            capabilities: target.capabilities,
            default_cwd: target.default_cwd,
            metadata: target.metadata,
            observed_at_ms,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListEnvironmentTargets {
    pub provider_id: Option<EnvironmentProviderId>,
    pub status: Option<HostTargetStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateEnvironmentTargetStatus {
    pub provider_id: EnvironmentProviderId,
    pub target_id: HostTargetId,
    pub status: HostTargetStatus,
    pub observed_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEnvironmentBindingRecord {
    pub session_id: SessionId,
    pub env_id: EnvironmentId,
    pub provider_id: EnvironmentProviderId,
    pub target_id: HostTargetId,
    pub exec_target: ToolExecutionTarget,
    pub kind: SessionEnvironmentKind,
    pub status: SessionEnvironmentBindingStatus,
    pub capabilities: SessionEnvironmentCapabilities,
    pub connection: HostConnectionSpec,
    pub cwd: Option<HostPath>,
    pub fs_routes: Vec<SessionEnvironmentFsRoute>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl SessionEnvironmentBindingRecord {
    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        validate_host_target_id(&self.target_id)?;
        self.exec_target
            .validate()
            .map_err(|error| EnvironmentRegistryError::InvalidInput {
                message: format!("invalid exec_target: {error}"),
            })?;
        if self.exec_target.namespace != "env" || self.exec_target.id != self.env_id.as_str() {
            return Err(EnvironmentRegistryError::InvalidInput {
                message: format!(
                    "exec_target must be env:{} for session environment binding",
                    self.env_id
                ),
            });
        }
        self.capabilities.validate()?;
        validate_host_connection(&self.connection)?;
        for route in &self.fs_routes {
            route.validate()?;
        }
        validate_nonnegative_i64(self.created_at_ms, "created_at_ms")?;
        validate_nonnegative_i64(self.updated_at_ms, "updated_at_ms")?;
        if self.updated_at_ms < self.created_at_ms {
            return Err(EnvironmentRegistryError::InvalidInput {
                message: format!(
                    "updated_at_ms {} must be >= created_at_ms {}",
                    self.updated_at_ms, self.created_at_ms
                ),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSessionEnvironmentBinding {
    pub session_id: SessionId,
    pub env_id: EnvironmentId,
    pub provider_id: EnvironmentProviderId,
    pub target_id: HostTargetId,
    pub kind: SessionEnvironmentKind,
    pub status: SessionEnvironmentBindingStatus,
    pub capabilities: SessionEnvironmentCapabilities,
    pub connection: HostConnectionSpec,
    pub cwd: Option<HostPath>,
    pub fs_routes: Vec<SessionEnvironmentFsRoute>,
    pub created_at_ms: i64,
}

impl CreateSessionEnvironmentBinding {
    pub fn into_record(self) -> SessionEnvironmentBindingRecord {
        SessionEnvironmentBindingRecord {
            exec_target: ToolExecutionTarget::new("env", self.env_id.as_str()),
            session_id: self.session_id,
            env_id: self.env_id,
            provider_id: self.provider_id,
            target_id: self.target_id,
            kind: self.kind,
            status: self.status,
            capabilities: self.capabilities,
            connection: self.connection,
            cwd: self.cwd,
            fs_routes: self.fs_routes,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.created_at_ms,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateSessionEnvironmentBindingStatus {
    pub session_id: SessionId,
    pub env_id: EnvironmentId,
    pub status: SessionEnvironmentBindingStatus,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEnvironmentCredentialRecord {
    pub session_id: SessionId,
    pub env_id: EnvironmentId,
    pub env_name: String,
    pub source: SessionEnvironmentCredentialSource,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl SessionEnvironmentCredentialRecord {
    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        validate_env_name(&self.env_name)?;
        validate_nonnegative_i64(self.created_at_ms, "created_at_ms")?;
        validate_nonnegative_i64(self.updated_at_ms, "updated_at_ms")?;
        if self.updated_at_ms < self.created_at_ms {
            return Err(EnvironmentRegistryError::InvalidInput {
                message: format!(
                    "updated_at_ms {} must be >= created_at_ms {}",
                    self.updated_at_ms, self.created_at_ms
                ),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSessionEnvironmentCredential {
    pub session_id: SessionId,
    pub env_id: EnvironmentId,
    pub env_name: String,
    pub source: SessionEnvironmentCredentialSource,
    pub created_at_ms: i64,
}

impl CreateSessionEnvironmentCredential {
    pub fn into_record(self) -> SessionEnvironmentCredentialRecord {
        SessionEnvironmentCredentialRecord {
            session_id: self.session_id,
            env_id: self.env_id,
            env_name: self.env_name,
            source: self.source,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.created_at_ms,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEnvironmentCredentialSource {
    AuthGrant { grant_id: AuthGrantId },
    AuthProviderCredential { provider_id: AuthProviderId },
    DirectSecret { secret_id: SecretId },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListSessionEnvironmentCredentials {
    pub session_id: SessionId,
    pub env_id: EnvironmentId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEnvironmentKind {
    Sandbox,
    AttachedHost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEnvironmentBindingStatus {
    Attaching,
    Ready,
    Degraded,
    Detached,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEnvironmentCapabilities {
    #[serde(default)]
    pub fs_read: bool,
    #[serde(default)]
    pub fs_write: bool,
    #[serde(default)]
    pub process_exec: bool,
    #[serde(default)]
    pub process_stdin: bool,
    #[serde(default)]
    pub job_start: bool,
    #[serde(default)]
    pub job_list: bool,
    #[serde(default)]
    pub job_read: bool,
    #[serde(default)]
    pub job_cancel: bool,
    #[serde(default)]
    pub job_wait_hint: bool,
    #[serde(default)]
    pub job_dependencies: bool,
    #[serde(default)]
    pub job_queue_keys: bool,
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub persistent: bool,
}

impl SessionEnvironmentCapabilities {
    pub fn from_host(capabilities: &HostCapabilities, persistent: bool) -> Self {
        Self {
            fs_read: capabilities.filesystem_read,
            fs_write: capabilities.filesystem_write,
            process_exec: capabilities.process_start,
            process_stdin: capabilities.process_stdin,
            job_start: capabilities.job_start,
            job_list: capabilities.job_list,
            job_read: capabilities.job_read,
            job_cancel: capabilities.job_cancel,
            job_wait_hint: capabilities.job_wait_hint,
            job_dependencies: capabilities.job_dependencies,
            job_queue_keys: capabilities.job_queue_keys,
            network: false,
            persistent,
        }
    }

    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        if self.fs_write && !self.fs_read {
            return Err(EnvironmentRegistryError::InvalidInput {
                message: "fs_write requires fs_read".to_owned(),
            });
        }
        if self.process_stdin && !self.process_exec {
            return Err(EnvironmentRegistryError::InvalidInput {
                message: "process_stdin requires process_exec".to_owned(),
            });
        }
        if self.job_wait_hint && !self.job_read {
            return Err(EnvironmentRegistryError::InvalidInput {
                message: "job_wait_hint requires job_read".to_owned(),
            });
        }
        if self.job_list && !self.job_read {
            return Err(EnvironmentRegistryError::InvalidInput {
                message: "job_list requires job_read".to_owned(),
            });
        }
        if self.job_dependencies && !self.job_start {
            return Err(EnvironmentRegistryError::InvalidInput {
                message: "job_dependencies requires job_start".to_owned(),
            });
        }
        if self.job_queue_keys && !self.job_start {
            return Err(EnvironmentRegistryError::InvalidInput {
                message: "job_queue_keys requires job_start".to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEnvironmentFsRoute {
    pub path: HostPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<HostPath>,
    pub access: SessionEnvironmentFsRouteAccess,
    pub same_state_as_active_env: Option<EnvironmentId>,
}

impl SessionEnvironmentFsRoute {
    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        if !self.path.is_absolute() {
            return Err(EnvironmentRegistryError::InvalidInput {
                message: format!("environment fs route path must be absolute: {}", self.path),
            });
        }
        if let Some(source_path) = self.source_path.as_ref() {
            if !source_path.is_absolute() {
                return Err(EnvironmentRegistryError::InvalidInput {
                    message: format!(
                        "environment fs route source_path must be absolute: {source_path}"
                    ),
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEnvironmentFsRouteAccess {
    ReadOnly,
    ReadWrite,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobHandleRecord {
    pub session_id: SessionId,
    pub env_id: EnvironmentId,
    pub provider_id: EnvironmentProviderId,
    pub target_id: HostTargetId,
    pub namespace: String,
    pub job_id: JobId,
    pub name: Option<String>,
    pub queue_key: Option<String>,
    pub created_by_run_id: Option<RunId>,
    pub created_by_turn_id: Option<TurnId>,
    pub created_by_tool_call_id: Option<ToolCallId>,
    pub created_at_ms: i64,
    pub start_request_hash: String,
}

impl JobHandleRecord {
    pub fn validate(&self) -> Result<(), EnvironmentRegistryError> {
        validate_host_job_id(&self.job_id)?;
        validate_host_target_id(&self.target_id)?;
        validate_general_string_id("namespace", &self.namespace).map_err(|error| {
            EnvironmentRegistryError::InvalidInput {
                message: format!("invalid namespace: {error}"),
            }
        })?;
        if self.namespace != self.session_id.as_str() {
            return Err(EnvironmentRegistryError::InvalidInput {
                message: "job namespace must equal session_id".to_owned(),
            });
        }
        validate_nonempty_optional("job name", self.name.as_deref())?;
        validate_nonempty_optional("queue_key", self.queue_key.as_deref())?;
        validate_optional_metadata_component("job name", self.name.as_deref())?;
        if let Some(queue_key) = self.queue_key.as_deref() {
            validate_general_string_id("queue_key", queue_key).map_err(|error| {
                EnvironmentRegistryError::InvalidInput {
                    message: format!("invalid queue_key: {error}"),
                }
            })?;
        }
        validate_nonempty_string("start_request_hash", &self.start_request_hash)?;
        validate_metadata_component("start_request_hash", &self.start_request_hash)?;
        validate_nonnegative_i64(self.created_at_ms, "created_at_ms")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateJobHandle {
    pub session_id: SessionId,
    pub env_id: EnvironmentId,
    pub provider_id: EnvironmentProviderId,
    pub target_id: HostTargetId,
    pub namespace: String,
    pub job_id: JobId,
    pub name: Option<String>,
    pub queue_key: Option<String>,
    pub created_by_run_id: Option<RunId>,
    pub created_by_turn_id: Option<TurnId>,
    pub created_by_tool_call_id: Option<ToolCallId>,
    pub created_at_ms: i64,
    pub start_request_hash: String,
}

impl CreateJobHandle {
    pub fn into_record(self) -> JobHandleRecord {
        JobHandleRecord {
            session_id: self.session_id,
            env_id: self.env_id,
            provider_id: self.provider_id,
            target_id: self.target_id,
            namespace: self.namespace,
            job_id: self.job_id,
            name: self.name,
            queue_key: self.queue_key,
            created_by_run_id: self.created_by_run_id,
            created_by_turn_id: self.created_by_turn_id,
            created_by_tool_call_id: self.created_by_tool_call_id,
            created_at_ms: self.created_at_ms,
            start_request_hash: self.start_request_hash,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListJobHandles {
    pub session_id: SessionId,
    pub env_id: Option<EnvironmentId>,
    pub limit: Option<usize>,
}

#[async_trait]
pub trait EnvironmentProviderStore: Send + Sync {
    async fn register_provider(
        &self,
        record: RegisterEnvironmentProvider,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError>;

    async fn read_provider(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError>;

    async fn list_providers(
        &self,
        request: ListEnvironmentProviders,
    ) -> Result<Vec<EnvironmentProviderRecord>, EnvironmentRegistryError>;

    async fn update_provider_heartbeat(
        &self,
        heartbeat: EnvironmentProviderHeartbeat,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError>;

    async fn update_provider_status(
        &self,
        request: UpdateEnvironmentProviderStatus,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError>;

    async fn delete_provider(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError>;
}

#[async_trait]
pub trait EnvironmentTargetStore: Send + Sync {
    async fn upsert_target(
        &self,
        record: UpsertEnvironmentTargetRecord,
    ) -> Result<EnvironmentTargetRecord, EnvironmentRegistryError>;

    async fn read_target(
        &self,
        provider_id: &EnvironmentProviderId,
        target_id: &HostTargetId,
    ) -> Result<EnvironmentTargetRecord, EnvironmentRegistryError>;

    async fn list_targets(
        &self,
        request: ListEnvironmentTargets,
    ) -> Result<Vec<EnvironmentTargetRecord>, EnvironmentRegistryError>;

    async fn update_target_status(
        &self,
        request: UpdateEnvironmentTargetStatus,
    ) -> Result<EnvironmentTargetRecord, EnvironmentRegistryError>;
}

#[async_trait]
pub trait SessionEnvironmentBindingStore: Send + Sync {
    async fn create_binding(
        &self,
        record: CreateSessionEnvironmentBinding,
    ) -> Result<SessionEnvironmentBindingRecord, EnvironmentRegistryError>;

    async fn read_binding(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
    ) -> Result<SessionEnvironmentBindingRecord, EnvironmentRegistryError>;

    async fn list_bindings_for_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<SessionEnvironmentBindingRecord>, EnvironmentRegistryError>;

    async fn update_binding_status(
        &self,
        request: UpdateSessionEnvironmentBindingStatus,
    ) -> Result<SessionEnvironmentBindingRecord, EnvironmentRegistryError>;

    async fn delete_binding(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
    ) -> Result<SessionEnvironmentBindingRecord, EnvironmentRegistryError>;
}

#[async_trait]
pub trait SessionEnvironmentCredentialStore: Send + Sync {
    async fn bind_credential(
        &self,
        record: CreateSessionEnvironmentCredential,
    ) -> Result<SessionEnvironmentCredentialRecord, EnvironmentRegistryError>;

    async fn list_credentials(
        &self,
        request: ListSessionEnvironmentCredentials,
    ) -> Result<Vec<SessionEnvironmentCredentialRecord>, EnvironmentRegistryError>;

    async fn unbind_credential(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
        env_name: &str,
    ) -> Result<SessionEnvironmentCredentialRecord, EnvironmentRegistryError>;
}

#[async_trait]
pub trait JobHandleStore: Send + Sync {
    async fn create_job_handles(
        &self,
        records: Vec<CreateJobHandle>,
    ) -> Result<Vec<JobHandleRecord>, EnvironmentRegistryError>;

    async fn read_job_handle(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
        job_id: &JobId,
    ) -> Result<JobHandleRecord, EnvironmentRegistryError>;

    async fn list_job_handles(
        &self,
        request: ListJobHandles,
    ) -> Result<Vec<JobHandleRecord>, EnvironmentRegistryError>;

    async fn delete_job_handle(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
        job_id: &JobId,
    ) -> Result<JobHandleRecord, EnvironmentRegistryError>;
}

mod memory;
pub use memory::InMemoryEnvironmentRegistryStore;

fn validate_implementation(
    implementation: &ImplementationInfo,
) -> Result<(), EnvironmentRegistryError> {
    validate_nonempty_string("implementation name", &implementation.name)?;
    validate_nonempty_optional("implementation version", implementation.version.as_deref())
}

fn validate_host_connection(
    connection: &HostConnectionSpec,
) -> Result<(), EnvironmentRegistryError> {
    validate_host_target_id(&connection.target_id)?;
    validate_endpoint("host data-plane endpoint", &connection.endpoint)
}

fn validate_host_target_id(target_id: &HostTargetId) -> Result<(), EnvironmentRegistryError> {
    validate_general_string_id("HostTargetId", target_id.as_str()).map_err(|error| {
        EnvironmentRegistryError::InvalidInput {
            message: format!("invalid host target id: {error}"),
        }
    })
}

fn validate_host_job_id(job_id: &JobId) -> Result<(), EnvironmentRegistryError> {
    validate_general_string_id("JobId", job_id.as_str()).map_err(|error| {
        EnvironmentRegistryError::InvalidInput {
            message: format!("invalid job id: {error}"),
        }
    })
}

fn validate_list_job_handles(request: &ListJobHandles) -> Result<(), EnvironmentRegistryError> {
    if matches!(request.limit, Some(0)) {
        return Err(EnvironmentRegistryError::InvalidInput {
            message: "job handle list limit must be greater than zero".to_owned(),
        });
    }
    Ok(())
}

fn validate_env_name(value: &str) -> Result<(), EnvironmentRegistryError> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(EnvironmentRegistryError::InvalidInput {
            message: "credential env name must not be empty".to_owned(),
        });
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return Err(EnvironmentRegistryError::InvalidInput {
            message: format!("invalid credential env name: {value}"),
        });
    }
    let len = 1 + chars
        .try_fold(0usize, |count, ch| {
            if ch == '_' || ch.is_ascii_alphanumeric() {
                Ok(count + 1)
            } else {
                Err(())
            }
        })
        .map_err(|()| EnvironmentRegistryError::InvalidInput {
            message: format!("invalid credential env name: {value}"),
        })?;
    if len > 128 {
        return Err(EnvironmentRegistryError::InvalidInput {
            message: format!("credential env name is too long: {len} bytes, max 128"),
        });
    }
    Ok(())
}

fn validate_optional_metadata_component(
    name: &'static str,
    value: Option<&str>,
) -> Result<(), EnvironmentRegistryError> {
    if let Some(value) = value {
        validate_metadata_component(name, value)?;
    }
    Ok(())
}

fn validate_endpoint(name: &'static str, value: &str) -> Result<(), EnvironmentRegistryError> {
    validate_nonempty_string(name, value)?;
    if value.len() > 2048 {
        return Err(EnvironmentRegistryError::InvalidInput {
            message: format!("{name} is too long: {} bytes, max 2048", value.len()),
        });
    }
    if value.chars().any(char::is_whitespace) || value.chars().any(|ch| ch.is_control()) {
        return Err(EnvironmentRegistryError::InvalidInput {
            message: format!("{name} must not contain whitespace or control characters"),
        });
    }
    Ok(())
}

fn validate_metadata(metadata: &BTreeMap<String, String>) -> Result<(), EnvironmentRegistryError> {
    let mut seen = BTreeSet::new();
    for (key, value) in metadata {
        validate_metadata_component("metadata key", key)?;
        validate_metadata_component("metadata value", value)?;
        if !seen.insert(key.as_str()) {
            return Err(EnvironmentRegistryError::InvalidInput {
                message: format!("duplicate metadata key {key}"),
            });
        }
    }
    Ok(())
}

fn validate_metadata_component(
    name: &'static str,
    value: &str,
) -> Result<(), EnvironmentRegistryError> {
    if value.len() > 512 {
        return Err(EnvironmentRegistryError::InvalidInput {
            message: format!("{name} is too long: {} bytes, max 512", value.len()),
        });
    }
    if value.chars().any(|ch| ch.is_control()) {
        return Err(EnvironmentRegistryError::InvalidInput {
            message: format!("{name} must not contain control characters"),
        });
    }
    Ok(())
}

fn validate_nonempty_optional(
    name: &'static str,
    value: Option<&str>,
) -> Result<(), EnvironmentRegistryError> {
    if let Some(value) = value {
        validate_nonempty_string(name, value)?;
    }
    Ok(())
}

fn validate_nonempty_string(
    name: &'static str,
    value: &str,
) -> Result<(), EnvironmentRegistryError> {
    if value.is_empty() {
        return Err(EnvironmentRegistryError::InvalidInput {
            message: format!("{name} must not be empty"),
        });
    }
    Ok(())
}

fn validate_nonnegative_i64(
    value: i64,
    name: &'static str,
) -> Result<(), EnvironmentRegistryError> {
    if value < 0 {
        return Err(EnvironmentRegistryError::InvalidInput {
            message: format!("{name} must be nonnegative: {value}"),
        });
    }
    Ok(())
}

fn validate_positive_i64(value: i64, name: &'static str) -> Result<(), EnvironmentRegistryError> {
    if value <= 0 {
        return Err(EnvironmentRegistryError::InvalidInput {
            message: format!("{name} must be positive: {value}"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
