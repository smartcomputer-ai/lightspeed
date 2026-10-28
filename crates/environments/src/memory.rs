use std::{collections::BTreeMap, sync::RwLock};

use async_trait::async_trait;
use uuid::Uuid;

use super::*;

type CredentialKey = (EnvironmentId, String);
type BindingKey = (Uuid, EnvironmentProviderId);

struct StoredSecret {
    secret_hash: String,
    retired_at_ms: Option<i64>,
}

struct StoredRegistrationKey {
    record: EnvironmentRegistrationKeyRecord,
    secrets: Vec<StoredSecret>,
}

#[derive(Default)]
struct RegistryState {
    providers: BTreeMap<EnvironmentProviderId, EnvironmentProviderRecord>,
    provider_credentials: BTreeMap<EnvironmentProviderId, SecretValue>,
    bindings: BTreeMap<BindingKey, EnvironmentProviderBindingRecord>,
    environments: BTreeMap<EnvironmentId, EnvironmentRecord>,
    credentials: BTreeMap<CredentialKey, EnvironmentCredentialRecord>,
    registration_keys: BTreeMap<EnvironmentRegistrationKeyId, StoredRegistrationKey>,
}

impl RegistryState {
    fn by_request_id(
        &self,
        request_id: &EnvironmentProvisionRequestId,
    ) -> Option<&EnvironmentRecord> {
        self.environments
            .values()
            .find(|record| record.request_id.as_ref() == Some(request_id))
    }

    fn by_daemon_public_key(&self, daemon_public_key: &str) -> Option<&EnvironmentRecord> {
        self.environments.values().find(|record| {
            record
                .registered()
                .is_some_and(|source| source.daemon_public_key == daemon_public_key)
        })
    }

    fn insert_new(
        &mut self,
        record: EnvironmentRecord,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        if self.environments.contains_key(&record.environment_id) {
            return Err(EnvironmentRegistryError::AlreadyExists {
                kind: "environment",
                id: record.environment_id.to_string(),
            });
        }
        self.environments
            .insert(record.environment_id.clone(), record.clone());
        Ok(record)
    }

    fn registration_key(
        &self,
        registration_key_id: &EnvironmentRegistrationKeyId,
    ) -> Result<&EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
        self.registration_keys
            .get(registration_key_id)
            .map(|stored| &stored.record)
            .ok_or_else(|| not_found("environment_registration_key", registration_key_id))
    }
}

/// An in-memory registry for one universe, following the same rules as the
/// PostgreSQL store through the shared `apply` functions.
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

    /// Apply one write to one environment: on a copy, validated, stored
    /// only when valid. A refused change leaves the stored record untouched.
    fn change(
        &self,
        environment_id: &EnvironmentId,
        change: impl FnOnce(&mut EnvironmentRecord) -> Result<bool, EnvironmentRegistryError>,
    ) -> Result<(EnvironmentRecord, bool), EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        change_environment(&mut state, environment_id, change)
    }
}

fn change_environment(
    state: &mut RegistryState,
    environment_id: &EnvironmentId,
    change: impl FnOnce(&mut EnvironmentRecord) -> Result<bool, EnvironmentRegistryError>,
) -> Result<(EnvironmentRecord, bool), EnvironmentRegistryError> {
    let stored = state
        .environments
        .get_mut(environment_id)
        .ok_or_else(|| not_found("environment", environment_id))?;
    let mut candidate = stored.clone();
    let changed = change(&mut candidate)?;
    if !changed {
        return Ok((stored.clone(), false));
    }
    candidate.validate()?;
    *stored = candidate.clone();
    Ok((candidate, true))
}

#[async_trait]
impl EnvironmentProviderStore for InMemoryEnvironmentRegistryStore {
    async fn put_provider(
        &self,
        request: PutEnvironmentProvider,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        let credential = request.credential.clone();
        let existing = state.providers.get(&request.provider_id).cloned();
        let record = request.into_record(existing.as_ref())?;
        match credential {
            ProviderCredentialChange::Keep => {}
            ProviderCredentialChange::Set(credential) => {
                state
                    .provider_credentials
                    .insert(record.provider_id.clone(), credential);
            }
            ProviderCredentialChange::Clear => {
                state.provider_credentials.remove(&record.provider_id);
            }
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

    async fn read_provider_credential(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<Option<SecretValue>, EnvironmentRegistryError> {
        let state = self.read_state()?;
        if !state.providers.contains_key(provider_id) {
            return Err(not_found("environment_provider", provider_id));
        }
        Ok(state.provider_credentials.get(provider_id).cloned())
    }

    async fn list_providers(
        &self,
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
        if state.environments.values().any(|record| {
            record.state != EnvironmentState::Closed && record.provider_id() == Some(provider_id)
        }) {
            return invalid("environment provider is referenced by a non-closed environment");
        }
        let record = state
            .providers
            .remove(provider_id)
            .ok_or_else(|| not_found("environment_provider", provider_id))?;
        state.provider_credentials.remove(provider_id);
        Ok(record)
    }
}

#[async_trait]
impl EnvironmentProviderBindingStore for InMemoryEnvironmentRegistryStore {
    async fn put_provider_binding(
        &self,
        request: PutEnvironmentProviderBinding,
    ) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        if !state.providers.contains_key(&request.provider_id) {
            return Err(not_found("environment_provider", &request.provider_id));
        }
        let key = (request.universe_id, request.provider_id.clone());
        let record = request.into_record(state.bindings.get(&key))?;
        state.bindings.insert(key, record.clone());
        Ok(record)
    }

    async fn read_provider_binding(
        &self,
        universe_id: Uuid,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError> {
        self.read_state()?
            .bindings
            .get(&(universe_id, provider_id.clone()))
            .cloned()
            .ok_or_else(|| not_found("environment_provider_binding", provider_id))
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
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        if universe_id == self.universe_id
            && state.environments.values().any(|record| {
                record.state != EnvironmentState::Closed
                    && record.provider_id() == Some(provider_id)
            })
        {
            return invalid("provider binding is referenced by a non-closed environment");
        }
        state
            .bindings
            .remove(&(universe_id, provider_id.clone()))
            .ok_or_else(|| not_found("environment_provider_binding", provider_id))
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
        if let Some(existing) = state.by_request_id(&request.request_id) {
            return if request.matches(existing) {
                Ok(existing.clone())
            } else {
                Err(EnvironmentRegistryError::AlreadyExists {
                    kind: "environment_request",
                    id: request.request_id.to_string(),
                })
            };
        }
        let binding = state
            .bindings
            .get(&(self.universe_id, request.provider_id.clone()))
            .ok_or_else(|| not_found("environment_provider_binding", &request.provider_id))?;
        if !binding.enabled {
            return invalid("environment provider binding is disabled");
        }
        let record = request.record()?;
        state.insert_new(record)
    }

    async fn create_external_environment(
        &self,
        request: CreateExternalEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.created_at_ms, "created_at_ms")?;
        let mut state = self.write_state()?;
        if let Some(existing) = state.by_request_id(&request.request_id) {
            return if request.matches(existing) {
                Ok(existing.clone())
            } else {
                Err(EnvironmentRegistryError::AlreadyExists {
                    kind: "environment_request",
                    id: request.request_id.to_string(),
                })
            };
        }
        let record = request.record()?;
        state.insert_new(record)
    }

    async fn create_registered_environment(
        &self,
        request: CreateRegisteredEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.created_at_ms, "created_at_ms")?;
        validate_daemon_public_key(&request.daemon_public_key)?;
        validate_secret_hash(&request.secret_hash)?;
        let mut state = self.write_state()?;
        if let Some(existing) = state.by_daemon_public_key(&request.daemon_public_key) {
            return Ok(existing.clone());
        }
        let key = state
            .registration_key(&request.registration_key_id)?
            .clone();
        key.check_admits(request.created_at_ms)?;
        let live = state
            .registration_keys
            .get(&request.registration_key_id)
            .is_some_and(|stored| {
                stored.secrets.iter().any(|secret| {
                    secret.retired_at_ms.is_none() && secret.secret_hash == request.secret_hash
                })
            });
        if !live {
            return Err(request.secret_retired());
        }
        if let Some(limit) = key.max_active_environments {
            let active = state
                .environments
                .values()
                .filter(|record| {
                    record.registration_key_id() == Some(&key.registration_key_id)
                        && record.state != EnvironmentState::Closed
                })
                .count();
            if active >= limit as usize {
                return Err(EnvironmentRegistryError::RegistrationCapacityExhausted {
                    registration_key_id: key.registration_key_id.to_string(),
                    limit,
                });
            }
        }
        let record = request.record(&key)?;
        state.insert_new(record)
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

    async fn read_environment_by_daemon_public_key(
        &self,
        daemon_public_key: &str,
    ) -> Result<Option<EnvironmentRecord>, EnvironmentRegistryError> {
        Ok(self
            .read_state()?
            .by_daemon_public_key(daemon_public_key)
            .cloned())
    }

    async fn list_environments(
        &self,
        request: ListEnvironments,
    ) -> Result<EnvironmentPage, EnvironmentRegistryError> {
        request.validate()?;
        let state = self.read_state()?;
        let mut matching = state
            .environments
            .values()
            .filter(|record| {
                request
                    .after
                    .as_ref()
                    .is_none_or(|after| &record.environment_id > after)
            })
            .filter(|record| request.matches(record));
        let environments: Vec<EnvironmentRecord> = matching
            .by_ref()
            .take(request.limit as usize)
            .cloned()
            .collect();
        let next_after = if matching.next().is_some() {
            environments
                .last()
                .map(|record| record.environment_id.clone())
        } else {
            None
        };
        Ok(EnvironmentPage {
            environments,
            next_after,
        })
    }

    async fn update_environment(
        &self,
        request: UpdateEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change(&request.environment_id, |record| request.apply(record))
            .map(|(record, _)| record)
    }

    async fn begin_close_environment(
        &self,
        request: BeginCloseEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change(&request.environment_id, |record| request.apply(record))
            .map(|(record, _)| record)
    }

    async fn finish_close_environment(
        &self,
        request: FinishCloseEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change(&request.environment_id, |record| request.apply(record))
            .map(|(record, _)| record)
    }

    async fn mark_registered_connected(
        &self,
        request: MarkRegisteredConnected,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change(&request.environment_id, |record| request.apply(record))
            .map(|(record, _)| record)
    }

    async fn mark_registered_disconnected(
        &self,
        request: MarkRegisteredDisconnected,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change(&request.environment_id, |record| request.apply(record))
            .map(|(record, _)| record)
    }

    async fn stamp_registered_seen(
        &self,
        request: StampRegisteredSeen,
    ) -> Result<bool, EnvironmentRegistryError> {
        self.change(&request.environment_id, |record| request.apply(record))
            .map(|(_, written)| written)
    }

    async fn sweep_registered_environments(
        &self,
        now_ms: i64,
    ) -> Result<RegisteredSweep, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        let candidates: Vec<(EnvironmentId, EnvironmentRegistrationKeyRecord)> = state
            .environments
            .values()
            .filter(|record| record.state == EnvironmentState::Open)
            .filter_map(|record| {
                let key = state.registration_keys.get(record.registration_key_id()?)?;
                Some((record.environment_id.clone(), key.record.clone()))
            })
            .collect();
        let mut sweep = RegisteredSweep::default();
        for (environment_id, key) in candidates {
            let mut outcome = (false, None);
            change_environment(&mut state, &environment_id, |record| {
                outcome = sweep_registered_environment(record, &key, now_ms);
                Ok(outcome.0 || outcome.1.is_some())
            })?;
            if outcome.0 {
                sweep.disconnected.push(environment_id.clone());
            }
            if let Some(reason) = outcome.1 {
                sweep.closed.push((environment_id, reason));
            }
        }
        Ok(sweep)
    }

    async fn repair_registered_connections(
        &self,
        now_ms: i64,
    ) -> Result<u64, EnvironmentRegistryError> {
        validate_nonnegative_i64(now_ms, "now_ms")?;
        let mut state = self.write_state()?;
        let mut repaired = 0;
        for record in state.environments.values_mut() {
            let Some(source) = record.registered_mut() else {
                continue;
            };
            if source.connected {
                source.connected = false;
                source.connection_changed_at_ms = now_ms;
                record.touch(now_ms);
                repaired += 1;
            }
        }
        Ok(repaired)
    }

    async fn list_environment_work(
        &self,
        now_ms: i64,
        limit: u32,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError> {
        let state = self.read_state()?;
        let mut due: Vec<(i64, &EnvironmentRecord)> = state
            .environments
            .values()
            .filter_map(|record| {
                let next = record.provisioned()?.next_attempt_at_ms?;
                (next <= now_ms).then_some((next, record))
            })
            .collect();
        due.sort_by(|(left_at, left), (right_at, right)| {
            left_at
                .cmp(right_at)
                .then_with(|| left.environment_id.cmp(&right.environment_id))
        });
        Ok(due
            .into_iter()
            .take(limit as usize)
            .map(|(_, record)| record.clone())
            .collect())
    }

    async fn list_open_provisioned_environments(
        &self,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError> {
        Ok(self
            .read_state()?
            .environments
            .values()
            .filter(|record| {
                record.state == EnvironmentState::Open && record.provisioned().is_some()
            })
            .cloned()
            .collect())
    }

    async fn record_target_observation(
        &self,
        request: RecordTargetObservation,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        if let Some(target_id) = &request.provider_target_id
            && let Some(provider_id) = state
                .environments
                .get(&request.environment_id)
                .and_then(EnvironmentRecord::provider_id)
            && state.environments.values().any(|other| {
                other.environment_id != request.environment_id
                    && other.state != EnvironmentState::Closed
                    && other.provider_id() == Some(provider_id)
                    && other
                        .provisioned()
                        .and_then(|source| source.provider_target_id.as_ref())
                        == Some(target_id)
            })
        {
            return Err(EnvironmentRegistryError::AlreadyExists {
                kind: "provider_target",
                id: target_id.to_string(),
            });
        }
        change_environment(&mut state, &request.environment_id, |record| {
            request.apply(record)
        })
        .map(|(record, _)| record)
    }

    async fn schedule_environment_attempt(
        &self,
        request: ScheduleEnvironmentAttempt,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change(&request.environment_id, |record| request.apply(record))
            .map(|(record, _)| record)
    }

    async fn open_environment(
        &self,
        request: OpenEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change(&request.environment_id, |record| request.apply(record))
            .map(|(record, _)| record)
    }

    async fn fail_environment(
        &self,
        request: FailEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change(&request.environment_id, |record| request.apply(record))
            .map(|(record, _)| record)
    }

    async fn end_provisioned_environment(
        &self,
        request: EndProvisionedEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change(&request.environment_id, |record| request.apply(record))
            .map(|(record, _)| record)
    }
}

#[async_trait]
impl EnvironmentRegistrationKeyStore for InMemoryEnvironmentRegistryStore {
    async fn create_registration_key(
        &self,
        request: CreateEnvironmentRegistrationKey,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
        request.record.validate()?;
        validate_secret_hash(&request.secret_hash)?;
        let mut state = self.write_state()?;
        if state
            .registration_keys
            .contains_key(&request.record.registration_key_id)
            || secret_known(&state, &request.secret_hash)
        {
            return Err(EnvironmentRegistryError::AlreadyExists {
                kind: "environment_registration_key",
                id: request.record.registration_key_id.to_string(),
            });
        }
        state.registration_keys.insert(
            request.record.registration_key_id.clone(),
            StoredRegistrationKey {
                record: request.record.clone(),
                secrets: vec![StoredSecret {
                    secret_hash: request.secret_hash,
                    retired_at_ms: None,
                }],
            },
        );
        Ok(request.record)
    }

    async fn read_registration_key(
        &self,
        registration_key_id: &EnvironmentRegistrationKeyId,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
        self.read_state()?
            .registration_key(registration_key_id)
            .cloned()
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

    async fn update_registration_key(
        &self,
        request: UpdateEnvironmentRegistrationKey,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        let stored = state
            .registration_keys
            .get_mut(&request.registration_key_id)
            .ok_or_else(|| {
                not_found("environment_registration_key", &request.registration_key_id)
            })?;
        request.apply(&mut stored.record)?;
        Ok(stored.record.clone())
    }

    async fn rotate_registration_key(
        &self,
        request: RotateEnvironmentRegistrationKey,
    ) -> Result<EnvironmentRegistrationKeyRecord, EnvironmentRegistryError> {
        let mut state = self.write_state()?;
        if secret_known(&state, &request.secret_hash) {
            return Err(EnvironmentRegistryError::AlreadyExists {
                kind: "environment_registration_key_secret",
                id: request.key_prefix.clone(),
            });
        }
        let stored = state
            .registration_keys
            .get_mut(&request.registration_key_id)
            .ok_or_else(|| {
                not_found("environment_registration_key", &request.registration_key_id)
            })?;
        request.apply(&mut stored.record)?;
        for secret in &mut stored.secrets {
            if secret.retired_at_ms.is_none() {
                secret.retired_at_ms = Some(request.rotated_at_ms);
            }
        }
        stored.secrets.push(StoredSecret {
            secret_hash: request.secret_hash,
            retired_at_ms: None,
        });
        Ok(stored.record.clone())
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
            let revoked_at_ms = request.revoked_at_ms.max(stored.record.created_at_ms);
            stored.record.revoked_at_ms = Some(revoked_at_ms);
            stored.record.updated_at_ms = stored.record.updated_at_ms.max(revoked_at_ms);
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
            .find(|stored| {
                stored.secrets.iter().any(|secret| {
                    secret.retired_at_ms.is_none() && secret.secret_hash == secret_hash
                })
            })
            .map(|stored| stored.record.clone()))
    }

    async fn registration_key_usage(
        &self,
        registration_key_id: &EnvironmentRegistrationKeyId,
    ) -> Result<RegistrationKeyUsage, EnvironmentRegistryError> {
        let state = self.read_state()?;
        state.registration_key(registration_key_id)?;
        let mut usage = RegistrationKeyUsage::default();
        for record in state
            .environments
            .values()
            .filter(|record| record.registration_key_id() == Some(registration_key_id))
        {
            usage.registered += 1;
            if record.state != EnvironmentState::Closed {
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

/// Secret hashes are unique across every key, retired ones included.
fn secret_known(state: &RegistryState, secret_hash: &str) -> bool {
    state.registration_keys.values().any(|stored| {
        stored
            .secrets
            .iter()
            .any(|secret| secret.secret_hash == secret_hash)
    })
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
