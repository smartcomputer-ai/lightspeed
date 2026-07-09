use std::{
    collections::{BTreeMap, BTreeSet},
    sync::RwLock,
};

use async_trait::async_trait;
use engine::SessionId;
use host_protocol::{
    control::targets::HostTargetStatus,
    shared::{HostTargetId, JobId},
};

use super::*;

type BindingKey = (SessionId, EnvironmentId);
type CredentialKey = (SessionId, EnvironmentId, String);
type ProviderTargetKey = (EnvironmentProviderId, HostTargetId);
type JobGroupKey = (EnvironmentInstanceId, EnvironmentJobGroupId);
type JobKey = (EnvironmentInstanceId, JobId);

#[derive(Default)]
struct RegistryState {
    providers: BTreeMap<EnvironmentProviderId, EnvironmentProviderRecord>,
    instances: BTreeMap<EnvironmentInstanceId, EnvironmentInstanceRecord>,
    provider_targets: BTreeMap<ProviderTargetKey, EnvironmentInstanceId>,
    bindings: BTreeMap<BindingKey, SessionEnvironmentBindingRecord>,
    credentials: BTreeMap<CredentialKey, SessionEnvironmentCredentialRecord>,
    job_groups: BTreeMap<JobGroupKey, EnvironmentJobGroupRecord>,
    jobs: BTreeMap<JobKey, JobHandleRecord>,
}

#[derive(Default)]
pub struct InMemoryEnvironmentRegistryStore {
    state: RwLock<RegistryState>,
}

impl InMemoryEnvironmentRegistryStore {
    pub fn new() -> Self {
        Self::default()
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
    async fn register_provider(
        &self,
        record: RegisterEnvironmentProvider,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        let mut record = record.into_record()?;
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
        request: ListEnvironmentProviders,
    ) -> Result<Vec<EnvironmentProviderRecord>, EnvironmentRegistryError> {
        Ok(self
            .read_state()?
            .providers
            .values()
            .filter(|provider| request.status.is_none_or(|value| provider.status == value))
            .filter(|provider| {
                request
                    .provider_kind
                    .is_none_or(|value| provider.provider_kind == value)
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
        let mut state = self.write_state()?;
        let provider = state
            .providers
            .get_mut(&heartbeat.provider_id)
            .ok_or_else(|| not_found("environment_provider", &heartbeat.provider_id))?;
        let ttl = heartbeat.lease_ttl_ms.unwrap_or_else(|| {
            provider
                .lease_expires_ms
                .saturating_sub(provider.last_seen_ms)
        });
        validate_positive_i64(ttl, "lease_ttl_ms")?;
        provider.last_seen_ms = heartbeat.observed_at_ms;
        provider.lease_expires_ms = heartbeat.observed_at_ms.checked_add(ttl).ok_or_else(|| {
            EnvironmentRegistryError::InvalidInput {
                message: "lease expiry timestamp overflowed".to_owned(),
            }
        })?;
        provider.updated_at_ms = heartbeat.observed_at_ms;
        provider.status = EnvironmentProviderStatus::Online;
        provider.validate()?;
        Ok(provider.clone())
    }

    async fn update_provider_status(
        &self,
        request: UpdateEnvironmentProviderStatus,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.updated_at_ms, "updated_at_ms")?;
        let mut state = self.write_state()?;
        let provider = state
            .providers
            .get_mut(&request.provider_id)
            .ok_or_else(|| not_found("environment_provider", &request.provider_id))?;
        provider.status = request.status;
        provider.updated_at_ms = request.updated_at_ms;
        provider.validate()?;
        Ok(provider.clone())
    }

    async fn delete_provider(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        self.write_state()?
            .providers
            .remove(provider_id)
            .ok_or_else(|| not_found("environment_provider", provider_id))
    }
}

#[async_trait]
impl EnvironmentInstanceStore for InMemoryEnvironmentRegistryStore {
    async fn observe_instance(
        &self,
        record: ObserveEnvironmentInstance,
    ) -> Result<EnvironmentInstanceRecord, EnvironmentRegistryError> {
        let incoming = record.into_record();
        incoming.validate()?;
        let mut state = self.write_state()?;
        if !state.providers.contains_key(&incoming.provider_id) {
            return Err(not_found("environment_provider", &incoming.provider_id));
        }
        let provider_key = (
            incoming.provider_id.clone(),
            incoming.provider_target_id.clone(),
        );
        let instance_id = state
            .provider_targets
            .get(&provider_key)
            .cloned()
            .unwrap_or_else(|| incoming.instance_id.clone());
        let record = if let Some(existing) = state.instances.get(&instance_id) {
            if incoming.observed_at_ms < existing.observed_at_ms {
                return Ok(existing.clone());
            }
            EnvironmentInstanceRecord {
                instance_id: instance_id.clone(),
                origin: if existing.origin == EnvironmentInstanceOrigin::Provisioned {
                    EnvironmentInstanceOrigin::Provisioned
                } else {
                    incoming.origin
                },
                created_at_ms: existing.created_at_ms,
                ..incoming
            }
        } else {
            EnvironmentInstanceRecord {
                instance_id: instance_id.clone(),
                ..incoming
            }
        };
        record.validate()?;
        state
            .provider_targets
            .insert(provider_key, instance_id.clone());
        state.instances.insert(instance_id, record.clone());
        Ok(record)
    }

    async fn read_instance(
        &self,
        instance_id: &EnvironmentInstanceId,
    ) -> Result<EnvironmentInstanceRecord, EnvironmentRegistryError> {
        self.read_state()?
            .instances
            .get(instance_id)
            .cloned()
            .ok_or_else(|| not_found("environment_instance", instance_id))
    }

    async fn read_instance_by_provider_target(
        &self,
        provider_id: &EnvironmentProviderId,
        provider_target_id: &HostTargetId,
    ) -> Result<EnvironmentInstanceRecord, EnvironmentRegistryError> {
        let state = self.read_state()?;
        let key = (provider_id.clone(), provider_target_id.clone());
        let instance_id =
            state
                .provider_targets
                .get(&key)
                .ok_or_else(|| EnvironmentRegistryError::NotFound {
                    kind: "environment_instance",
                    id: format!("{provider_id}/{provider_target_id}"),
                })?;
        state
            .instances
            .get(instance_id)
            .cloned()
            .ok_or_else(|| not_found("environment_instance", instance_id))
    }

    async fn list_instances(
        &self,
        request: ListEnvironmentInstances,
    ) -> Result<Vec<EnvironmentInstanceRecord>, EnvironmentRegistryError> {
        Ok(self
            .read_state()?
            .instances
            .values()
            .filter(|record| {
                request
                    .provider_id
                    .as_ref()
                    .is_none_or(|id| id == &record.provider_id)
            })
            .filter(|record| request.status.is_none_or(|status| status == record.status))
            .filter(|record| request.origin.is_none_or(|origin| origin == record.origin))
            .cloned()
            .collect())
    }

    async fn mark_missing_provided_instances_unknown(
        &self,
        provider_id: &EnvironmentProviderId,
        observed_target_ids: &BTreeSet<HostTargetId>,
        observed_at_ms: i64,
    ) -> Result<Vec<EnvironmentInstanceRecord>, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        let mut changed = Vec::new();
        for record in state.instances.values_mut() {
            if &record.provider_id == provider_id
                && record.origin == EnvironmentInstanceOrigin::Provided
                && !observed_target_ids.contains(&record.provider_target_id)
                && record.observed_at_ms <= observed_at_ms
            {
                record.status = HostTargetStatus::Unknown;
                record.observed_at_ms = observed_at_ms;
                record.updated_at_ms = observed_at_ms;
                changed.push(record.clone());
            }
        }
        Ok(changed)
    }

    async fn update_instance_status(
        &self,
        request: UpdateEnvironmentInstanceStatus,
    ) -> Result<EnvironmentInstanceRecord, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        let record = state
            .instances
            .get_mut(&request.instance_id)
            .ok_or_else(|| not_found("environment_instance", &request.instance_id))?;
        record.status = request.status;
        record.observed_at_ms = request.observed_at_ms;
        record.updated_at_ms = request.observed_at_ms;
        record.validate()?;
        Ok(record.clone())
    }

    async fn begin_close_instance(
        &self,
        request: BeginCloseEnvironmentInstance,
    ) -> Result<EnvironmentInstanceRecord, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        let bindings = state
            .bindings
            .values()
            .filter(|binding| {
                binding.instance_id == request.instance_id
                    && binding.state == SessionEnvironmentBindingState::Attached
            })
            .map(|binding| format!("{}/{}", binding.session_id, binding.env_id))
            .collect::<Vec<_>>();
        let job_groups = state
            .job_groups
            .values()
            .filter(|group| group.instance_id == request.instance_id && !group.status.is_terminal())
            .map(|group| group.job_group_id.clone())
            .collect::<Vec<_>>();
        if !bindings.is_empty() || !job_groups.is_empty() {
            return Err(EnvironmentRegistryError::Occupied {
                instance_id: request.instance_id,
                bindings,
                job_groups,
            });
        }
        let record = state
            .instances
            .get_mut(&request.instance_id)
            .ok_or_else(|| not_found("environment_instance", &request.instance_id))?;
        record.status = HostTargetStatus::Closing;
        record.updated_at_ms = request.updated_at_ms;
        Ok(record.clone())
    }
}

#[async_trait]
impl SessionEnvironmentBindingStore for InMemoryEnvironmentRegistryStore {
    async fn put_binding(
        &self,
        record: PutSessionEnvironmentBinding,
    ) -> Result<SessionEnvironmentBindingRecord, EnvironmentRegistryError> {
        let incoming = record.into_record();
        incoming.validate()?;
        let mut state = self.write_state()?;
        let instance = state
            .instances
            .get(&incoming.instance_id)
            .ok_or_else(|| not_found("environment_instance", &incoming.instance_id))?;
        if !instance.is_attachable() {
            return invalid(format!(
                "environment instance {} is not attachable: {:?}",
                instance.instance_id, instance.status
            ));
        }
        let key = (incoming.session_id.clone(), incoming.env_id.clone());
        let record = if let Some(existing) = state.bindings.get(&key).cloned() {
            if existing.state == SessionEnvironmentBindingState::Attached
                && existing.instance_id != incoming.instance_id
            {
                return Err(EnvironmentRegistryError::AlreadyExists {
                    kind: "session_environment_binding",
                    id: format!("{}/{}", incoming.session_id, incoming.env_id),
                });
            }
            if existing.instance_id != incoming.instance_id {
                state.credentials.retain(|(session_id, env_id, _), _| {
                    session_id != &incoming.session_id || env_id != &incoming.env_id
                });
            }
            SessionEnvironmentBindingRecord {
                created_at_ms: existing.created_at_ms,
                ..incoming
            }
        } else {
            incoming
        };
        state.bindings.insert(key, record.clone());
        Ok(record)
    }

    async fn read_binding(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
    ) -> Result<SessionEnvironmentBindingRecord, EnvironmentRegistryError> {
        self.read_state()?
            .bindings
            .get(&(session_id.clone(), env_id.clone()))
            .cloned()
            .ok_or_else(|| binding_not_found(session_id, env_id))
    }

    async fn list_bindings_for_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<SessionEnvironmentBindingRecord>, EnvironmentRegistryError> {
        Ok(self
            .read_state()?
            .bindings
            .values()
            .filter(|binding| &binding.session_id == session_id)
            .cloned()
            .collect())
    }

    async fn list_bindings_for_instance(
        &self,
        instance_id: &EnvironmentInstanceId,
    ) -> Result<Vec<SessionEnvironmentBindingRecord>, EnvironmentRegistryError> {
        Ok(self
            .read_state()?
            .bindings
            .values()
            .filter(|binding| &binding.instance_id == instance_id)
            .cloned()
            .collect())
    }

    async fn update_binding_state(
        &self,
        request: UpdateSessionEnvironmentBindingState,
    ) -> Result<SessionEnvironmentBindingRecord, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        let record = state
            .bindings
            .get_mut(&(request.session_id.clone(), request.env_id.clone()))
            .ok_or_else(|| binding_not_found(&request.session_id, &request.env_id))?;
        record.state = request.state;
        record.updated_at_ms = request.updated_at_ms;
        record.validate()?;
        Ok(record.clone())
    }

    async fn delete_binding(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
    ) -> Result<SessionEnvironmentBindingRecord, EnvironmentRegistryError> {
        self.write_state()?
            .bindings
            .remove(&(session_id.clone(), env_id.clone()))
            .ok_or_else(|| binding_not_found(session_id, env_id))
    }
}

#[async_trait]
impl SessionEnvironmentCredentialStore for InMemoryEnvironmentRegistryStore {
    async fn bind_credential(
        &self,
        record: CreateSessionEnvironmentCredential,
    ) -> Result<SessionEnvironmentCredentialRecord, EnvironmentRegistryError> {
        let record = record.into_record();
        record.validate()?;
        let mut state = self.write_state()?;
        let binding = state
            .bindings
            .get(&(record.session_id.clone(), record.env_id.clone()))
            .ok_or_else(|| binding_not_found(&record.session_id, &record.env_id))?;
        if binding.state != SessionEnvironmentBindingState::Attached {
            return invalid("credentials require an attached environment binding");
        }
        let key = (
            record.session_id.clone(),
            record.env_id.clone(),
            record.env_name.clone(),
        );
        let record = if let Some(existing) = state.credentials.get(&key) {
            SessionEnvironmentCredentialRecord {
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
        request: ListSessionEnvironmentCredentials,
    ) -> Result<Vec<SessionEnvironmentCredentialRecord>, EnvironmentRegistryError> {
        Ok(self
            .read_state()?
            .credentials
            .values()
            .filter(|record| {
                record.session_id == request.session_id && record.env_id == request.env_id
            })
            .cloned()
            .collect())
    }

    async fn unbind_credential(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
        env_name: &str,
    ) -> Result<SessionEnvironmentCredentialRecord, EnvironmentRegistryError> {
        self.write_state()?
            .credentials
            .remove(&(session_id.clone(), env_id.clone(), env_name.to_owned()))
            .ok_or_else(|| EnvironmentRegistryError::NotFound {
                kind: "session_environment_credential",
                id: format!("{session_id}/{env_id}/{env_name}"),
            })
    }
}

#[async_trait]
impl JobHandleStore for InMemoryEnvironmentRegistryStore {
    async fn reserve_job_group(
        &self,
        record: ReserveEnvironmentJobGroup,
    ) -> Result<EnvironmentJobGroupRecord, EnvironmentRegistryError> {
        let record = record.into_record();
        record.validate()?;
        let mut state = self.write_state()?;
        let instance = state
            .instances
            .get(&record.instance_id)
            .ok_or_else(|| not_found("environment_instance", &record.instance_id))?;
        if matches!(
            instance.status,
            HostTargetStatus::Closing | HostTargetStatus::Closed
        ) {
            return invalid("cannot start jobs on a closing environment instance");
        }
        let key = (record.instance_id.clone(), record.job_group_id.clone());
        if let Some(existing) = state.job_groups.get(&key) {
            if existing.start_request_hash == record.start_request_hash {
                return Ok(existing.clone());
            }
            return Err(EnvironmentRegistryError::AlreadyExists {
                kind: "environment_job_group",
                id: format!("{}/{}", record.instance_id, record.job_group_id),
            });
        }
        state.job_groups.insert(key, record.clone());
        Ok(record)
    }

    async fn read_job_group(
        &self,
        instance_id: &EnvironmentInstanceId,
        job_group_id: &EnvironmentJobGroupId,
    ) -> Result<EnvironmentJobGroupRecord, EnvironmentRegistryError> {
        self.read_state()?
            .job_groups
            .get(&(instance_id.clone(), job_group_id.clone()))
            .cloned()
            .ok_or_else(|| job_group_not_found(instance_id, job_group_id))
    }

    async fn update_job_group_status(
        &self,
        request: UpdateEnvironmentJobGroupStatus,
    ) -> Result<EnvironmentJobGroupRecord, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        let record = state
            .job_groups
            .get_mut(&(request.instance_id.clone(), request.job_group_id.clone()))
            .ok_or_else(|| job_group_not_found(&request.instance_id, &request.job_group_id))?;
        record.status = request.status;
        record.updated_at_ms = request.updated_at_ms;
        record.terminal_at_ms = request
            .status
            .is_terminal()
            .then_some(request.updated_at_ms);
        record.validate()?;
        Ok(record.clone())
    }

    async fn create_job_handles(
        &self,
        records: Vec<CreateJobHandle>,
    ) -> Result<Vec<JobHandleRecord>, EnvironmentRegistryError> {
        let records = records
            .into_iter()
            .map(CreateJobHandle::into_record)
            .collect::<Vec<_>>();
        for record in &records {
            record.validate()?;
        }
        let mut state = self.write_state()?;
        let mut created = Vec::with_capacity(records.len());
        for record in records {
            let group_key = (record.instance_id.clone(), record.job_group_id.clone());
            if !state.job_groups.contains_key(&group_key) {
                return Err(job_group_not_found(
                    &record.instance_id,
                    &record.job_group_id,
                ));
            }
            let key = (record.instance_id.clone(), record.job_id.clone());
            if let Some(existing) = state.jobs.get(&key) {
                if existing.start_request_hash == record.start_request_hash {
                    created.push(existing.clone());
                    continue;
                }
                return Err(EnvironmentRegistryError::AlreadyExists {
                    kind: "job_handle",
                    id: format!("{}/{}", record.instance_id, record.job_id),
                });
            }
            state.jobs.insert(key, record.clone());
            created.push(record);
        }
        Ok(created)
    }

    async fn read_job_handle(
        &self,
        instance_id: &EnvironmentInstanceId,
        job_id: &JobId,
    ) -> Result<JobHandleRecord, EnvironmentRegistryError> {
        self.read_state()?
            .jobs
            .get(&(instance_id.clone(), job_id.clone()))
            .cloned()
            .ok_or_else(|| job_not_found(instance_id, job_id))
    }

    async fn list_job_handles(
        &self,
        request: ListJobHandles,
    ) -> Result<Vec<JobHandleRecord>, EnvironmentRegistryError> {
        if matches!(request.limit, Some(0)) {
            return invalid("job handle list limit must be greater than zero");
        }
        let mut records = self
            .read_state()?
            .jobs
            .values()
            .filter(|record| {
                request
                    .instance_id
                    .as_ref()
                    .is_none_or(|id| id == &record.instance_id)
            })
            .filter(|record| {
                request
                    .job_group_id
                    .as_ref()
                    .is_none_or(|id| id == &record.job_group_id)
            })
            .filter(|record| {
                request
                    .created_by_session_id
                    .as_ref()
                    .is_none_or(|id| record.created_by_session_id.as_ref() == Some(id))
            })
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|left, right| {
            right
                .created_at_ms
                .cmp(&left.created_at_ms)
                .then_with(|| left.job_id.cmp(&right.job_id))
        });
        if let Some(limit) = request.limit {
            records.truncate(limit);
        }
        Ok(records)
    }

    async fn delete_job_handle(
        &self,
        instance_id: &EnvironmentInstanceId,
        job_id: &JobId,
    ) -> Result<JobHandleRecord, EnvironmentRegistryError> {
        self.write_state()?
            .jobs
            .remove(&(instance_id.clone(), job_id.clone()))
            .ok_or_else(|| job_not_found(instance_id, job_id))
    }
}

fn not_found(kind: &'static str, id: &impl ToString) -> EnvironmentRegistryError {
    EnvironmentRegistryError::NotFound {
        kind,
        id: id.to_string(),
    }
}

fn binding_not_found(session_id: &SessionId, env_id: &EnvironmentId) -> EnvironmentRegistryError {
    EnvironmentRegistryError::NotFound {
        kind: "session_environment_binding",
        id: format!("{session_id}/{env_id}"),
    }
}

fn job_group_not_found(
    instance_id: &EnvironmentInstanceId,
    job_group_id: &EnvironmentJobGroupId,
) -> EnvironmentRegistryError {
    EnvironmentRegistryError::NotFound {
        kind: "environment_job_group",
        id: format!("{instance_id}/{job_group_id}"),
    }
}

fn job_not_found(instance_id: &EnvironmentInstanceId, job_id: &JobId) -> EnvironmentRegistryError {
    EnvironmentRegistryError::NotFound {
        kind: "job_handle",
        id: format!("{instance_id}/{job_id}"),
    }
}
