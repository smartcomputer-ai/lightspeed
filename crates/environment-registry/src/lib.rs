//! Runtime environment provider registry contracts.
//!
//! This crate owns provider-independent records and store traits for the
//! hosted runtime's environment-provider registry. Concrete persistence
//! adapters, such as `store-pg`, implement these traits outside this crate.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    str::FromStr,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use engine::{SessionId, StringIdError, ToolExecutionTarget, validate_general_string_id};
use host_protocol::{
    control::{
        handshake::ControllerCapabilities,
        targets::{HostTargetStatus, HostTargetSummary},
    },
    shared::{
        HostCapabilities, HostConnectionSpec, HostPath, HostScope, HostTargetId, HostTransport,
        ImplementationInfo,
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

#[derive(Clone, Default)]
pub struct InMemoryEnvironmentRegistryStore {
    providers: Arc<RwLock<BTreeMap<EnvironmentProviderId, EnvironmentProviderRecord>>>,
    targets: Arc<RwLock<BTreeMap<(EnvironmentProviderId, HostTargetId), EnvironmentTargetRecord>>>,
    bindings: Arc<RwLock<BTreeMap<(SessionId, EnvironmentId), SessionEnvironmentBindingRecord>>>,
}

impl InMemoryEnvironmentRegistryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl EnvironmentProviderStore for InMemoryEnvironmentRegistryStore {
    async fn register_provider(
        &self,
        record: RegisterEnvironmentProvider,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        let record = record.into_record()?;
        let mut providers =
            self.providers
                .write()
                .map_err(|_| EnvironmentRegistryError::Store {
                    message: "environment provider registry write lock poisoned".to_owned(),
                })?;
        let created_at_ms = providers
            .get(&record.provider_id)
            .map(|existing| existing.created_at_ms)
            .unwrap_or(record.created_at_ms);
        let mut record = record;
        record.created_at_ms = created_at_ms;
        record.validate()?;
        providers.insert(record.provider_id.clone(), record.clone());
        Ok(record)
    }

    async fn read_provider(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        let providers = self
            .providers
            .read()
            .map_err(|_| EnvironmentRegistryError::Store {
                message: "environment provider registry read lock poisoned".to_owned(),
            })?;
        providers
            .get(provider_id)
            .cloned()
            .ok_or_else(|| provider_not_found(provider_id))
    }

    async fn list_providers(
        &self,
        request: ListEnvironmentProviders,
    ) -> Result<Vec<EnvironmentProviderRecord>, EnvironmentRegistryError> {
        let providers = self
            .providers
            .read()
            .map_err(|_| EnvironmentRegistryError::Store {
                message: "environment provider registry read lock poisoned".to_owned(),
            })?;
        Ok(providers
            .values()
            .filter(|record| request.status.is_none_or(|status| record.status == status))
            .filter(|record| {
                request
                    .provider_kind
                    .is_none_or(|kind| record.provider_kind == kind)
            })
            .cloned()
            .collect())
    }

    async fn update_provider_heartbeat(
        &self,
        heartbeat: EnvironmentProviderHeartbeat,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(heartbeat.observed_at_ms, "observed_at_ms")?;
        if let Some(ttl) = heartbeat.lease_ttl_ms {
            validate_positive_i64(ttl, "lease_ttl_ms")?;
        }
        let mut providers =
            self.providers
                .write()
                .map_err(|_| EnvironmentRegistryError::Store {
                    message: "environment provider registry write lock poisoned".to_owned(),
                })?;
        let record = providers
            .get_mut(&heartbeat.provider_id)
            .ok_or_else(|| provider_not_found(&heartbeat.provider_id))?;
        let ttl = heartbeat
            .lease_ttl_ms
            .unwrap_or_else(|| record.lease_expires_ms.saturating_sub(record.last_seen_ms));
        record.last_seen_ms = heartbeat.observed_at_ms;
        record.lease_expires_ms = heartbeat.observed_at_ms.checked_add(ttl).ok_or_else(|| {
            EnvironmentRegistryError::InvalidInput {
                message: "lease expiry timestamp overflowed".to_owned(),
            }
        })?;
        record.updated_at_ms = heartbeat.observed_at_ms;
        record.status = EnvironmentProviderStatus::Online;
        record.validate()?;
        Ok(record.clone())
    }

    async fn update_provider_status(
        &self,
        request: UpdateEnvironmentProviderStatus,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.updated_at_ms, "updated_at_ms")?;
        let mut providers =
            self.providers
                .write()
                .map_err(|_| EnvironmentRegistryError::Store {
                    message: "environment provider registry write lock poisoned".to_owned(),
                })?;
        let record = providers
            .get_mut(&request.provider_id)
            .ok_or_else(|| provider_not_found(&request.provider_id))?;
        record.status = request.status;
        record.updated_at_ms = request.updated_at_ms;
        record.validate()?;
        Ok(record.clone())
    }

    async fn delete_provider(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        let mut providers =
            self.providers
                .write()
                .map_err(|_| EnvironmentRegistryError::Store {
                    message: "environment provider registry write lock poisoned".to_owned(),
                })?;
        providers
            .remove(provider_id)
            .ok_or_else(|| provider_not_found(provider_id))
    }
}

#[async_trait]
impl EnvironmentTargetStore for InMemoryEnvironmentRegistryStore {
    async fn upsert_target(
        &self,
        record: UpsertEnvironmentTargetRecord,
    ) -> Result<EnvironmentTargetRecord, EnvironmentRegistryError> {
        let record = record.into_record();
        record.validate()?;
        let mut targets = self
            .targets
            .write()
            .map_err(|_| EnvironmentRegistryError::Store {
                message: "environment target registry write lock poisoned".to_owned(),
            })?;
        targets.insert(
            (record.provider_id.clone(), record.target_id.clone()),
            record.clone(),
        );
        Ok(record)
    }

    async fn read_target(
        &self,
        provider_id: &EnvironmentProviderId,
        target_id: &HostTargetId,
    ) -> Result<EnvironmentTargetRecord, EnvironmentRegistryError> {
        let targets = self
            .targets
            .read()
            .map_err(|_| EnvironmentRegistryError::Store {
                message: "environment target registry read lock poisoned".to_owned(),
            })?;
        targets
            .get(&(provider_id.clone(), target_id.clone()))
            .cloned()
            .ok_or_else(|| target_not_found(provider_id, target_id))
    }

    async fn list_targets(
        &self,
        request: ListEnvironmentTargets,
    ) -> Result<Vec<EnvironmentTargetRecord>, EnvironmentRegistryError> {
        let targets = self
            .targets
            .read()
            .map_err(|_| EnvironmentRegistryError::Store {
                message: "environment target registry read lock poisoned".to_owned(),
            })?;
        Ok(targets
            .values()
            .filter(|record| {
                request
                    .provider_id
                    .as_ref()
                    .is_none_or(|provider_id| &record.provider_id == provider_id)
            })
            .filter(|record| request.status.is_none_or(|status| record.status == status))
            .cloned()
            .collect())
    }

    async fn update_target_status(
        &self,
        request: UpdateEnvironmentTargetStatus,
    ) -> Result<EnvironmentTargetRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.observed_at_ms, "observed_at_ms")?;
        let mut targets = self
            .targets
            .write()
            .map_err(|_| EnvironmentRegistryError::Store {
                message: "environment target registry write lock poisoned".to_owned(),
            })?;
        let record = targets
            .get_mut(&(request.provider_id.clone(), request.target_id.clone()))
            .ok_or_else(|| target_not_found(&request.provider_id, &request.target_id))?;
        record.status = request.status;
        record.observed_at_ms = request.observed_at_ms;
        record.validate()?;
        Ok(record.clone())
    }
}

#[async_trait]
impl SessionEnvironmentBindingStore for InMemoryEnvironmentRegistryStore {
    async fn create_binding(
        &self,
        record: CreateSessionEnvironmentBinding,
    ) -> Result<SessionEnvironmentBindingRecord, EnvironmentRegistryError> {
        let record = record.into_record();
        record.validate()?;
        let mut bindings = self
            .bindings
            .write()
            .map_err(|_| EnvironmentRegistryError::Store {
                message: "session environment binding registry write lock poisoned".to_owned(),
            })?;
        let key = (record.session_id.clone(), record.env_id.clone());
        if bindings.contains_key(&key) {
            return Err(EnvironmentRegistryError::AlreadyExists {
                kind: "session_environment_binding",
                id: binding_id(&record.session_id, &record.env_id),
            });
        }
        bindings.insert(key, record.clone());
        Ok(record)
    }

    async fn read_binding(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
    ) -> Result<SessionEnvironmentBindingRecord, EnvironmentRegistryError> {
        let bindings = self
            .bindings
            .read()
            .map_err(|_| EnvironmentRegistryError::Store {
                message: "session environment binding registry read lock poisoned".to_owned(),
            })?;
        bindings
            .get(&(session_id.clone(), env_id.clone()))
            .cloned()
            .ok_or_else(|| binding_not_found(session_id, env_id))
    }

    async fn list_bindings_for_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<SessionEnvironmentBindingRecord>, EnvironmentRegistryError> {
        let bindings = self
            .bindings
            .read()
            .map_err(|_| EnvironmentRegistryError::Store {
                message: "session environment binding registry read lock poisoned".to_owned(),
            })?;
        Ok(bindings
            .values()
            .filter(|record| &record.session_id == session_id)
            .cloned()
            .collect())
    }

    async fn update_binding_status(
        &self,
        request: UpdateSessionEnvironmentBindingStatus,
    ) -> Result<SessionEnvironmentBindingRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.updated_at_ms, "updated_at_ms")?;
        let mut bindings = self
            .bindings
            .write()
            .map_err(|_| EnvironmentRegistryError::Store {
                message: "session environment binding registry write lock poisoned".to_owned(),
            })?;
        let record = bindings
            .get_mut(&(request.session_id.clone(), request.env_id.clone()))
            .ok_or_else(|| binding_not_found(&request.session_id, &request.env_id))?;
        record.status = request.status;
        record.updated_at_ms = request.updated_at_ms;
        record.validate()?;
        Ok(record.clone())
    }

    async fn delete_binding(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
    ) -> Result<SessionEnvironmentBindingRecord, EnvironmentRegistryError> {
        let mut bindings = self
            .bindings
            .write()
            .map_err(|_| EnvironmentRegistryError::Store {
                message: "session environment binding registry write lock poisoned".to_owned(),
            })?;
        bindings
            .remove(&(session_id.clone(), env_id.clone()))
            .ok_or_else(|| binding_not_found(session_id, env_id))
    }
}

fn provider_not_found(provider_id: &EnvironmentProviderId) -> EnvironmentRegistryError {
    EnvironmentRegistryError::NotFound {
        kind: "environment_provider",
        id: provider_id.as_str().to_owned(),
    }
}

fn target_not_found(
    provider_id: &EnvironmentProviderId,
    target_id: &HostTargetId,
) -> EnvironmentRegistryError {
    EnvironmentRegistryError::NotFound {
        kind: "environment_target",
        id: format!("{provider_id}/{}", target_id.as_str()),
    }
}

fn binding_not_found(session_id: &SessionId, env_id: &EnvironmentId) -> EnvironmentRegistryError {
    EnvironmentRegistryError::NotFound {
        kind: "session_environment_binding",
        id: binding_id(session_id, env_id),
    }
}

fn binding_id(session_id: &SessionId, env_id: &EnvironmentId) -> String {
    format!("{session_id}/{}", env_id.as_str())
}

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
mod tests {
    use super::*;
    use host_protocol::shared::HostTransport;

    fn provider_registration(provider_id: &str) -> RegisterEnvironmentProvider {
        RegisterEnvironmentProvider {
            provider_id: EnvironmentProviderId::new(provider_id),
            provider_kind: EnvironmentProviderKind::Bridge,
            display_name: Some("Local bridge".to_owned()),
            controller_connection: HostControllerConnectionSpec::new(
                "ws://127.0.0.1:9000/controller",
                HostTransport::WebSocket,
            ),
            capabilities: EnvironmentProviderCapabilities {
                list_targets: true,
                attach_target: true,
                get_target: true,
                ..EnvironmentProviderCapabilities::default()
            },
            implementation: ImplementationInfo {
                name: "test-bridge".to_owned(),
                version: Some("1.0.0".to_owned()),
            },
            lease_ttl_ms: 30_000,
            metadata: BTreeMap::new(),
            observed_at_ms: 10,
        }
    }

    fn host_connection(target_id: &str) -> HostConnectionSpec {
        HostConnectionSpec {
            target_id: HostTargetId::new(target_id),
            endpoint: "ws://127.0.0.1:9001/data".to_owned(),
            transport: HostTransport::WebSocket,
            scope: HostScope::Session {
                session_id: "session_1".to_owned(),
            },
            default_cwd: Some(HostPath::new("/workspace").expect("cwd")),
            capabilities: HostCapabilities::filesystem(true, true).with_process(),
        }
    }

    fn target(provider_id: &str, target_id: &str) -> UpsertEnvironmentTargetRecord {
        UpsertEnvironmentTargetRecord {
            provider_id: EnvironmentProviderId::new(provider_id),
            target_id: HostTargetId::new(target_id),
            display_name: Some("Local host".to_owned()),
            status: HostTargetStatus::Ready,
            scope: HostScope::Default,
            capabilities: HostCapabilities::filesystem(true, true).with_process(),
            default_cwd: Some(HostPath::new("/workspace").expect("cwd")),
            metadata: BTreeMap::new(),
            observed_at_ms: 20,
        }
    }

    fn binding(session_id: &str, env_id: &str) -> CreateSessionEnvironmentBinding {
        CreateSessionEnvironmentBinding {
            session_id: SessionId::new(session_id),
            env_id: EnvironmentId::new(env_id),
            provider_id: EnvironmentProviderId::new("bridge-local"),
            target_id: HostTargetId::new("local-host"),
            kind: SessionEnvironmentKind::AttachedHost,
            status: SessionEnvironmentBindingStatus::Ready,
            capabilities: SessionEnvironmentCapabilities {
                fs_read: true,
                fs_write: true,
                process_exec: true,
                process_stdin: true,
                network: false,
                persistent: true,
            },
            connection: host_connection("local-host"),
            cwd: Some(HostPath::new("/workspace").expect("cwd")),
            fs_routes: vec![SessionEnvironmentFsRoute {
                path: HostPath::new("/workspace").expect("route"),
                source_path: None,
                access: SessionEnvironmentFsRouteAccess::ReadWrite,
                same_state_as_active_env: Some(EnvironmentId::new(env_id)),
            }],
            created_at_ms: 30,
        }
    }

    #[test]
    fn provider_records_validate_controller_shape() {
        let record = provider_registration("bridge-local")
            .into_record()
            .expect("record");

        record.validate().expect("valid provider");
    }

    #[test]
    fn provider_records_reject_empty_capabilities() {
        let mut registration = provider_registration("bridge-local");
        registration.capabilities = EnvironmentProviderCapabilities::default();

        let error = registration
            .into_record()
            .expect_err("empty provider capabilities");

        assert!(matches!(
            error,
            EnvironmentRegistryError::InvalidInput { .. }
        ));
    }

    #[test]
    fn binding_records_require_matching_env_execution_target() {
        let mut record = binding("session_1", "local").into_record();
        record.exec_target = ToolExecutionTarget::new("env", "other");

        let error = record
            .validate()
            .expect_err("mismatched env execution target");

        assert!(matches!(
            error,
            EnvironmentRegistryError::InvalidInput { .. }
        ));
    }

    #[tokio::test]
    async fn in_memory_store_registers_heartbeats_lists_and_deletes_providers() {
        let store = InMemoryEnvironmentRegistryStore::new();

        let registered = store
            .register_provider(provider_registration("bridge-local"))
            .await
            .expect("register provider");
        assert_eq!(registered.provider_id.as_str(), "bridge-local");
        assert_eq!(registered.status, EnvironmentProviderStatus::Online);

        let heartbeat = store
            .update_provider_heartbeat(EnvironmentProviderHeartbeat {
                provider_id: EnvironmentProviderId::new("bridge-local"),
                observed_at_ms: 40,
                lease_ttl_ms: Some(10_000),
                observed_targets: Vec::new(),
            })
            .await
            .expect("heartbeat");
        assert_eq!(heartbeat.last_seen_ms, 40);
        assert_eq!(heartbeat.lease_expires_ms, 10_040);

        let online = store
            .list_providers(ListEnvironmentProviders {
                status: Some(EnvironmentProviderStatus::Online),
                provider_kind: Some(EnvironmentProviderKind::Bridge),
            })
            .await
            .expect("list providers");
        assert_eq!(online, vec![heartbeat.clone()]);

        let deleted = store
            .delete_provider(&EnvironmentProviderId::new("bridge-local"))
            .await
            .expect("delete provider");
        assert_eq!(deleted, heartbeat);
    }

    #[tokio::test]
    async fn in_memory_store_upserts_targets() {
        let store = InMemoryEnvironmentRegistryStore::new();

        let created = store
            .upsert_target(target("bridge-local", "local-host"))
            .await
            .expect("upsert target");
        assert_eq!(created.target_id.as_str(), "local-host");

        let ready = store
            .list_targets(ListEnvironmentTargets {
                provider_id: Some(EnvironmentProviderId::new("bridge-local")),
                status: Some(HostTargetStatus::Ready),
            })
            .await
            .expect("list targets");
        assert_eq!(ready, vec![created.clone()]);

        let stopped = store
            .update_target_status(UpdateEnvironmentTargetStatus {
                provider_id: EnvironmentProviderId::new("bridge-local"),
                target_id: HostTargetId::new("local-host"),
                status: HostTargetStatus::Stopped,
                observed_at_ms: 50,
            })
            .await
            .expect("update target status");
        assert_eq!(stopped.status, HostTargetStatus::Stopped);
    }

    #[tokio::test]
    async fn in_memory_store_creates_lists_updates_and_deletes_bindings() {
        let store = InMemoryEnvironmentRegistryStore::new();

        let created = store
            .create_binding(binding("session_1", "local"))
            .await
            .expect("create binding");
        assert_eq!(
            created.exec_target,
            ToolExecutionTarget::new("env", "local")
        );

        let listed = store
            .list_bindings_for_session(&SessionId::new("session_1"))
            .await
            .expect("list bindings");
        assert_eq!(listed, vec![created.clone()]);

        let degraded = store
            .update_binding_status(UpdateSessionEnvironmentBindingStatus {
                session_id: SessionId::new("session_1"),
                env_id: EnvironmentId::new("local"),
                status: SessionEnvironmentBindingStatus::Degraded,
                updated_at_ms: 60,
            })
            .await
            .expect("update binding");
        assert_eq!(degraded.status, SessionEnvironmentBindingStatus::Degraded);

        let deleted = store
            .delete_binding(&SessionId::new("session_1"), &EnvironmentId::new("local"))
            .await
            .expect("delete binding");
        assert_eq!(deleted, degraded);
    }
}
