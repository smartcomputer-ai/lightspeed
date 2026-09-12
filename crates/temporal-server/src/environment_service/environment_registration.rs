//! Registration keys: the universe-management surface of key-based outbound
//! `envd` registration. Keys are minted here; admission itself happens on
//! the environment gateway's connect route.

use std::collections::BTreeMap;

use ::environments::{
    BeginCloseEnvironment, CreateEnvironmentRegistrationKey, EnvironmentRecord,
    EnvironmentRegistrationKeyRecord, EnvironmentRegistrationKeyStatus,
    EnvironmentRegistrationKeyStore, EnvironmentStatus, EnvironmentStore, ListEnvironments,
    ObserveRegisteredEnvironment, RegisteredConnectionObservation, RegisteredIdentityMode,
    RegistrationKeyPolicy, RevokeEnvironmentRegistrationKey, mint_registration_key,
};

/// A registered environment whose heartbeat stamp is older than this is
/// treated as disconnected even if its row still says `Ready`: the gateway
/// that held its control connection stopped without recording the
/// disconnect. Three missed heartbeats leaves room for one slow pong.
pub(crate) const REGISTERED_STALE_AFTER_MS: i64 =
    3 * crate::gateway::registration::HEARTBEAT_INTERVAL.as_millis() as i64;

/// What the reconciler does with one open registered environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegisteredSweepAction {
    /// `Ready` with a stale stamp: record the disconnect the gateway lost.
    MarkOffline,
    /// Ephemeral and away past the key's grace: close it.
    Close,
}

/// Pure decision for one open registered environment. Persistent
/// environments only ever get repaired; ephemeral ones close once their
/// daemon has been away longer than the key's disconnect grace.
pub(crate) fn registered_sweep_action(
    environment: &EnvironmentRecord,
    key: &EnvironmentRegistrationKeyRecord,
    now_ms: i64,
) -> Option<RegisteredSweepAction> {
    if !environment.registered_daemon_absent(now_ms, REGISTERED_STALE_AFTER_MS) {
        return None;
    }
    let away_ms = environment
        .last_seen_at_ms
        .map_or(i64::MAX, |seen| now_ms.saturating_sub(seen));
    if key.identity_mode == RegisteredIdentityMode::Ephemeral
        && away_ms > i64::try_from(key.ephemeral_disconnect_grace_ms()).unwrap_or(i64::MAX)
    {
        return Some(RegisteredSweepAction::Close);
    }
    if environment.status == EnvironmentStatus::Ready {
        return Some(RegisteredSweepAction::MarkOffline);
    }
    None
}

use super::*;
use super::{
    environment_lifecycle::parse_registration_key_id,
    environment_providers::{identity_mode_view, registry_identity_mode},
};

impl EnvironmentService {
    pub(crate) async fn create_environment_registration_key_record(
        &self,
        params: EnvironmentRegistrationKeyCreateParams,
    ) -> Result<EnvironmentRegistrationKeyCreateResponse, AgentApiError> {
        let now = now_ms()?;
        let minted = mint_registration_key(
            allocate_registration_key_id(),
            RegistrationKeyPolicy {
                display_name: params.display_name,
                identity_mode: registry_identity_mode(params.identity_mode),
                max_active_environments: params.max_active_environments,
                ephemeral_disconnect_grace_ms: params.ephemeral_disconnect_grace_ms,
                expires_at_ms: params.expires_at_ms,
            },
            now,
        )
        .map_err(map_environments_error)?;
        let record = EnvironmentRegistrationKeyStore::create_registration_key(
            self.store.as_ref(),
            CreateEnvironmentRegistrationKey {
                secret_hash: minted.secret_hash,
                record: minted.record,
            },
        )
        .await
        .map_err(map_environments_error)?;
        tracing::info!(
            target: "temporal_server",
            registration_key_id = %record.registration_key_id,
            display_name = %record.display_name,
            identity_mode = record.identity_mode.as_str(),
            "environment registration key created"
        );
        Ok(EnvironmentRegistrationKeyCreateResponse {
            registration_key: self.registration_key_view(&record, now).await?,
            secret: EnvironmentRegistrationSecretView(minted.secret.expose().to_owned()),
        })
    }

    pub(crate) async fn read_environment_registration_key_record(
        &self,
        params: EnvironmentRegistrationKeyReadParams,
    ) -> Result<EnvironmentRegistrationKeyReadResponse, AgentApiError> {
        let registration_key_id = parse_registration_key_id(params.registration_key_id)?;
        let record = EnvironmentRegistrationKeyStore::read_registration_key(
            self.store.as_ref(),
            &registration_key_id,
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentRegistrationKeyReadResponse {
            registration_key: self.registration_key_view(&record, now_ms()?).await?,
        })
    }

    pub(crate) async fn list_environment_registration_key_records(
        &self,
        _params: EnvironmentRegistrationKeyListParams,
    ) -> Result<EnvironmentRegistrationKeyListResponse, AgentApiError> {
        let now = now_ms()?;
        let records = EnvironmentRegistrationKeyStore::list_registration_keys(self.store.as_ref())
            .await
            .map_err(map_environments_error)?;
        let mut registration_keys = Vec::with_capacity(records.len());
        for record in &records {
            registration_keys.push(self.registration_key_view(record, now).await?);
        }
        Ok(EnvironmentRegistrationKeyListResponse { registration_keys })
    }

    pub(crate) async fn revoke_environment_registration_key_record(
        &self,
        params: EnvironmentRegistrationKeyRevokeParams,
    ) -> Result<EnvironmentRegistrationKeyRevokeResponse, AgentApiError> {
        let registration_key_id = parse_registration_key_id(params.registration_key_id)?;
        let now = now_ms()?;
        let record = EnvironmentRegistrationKeyStore::revoke_registration_key(
            self.store.as_ref(),
            RevokeEnvironmentRegistrationKey {
                registration_key_id: registration_key_id.clone(),
                revoked_at_ms: now,
            },
        )
        .await
        .map_err(map_environments_error)?;
        let mut closed_environment_ids = Vec::new();
        if params.close_environments {
            let environments = EnvironmentStore::list_environments(
                self.store.as_ref(),
                ListEnvironments {
                    metadata: Default::default(),
                    registration_key_id: Some(registration_key_id.clone()),
                    ..ListEnvironments::default()
                },
            )
            .await
            .map_err(map_environments_error)?;
            for environment in environments {
                if matches!(
                    environment.status,
                    EnvironmentStatus::Closing | EnvironmentStatus::Closed
                ) {
                    continue;
                }
                EnvironmentStore::begin_close_environment(
                    self.store.as_ref(),
                    BeginCloseEnvironment {
                        environment_id: environment.environment_id.clone(),
                        updated_at_ms: now,
                    },
                )
                .await
                .map_err(map_environments_error)?;
                closed_environment_ids.push(environment.environment_id.to_string());
            }
        }
        tracing::info!(
            target: "temporal_server",
            registration_key_id = %record.registration_key_id,
            closed = closed_environment_ids.len(),
            "environment registration key revoked"
        );
        Ok(EnvironmentRegistrationKeyRevokeResponse {
            registration_key: self.registration_key_view(&record, now).await?,
            closed_environment_ids,
        })
    }

    /// One pass over open registered environments: repair stale `Ready`
    /// rows and close ephemeral environments past their disconnect grace.
    /// Returns how many rows changed.
    pub(crate) async fn reconcile_registered_once(&self) -> Result<usize, AgentApiError> {
        let environments = EnvironmentStore::list_open_registered_environments(self.store.as_ref())
            .await
            .map_err(map_environments_error)?;
        if environments.is_empty() {
            return Ok(0);
        }
        let now = now_ms()?;
        let mut keys: BTreeMap<String, EnvironmentRegistrationKeyRecord> = BTreeMap::new();
        let mut changed = 0;
        for environment in environments {
            let Some(key_id) = environment.registration_key_id() else {
                continue;
            };
            let key = match keys.get(key_id.as_str()) {
                Some(key) => key.clone(),
                None => {
                    let key = EnvironmentRegistrationKeyStore::read_registration_key(
                        self.store.as_ref(),
                        key_id,
                    )
                    .await
                    .map_err(map_environments_error)?;
                    keys.insert(key_id.to_string(), key.clone());
                    key
                }
            };
            match registered_sweep_action(&environment, &key, now) {
                None => {}
                Some(RegisteredSweepAction::MarkOffline) => {
                    tracing::info!(
                        target: "temporal_server",
                        environment = %environment.environment_id,
                        "registered environment heartbeat is stale; marking offline"
                    );
                    EnvironmentStore::observe_registered_environment(
                        self.store.as_ref(),
                        ObserveRegisteredEnvironment {
                            environment_id: environment.environment_id.clone(),
                            observation: RegisteredConnectionObservation::Disconnected,
                            observed_at_ms: now,
                            metadata: BTreeMap::new(),
                        },
                    )
                    .await
                    .map_err(map_environments_error)?;
                    changed += 1;
                }
                Some(RegisteredSweepAction::Close) => {
                    tracing::info!(
                        target: "temporal_server",
                        environment = %environment.environment_id,
                        registration_key_id = %key.registration_key_id,
                        "ephemeral registered environment exceeded its disconnect grace; closing"
                    );
                    EnvironmentStore::begin_close_environment(
                        self.store.as_ref(),
                        BeginCloseEnvironment {
                            environment_id: environment.environment_id.clone(),
                            updated_at_ms: now,
                        },
                    )
                    .await
                    .map_err(map_environments_error)?;
                    changed += 1;
                }
            }
        }
        Ok(changed)
    }

    async fn registration_key_view(
        &self,
        record: &EnvironmentRegistrationKeyRecord,
        now_ms: i64,
    ) -> Result<EnvironmentRegistrationKeyView, AgentApiError> {
        let usage = EnvironmentRegistrationKeyStore::registration_key_usage(
            self.store.as_ref(),
            &record.registration_key_id,
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentRegistrationKeyView {
            registration_key_id: record.registration_key_id.to_string(),
            display_name: record.display_name.clone(),
            key_prefix: record.key_prefix.clone(),
            identity_mode: identity_mode_view(record.identity_mode),
            max_active_environments: record.max_active_environments,
            ephemeral_disconnect_grace_ms: record.ephemeral_disconnect_grace_ms(),
            expires_at_ms: record.expires_at_ms,
            status: match record.status(now_ms) {
                EnvironmentRegistrationKeyStatus::Active => {
                    EnvironmentRegistrationKeyStatusView::Active
                }
                EnvironmentRegistrationKeyStatus::Revoked => {
                    EnvironmentRegistrationKeyStatusView::Revoked
                }
                EnvironmentRegistrationKeyStatus::Expired => {
                    EnvironmentRegistrationKeyStatusView::Expired
                }
            },
            registered_environment_count: usage.registered,
            active_environment_count: usage.active,
            last_registered_at_ms: usage.last_registered_at_ms,
            created_at_ms: record.created_at_ms,
            revoked_at_ms: record.revoked_at_ms,
        })
    }
}

fn allocate_registration_key_id() -> ::environments::EnvironmentRegistrationKeyId {
    ::environments::EnvironmentRegistrationKeyId::new(format!(
        "registration_key_{}",
        uuid::Uuid::new_v4().simple()
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ::environments::{
        EnvironmentDaemonId, EnvironmentId, EnvironmentIncarnationId, EnvironmentIncarnationRecord,
        EnvironmentProvisionRequestId, EnvironmentRegistrationKeyId, EnvironmentSource, PowerState,
    };

    use super::*;

    fn key(
        mode: RegisteredIdentityMode,
        grace_ms: Option<u64>,
    ) -> EnvironmentRegistrationKeyRecord {
        EnvironmentRegistrationKeyRecord {
            registration_key_id: EnvironmentRegistrationKeyId::new("rk"),
            display_name: "pool".to_owned(),
            key_prefix: "lsrk_abcdefgh".to_owned(),
            identity_mode: mode,
            max_active_environments: None,
            ephemeral_disconnect_grace_ms: grace_ms,
            expires_at_ms: None,
            created_at_ms: 0,
            revoked_at_ms: None,
        }
    }

    fn environment(status: EnvironmentStatus, last_seen_at_ms: Option<i64>) -> EnvironmentRecord {
        let public_key = "ab".repeat(32);
        let daemon_id = EnvironmentDaemonId::from_public_key(&[0xab; 32]);
        EnvironmentRecord {
            environment_id: EnvironmentId::new("env"),
            request_id: EnvironmentProvisionRequestId::for_daemon(&daemon_id),
            source: EnvironmentSource::Registered {
                registration_key_id: EnvironmentRegistrationKeyId::new("rk"),
                daemon_id,
                daemon_public_key: public_key,
                identity_mode: RegisteredIdentityMode::Ephemeral,
            },
            display_name: None,
            status,
            desired_power: PowerState::Running,
            idle_policy: None,
            incarnation: EnvironmentIncarnationRecord {
                incarnation_id: EnvironmentIncarnationId::new("inc"),
                provision_request_id: None,
                provider_target_id: None,
                template_id: None,
                adoption_source_target: None,
                power_states: Vec::new(),
                created_at_ms: 0,
                updated_at_ms: 0,
            },
            public_ingress_enabled: false,
            public_endpoint: None,
            metadata: BTreeMap::new(),
            last_seen_at_ms,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    #[test]
    fn fresh_heartbeats_need_nothing() {
        let ready = environment(EnvironmentStatus::Ready, Some(1_000));
        assert_eq!(
            registered_sweep_action(
                &ready,
                &key(RegisteredIdentityMode::Ephemeral, Some(1)),
                1_500
            ),
            None
        );
    }

    #[test]
    fn stale_ready_rows_are_marked_offline_before_grace_and_closed_after() {
        let stale = environment(EnvironmentStatus::Ready, Some(0));
        let now = REGISTERED_STALE_AFTER_MS + 1;
        assert_eq!(
            registered_sweep_action(
                &stale,
                &key(RegisteredIdentityMode::Ephemeral, Some(1_000_000)),
                now
            ),
            Some(RegisteredSweepAction::MarkOffline)
        );
        assert_eq!(
            registered_sweep_action(
                &stale,
                &key(RegisteredIdentityMode::Ephemeral, Some(1_000)),
                now
            ),
            Some(RegisteredSweepAction::Close)
        );
        assert_eq!(
            registered_sweep_action(
                &stale,
                &key(RegisteredIdentityMode::Persistent, Some(1)),
                now
            ),
            Some(RegisteredSweepAction::MarkOffline)
        );
    }

    #[test]
    fn offline_environments_close_only_when_ephemeral_and_past_grace() {
        let offline = environment(EnvironmentStatus::Offline, Some(10_000));
        let persistent = key(RegisteredIdentityMode::Persistent, Some(1));
        assert_eq!(
            registered_sweep_action(&offline, &persistent, 1_000_000),
            None
        );
        let ephemeral = key(RegisteredIdentityMode::Ephemeral, Some(5_000));
        assert_eq!(registered_sweep_action(&offline, &ephemeral, 14_000), None);
        assert_eq!(
            registered_sweep_action(&offline, &ephemeral, 15_001),
            Some(RegisteredSweepAction::Close)
        );
        let default_grace = key(RegisteredIdentityMode::Ephemeral, None);
        assert_eq!(
            registered_sweep_action(
                &offline,
                &default_grace,
                10_000 + ::environments::DEFAULT_EPHEMERAL_DISCONNECT_GRACE_MS as i64 + 1
            ),
            Some(RegisteredSweepAction::Close)
        );
    }
}
