use std::{collections::BTreeMap, sync::RwLock};

use async_trait::async_trait;
use uuid::Uuid;

use super::*;

type CredentialKey = (EnvironmentId, String);
type BindingKey = (Uuid, EnvironmentProviderBindingId);

struct StoredRegistrationKey {
    secret_hash: String,
    record: EnvironmentRegistrationKeyRecord,
}

#[derive(Default)]
struct RegistryState {
    providers: BTreeMap<EnvironmentProviderId, EnvironmentProviderRecord>,
    bindings: BTreeMap<BindingKey, EnvironmentProviderBindingRecord>,
    environments: BTreeMap<EnvironmentId, EnvironmentRecord>,
    requests: BTreeMap<EnvironmentProvisionRequestId, EnvironmentId>,
    credentials: BTreeMap<CredentialKey, EnvironmentCredentialRecord>,
    registration_keys: BTreeMap<EnvironmentRegistrationKeyId, StoredRegistrationKey>,
}

pub struct InMemoryEnvironmentRegistryStore {
    universe_id: Uuid,
    state: RwLock<RegistryState>,
}

impl Default for InMemoryEnvironmentRegistryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryEnvironmentRegistryStore {
    pub fn new() -> Self {
        Self::for_universe(Uuid::nil())
    }

    pub fn for_universe(universe_id: Uuid) -> Self {
        Self {
            universe_id,
            state: RwLock::new(RegistryState::default()),
        }
    }

    pub fn universe_id(&self) -> Uuid {
        self.universe_id
    }

    fn read_state(
        &self,
    ) -> Result<std::sync::RwLockReadGuard<'_, RegistryState>, EnvironmentRegistryError> {
        self.state
            .read()
            .map_err(|_| EnvironmentRegistryError::Store {
                message: "environment registry read lock poisoned".to_owned(),
            })
    }

    fn write_state(
        &self,
    ) -> Result<std::sync::RwLockWriteGuard<'_, RegistryState>, EnvironmentRegistryError> {
        self.state
            .write()
            .map_err(|_| EnvironmentRegistryError::Store {
                message: "environment registry write lock poisoned".to_owned(),
            })
    }
}

#[async_trait]
impl EnvironmentProviderStore for InMemoryEnvironmentRegistryStore {
    async fn put_provider(
        &self,
        request: PutEnvironmentProvider,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        let mut record = request.into_record()?;
        let mut state = self.write_state()?;
        if let Some(existing) = state.providers.get(&record.provider_id) {
            record.created_at_ms = existing.created_at_ms;
        }
        state
            .providers
            .insert(record.provider_id.clone(), record.clone());
        Ok(record)
    }

    async fn read_provider(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        self.read_state()?
            .providers
            .get(provider_id)
            .cloned()
            .ok_or_else(|| not_found("environment_provider", provider_id))
    }

    async fn list_providers(
        &self,
        _request: ListEnvironmentProviders,
    ) -> Result<Vec<EnvironmentProviderRecord>, EnvironmentRegistryError> {
        Ok(self.read_state()?.providers.values().cloned().collect())
    }

    async fn delete_provider(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        if state
            .bindings
            .values()
            .any(|binding| &binding.provider_id == provider_id)
        {
            return invalid("environment provider is referenced by a universe binding");
        }
        state
            .providers
            .remove(provider_id)
            .ok_or_else(|| not_found("environment_provider", provider_id))
    }
}

#[async_trait]
impl EnvironmentProviderBindingStore for InMemoryEnvironmentRegistryStore {
    async fn put_provider_binding(
        &self,
        request: PutEnvironmentProviderBinding,
    ) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.updated_at_ms, "updated_at_ms")?;
        let mut state = self.write_state()?;
        if !state.providers.contains_key(&request.provider_id) {
            return Err(not_found("environment_provider", &request.provider_id));
        }
        if state.bindings.values().any(|binding| {
            binding.universe_id == request.universe_id
                && binding.provider_id == request.provider_id
                && binding.binding_id != request.binding_id
        }) {
            return Err(EnvironmentRegistryError::AlreadyExists {
                kind: "environment_provider_binding",
                id: format!("{}/{}", request.universe_id, request.provider_id),
            });
        }
        let key = (request.universe_id, request.binding_id.clone());
        let existing = state.bindings.get(&key);
        if existing.is_some_and(|record| record.provider_id != request.provider_id) {
            return invalid("provider_id is immutable for an existing binding");
        }
        let actual = existing.map(|record| record.revision);
        if request.expected_revision != actual {
            return Err(EnvironmentRegistryError::RevisionConflict {
                kind: "environment_provider_binding",
                id: request.binding_id.to_string(),
                expected: request.expected_revision,
                actual,
            });
        }
        let record = EnvironmentProviderBindingRecord {
            universe_id: request.universe_id,
            binding_id: request.binding_id,
            provider_id: request.provider_id,
            status: request.status,
            revision: actual.unwrap_or(0) + 1,
            metadata: request.metadata,
            created_at_ms: existing
                .map(|record| record.created_at_ms)
                .unwrap_or(request.updated_at_ms),
            updated_at_ms: request.updated_at_ms,
        };
        record.validate()?;
        state.bindings.insert(key, record.clone());
        Ok(record)
    }

    async fn read_provider_binding(
        &self,
        universe_id: Uuid,
        binding_id: &EnvironmentProviderBindingId,
    ) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError> {
        self.read_state()?
            .bindings
            .get(&(universe_id, binding_id.clone()))
            .cloned()
            .ok_or_else(|| not_found("environment_provider_binding", binding_id))
    }

    async fn list_provider_bindings(
        &self,
        universe_id: Uuid,
    ) -> Result<Vec<EnvironmentProviderBindingRecord>, EnvironmentRegistryError> {
        Ok(self
            .read_state()?
            .bindings
            .values()
            .filter(|binding| binding.universe_id == universe_id)
            .cloned()
            .collect())
    }

    async fn delete_provider_binding(
        &self,
        universe_id: Uuid,
        binding_id: &EnvironmentProviderBindingId,
    ) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        if state.environments.values().any(|environment| {
            environment.status != EnvironmentStatus::Closed
                && environment.binding_id() == Some(binding_id)
        }) {
            return invalid("provider binding is referenced by a non-closed environment");
        }
        state
            .bindings
            .remove(&(universe_id, binding_id.clone()))
            .ok_or_else(|| not_found("environment_provider_binding", binding_id))
    }
}

#[async_trait]
impl EnvironmentStore for InMemoryEnvironmentRegistryStore {
    async fn create_environment(
        &self,
        request: CreateEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.created_at_ms, "created_at_ms")?;
        let mut state = self.write_state()?;
        if let Some(environment_id) = state.requests.get(&request.request_id) {
            return state
                .environments
                .get(environment_id)
                .cloned()
                .ok_or_else(|| not_found("environment", environment_id));
        }
        let binding = state
            .bindings
            .get(&(self.universe_id, request.binding_id.clone()))
            .cloned()
            .ok_or_else(|| not_found("environment_provider_binding", &request.binding_id))?;
        if binding.status != EnvironmentProviderBindingStatus::Enabled {
            return invalid("environment provider binding is disabled");
        }
        if state.environments.contains_key(&request.environment_id) {
            return Err(EnvironmentRegistryError::AlreadyExists {
                kind: "environment",
                id: request.environment_id.to_string(),
            });
        }
        let record = EnvironmentRecord {
            environment_id: request.environment_id.clone(),
            request_id: request.request_id.clone(),
            source: EnvironmentSource::Provisioned {
                provider_id: binding.provider_id,
                binding_id: request.binding_id,
            },
            display_name: request.display_name,
            status: EnvironmentStatus::Provisioning,
            desired_power: PowerState::Running,
            idle_policy: request.idle_policy,
            incarnation: EnvironmentIncarnationRecord {
                incarnation_id: request.incarnation_id,
                provision_request_id: Some(request.request_id.clone()),
                provider_target_id: None,
                template_id: Some(request.template_id),
                adoption_source_target: None,
                power_states: Vec::new(),
                created_at_ms: request.created_at_ms,
                updated_at_ms: request.created_at_ms,
            },
            public_ingress_enabled: false,
            public_endpoint: None,
            metadata: request.metadata,
            last_seen_at_ms: None,
            created_at_ms: request.created_at_ms,
            updated_at_ms: request.created_at_ms,
        };
        record.validate()?;
        state
            .requests
            .insert(request.request_id, request.environment_id.clone());
        state
            .environments
            .insert(request.environment_id, record.clone());
        Ok(record)
    }

    async fn adopt_environment(
        &self,
        request: AdoptEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.created_at_ms, "created_at_ms")?;
        if request.source_target.is_empty()
            || request.source_target.len() > 255
            || request.source_target.chars().any(char::is_control)
        {
            return invalid(
                "adoption source must be non-empty, at most 255 bytes, and contain no control characters",
            );
        }
        let mut state = self.write_state()?;
        if let Some(environment_id) = state.requests.get(&request.request_id) {
            return state
                .environments
                .get(environment_id)
                .cloned()
                .ok_or_else(|| not_found("environment", environment_id));
        }
        let binding = state
            .bindings
            .get(&(self.universe_id, request.binding_id.clone()))
            .cloned()
            .ok_or_else(|| not_found("environment_provider_binding", &request.binding_id))?;
        if binding.status != EnvironmentProviderBindingStatus::Enabled {
            return invalid("environment provider binding is disabled");
        }
        if state.environments.contains_key(&request.environment_id) {
            return Err(EnvironmentRegistryError::AlreadyExists {
                kind: "environment",
                id: request.environment_id.to_string(),
            });
        }
        let record = EnvironmentRecord {
            environment_id: request.environment_id.clone(),
            request_id: request.request_id.clone(),
            source: EnvironmentSource::Provisioned {
                provider_id: binding.provider_id,
                binding_id: request.binding_id,
            },
            display_name: request.display_name,
            status: EnvironmentStatus::Provisioning,
            desired_power: PowerState::Running,
            idle_policy: None,
            incarnation: EnvironmentIncarnationRecord {
                incarnation_id: request.incarnation_id,
                provision_request_id: Some(request.request_id.clone()),
                provider_target_id: None,
                template_id: None,
                adoption_source_target: Some(request.source_target),
                power_states: Vec::new(),
                created_at_ms: request.created_at_ms,
                updated_at_ms: request.created_at_ms,
            },
            public_ingress_enabled: false,
            public_endpoint: None,
            metadata: request.metadata,
            last_seen_at_ms: None,
            created_at_ms: request.created_at_ms,
            updated_at_ms: request.created_at_ms,
        };
        record.validate()?;
        state
            .requests
            .insert(request.request_id, request.environment_id.clone());
        state
            .environments
            .insert(request.environment_id, record.clone());
        Ok(record)
    }

    async fn create_external_environment(
        &self,
        request: CreateExternalEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.created_at_ms, "created_at_ms")?;
        request.connection.validate()?;
        let mut state = self.write_state()?;
        if let Some(environment_id) = state.requests.get(&request.request_id) {
            return state
                .environments
                .get(environment_id)
                .cloned()
                .ok_or_else(|| not_found("environment", environment_id));
        }
        if state.environments.contains_key(&request.environment_id) {
            return Err(EnvironmentRegistryError::AlreadyExists {
                kind: "environment",
                id: request.environment_id.to_string(),
            });
        }
        let record = EnvironmentRecord {
            environment_id: request.environment_id.clone(),
            request_id: request.request_id.clone(),
            source: EnvironmentSource::External {
                connection: request.connection,
            },
            display_name: request.display_name,
            status: EnvironmentStatus::Ready,
            desired_power: PowerState::Running,
            idle_policy: None,
            incarnation: EnvironmentIncarnationRecord {
                incarnation_id: request.incarnation_id,
                provision_request_id: None,
                provider_target_id: None,
                template_id: None,
                adoption_source_target: None,
                power_states: Vec::new(),
                created_at_ms: request.created_at_ms,
                updated_at_ms: request.created_at_ms,
            },
            public_ingress_enabled: false,
            public_endpoint: None,
            metadata: request.metadata,
            last_seen_at_ms: None,
            created_at_ms: request.created_at_ms,
            updated_at_ms: request.created_at_ms,
        };
        record.validate()?;
        state
            .requests
            .insert(request.request_id, request.environment_id.clone());
        state
            .environments
            .insert(request.environment_id, record.clone());
        Ok(record)
    }

    async fn create_registered_environment(
        &self,
        request: CreateRegisteredEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.created_at_ms, "created_at_ms")?;
        let daemon_id = request.daemon_id()?;
        let request_id = EnvironmentProvisionRequestId::for_daemon(&daemon_id);
        let mut state = self.write_state()?;
        if let Some(environment_id) = state.requests.get(&request_id) {
            return state
                .environments
                .get(environment_id)
                .cloned()
                .ok_or_else(|| not_found("environment", environment_id));
        }
        if state.environments.values().any(|record| {
            matches!(&record.source, EnvironmentSource::Registered { daemon_public_key, .. }
                if daemon_public_key == &request.daemon_public_key)
        }) {
            return Err(EnvironmentRegistryError::AlreadyExists {
                kind: "daemon_identity",
                id: daemon_id.to_string(),
            });
        }
        let key = state
            .registration_keys
            .get(&request.registration_key_id)
            .map(|stored| stored.record.clone())
            .ok_or_else(|| {
                not_found("environment_registration_key", &request.registration_key_id)
            })?;
        key.check_admits(request.created_at_ms)?;
        if let Some(limit) = key.max_active_environments {
            let active = state
                .environments
                .values()
                .filter(|record| {
                    record.registration_key_id() == Some(&key.registration_key_id)
                        && record.status != EnvironmentStatus::Closed
                })
                .count();
            if active >= limit as usize {
                return Err(EnvironmentRegistryError::RegistrationCapacityExhausted {
                    registration_key_id: key.registration_key_id.to_string(),
                    limit,
                });
            }
        }
        if state.environments.contains_key(&request.environment_id) {
            return Err(EnvironmentRegistryError::AlreadyExists {
                kind: "environment",
                id: request.environment_id.to_string(),
            });
        }
        let record = EnvironmentRecord {
            environment_id: request.environment_id.clone(),
            request_id: request_id.clone(),
            source: EnvironmentSource::Registered {
                registration_key_id: key.registration_key_id,
                daemon_id,
                daemon_public_key: request.daemon_public_key,
                identity_mode: key.identity_mode,
            },
            display_name: request.display_name,
            status: EnvironmentStatus::Ready,
            desired_power: PowerState::Running,
            idle_policy: None,
            incarnation: EnvironmentIncarnationRecord {
                incarnation_id: request.incarnation_id,
                provision_request_id: None,
                provider_target_id: None,
                template_id: None,
                adoption_source_target: None,
                power_states: Vec::new(),
                created_at_ms: request.created_at_ms,
                updated_at_ms: request.created_at_ms,
            },
            public_ingress_enabled: false,
            public_endpoint: None,
            metadata: request.metadata,
            last_seen_at_ms: Some(request.created_at_ms),
            created_at_ms: request.created_at_ms,
            updated_at_ms: request.created_at_ms,
        };
        record.validate()?;
        state
            .requests
            .insert(request_id, request.environment_id.clone());
        state
            .environments
            .insert(request.environment_id, record.clone());
        Ok(record)
    }

    async fn read_environment_by_daemon_public_key(
        &self,
        daemon_public_key: &str,
    ) -> Result<Option<EnvironmentRecord>, EnvironmentRegistryError> {
        Ok(self
            .read_state()?
            .environments
            .values()
            .find(|record| {
                matches!(&record.source, EnvironmentSource::Registered { daemon_public_key: key, .. }
                    if key == daemon_public_key)
            })
            .cloned())
    }

    async fn observe_registered_environment(
        &self,
        request: ObserveRegisteredEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.observed_at_ms, "observed_at_ms")?;
        let mut state = self.write_state()?;
        let record = state
            .environments
            .get_mut(&request.environment_id)
            .ok_or_else(|| not_found("environment", &request.environment_id))?;
        apply_registered_observation(
            record,
            request.observation,
            request.observed_at_ms,
            request.metadata,
        )?;
        record.validate()?;
        Ok(record.clone())
    }

    async fn list_open_registered_environments(
        &self,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError> {
        Ok(self
            .read_state()?
            .environments
            .values()
            .filter(|record| {
                record.is_registered()
                    && !matches!(
                        record.status,
                        EnvironmentStatus::Closing | EnvironmentStatus::Closed
                    )
            })
            .cloned()
            .collect())
    }

    async fn read_environment(
        &self,
        environment_id: &EnvironmentId,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.read_state()?
            .environments
            .get(environment_id)
            .cloned()
            .ok_or_else(|| not_found("environment", environment_id))
    }

    async fn read_environment_by_request_id(
        &self,
        request_id: &EnvironmentProvisionRequestId,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let state = self.read_state()?;
        let environment_id = state
            .requests
            .get(request_id)
            .ok_or_else(|| not_found("environment_request", request_id))?;
        state
            .environments
            .get(environment_id)
            .cloned()
            .ok_or_else(|| not_found("environment", environment_id))
    }

    async fn list_environments(
        &self,
        request: ListEnvironments,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError> {
        Ok(self
            .read_state()?
            .environments
            .values()
            .filter(|record| {
                request
                    .provider_id
                    .as_ref()
                    .is_none_or(|id| record.provider_id() == Some(id))
            })
            .filter(|record| {
                request
                    .binding_id
                    .as_ref()
                    .is_none_or(|id| record.binding_id() == Some(id))
            })
            .filter(|record| request.status.is_none_or(|status| status == record.status))
            .filter(|record| {
                request
                    .registration_key_id
                    .as_ref()
                    .is_none_or(|id| record.registration_key_id() == Some(id))
            })
            .filter(|record| engine::storage::metadata_matches(&record.metadata, &request.metadata))
            .cloned()
            .collect())
    }

    async fn list_environments_needing_reconcile(
        &self,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError> {
        Ok(self
            .read_state()?
            .environments
            .values()
            .filter(|record| {
                matches!(
                    record.status,
                    EnvironmentStatus::Provisioning
                        | EnvironmentStatus::Booting
                        | EnvironmentStatus::Closing
                        | EnvironmentStatus::Unknown
                ) || record.power_diverges()
            })
            .cloned()
            .collect())
    }

    async fn list_environments_with_idle_policy(
        &self,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError> {
        Ok(self
            .read_state()?
            .environments
            .values()
            .filter(|record| {
                record.status == EnvironmentStatus::Ready && record.idle_policy.is_some()
            })
            .cloned()
            .collect())
    }

    async fn observe_provisioned_environment(
        &self,
        request: ObserveProvisionedEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        let record = state
            .environments
            .get_mut(&request.environment_id)
            .ok_or_else(|| not_found("environment", &request.environment_id))?;
        if request.observed_at_ms < record.incarnation.updated_at_ms {
            return Ok(record.clone());
        }
        if record
            .incarnation
            .provider_target_id
            .as_ref()
            .is_some_and(|existing| existing != &request.provider_target_id)
        {
            return invalid("provider target conflicts with current incarnation");
        }
        record.incarnation.provider_target_id = Some(request.provider_target_id);
        record.incarnation.power_states = request.power_states;
        record.incarnation.updated_at_ms = request.observed_at_ms;
        record.updated_at_ms = request.observed_at_ms;
        record.status = request.status;
        record.validate()?;
        Ok(record.clone())
    }

    async fn fail_environment_lifecycle(
        &self,
        request: FailEnvironmentLifecycle,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        let record = state
            .environments
            .get_mut(&request.environment_id)
            .ok_or_else(|| not_found("environment", &request.environment_id))?;
        record.status = EnvironmentStatus::Failed;
        record
            .metadata
            .insert("lifecycleError".to_owned(), request.message);
        record.incarnation.updated_at_ms = request.observed_at_ms;
        record.updated_at_ms = request.observed_at_ms;
        Ok(record.clone())
    }

    async fn begin_close_environment(
        &self,
        request: BeginCloseEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        let record = state
            .environments
            .get_mut(&request.environment_id)
            .ok_or_else(|| not_found("environment", &request.environment_id))?;
        if record.status != EnvironmentStatus::Closed {
            record.status = EnvironmentStatus::Closing;
            record.updated_at_ms = request.updated_at_ms;
        }
        Ok(record.clone())
    }

    async fn finish_close_environment(
        &self,
        request: FinishCloseEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        let record = state
            .environments
            .get_mut(&request.environment_id)
            .ok_or_else(|| not_found("environment", &request.environment_id))?;
        record.status = EnvironmentStatus::Closed;
        record.public_ingress_enabled = false;
        record.public_endpoint = None;
        record.incarnation.updated_at_ms = request.observed_at_ms;
        record.updated_at_ms = request.observed_at_ms;
        Ok(record.clone())
    }

    async fn set_environment_ingress(
        &self,
        request: SetEnvironmentIngress,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.updated_at_ms, "updated_at_ms")?;
        let mut state = self.write_state()?;
        let record = state
            .environments
            .get_mut(&request.environment_id)
            .ok_or_else(|| not_found("environment", &request.environment_id))?;
        if !matches!(record.source, EnvironmentSource::Provisioned { .. }) {
            return invalid("provider-managed ingress requires a provisioned environment");
        }
        if matches!(
            record.status,
            EnvironmentStatus::Closing | EnvironmentStatus::Closed
        ) && request.enabled
        {
            return invalid("cannot enable ingress for a closing environment");
        }
        if request.enabled != request.public_endpoint.is_some() {
            return invalid(
                "enabled ingress requires a public endpoint and disabled ingress forbids one",
            );
        }
        record.public_ingress_enabled = request.enabled;
        record.public_endpoint = request.public_endpoint;
        record.updated_at_ms = record.updated_at_ms.max(request.updated_at_ms);
        record.validate()?;
        Ok(record.clone())
    }

    async fn set_environment_power(
        &self,
        request: SetEnvironmentPower,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.updated_at_ms, "updated_at_ms")?;
        let mut state = self.write_state()?;
        let record = state
            .environments
            .get_mut(&request.environment_id)
            .ok_or_else(|| not_found("environment", &request.environment_id))?;
        check_power_mutable(record)?;
        record.desired_power = request.desired_power;
        record.updated_at_ms = record.updated_at_ms.max(request.updated_at_ms);
        record.validate()?;
        Ok(record.clone())
    }

    async fn set_environment_idle_policy(
        &self,
        request: SetEnvironmentIdlePolicy,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.updated_at_ms, "updated_at_ms")?;
        if let Some(policy) = &request.idle_policy {
            policy.validate()?;
        }
        let mut state = self.write_state()?;
        let record = state
            .environments
            .get_mut(&request.environment_id)
            .ok_or_else(|| not_found("environment", &request.environment_id))?;
        check_power_mutable(record)?;
        record.idle_policy = request.idle_policy;
        record.updated_at_ms = record.updated_at_ms.max(request.updated_at_ms);
        record.validate()?;
        Ok(record.clone())
    }
}

/// Apply a gateway connection observation to a registered environment.
/// Closing and closed environments keep their state: a late heartbeat must
/// not resurrect them.
pub fn apply_registered_observation(
    record: &mut EnvironmentRecord,
    observation: RegisteredConnectionObservation,
    observed_at_ms: i64,
    metadata: std::collections::BTreeMap<String, String>,
) -> Result<(), EnvironmentRegistryError> {
    if !record.is_registered() {
        return invalid("connection observations apply only to registered environments");
    }
    if matches!(
        record.status,
        EnvironmentStatus::Closing | EnvironmentStatus::Closed
    ) {
        return Ok(());
    }
    record.metadata.extend(metadata);
    match observation {
        RegisteredConnectionObservation::Connected => {
            record.status = EnvironmentStatus::Ready;
            record.last_seen_at_ms = Some(observed_at_ms);
        }
        RegisteredConnectionObservation::Heartbeat => {
            record.last_seen_at_ms = Some(observed_at_ms);
        }
        RegisteredConnectionObservation::Disconnected => {
            record.status = EnvironmentStatus::Offline;
        }
    }
    record.updated_at_ms = record.updated_at_ms.max(observed_at_ms);
    Ok(())
}

#[async_trait]
impl EnvironmentRegistrationKeyStore for InMemoryEnvironmentRegistryStore {
    async fn create_registration_key(
        &self,
        request: CreateEnvironmentRegistrationKey,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
        request.record.validate()?;
        let mut state = self.write_state()?;
        if state
            .registration_keys
            .contains_key(&request.record.registration_key_id)
            || state
                .registration_keys
                .values()
                .any(|stored| stored.secret_hash == request.secret_hash)
        {
            return Err(EnvironmentRegistryError::AlreadyExists {
                kind: "environment_registration_key",
                id: request.record.registration_key_id.to_string(),
            });
        }
        state.registration_keys.insert(
            request.record.registration_key_id.clone(),
            StoredRegistrationKey {
                secret_hash: request.secret_hash,
                record: request.record.clone(),
            },
        );
        Ok(request.record)
    }

    async fn read_registration_key(
        &self,
        registration_key_id: &EnvironmentRegistrationKeyId,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
        self.read_state()?
            .registration_keys
            .get(registration_key_id)
            .map(|stored| stored.record.clone())
            .ok_or_else(|| not_found("environment_registration_key", registration_key_id))
    }

    async fn list_registration_keys(
        &self,
    ) -> Result<Vec<EnvironmentRegistrationKeyRecord>, EnvironmentRegistryError> {
        Ok(self
            .read_state()?
            .registration_keys
            .values()
            .map(|stored| stored.record.clone())
            .collect())
    }

    async fn revoke_registration_key(
        &self,
        request: RevokeEnvironmentRegistrationKey,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.revoked_at_ms, "revoked_at_ms")?;
        let mut state = self.write_state()?;
        let stored = state
            .registration_keys
            .get_mut(&request.registration_key_id)
            .ok_or_else(|| {
                not_found("environment_registration_key", &request.registration_key_id)
            })?;
        if stored.record.revoked_at_ms.is_none() {
            stored.record.revoked_at_ms =
                Some(request.revoked_at_ms.max(stored.record.created_at_ms));
        }
        Ok(stored.record.clone())
    }

    async fn resolve_registration_key(
        &self,
        secret_hash: &str,
    ) -> Result<Option<EnvironmentRegistrationKeyRecord>, EnvironmentRegistryError> {
        Ok(self
            .read_state()?
            .registration_keys
            .values()
            .find(|stored| stored.secret_hash == secret_hash)
            .map(|stored| stored.record.clone()))
    }

    async fn registration_key_usage(
        &self,
        registration_key_id: &EnvironmentRegistrationKeyId,
    ) -> Result<RegistrationKeyUsage, EnvironmentRegistryError> {
        let state = self.read_state()?;
        if !state.registration_keys.contains_key(registration_key_id) {
            return Err(not_found(
                "environment_registration_key",
                registration_key_id,
            ));
        }
        let mut usage = RegistrationKeyUsage::default();
        for record in state
            .environments
            .values()
            .filter(|record| record.registration_key_id() == Some(registration_key_id))
        {
            usage.registered += 1;
            if record.status != EnvironmentStatus::Closed {
                usage.active += 1;
            }
            usage.last_registered_at_ms = Some(
                usage
                    .last_registered_at_ms
                    .map_or(record.created_at_ms, |last| last.max(record.created_at_ms)),
            );
        }
        Ok(usage)
    }
}

/// Power intent and idle policy exist only for provisioned environments that
/// are not on their way out.
pub(crate) fn check_power_mutable(
    record: &EnvironmentRecord,
) -> Result<(), EnvironmentRegistryError> {
    if !matches!(record.source, EnvironmentSource::Provisioned { .. }) {
        return invalid("only provisioned environments have power control");
    }
    if matches!(
        record.status,
        EnvironmentStatus::Closing | EnvironmentStatus::Closed | EnvironmentStatus::Failed
    ) {
        return invalid("cannot change power of a closing, closed, or failed environment");
    }
    Ok(())
}

#[async_trait]
impl EnvironmentCredentialStore for InMemoryEnvironmentRegistryStore {
    async fn bind_credential(
        &self,
        record: PutEnvironmentCredential,
    ) -> Result<EnvironmentCredentialRecord, EnvironmentRegistryError> {
        let record = record.into_record();
        record.validate()?;
        let mut state = self.write_state()?;
        if !state.environments.contains_key(&record.environment_id) {
            return Err(not_found("environment", &record.environment_id));
        }
        let key = (record.environment_id.clone(), record.env_name.clone());
        let record = if let Some(existing) = state.credentials.get(&key) {
            EnvironmentCredentialRecord {
                created_at_ms: existing.created_at_ms,
                ..record
            }
        } else {
            record
        };
        state.credentials.insert(key, record.clone());
        Ok(record)
    }

    async fn list_credentials(
        &self,
        request: ListEnvironmentCredentials,
    ) -> Result<Vec<EnvironmentCredentialRecord>, EnvironmentRegistryError> {
        Ok(self
            .read_state()?
            .credentials
            .values()
            .filter(|record| record.environment_id == request.environment_id)
            .cloned()
            .collect())
    }

    async fn unbind_credential(
        &self,
        environment_id: &EnvironmentId,
        env_name: &str,
    ) -> Result<EnvironmentCredentialRecord, EnvironmentRegistryError> {
        self.write_state()?
            .credentials
            .remove(&(environment_id.clone(), env_name.to_owned()))
            .ok_or_else(|| EnvironmentRegistryError::NotFound {
                kind: "environment_credential",
                id: format!("{environment_id}/{env_name}"),
            })
    }
}
