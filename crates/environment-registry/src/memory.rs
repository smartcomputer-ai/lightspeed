use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use engine::SessionId;
use host_protocol::shared::{HostTargetId, JobId};

use crate::{
    CreateJobHandle, CreateSessionEnvironmentBinding, EnvironmentId, EnvironmentProviderHeartbeat,
    EnvironmentProviderId, EnvironmentProviderRecord, EnvironmentProviderStatus,
    EnvironmentProviderStore, EnvironmentRegistryError, EnvironmentTargetRecord,
    EnvironmentTargetStore, JobHandleRecord, JobHandleStore, ListEnvironmentProviders,
    ListEnvironmentTargets, ListJobHandles, RegisterEnvironmentProvider,
    SessionEnvironmentBindingRecord, SessionEnvironmentBindingStore,
    UpdateEnvironmentProviderStatus, UpdateEnvironmentTargetStatus,
    UpdateSessionEnvironmentBindingStatus, UpsertEnvironmentTargetRecord,
    validate_list_job_handles, validate_nonnegative_i64, validate_positive_i64,
};

#[derive(Clone, Default)]
pub struct InMemoryEnvironmentRegistryStore {
    providers: Arc<RwLock<BTreeMap<EnvironmentProviderId, EnvironmentProviderRecord>>>,
    targets: Arc<RwLock<BTreeMap<(EnvironmentProviderId, HostTargetId), EnvironmentTargetRecord>>>,
    bindings: Arc<RwLock<BTreeMap<(SessionId, EnvironmentId), SessionEnvironmentBindingRecord>>>,
    job_handles: Arc<RwLock<BTreeMap<(SessionId, EnvironmentId, JobId), JobHandleRecord>>>,
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

#[async_trait]
impl JobHandleStore for InMemoryEnvironmentRegistryStore {
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

        let mut handles =
            self.job_handles
                .write()
                .map_err(|_| EnvironmentRegistryError::Store {
                    message: "job handle registry write lock poisoned".to_owned(),
                })?;

        for record in &records {
            let key = job_handle_key(&record.session_id, &record.env_id, &record.job_id);
            if let Some(existing) = handles.get(&key) {
                if existing.start_request_hash != record.start_request_hash {
                    return Err(EnvironmentRegistryError::AlreadyExists {
                        kind: "job_handle",
                        id: job_handle_id(&record.session_id, &record.env_id, &record.job_id),
                    });
                }
            }
        }

        let mut created = Vec::with_capacity(records.len());
        for record in records {
            let key = job_handle_key(&record.session_id, &record.env_id, &record.job_id);
            if let Some(existing) = handles.get(&key) {
                created.push(existing.clone());
                continue;
            }
            handles.insert(key, record.clone());
            created.push(record);
        }
        Ok(created)
    }

    async fn read_job_handle(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
        job_id: &JobId,
    ) -> Result<JobHandleRecord, EnvironmentRegistryError> {
        let handles = self
            .job_handles
            .read()
            .map_err(|_| EnvironmentRegistryError::Store {
                message: "job handle registry read lock poisoned".to_owned(),
            })?;
        handles
            .get(&job_handle_key(session_id, env_id, job_id))
            .cloned()
            .ok_or_else(|| job_handle_not_found(session_id, env_id, job_id))
    }

    async fn list_job_handles(
        &self,
        request: ListJobHandles,
    ) -> Result<Vec<JobHandleRecord>, EnvironmentRegistryError> {
        validate_list_job_handles(&request)?;
        let handles = self
            .job_handles
            .read()
            .map_err(|_| EnvironmentRegistryError::Store {
                message: "job handle registry read lock poisoned".to_owned(),
            })?;
        let mut records = handles
            .values()
            .filter(|record| record.session_id == request.session_id)
            .filter(|record| {
                request
                    .env_id
                    .as_ref()
                    .is_none_or(|env_id| &record.env_id == env_id)
            })
            .filter(|record| {
                request
                    .deck_id
                    .as_ref()
                    .is_none_or(|deck_id| record.deck_id.as_ref() == Some(deck_id))
            })
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|left, right| {
            (&left.env_id, &left.deck_id, &left.job_id).cmp(&(
                &right.env_id,
                &right.deck_id,
                &right.job_id,
            ))
        });
        if let Some(limit) = request.limit {
            records.truncate(limit);
        }
        Ok(records)
    }

    async fn delete_job_handle(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
        job_id: &JobId,
    ) -> Result<JobHandleRecord, EnvironmentRegistryError> {
        let mut handles =
            self.job_handles
                .write()
                .map_err(|_| EnvironmentRegistryError::Store {
                    message: "job handle registry write lock poisoned".to_owned(),
                })?;
        handles
            .remove(&job_handle_key(session_id, env_id, job_id))
            .ok_or_else(|| job_handle_not_found(session_id, env_id, job_id))
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

fn job_handle_key(
    session_id: &SessionId,
    env_id: &EnvironmentId,
    job_id: &JobId,
) -> (SessionId, EnvironmentId, JobId) {
    (session_id.clone(), env_id.clone(), job_id.clone())
}

fn job_handle_not_found(
    session_id: &SessionId,
    env_id: &EnvironmentId,
    job_id: &JobId,
) -> EnvironmentRegistryError {
    EnvironmentRegistryError::NotFound {
        kind: "job_handle",
        id: job_handle_id(session_id, env_id, job_id),
    }
}

fn job_handle_id(session_id: &SessionId, env_id: &EnvironmentId, job_id: &JobId) -> String {
    format!("{session_id}/{}/{}", env_id.as_str(), job_id.as_str())
}
