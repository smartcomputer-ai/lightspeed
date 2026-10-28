//! Registration keys: the universe-management surface of key-based outbound
//! `envd` registration. Keys are minted, edited and rotated here; admission
//! itself happens on the environment gateway's connect route, and the
//! gateway's registered sweep applies the keys' disconnect policies.

use ::environments::{
    CreateEnvironmentRegistrationKey, EnvironmentCloseReason, EnvironmentRegistrationKeyRecord,
    EnvironmentRegistrationKeyStatus, EnvironmentRegistrationKeyStore, EnvironmentState,
    EnvironmentStore, ListEnvironments, RegistrationKeyPolicy, RegistrationKeySettings,
    RevokeEnvironmentRegistrationKey, RotateEnvironmentRegistrationKey,
    UpdateEnvironmentRegistrationKey, mint_registration_key, mint_registration_secret,
};

use super::*;
use super::{
    lifecycle::parse_registration_key_id,
    providers::{identity_mode_view, registry_identity_mode},
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
                offline_close_after_ms: params.offline_close_after_ms,
                expires_at_ms: params.expires_at_ms,
            },
            now,
        )
        .map_err(map_environments_error)?;
        let record = EnvironmentRegistrationKeyStore::create_registration_key(
            self.store.as_ref(),
            CreateEnvironmentRegistrationKey {
                secret_hash: minted.secret.secret_hash,
                record: minted.record,
            },
        )
        .await
        .map_err(map_environments_error)?;
        tracing::info!(
            target: "temporal_runtime",
            registration_key_id = %record.registration_key_id,
            display_name = %record.display_name,
            identity_mode = record.identity_mode.as_str(),
            "environment registration key created"
        );
        Ok(EnvironmentRegistrationKeyCreateResponse {
            registration_key: self.registration_key_view(&record, now).await?,
            secret: EnvironmentRegistrationSecretView(minted.secret.secret.expose().to_owned()),
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

    /// `environments/registration-keys/update`: replace the editable policy.
    /// The identity mode never changes.
    pub(crate) async fn update_environment_registration_key_record(
        &self,
        params: EnvironmentRegistrationKeyUpdateParams,
    ) -> Result<EnvironmentRegistrationKeyUpdateResponse, AgentApiError> {
        let registration_key_id = parse_registration_key_id(params.registration_key_id)?;
        let now = now_ms()?;
        let record = EnvironmentRegistrationKeyStore::update_registration_key(
            self.store.as_ref(),
            UpdateEnvironmentRegistrationKey {
                registration_key_id,
                settings: RegistrationKeySettings {
                    display_name: params.display_name,
                    max_active_environments: params.max_active_environments,
                    ephemeral_disconnect_grace_ms: params.ephemeral_disconnect_grace_ms,
                    offline_close_after_ms: params.offline_close_after_ms,
                    expires_at_ms: params.expires_at_ms,
                },
                updated_at_ms: now,
            },
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentRegistrationKeyUpdateResponse {
            registration_key: self.registration_key_view(&record, now).await?,
        })
    }

    /// `environments/registration-keys/rotate`: retire the live secret at
    /// once and mint a new one. The key's group, allowlists and admitted
    /// daemons are unaffected.
    pub(crate) async fn rotate_environment_registration_key_record(
        &self,
        params: EnvironmentRegistrationKeyRotateParams,
    ) -> Result<EnvironmentRegistrationKeyRotateResponse, AgentApiError> {
        let registration_key_id = parse_registration_key_id(params.registration_key_id)?;
        let now = now_ms()?;
        let minted = mint_registration_secret();
        let record = EnvironmentRegistrationKeyStore::rotate_registration_key(
            self.store.as_ref(),
            RotateEnvironmentRegistrationKey {
                registration_key_id,
                secret_hash: minted.secret_hash,
                key_prefix: minted.key_prefix,
                rotated_at_ms: now,
            },
        )
        .await
        .map_err(map_environments_error)?;
        tracing::info!(
            target: "temporal_runtime",
            registration_key_id = %record.registration_key_id,
            "environment registration key rotated"
        );
        Ok(EnvironmentRegistrationKeyRotateResponse {
            registration_key: self.registration_key_view(&record, now).await?,
            secret: EnvironmentRegistrationSecretView(minted.secret.expose().to_owned()),
        })
    }

    /// Revoke a key; with `closeEnvironments`, close every environment it
    /// admitted (`key_revoked`) and end their live routes.
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
            let mut after = None;
            loop {
                let page = EnvironmentStore::list_environments(
                    self.store.as_ref(),
                    ListEnvironments {
                        registration_key_id: Some(registration_key_id.clone()),
                        limit: ::environments::LIST_ENVIRONMENTS_MAX_LIMIT,
                        after,
                        ..ListEnvironments::default()
                    },
                )
                .await
                .map_err(map_environments_error)?;
                for environment in &page.environments {
                    if matches!(
                        environment.state,
                        EnvironmentState::Closing | EnvironmentState::Closed
                    ) {
                        continue;
                    }
                    self.begin_close(
                        &environment.environment_id,
                        EnvironmentCloseReason::KeyRevoked,
                    )
                    .await?;
                    closed_environment_ids.push(environment.environment_id.to_string());
                }
                after = page.next_after;
                if after.is_none() {
                    break;
                }
            }
        }
        tracing::info!(
            target: "temporal_runtime",
            registration_key_id = %record.registration_key_id,
            closed = closed_environment_ids.len(),
            "environment registration key revoked"
        );
        Ok(EnvironmentRegistrationKeyRevokeResponse {
            registration_key: self.registration_key_view(&record, now).await?,
            closed_environment_ids,
        })
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
        Ok(registration_key_view(record, usage, now_ms))
    }
}

pub(crate) fn registration_key_view(
    record: &EnvironmentRegistrationKeyRecord,
    usage: ::environments::RegistrationKeyUsage,
    now_ms: i64,
) -> EnvironmentRegistrationKeyView {
    EnvironmentRegistrationKeyView {
        registration_key_id: record.registration_key_id.to_string(),
        display_name: record.display_name.clone(),
        key_prefix: record.key_prefix.clone(),
        identity_mode: identity_mode_view(record.identity_mode),
        max_active_environments: record.max_active_environments,
        ephemeral_disconnect_grace_ms: record.ephemeral_disconnect_grace_ms(),
        offline_close_after_ms: record.offline_close_after_ms,
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
        updated_at_ms: record.updated_at_ms,
        revoked_at_ms: record.revoked_at_ms,
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
    use ::environments::{
        EnvironmentRegistrationKeyId, RegisteredIdentityMode, RegistrationKeyUsage,
    };

    use super::*;

    #[test]
    fn the_key_view_carries_the_editable_policy_and_its_update_time() {
        let mut minted = mint_registration_key(
            EnvironmentRegistrationKeyId::new("rk"),
            RegistrationKeyPolicy {
                display_name: "fleet".to_owned(),
                identity_mode: RegisteredIdentityMode::Persistent,
                max_active_environments: Some(3),
                ephemeral_disconnect_grace_ms: None,
                offline_close_after_ms: Some(86_400_000),
                expires_at_ms: None,
            },
            5,
        )
        .unwrap();
        minted.record.updated_at_ms = 9;
        let view = registration_key_view(
            &minted.record,
            RegistrationKeyUsage {
                registered: 2,
                active: 1,
                last_registered_at_ms: Some(7),
            },
            10,
        );
        assert_eq!(view.offline_close_after_ms, Some(86_400_000));
        assert_eq!(view.updated_at_ms, 9);
        assert_eq!(view.status, EnvironmentRegistrationKeyStatusView::Active);
        assert_eq!(
            (
                view.registered_environment_count,
                view.active_environment_count
            ),
            (2, 1)
        );
        assert_eq!(view.key_prefix, minted.secret.key_prefix);
    }
}
