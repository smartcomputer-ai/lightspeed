use std::{collections::BTreeMap, sync::RwLock};

use async_trait::async_trait;
use uuid::Uuid;

use super::*;

type CredentialKey = (EnvironmentId, String);
type BindingKey = (Uuid, EnvironmentProviderBindingId);
type DaemonEnrollmentKey = (EnvironmentId, EnvironmentIncarnationId);

struct RegistryState {
    providers: BTreeMap<EnvironmentProviderId, EnvironmentProviderRecord>,
    bindings: BTreeMap<BindingKey, EnvironmentProviderBindingRecord>,
    environments: BTreeMap<EnvironmentId, EnvironmentRecord>,
    requests: BTreeMap<EnvironmentProvisionRequestId, EnvironmentId>,
    daemon_enrollments: BTreeMap<DaemonEnrollmentKey, EnvironmentDaemonEnrollmentRecord>,
    credentials: BTreeMap<CredentialKey, EnvironmentCredentialRecord>,
}

impl Default for RegistryState {
    fn default() -> Self {
        Self {
            providers: BTreeMap::new(),
            bindings: BTreeMap::new(),
            environments: BTreeMap::new(),
            requests: BTreeMap::new(),
            daemon_enrollments: BTreeMap::new(),
            credentials: BTreeMap::new(),
        }
    }
}

#[async_trait]
impl EnvironmentDaemonEnrollmentStore for InMemoryEnvironmentRegistryStore {
    async fn create_enrollment(
        &self,
        request: CreateEnvironmentEnrollment,
    ) -> Result<
        (EnvironmentRecord, EnvironmentDaemonEnrollmentRecord, bool),
        EnvironmentRegistryError,
    > {
        validate_nonnegative_i64(request.created_at_ms, "created_at_ms")?;
        let mut state = self.write_state()?;
        if let Some(environment_id) = state.requests.get(&request.request_id) {
            let environment = state
                .environments
                .get(environment_id)
                .cloned()
                .ok_or_else(|| not_found("environment", environment_id))?;
            let enrollment = state
                .daemon_enrollments
                .get(&(
                    environment_id.clone(),
                    environment.incarnation.incarnation_id.clone(),
                ))
                .cloned()
                .ok_or_else(|| not_found("environment_enrollment", environment_id))?;
            return Ok((environment, enrollment, false));
        }
        if state.environments.contains_key(&request.environment_id) {
            return Err(EnvironmentRegistryError::AlreadyExists {
                kind: "environment",
                id: request.environment_id.to_string(),
            });
        }
        let environment = EnvironmentRecord {
            environment_id: request.environment_id.clone(),
            request_id: request.request_id.clone(),
            source: EnvironmentSource::Enrolled,
            display_name: request.display_name,
            status: EnvironmentStatus::WaitingForDaemon,
            incarnation: EnvironmentIncarnationRecord {
                incarnation_id: request.incarnation_id.clone(),
                provision_request_id: None,
                provider_target_id: None,
                template_id: None,
                created_at_ms: request.created_at_ms,
                updated_at_ms: request.created_at_ms,
            },
            metadata: request.metadata,
            created_at_ms: request.created_at_ms,
            updated_at_ms: request.created_at_ms,
        };
        environment.validate()?;
        let enrollment = EnvironmentDaemonEnrollmentRecord {
            environment_id: request.environment_id.clone(),
            incarnation_id: request.incarnation_id,
            ticket_hash: request.ticket_hash,
            ticket_expires_at_ms: request.ticket_expires_at_ms,
            ticket_redeemed_at_ms: None,
            revoked_at_ms: None,
            daemon_id: None,
            daemon_public_key: None,
            enrolled_at_ms: None,
            created_at_ms: request.created_at_ms,
            updated_at_ms: request.created_at_ms,
        };
        enrollment.validate()?;
        state
            .requests
            .insert(request.request_id, request.environment_id.clone());
        state
            .environments
            .insert(request.environment_id.clone(), environment.clone());
        state.daemon_enrollments.insert(
            (request.environment_id, enrollment.incarnation_id.clone()),
            enrollment.clone(),
        );
        Ok((environment, enrollment, true))
    }

    async fn read_enrollment(
        &self,
        environment_id: &EnvironmentId,
    ) -> Result<EnvironmentDaemonEnrollmentRecord, EnvironmentRegistryError> {
        let state = self.read_state()?;
        let environment = state
            .environments
            .get(environment_id)
            .ok_or_else(|| not_found("environment", environment_id))?;
        state
            .daemon_enrollments
            .get(&(
                environment_id.clone(),
                environment.incarnation.incarnation_id.clone(),
            ))
            .cloned()
            .ok_or_else(|| not_found("environment_enrollment", environment_id))
    }

    async fn enroll_daemon(
        &self,
        request: EnrollEnvironmentDaemon,
    ) -> Result<EnvironmentDaemonEnrollmentRecord, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        let enrollment = state
            .daemon_enrollments
            .get_mut(&(
                request.environment_id.clone(),
                request.incarnation_id.clone(),
            ))
            .ok_or_else(|| not_found("environment_enrollment", &request.environment_id))?;
        if enrollment.incarnation_id != request.incarnation_id {
            return invalid("stale environment incarnation");
        }
        if enrollment.revoked_at_ms.is_some() {
            return invalid("environment enrollment is revoked");
        }
        if enrollment.ticket_redeemed_at_ms.is_some() {
            return invalid("environment enrollment ticket was already redeemed");
        }
        if request.enrolled_at_ms > enrollment.ticket_expires_at_ms {
            return invalid("environment enrollment ticket expired");
        }
        if enrollment.ticket_hash != request.ticket_hash {
            return invalid("invalid environment enrollment ticket");
        }
        enrollment.ticket_redeemed_at_ms = Some(request.enrolled_at_ms);
        enrollment.daemon_id = Some(request.daemon_id);
        enrollment.daemon_public_key = Some(request.daemon_public_key);
        enrollment.enrolled_at_ms = Some(request.enrolled_at_ms);
        enrollment.updated_at_ms = request.enrolled_at_ms;
        enrollment.validate()?;
        Ok(enrollment.clone())
    }

    async fn revoke_enrollment(
        &self,
        environment_id: &EnvironmentId,
        revoked_at_ms: i64,
    ) -> Result<EnvironmentDaemonEnrollmentRecord, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        let incarnation_id = state
            .environments
            .get(environment_id)
            .ok_or_else(|| not_found("environment", environment_id))?
            .incarnation
            .incarnation_id
            .clone();
        let enrollment = state
            .daemon_enrollments
            .get_mut(&(environment_id.clone(), incarnation_id))
            .ok_or_else(|| not_found("environment_enrollment", environment_id))?;
        enrollment.revoked_at_ms.get_or_insert(revoked_at_ms);
        enrollment.updated_at_ms = enrollment.updated_at_ms.max(revoked_at_ms);
        Ok(enrollment.clone())
    }
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
            incarnation: EnvironmentIncarnationRecord {
                incarnation_id: request.incarnation_id,
                provision_request_id: Some(request.request_id.clone()),
                provider_target_id: None,
                template_id: Some(request.template_id),
                created_at_ms: request.created_at_ms,
                updated_at_ms: request.created_at_ms,
            },
            metadata: request.metadata,
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
                )
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
        record.incarnation.updated_at_ms = request.observed_at_ms;
        record.updated_at_ms = request.observed_at_ms;
        Ok(record.clone())
    }
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
