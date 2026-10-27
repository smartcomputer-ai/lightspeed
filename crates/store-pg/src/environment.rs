// Formatted SQL uses internal schema fragments; request values use bind parameters.
use async_trait::async_trait;
use auth::{AuthGrantId, AuthProviderId, SecretId, SecretValue};
use environment_protocol::{ids::ProviderTargetId, routes::DataRouteBase};
use environments::{
    AdoptEnvironment, BeginCloseEnvironment, ControllerFeatures, CreateEnvironment,
    CreateExternalEnvironment, CreateRegisteredEnvironment, EnvironmentCredentialRecord,
    EnvironmentCredentialSource, EnvironmentCredentialStore, EnvironmentDaemonId,
    EnvironmentEndpoint, EnvironmentId, EnvironmentIncarnationId, EnvironmentIncarnationRecord,
    EnvironmentLifecycleError, EnvironmentProviderBindingId, EnvironmentProviderBindingRecord,
    EnvironmentProviderBindingStatus, EnvironmentProviderBindingStore, EnvironmentProviderId,
    EnvironmentProviderRecord, EnvironmentProviderStore, EnvironmentProvisionRequestId,
    EnvironmentRecord, EnvironmentRegistrationKeyId, EnvironmentRegistryError, EnvironmentSource,
    EnvironmentStatus, EnvironmentStore, EnvironmentTemplateId, FailEnvironmentLifecycle,
    FinishCloseEnvironment, ListEnvironmentCredentials, ListEnvironmentProviders, ListEnvironments,
    ObserveProvisionedEnvironment, ObserveRegisteredEnvironment, PowerState,
    PutEnvironmentCredential, PutEnvironmentProvider, PutEnvironmentProviderBinding,
    ReconcileScope, RecordEnvironmentIngress, RecordEnvironmentLifecycleError,
    RecordProviderController, RegisteredConnectionObservation, RegisteredIdentityMode,
    SetEnvironmentIdlePolicy, SetEnvironmentIngress, SetEnvironmentPower,
    apply_registered_observation,
};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{
    PgStore,
    auth::{LOCAL_KEY_ID, open_secret, seal_secret},
};

const PROVIDER_COLUMNS: &str = r#"
    provider_id, display_name, controller_endpoint, metadata_json,
    controller_adopt_target, controller_ingress, data_route_base,
    created_at_ms, updated_at_ms
"#;

const BINDING_COLUMNS: &str = r#"
    universe_id, binding_id, provider_id, status, revision, metadata_json,
    created_at_ms, updated_at_ms
"#;

const ENVIRONMENT_COLUMNS: &str = r#"
    e.environment_id, e.request_id, e.source_kind, e.provider_id, e.binding_id, e.daemon_endpoint,
    e.registration_key_id, e.daemon_id, e.daemon_public_key, e.identity_mode, e.last_seen_at_ms,
    e.display_name, e.status, e.desired_power, e.idle_policy_json,
    e.public_ingress_enabled, e.public_endpoint, e.metadata_json,
    e.lifecycle_error, e.lifecycle_error_first_at_ms, e.lifecycle_error_last_at_ms,
    e.created_at_ms, e.updated_at_ms,
    i.incarnation_id, i.provision_request_id, i.provider_target_id,
    i.template_id, i.adoption_source_target, i.power_states_json,
    i.created_at_ms AS incarnation_created_at_ms,
    i.updated_at_ms AS incarnation_updated_at_ms
"#;

/// SQL predicate: the observed steady power state differs from the desired
/// one, so the lifecycle reconciler has power work to do.
/// `EnvironmentRecord::needs_reconcile` over `environments e` without the
/// steady clause: convergence pending (status, power or ingress).
pub(crate) const CONVERGING_SQL: &str = r#"(
    e.status IN ('provisioning','booting','closing','unknown')
    OR (e.source_kind = 'provisioned' AND (
        (e.status = 'ready' AND e.desired_power <> 'running')
        OR (e.status = 'paused' AND e.desired_power <> 'paused')
        OR (e.status = 'suspended' AND e.desired_power <> 'suspended')
        OR (e.status = 'offline' AND e.desired_power <> 'stopped')
        OR (e.status = 'ready' AND e.public_ingress_enabled <> (e.public_endpoint IS NOT NULL))
    ))
)"#;

/// Steady provisioned environments, over `environments e`; with the
/// incarnation join the recorded target is required too.
pub(crate) const STEADY_SQL: &str = r#"(
    e.source_kind = 'provisioned' AND e.status IN ('ready','offline','paused','suspended')
)"#;

const ENVIRONMENT_JOIN: &str = r#"
    FROM environments e
    JOIN environment_incarnations i
      ON i.universe_id = e.universe_id
     AND i.environment_id = e.environment_id
     AND i.incarnation_id = e.current_incarnation_id
"#;

const CREDENTIAL_COLUMNS: &str = r#"
    environment_id, env_name, source_kind, grant_id, auth_provider_id,
    secret_id, created_at_ms, updated_at_ms
"#;

#[async_trait]
impl EnvironmentProviderStore for PgStore {
    async fn put_provider(
        &self,
        request: PutEnvironmentProvider,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin environment provider put", error))?;
        let query = format!(
            "SELECT {PROVIDER_COLUMNS} FROM environment_providers WHERE provider_id = $1 FOR UPDATE"
        );
        // A row that no longer decodes is replaced as if it were absent, so
        // an operator can repair it with a put that carries a credential.
        let existing = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(request.provider_id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| sql_error("lock environment provider", error))?
            .and_then(|row| decoded_or_logged(provider_from_row(&row), "environment provider"));
        let credential = request.credential.clone();
        let record = request.into_record(existing.as_ref())?;
        let features = record.controller_features;
        let row = match credential {
            Some(credential) => {
                let (key_id, nonce, ciphertext) =
                    self.seal_provider_credential(&record.provider_id, &credential)?;
                let query = format!(
                    r#"
                    INSERT INTO environment_providers (
                        provider_id, display_name, controller_endpoint, metadata_json,
                        controller_adopt_target, controller_ingress, data_route_base,
                        credential_key_id, credential_nonce, credential_ciphertext,
                        created_at_ms, updated_at_ms
                    ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
                    ON CONFLICT (provider_id) DO UPDATE SET
                        display_name = EXCLUDED.display_name,
                        controller_endpoint = EXCLUDED.controller_endpoint,
                        metadata_json = EXCLUDED.metadata_json,
                        controller_adopt_target = EXCLUDED.controller_adopt_target,
                        controller_ingress = EXCLUDED.controller_ingress,
                        data_route_base = EXCLUDED.data_route_base,
                        credential_key_id = EXCLUDED.credential_key_id,
                        credential_nonce = EXCLUDED.credential_nonce,
                        credential_ciphertext = EXCLUDED.credential_ciphertext,
                        created_at_ms = EXCLUDED.created_at_ms,
                        updated_at_ms = EXCLUDED.updated_at_ms
                    RETURNING {PROVIDER_COLUMNS}
                "#
                );
                sqlx::query(sqlx::AssertSqlSafe(query))
                    .bind(record.provider_id.as_str())
                    .bind(record.display_name.as_deref())
                    .bind(record.controller_endpoint.as_str())
                    .bind(json_value("encode provider metadata", &record.metadata)?)
                    .bind(features.map(|features| features.adopt_target))
                    .bind(features.map(|features| features.ingress))
                    .bind(record.data_route_base.as_ref().map(DataRouteBase::as_str))
                    .bind(key_id)
                    .bind(nonce)
                    .bind(ciphertext)
                    .bind(record.created_at_ms)
                    .bind(record.updated_at_ms)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|error| sql_error("put environment provider", error))?
            }
            None => {
                let query = format!(
                    r#"
                    UPDATE environment_providers SET
                        display_name = $2, controller_endpoint = $3, metadata_json = $4,
                        controller_adopt_target = $5, controller_ingress = $6,
                        data_route_base = $7, created_at_ms = $8, updated_at_ms = $9
                    WHERE provider_id = $1
                    RETURNING {PROVIDER_COLUMNS}
                "#
                );
                sqlx::query(sqlx::AssertSqlSafe(query))
                    .bind(record.provider_id.as_str())
                    .bind(record.display_name.as_deref())
                    .bind(record.controller_endpoint.as_str())
                    .bind(json_value("encode provider metadata", &record.metadata)?)
                    .bind(features.map(|features| features.adopt_target))
                    .bind(features.map(|features| features.ingress))
                    .bind(record.data_route_base.as_ref().map(DataRouteBase::as_str))
                    .bind(record.created_at_ms)
                    .bind(record.updated_at_ms)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|error| sql_error("update environment provider", error))?
                    .ok_or_else(|| {
                        invalid_error("a new environment provider requires a credential")
                    })?
            }
        };
        tx.commit()
            .await
            .map_err(|error| sql_error("commit environment provider put", error))?;
        provider_from_row(&row)
    }

    async fn read_provider(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        let query =
            format!("SELECT {PROVIDER_COLUMNS} FROM environment_providers WHERE provider_id = $1");
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(provider_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| sql_error("read environment provider", error))?
            .ok_or_else(|| not_found("environment_provider", provider_id))?;
        provider_from_row(&row)
    }

    async fn read_provider_credential(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<SecretValue, EnvironmentRegistryError> {
        let row = sqlx::query(
            "SELECT credential_key_id, credential_nonce, credential_ciphertext FROM environment_providers WHERE provider_id = $1",
        )
        .bind(provider_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| sql_error("read environment provider credential", error))?
        .ok_or_else(|| not_found("environment_provider", provider_id))?;
        let key_id: String = row
            .try_get("credential_key_id")
            .map_err(|error| sql_error("decode provider credential key id", error))?;
        if key_id != LOCAL_KEY_ID {
            return Err(store_message(format!(
                "unsupported provider credential key id '{key_id}'"
            )));
        }
        let nonce: Vec<u8> = row
            .try_get("credential_nonce")
            .map_err(|error| sql_error("decode provider credential nonce", error))?;
        let ciphertext: Vec<u8> = row
            .try_get("credential_ciphertext")
            .map_err(|error| sql_error("decode provider credential", error))?;
        let (cipher, _) = self
            .secrets_cipher()
            .map_err(|error| store_message(error.to_string()))?;
        open_secret(
            &cipher,
            &provider_credential_aad(provider_id),
            &nonce,
            &ciphertext,
        )
        .map_err(|error| store_message(format!("open provider credential: {error}")))
    }

    async fn record_provider_controller(
        &self,
        request: RecordProviderController,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        sqlx::query(
            r#"
            UPDATE environment_providers SET
                controller_adopt_target = $3, controller_ingress = $4, data_route_base = $5
            WHERE provider_id = $1 AND controller_endpoint = $2
            "#,
        )
        .bind(request.provider_id.as_str())
        .bind(request.controller_endpoint.as_str())
        .bind(request.features.adopt_target)
        .bind(request.features.ingress)
        .bind(request.data_route_base.as_str())
        .execute(&self.pool)
        .await
        .map_err(|error| sql_error("record environment provider controller", error))?;
        self.read_provider(&request.provider_id).await
    }

    async fn list_providers(
        &self,
        _request: ListEnvironmentProviders,
    ) -> Result<Vec<EnvironmentProviderRecord>, EnvironmentRegistryError> {
        let query =
            format!("SELECT {PROVIDER_COLUMNS} FROM environment_providers ORDER BY provider_id");
        let rows = sqlx::query(sqlx::AssertSqlSafe(query))
            .fetch_all(&self.pool)
            .await
            .map_err(|error| sql_error("list environment providers", error))?;
        Ok(decode_rows(
            &rows,
            provider_from_row,
            "environment provider",
        ))
    }

    async fn delete_provider(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        let bindings: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM environment_provider_bindings WHERE provider_id = $1",
        )
        .bind(provider_id.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(|error| sql_error("count environment provider bindings", error))?;
        if bindings != 0 {
            return invalid("environment provider is referenced by a universe binding");
        }
        let query = format!(
            "DELETE FROM environment_providers WHERE provider_id = $1 RETURNING {PROVIDER_COLUMNS}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(provider_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_provider_delete_error)?
            .ok_or_else(|| not_found("environment_provider", provider_id))?;
        provider_from_row(&row)
    }
}

impl PgStore {
    fn seal_provider_credential(
        &self,
        provider_id: &EnvironmentProviderId,
        credential: &SecretValue,
    ) -> Result<(&'static str, Vec<u8>, Vec<u8>), EnvironmentRegistryError> {
        let (cipher, key_id) = self
            .secrets_cipher()
            .map_err(|error| store_message(error.to_string()))?;
        let (nonce, ciphertext) =
            seal_secret(&cipher, &provider_credential_aad(provider_id), credential)
                .map_err(|error| store_message(format!("seal provider credential: {error}")))?;
        Ok((key_id, nonce, ciphertext))
    }
}

/// Binds a sealed provider credential to its provider, so ciphertexts
/// cannot be moved between provider rows.
fn provider_credential_aad(provider_id: &EnvironmentProviderId) -> Vec<u8> {
    format!("environment-provider/{}/credential", provider_id.as_str()).into_bytes()
}

#[async_trait]
impl EnvironmentProviderBindingStore for PgStore {
    async fn put_provider_binding(
        &self,
        request: PutEnvironmentProviderBinding,
    ) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError> {
        if request.updated_at_ms < 0 {
            return invalid("updated_at_ms must be nonnegative");
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin binding put", error))?;
        let existing = sqlx::query(
            "SELECT revision, provider_id FROM environment_provider_bindings WHERE universe_id = $1 AND binding_id = $2 FOR UPDATE"
        ).bind(request.universe_id).bind(request.binding_id.as_str()).fetch_optional(&mut *tx).await
            .map_err(|error| sql_error("read provider binding revision", error))?;
        let actual: Option<i64> = existing
            .as_ref()
            .map(|row| row.try_get("revision"))
            .transpose()
            .map_err(|error| sql_error("decode provider binding revision", error))?;
        if let Some(row) = &existing {
            let provider_id: String = row
                .try_get("provider_id")
                .map_err(|error| sql_error("decode provider binding provider", error))?;
            if provider_id != request.provider_id.as_str() {
                return invalid("provider_id is immutable for an existing binding");
            }
        }
        let actual_u64 = actual.map(|value| value as u64);
        if actual_u64 != request.expected_revision {
            return Err(EnvironmentRegistryError::RevisionConflict {
                kind: "environment_provider_binding",
                id: request.binding_id.to_string(),
                expected: request.expected_revision,
                actual: actual_u64,
            });
        }
        let revision = actual.unwrap_or(0) + 1;
        let query = format!(
            r#"
            INSERT INTO environment_provider_bindings (
                universe_id, binding_id, provider_id, status, revision,
                metadata_json, created_at_ms, updated_at_ms
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$7)
            ON CONFLICT (universe_id, binding_id) DO UPDATE SET
                status = EXCLUDED.status,
                metadata_json = EXCLUDED.metadata_json,
                revision = EXCLUDED.revision, updated_at_ms = EXCLUDED.updated_at_ms
            RETURNING {BINDING_COLUMNS}
        "#
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(request.universe_id)
            .bind(request.binding_id.as_str())
            .bind(request.provider_id.as_str())
            .bind(binding_status_to_str(request.status))
            .bind(revision)
            .bind(json_value(
                "encode provider binding metadata",
                &request.metadata,
            )?)
            .bind(request.updated_at_ms)
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| map_binding_write_error("put provider binding", error))?;
        tx.commit()
            .await
            .map_err(|error| sql_error("commit provider binding", error))?;
        binding_from_row(&row)
    }

    async fn read_provider_binding(
        &self,
        universe_id: Uuid,
        binding_id: &EnvironmentProviderBindingId,
    ) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {BINDING_COLUMNS} FROM environment_provider_bindings WHERE universe_id = $1 AND binding_id = $2"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(universe_id)
            .bind(binding_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| sql_error("read provider binding", error))?
            .ok_or_else(|| not_found("environment_provider_binding", binding_id))?;
        binding_from_row(&row)
    }

    async fn list_provider_bindings(
        &self,
        universe_id: Uuid,
    ) -> Result<Vec<EnvironmentProviderBindingRecord>, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {BINDING_COLUMNS} FROM environment_provider_bindings WHERE universe_id = $1 ORDER BY binding_id"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(universe_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| sql_error("list provider bindings", error))?;
        Ok(decode_rows(
            &rows,
            binding_from_row,
            "environment provider binding",
        ))
    }

    async fn delete_provider_binding(
        &self,
        universe_id: Uuid,
        binding_id: &EnvironmentProviderBindingId,
    ) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError> {
        let active: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM environments WHERE universe_id = $1 AND binding_id = $2 AND status <> 'closed'"
        ).bind(universe_id).bind(binding_id.as_str()).fetch_one(&self.pool).await
            .map_err(|error| sql_error("count binding environments", error))?;
        if active != 0 {
            return invalid("provider binding is referenced by a non-closed environment");
        }
        let query = format!(
            "DELETE FROM environment_provider_bindings WHERE universe_id = $1 AND binding_id = $2 RETURNING {BINDING_COLUMNS}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(universe_id)
            .bind(binding_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| sql_error("delete provider binding", error))?
            .ok_or_else(|| not_found("environment_provider_binding", binding_id))?;
        binding_from_row(&row)
    }
}

#[async_trait]
impl EnvironmentStore for PgStore {
    async fn create_environment(
        &self,
        request: CreateEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.ensure_universe()
            .await
            .map_err(|error| store_error("ensure universe", error))?;
        if request.created_at_ms < 0 {
            return invalid("environment timestamp must be nonnegative");
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin environment create", error))?;
        if let Some(existing_id) = sqlx::query_scalar::<_, String>(
            "SELECT environment_id FROM environments WHERE universe_id = $1 AND request_id = $2",
        )
        .bind(self.config.universe_id)
        .bind(request.request_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| sql_error("deduplicate environment create", error))?
        {
            tx.commit()
                .await
                .map_err(|error| sql_error("commit environment dedup", error))?;
            let environment_id = EnvironmentId::try_new(existing_id)
                .map_err(|error| store_message(format!("decode environment id: {error}")))?;
            return self.read_environment(&environment_id).await;
        }
        let query = format!(
            "SELECT {BINDING_COLUMNS} FROM environment_provider_bindings WHERE universe_id = $1 AND binding_id = $2 FOR UPDATE"
        );
        let binding_row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(request.binding_id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| sql_error("lock provider binding", error))?
            .ok_or_else(|| not_found("environment_provider_binding", &request.binding_id))?;
        let binding = binding_from_row(&binding_row)?;
        if binding.status != EnvironmentProviderBindingStatus::Enabled {
            return invalid("environment provider binding is disabled");
        }
        request.record(binding.provider_id.clone())?;
        sqlx::query(
            r#"
            INSERT INTO environments (
                universe_id, environment_id, request_id, source_kind, provider_id, binding_id,
                display_name, status, current_incarnation_id, metadata_json,
                idle_policy_json, created_at_ms, updated_at_ms
            ) VALUES ($1,$2,$3,'provisioned',$4,$5,$6,'provisioning',$7,$8,$9,$10,$10)
        "#,
        )
        .bind(self.config.universe_id)
        .bind(request.environment_id.as_str())
        .bind(request.request_id.as_str())
        .bind(binding.provider_id.as_str())
        .bind(request.binding_id.as_str())
        .bind(request.display_name.as_deref())
        .bind(request.incarnation_id.as_str())
        .bind(json_value(
            "encode environment metadata",
            &request.metadata,
        )?)
        .bind(
            request
                .idle_policy
                .as_ref()
                .map(|policy| json_value("encode idle policy", policy))
                .transpose()?,
        )
        .bind(request.created_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(map_environment_insert_error)?;
        sqlx::query(
            r#"
            INSERT INTO environment_incarnations (
                universe_id, environment_id, incarnation_id, provision_request_id,
                template_id, created_at_ms, updated_at_ms
            ) VALUES ($1,$2,$3,$4,$5,$6,$6)
        "#,
        )
        .bind(self.config.universe_id)
        .bind(request.environment_id.as_str())
        .bind(request.incarnation_id.as_str())
        .bind(request.request_id.as_str())
        .bind(request.template_id.as_str())
        .bind(request.created_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(|error| sql_error("insert environment incarnation", error))?;
        tx.commit()
            .await
            .map_err(|error| sql_error("commit environment create", error))?;
        self.read_environment(&request.environment_id).await
    }

    async fn adopt_environment(
        &self,
        request: AdoptEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.ensure_universe()
            .await
            .map_err(|error| store_error("ensure universe", error))?;
        if request.created_at_ms < 0 {
            return invalid("environment timestamp must be nonnegative");
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin environment adoption", error))?;
        if let Some(existing_id) = sqlx::query_scalar::<_, String>(
            "SELECT environment_id FROM environments WHERE universe_id = $1 AND request_id = $2",
        )
        .bind(self.config.universe_id)
        .bind(request.request_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| sql_error("deduplicate environment adoption", error))?
        {
            tx.commit()
                .await
                .map_err(|error| sql_error("commit environment adoption dedup", error))?;
            let environment_id = EnvironmentId::try_new(existing_id)
                .map_err(|error| store_message(format!("decode environment id: {error}")))?;
            return self.read_environment(&environment_id).await;
        }
        let query = format!(
            "SELECT {BINDING_COLUMNS} FROM environment_provider_bindings WHERE universe_id = $1 AND binding_id = $2 FOR UPDATE"
        );
        let binding_row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(request.binding_id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| sql_error("lock provider binding for adoption", error))?
            .ok_or_else(|| not_found("environment_provider_binding", &request.binding_id))?;
        let binding = binding_from_row(&binding_row)?;
        if binding.status != EnvironmentProviderBindingStatus::Enabled {
            return invalid("environment provider binding is disabled");
        }
        request.record(binding.provider_id.clone())?;
        sqlx::query(
            r#"
            INSERT INTO environments (
                universe_id, environment_id, request_id, source_kind, provider_id, binding_id,
                display_name, status, current_incarnation_id, metadata_json,
                created_at_ms, updated_at_ms
            ) VALUES ($1,$2,$3,'provisioned',$4,$5,$6,'provisioning',$7,$8,$9,$9)
        "#,
        )
        .bind(self.config.universe_id)
        .bind(request.environment_id.as_str())
        .bind(request.request_id.as_str())
        .bind(binding.provider_id.as_str())
        .bind(request.binding_id.as_str())
        .bind(request.display_name.as_deref())
        .bind(request.incarnation_id.as_str())
        .bind(json_value(
            "encode environment metadata",
            &request.metadata,
        )?)
        .bind(request.created_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(map_environment_insert_error)?;
        sqlx::query(
            r#"
            INSERT INTO environment_incarnations (
                universe_id, environment_id, incarnation_id, provision_request_id,
                adoption_source_target, created_at_ms, updated_at_ms
            ) VALUES ($1,$2,$3,$4,$5,$6,$6)
        "#,
        )
        .bind(self.config.universe_id)
        .bind(request.environment_id.as_str())
        .bind(request.incarnation_id.as_str())
        .bind(request.request_id.as_str())
        .bind(&request.source_target)
        .bind(request.created_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(|error| sql_error("insert adopted environment incarnation", error))?;
        tx.commit()
            .await
            .map_err(|error| sql_error("commit environment adoption", error))?;
        self.read_environment(&request.environment_id).await
    }

    async fn create_external_environment(
        &self,
        request: CreateExternalEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.ensure_universe()
            .await
            .map_err(|error| store_error("ensure universe", error))?;
        if request.created_at_ms < 0 {
            return invalid("environment timestamp must be nonnegative");
        }
        request.record()?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin external environment create", error))?;
        if let Some(existing_id) = sqlx::query_scalar::<_, String>(
            "SELECT environment_id FROM environments WHERE universe_id = $1 AND request_id = $2",
        )
        .bind(self.config.universe_id)
        .bind(request.request_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| sql_error("deduplicate external environment create", error))?
        {
            tx.commit()
                .await
                .map_err(|error| sql_error("commit external environment dedup", error))?;
            let environment_id = EnvironmentId::try_new(existing_id)
                .map_err(|error| store_message(format!("decode environment id: {error}")))?;
            return self.read_environment(&environment_id).await;
        }
        sqlx::query(
            r#"
            INSERT INTO environments (
                universe_id, environment_id, request_id, source_kind, daemon_endpoint,
                display_name, status, current_incarnation_id, metadata_json,
                created_at_ms, updated_at_ms
            ) VALUES ($1,$2,$3,'external',$4,$5,'ready',$6,$7,$8,$8)
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.environment_id.as_str())
        .bind(request.request_id.as_str())
        .bind(request.endpoint.as_str())
        .bind(request.display_name.as_deref())
        .bind(request.incarnation_id.as_str())
        .bind(json_value(
            "encode external environment metadata",
            &request.metadata,
        )?)
        .bind(request.created_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(map_environment_insert_error)?;
        sqlx::query(
            r#"
            INSERT INTO environment_incarnations (
                universe_id, environment_id, incarnation_id, created_at_ms, updated_at_ms
            ) VALUES ($1,$2,$3,$4,$4)
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.environment_id.as_str())
        .bind(request.incarnation_id.as_str())
        .bind(request.created_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(|error| sql_error("insert external environment incarnation", error))?;
        tx.commit()
            .await
            .map_err(|error| sql_error("commit external environment create", error))?;
        self.read_environment(&request.environment_id).await
    }

    async fn create_registered_environment(
        &self,
        request: CreateRegisteredEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.ensure_universe()
            .await
            .map_err(|error| store_error("ensure universe", error))?;
        if request.created_at_ms < 0 {
            return invalid("environment timestamp must be nonnegative");
        }
        let daemon_id = request.daemon_id()?;
        let request_id = EnvironmentProvisionRequestId::for_daemon(&daemon_id);
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin registered environment create", error))?;
        if let Some(existing_id) = sqlx::query_scalar::<_, String>(
            "SELECT environment_id FROM environments WHERE universe_id = $1 AND request_id = $2",
        )
        .bind(self.config.universe_id)
        .bind(request_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| sql_error("deduplicate registered environment create", error))?
        {
            tx.commit()
                .await
                .map_err(|error| sql_error("commit registered environment dedup", error))?;
            let environment_id = EnvironmentId::try_new(existing_id)
                .map_err(|error| store_message(format!("decode environment id: {error}")))?;
            return self.read_environment(&environment_id).await;
        }
        // Lock the key row: concurrent registrations against one key
        // serialize here, so the count below is exact.
        let key = crate::environment_registration::lock_registration_key(
            &mut tx,
            self.config.universe_id,
            &request.registration_key_id,
        )
        .await?;
        key.check_admits(request.created_at_ms)?;
        if let Some(limit) = key.max_active_environments {
            let active: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM environments WHERE universe_id = $1 AND registration_key_id = $2 AND status <> 'closed'",
            )
            .bind(self.config.universe_id)
            .bind(key.registration_key_id.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| sql_error("count registered environments", error))?;
            if active >= i64::from(limit) {
                return Err(EnvironmentRegistryError::RegistrationCapacityExhausted {
                    registration_key_id: key.registration_key_id.to_string(),
                    limit,
                });
            }
        }
        request.record(&key)?;
        sqlx::query(
            r#"
            INSERT INTO environments (
                universe_id, environment_id, request_id, source_kind,
                registration_key_id, daemon_id, daemon_public_key, identity_mode, last_seen_at_ms,
                display_name, status, current_incarnation_id, metadata_json,
                created_at_ms, updated_at_ms
            ) VALUES ($1,$2,$3,'registered',$4,$5,$6,$7,$8,$9,'ready',$10,$11,$8,$8)
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.environment_id.as_str())
        .bind(request_id.as_str())
        .bind(key.registration_key_id.as_str())
        .bind(daemon_id.as_str())
        .bind(&request.daemon_public_key)
        .bind(key.identity_mode.as_str())
        .bind(request.created_at_ms)
        .bind(request.display_name.as_deref())
        .bind(request.incarnation_id.as_str())
        .bind(json_value(
            "encode registered environment metadata",
            &request.metadata,
        )?)
        .execute(&mut *tx)
        .await
        .map_err(|error| map_registered_insert_error(error, &daemon_id))?;
        sqlx::query(
            r#"
            INSERT INTO environment_incarnations (
                universe_id, environment_id, incarnation_id, created_at_ms, updated_at_ms
            ) VALUES ($1,$2,$3,$4,$4)
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.environment_id.as_str())
        .bind(request.incarnation_id.as_str())
        .bind(request.created_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(|error| sql_error("insert registered environment incarnation", error))?;
        tx.commit()
            .await
            .map_err(|error| sql_error("commit registered environment create", error))?;
        self.read_environment(&request.environment_id).await
    }

    async fn read_environment_by_daemon_public_key(
        &self,
        daemon_public_key: &str,
    ) -> Result<Option<EnvironmentRecord>, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {ENVIRONMENT_COLUMNS} {ENVIRONMENT_JOIN} WHERE e.universe_id = $1 AND e.daemon_public_key = $2"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(daemon_public_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| sql_error("read environment by daemon key", error))?;
        row.as_ref().map(environment_from_row).transpose()
    }

    async fn observe_registered_environment(
        &self,
        request: ObserveRegisteredEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        if request.observed_at_ms < 0 {
            return invalid("observed_at_ms must be nonnegative");
        }
        let current = self.read_environment(&request.environment_id).await?;
        // Validate the merged result before anything becomes durable: a
        // build fact that breaks the record's bounds is refused here rather
        // than poisoning the row for every later read.
        let mut candidate = current.clone();
        apply_registered_observation(
            &mut candidate,
            request.observation,
            request.observed_at_ms,
            request.metadata.clone(),
        )?;
        candidate.validate()?;
        // One conditional statement rather than read-modify-write: a close
        // that lands between a read and a write must never be overwritten
        // by a late connect or disconnect observation.
        let observation = match request.observation {
            RegisteredConnectionObservation::Connected => "connected",
            RegisteredConnectionObservation::Heartbeat => "heartbeat",
            RegisteredConnectionObservation::Disconnected => "disconnected",
        };
        sqlx::query(
            r#"
            UPDATE environments SET
                status = CASE
                    WHEN status IN ('closing', 'closed') THEN status
                    WHEN $3 = 'connected' THEN 'ready'
                    WHEN $3 = 'disconnected' THEN 'offline'
                    ELSE status END,
                last_seen_at_ms = CASE
                    WHEN status IN ('closing', 'closed') THEN last_seen_at_ms
                    WHEN $3 IN ('connected', 'heartbeat') THEN $4
                    ELSE last_seen_at_ms END,
                metadata_json = CASE
                    WHEN status IN ('closing', 'closed') THEN metadata_json
                    ELSE metadata_json || $5::jsonb END,
                updated_at_ms = CASE
                    WHEN status IN ('closing', 'closed') THEN updated_at_ms
                    ELSE GREATEST(updated_at_ms, $4) END
            WHERE universe_id = $1 AND environment_id = $2 AND source_kind = 'registered'
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.environment_id.as_str())
        .bind(observation)
        .bind(request.observed_at_ms)
        .bind(json_value(
            "encode daemon build metadata",
            &request.metadata,
        )?)
        .execute(&self.pool)
        .await
        .map_err(|error| sql_error("observe registered environment", error))?;
        self.read_environment(&request.environment_id).await
    }

    async fn list_open_registered_environments(
        &self,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {ENVIRONMENT_COLUMNS} {ENVIRONMENT_JOIN} WHERE e.universe_id = $1 AND e.source_kind = 'registered' AND e.status NOT IN ('closing','closed') ORDER BY e.environment_id"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| sql_error("list open registered environments", error))?;
        Ok(decode_rows(&rows, environment_from_row, "environment"))
    }

    async fn read_environment(
        &self,
        environment_id: &EnvironmentId,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        read_environment_on(
            &self.pool,
            self.config.universe_id,
            "e.environment_id = $2",
            environment_id.as_str(),
            "read environment",
        )
        .await
    }

    async fn read_environment_by_request_id(
        &self,
        request_id: &EnvironmentProvisionRequestId,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        read_environment_on(
            &self.pool,
            self.config.universe_id,
            "e.request_id = $2",
            request_id.as_str(),
            "read environment request",
        )
        .await
    }

    async fn list_environments(
        &self,
        request: ListEnvironments,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError> {
        let mut query =
            format!("SELECT {ENVIRONMENT_COLUMNS} {ENVIRONMENT_JOIN} WHERE e.universe_id = $1");
        let mut next = 2;
        if request.provider_id.is_some() {
            query.push_str(&format!(" AND e.provider_id = ${next}"));
            next += 1;
        }
        if request.binding_id.is_some() {
            query.push_str(&format!(" AND e.binding_id = ${next}"));
            next += 1;
        }
        if request.status.is_some() {
            query.push_str(&format!(" AND e.status = ${next}"));
            next += 1;
        }
        if request.registration_key_id.is_some() {
            query.push_str(&format!(" AND e.registration_key_id = ${next}"));
            next += 1;
        }
        // Appended only when present so containment can use the GIN index.
        if !request.metadata.is_empty() {
            query.push_str(&format!(" AND e.metadata_json @> ${next}"));
        }
        query.push_str(" ORDER BY e.environment_id");
        let mut sql = sqlx::query(sqlx::AssertSqlSafe(query)).bind(self.config.universe_id);
        if let Some(id) = request.provider_id {
            sql = sql.bind(id.to_string());
        }
        if let Some(id) = request.binding_id {
            sql = sql.bind(id.to_string());
        }
        if let Some(status) = request.status {
            sql = sql.bind(environment_status_to_str(status));
        }
        if let Some(id) = request.registration_key_id {
            sql = sql.bind(id.to_string());
        }
        if !request.metadata.is_empty() {
            sql = sql.bind(metadata_filter_json(&request.metadata));
        }
        let rows = sql
            .fetch_all(&self.pool)
            .await
            .map_err(|error| sql_error("list environments", error))?;
        Ok(decode_rows(&rows, environment_from_row, "environment"))
    }

    async fn list_environments_needing_reconcile(
        &self,
        scope: ReconcileScope,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError> {
        let query = match scope {
            ReconcileScope::Converging => format!(
                "SELECT {ENVIRONMENT_COLUMNS} {ENVIRONMENT_JOIN} WHERE e.universe_id = $1 AND {CONVERGING_SQL} ORDER BY e.updated_at_ms, e.environment_id"
            ),
            ReconcileScope::WithSteady => format!(
                "SELECT {ENVIRONMENT_COLUMNS} {ENVIRONMENT_JOIN} WHERE e.universe_id = $1 AND ({CONVERGING_SQL} OR ({STEADY_SQL} AND i.provider_target_id IS NOT NULL)) ORDER BY e.updated_at_ms, e.environment_id"
            ),
        };
        let rows = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| sql_error("list environments needing reconcile", error))?;
        Ok(decode_rows(&rows, environment_from_row, "environment"))
    }

    async fn list_environments_with_idle_policy(
        &self,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {ENVIRONMENT_COLUMNS} {ENVIRONMENT_JOIN} WHERE e.universe_id = $1 AND e.status = 'ready' AND e.idle_policy_json IS NOT NULL ORDER BY e.updated_at_ms, e.environment_id"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| sql_error("list environments with idle policy", error))?;
        Ok(decode_rows(&rows, environment_from_row, "environment"))
    }

    async fn observe_provisioned_environment(
        &self,
        request: ObserveProvisionedEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let current = self.read_environment(&request.environment_id).await?;
        let mut candidate = current.clone();
        request.apply(&mut candidate)?;
        candidate.validate()?;
        if candidate == current {
            return Ok(current);
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin environment observation", error))?;
        // The target id and power states are recorded whatever the status,
        // so a close that raced the first create can still address the
        // target; the predicates refuse stale and conflicting observations
        // that raced this one.
        sqlx::query(
            r#"
            UPDATE environment_incarnations SET
                provider_target_id = COALESCE($4, provider_target_id),
                power_states_json = $5,
                updated_at_ms = $6
            WHERE universe_id = $1 AND environment_id = $2 AND incarnation_id = $3
              AND updated_at_ms <= $6
              AND ($4::text IS NULL OR provider_target_id IS NULL OR provider_target_id = $4)
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.environment_id.as_str())
        .bind(current.incarnation.incarnation_id.as_str())
        .bind(
            request
                .provider_target_id
                .as_ref()
                .map(ProviderTargetId::as_str),
        )
        .bind(json_value("encode power states", &request.power_states)?)
        .bind(request.observed_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(|error| sql_error("observe environment incarnation", error))?;
        // A close accepted while this observation was in flight wins.
        sqlx::query(
            r#"
            UPDATE environments SET
                status = $3,
                metadata_json = metadata_json || $4,
                lifecycle_error = CASE WHEN $5 THEN NULL ELSE lifecycle_error END,
                lifecycle_error_first_at_ms =
                    CASE WHEN $5 THEN NULL ELSE lifecycle_error_first_at_ms END,
                lifecycle_error_last_at_ms =
                    CASE WHEN $5 THEN NULL ELSE lifecycle_error_last_at_ms END,
                updated_at_ms = GREATEST(updated_at_ms, $6)
            WHERE universe_id = $1 AND environment_id = $2
              AND status NOT IN ('closing', 'closed')
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.environment_id.as_str())
        .bind(environment_status_to_str(request.status))
        .bind(json_value(
            "encode daemon build metadata",
            &request
                .daemon_build
                .as_ref()
                .map(|build| build.metadata())
                .unwrap_or_default(),
        )?)
        .bind(request.status.power_state().is_some())
        .bind(request.observed_at_ms)
        .execute(&mut *tx)
        .await
        .map_err(|error| sql_error("observe environment", error))?;
        tx.commit()
            .await
            .map_err(|error| sql_error("commit environment observation", error))?;
        self.read_environment(&request.environment_id).await
    }

    async fn fail_environment_lifecycle(
        &self,
        request: FailEnvironmentLifecycle,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let current = self.read_environment(&request.environment_id).await?;
        let mut candidate = current.clone();
        request.apply(&mut candidate)?;
        candidate.validate()?;
        let Some(error) = candidate
            .lifecycle_error
            .as_ref()
            .filter(|_| candidate != current)
        else {
            return Ok(current);
        };
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin environment failure", error))?;
        let failed = sqlx::query(
            r#"
            UPDATE environments SET
                status = 'failed',
                lifecycle_error = $3,
                lifecycle_error_first_at_ms = LEAST(COALESCE(lifecycle_error_first_at_ms, $4), $4),
                lifecycle_error_last_at_ms = $4,
                metadata_json = metadata_json || $5,
                updated_at_ms = GREATEST(updated_at_ms, $4)
            WHERE universe_id = $1 AND environment_id = $2
              AND status NOT IN ('closing', 'closed')
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.environment_id.as_str())
        .bind(&error.message)
        .bind(request.observed_at_ms)
        .bind(json_value(
            "encode daemon build metadata",
            &request
                .daemon_build
                .as_ref()
                .map(|build| build.metadata())
                .unwrap_or_default(),
        )?)
        .execute(&mut *tx)
        .await
        .map_err(|error| sql_error("fail environment", error))?;
        if failed.rows_affected() == 1 {
            // Observations older than the failure are stale from now on.
            sqlx::query(
                "UPDATE environment_incarnations SET updated_at_ms = GREATEST(updated_at_ms, $4) WHERE universe_id = $1 AND environment_id = $2 AND incarnation_id = $3",
            )
            .bind(self.config.universe_id)
            .bind(request.environment_id.as_str())
            .bind(current.incarnation.incarnation_id.as_str())
            .bind(request.observed_at_ms)
            .execute(&mut *tx)
            .await
            .map_err(|error| sql_error("fail environment incarnation", error))?;
        }
        tx.commit()
            .await
            .map_err(|error| sql_error("commit environment failure", error))?;
        self.read_environment(&request.environment_id).await
    }

    async fn record_environment_lifecycle_error(
        &self,
        request: RecordEnvironmentLifecycleError,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let current = self.read_environment(&request.environment_id).await?;
        let mut candidate = current.clone();
        request.apply(&mut candidate)?;
        candidate.validate()?;
        let Some(error) = candidate
            .lifecycle_error
            .as_ref()
            .filter(|_| candidate != current)
        else {
            return Ok(current);
        };
        sqlx::query(
            r#"
            UPDATE environments SET
                lifecycle_error = $3,
                lifecycle_error_first_at_ms = LEAST(COALESCE(lifecycle_error_first_at_ms, $4), $4),
                lifecycle_error_last_at_ms = $4,
                updated_at_ms = GREATEST(updated_at_ms, $4)
            WHERE universe_id = $1 AND environment_id = $2 AND status <> 'closed'
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.environment_id.as_str())
        .bind(&error.message)
        .bind(request.observed_at_ms)
        .execute(&self.pool)
        .await
        .map_err(|error| sql_error("record environment lifecycle error", error))?;
        self.read_environment(&request.environment_id).await
    }

    async fn begin_close_environment(
        &self,
        request: BeginCloseEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        sqlx::query("UPDATE environments SET status=CASE WHEN status='closed' THEN status ELSE 'closing' END, updated_at_ms=GREATEST(updated_at_ms,$3) WHERE universe_id=$1 AND environment_id=$2")
            .bind(self.config.universe_id).bind(request.environment_id.as_str()).bind(request.updated_at_ms)
            .execute(&self.pool).await.map_err(|error| sql_error("begin environment close", error))?;
        self.read_environment(&request.environment_id).await
    }

    async fn finish_close_environment(
        &self,
        request: FinishCloseEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let current = self.read_environment(&request.environment_id).await?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin finish close", error))?;
        sqlx::query("UPDATE environment_incarnations SET updated_at_ms=$4 WHERE universe_id=$1 AND environment_id=$2 AND incarnation_id=$3")
            .bind(self.config.universe_id).bind(request.environment_id.as_str()).bind(current.incarnation.incarnation_id.as_str()).bind(request.observed_at_ms)
            .execute(&mut *tx).await.map_err(|error| sql_error("close environment incarnation", error))?;
        sqlx::query("UPDATE environments SET status='closed', public_ingress_enabled=false, public_endpoint=NULL, updated_at_ms=$3 WHERE universe_id=$1 AND environment_id=$2")
            .bind(self.config.universe_id).bind(request.environment_id.as_str()).bind(request.observed_at_ms)
            .execute(&mut *tx).await.map_err(|error| sql_error("finish environment close", error))?;
        tx.commit()
            .await
            .map_err(|error| sql_error("commit environment close", error))?;
        self.read_environment(&request.environment_id).await
    }

    async fn set_environment_ingress(
        &self,
        request: SetEnvironmentIngress,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        validate_ingress_request_time(request.updated_at_ms)?;
        let environment = self.read_environment(&request.environment_id).await?;
        if !matches!(environment.source, EnvironmentSource::Provisioned { .. }) {
            return invalid_store("provider-managed ingress requires a provisioned environment");
        }
        if request.enabled && environment.status.is_closing_or_closed() {
            return invalid_store("cannot enable ingress for a closing environment");
        }
        EnvironmentRecord {
            public_ingress_enabled: request.enabled,
            ..environment
        }
        .validate()?;
        // Enabling never revives a close accepted in the meantime.
        sqlx::query("UPDATE environments SET public_ingress_enabled=$3, updated_at_ms=GREATEST(updated_at_ms,$4) WHERE universe_id=$1 AND environment_id=$2 AND (NOT $3 OR status NOT IN ('closing','closed'))")
            .bind(self.config.universe_id)
            .bind(request.environment_id.as_str())
            .bind(request.enabled)
            .bind(request.updated_at_ms)
            .execute(&self.pool).await.map_err(|error| sql_error("set environment ingress", error))?;
        self.read_environment(&request.environment_id).await
    }

    async fn record_environment_ingress(
        &self,
        request: RecordEnvironmentIngress,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let current = self.read_environment(&request.environment_id).await?;
        let mut candidate = current.clone();
        request.apply(&mut candidate)?;
        candidate.validate()?;
        if candidate == current {
            return Ok(current);
        }
        // A close accepted while this observation was in flight wins.
        sqlx::query("UPDATE environments SET public_endpoint=$3, updated_at_ms=GREATEST(updated_at_ms,$4) WHERE universe_id=$1 AND environment_id=$2 AND status NOT IN ('closing','closed')")
            .bind(self.config.universe_id)
            .bind(request.environment_id.as_str())
            .bind(request.public_endpoint)
            .bind(request.observed_at_ms)
            .execute(&self.pool).await.map_err(|error| sql_error("record environment ingress", error))?;
        self.read_environment(&request.environment_id).await
    }

    async fn set_environment_power(
        &self,
        request: SetEnvironmentPower,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        validate_ingress_request_time(request.updated_at_ms)?;
        let environment = self.read_environment(&request.environment_id).await?;
        check_power_mutable(&environment)?;
        EnvironmentRecord {
            desired_power: request.desired_power,
            ..environment
        }
        .validate()?;
        sqlx::query("UPDATE environments SET desired_power=$3, updated_at_ms=GREATEST(updated_at_ms,$4) WHERE universe_id=$1 AND environment_id=$2")
            .bind(self.config.universe_id)
            .bind(request.environment_id.as_str())
            .bind(request.desired_power.as_str())
            .bind(request.updated_at_ms)
            .execute(&self.pool).await.map_err(|error| sql_error("set environment power", error))?;
        self.read_environment(&request.environment_id).await
    }

    async fn set_environment_idle_policy(
        &self,
        request: SetEnvironmentIdlePolicy,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        validate_ingress_request_time(request.updated_at_ms)?;
        if let Some(policy) = &request.idle_policy {
            policy.validate()?;
        }
        let environment = self.read_environment(&request.environment_id).await?;
        check_power_mutable(&environment)?;
        EnvironmentRecord {
            idle_policy: request.idle_policy.clone(),
            ..environment
        }
        .validate()?;
        sqlx::query("UPDATE environments SET idle_policy_json=$3, updated_at_ms=GREATEST(updated_at_ms,$4) WHERE universe_id=$1 AND environment_id=$2")
            .bind(self.config.universe_id)
            .bind(request.environment_id.as_str())
            .bind(
                request
                    .idle_policy
                    .as_ref()
                    .map(|policy| json_value("encode idle policy", policy))
                    .transpose()?,
            )
            .bind(request.updated_at_ms)
            .execute(&self.pool).await.map_err(|error| sql_error("set environment idle policy", error))?;
        self.read_environment(&request.environment_id).await
    }
}

fn check_power_mutable(record: &EnvironmentRecord) -> Result<(), EnvironmentRegistryError> {
    if !matches!(record.source, EnvironmentSource::Provisioned { .. }) {
        return invalid_store("only provisioned environments have power control");
    }
    if record.status.is_closing_or_closed() || record.status == EnvironmentStatus::Failed {
        return invalid_store("cannot change power of a closing, closed, or failed environment");
    }
    Ok(())
}

#[async_trait]
impl EnvironmentCredentialStore for PgStore {
    async fn bind_credential(
        &self,
        request: PutEnvironmentCredential,
    ) -> Result<EnvironmentCredentialRecord, EnvironmentRegistryError> {
        let record = request.into_record();
        record.validate()?;
        let (kind, grant, provider, secret) = credential_source_columns(&record.source);
        let query = format!(
            r#"
            INSERT INTO environment_credentials (universe_id, environment_id, env_name, source_kind, grant_id, auth_provider_id, secret_id, created_at_ms, updated_at_ms)
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$8)
            ON CONFLICT (universe_id, environment_id, env_name) DO UPDATE SET
                source_kind=EXCLUDED.source_kind, grant_id=EXCLUDED.grant_id,
                auth_provider_id=EXCLUDED.auth_provider_id, secret_id=EXCLUDED.secret_id,
                updated_at_ms=EXCLUDED.updated_at_ms RETURNING {CREDENTIAL_COLUMNS}
        "#
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(record.environment_id.as_str())
            .bind(&record.env_name)
            .bind(kind)
            .bind(grant)
            .bind(provider)
            .bind(secret)
            .bind(record.created_at_ms)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| sql_error("bind environment credential", error))?;
        credential_from_row(&row)
    }

    async fn list_credentials(
        &self,
        request: ListEnvironmentCredentials,
    ) -> Result<Vec<EnvironmentCredentialRecord>, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {CREDENTIAL_COLUMNS} FROM environment_credentials WHERE universe_id=$1 AND environment_id=$2 ORDER BY env_name"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(request.environment_id.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|error| sql_error("list environment credentials", error))?;
        Ok(decode_rows(
            &rows,
            credential_from_row,
            "environment credential",
        ))
    }

    async fn unbind_credential(
        &self,
        environment_id: &EnvironmentId,
        env_name: &str,
    ) -> Result<EnvironmentCredentialRecord, EnvironmentRegistryError> {
        let query = format!(
            "DELETE FROM environment_credentials WHERE universe_id=$1 AND environment_id=$2 AND env_name=$3 RETURNING {CREDENTIAL_COLUMNS}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(environment_id.as_str())
            .bind(env_name)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| sql_error("unbind environment credential", error))?
            .ok_or_else(|| {
                not_found(
                    "environment_credential",
                    &format!("{environment_id}/{env_name}"),
                )
            })?;
        credential_from_row(&row)
    }
}

async fn read_environment_on(
    pool: &sqlx::PgPool,
    universe_id: Uuid,
    predicate: &'static str,
    value: &str,
    action: &str,
) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
    let query = format!(
        "SELECT {ENVIRONMENT_COLUMNS} {ENVIRONMENT_JOIN} WHERE e.universe_id = $1 AND {predicate}"
    );
    let row = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind(universe_id)
        .bind(value)
        .fetch_optional(pool)
        .await
        .map_err(|error| sql_error(action, error))?
        .ok_or_else(|| not_found("environment", &value))?;
    environment_from_row(&row)
}

fn provider_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
    let provider_id = parse_id(row, "provider_id", EnvironmentProviderId::try_new)?;
    let adopt_target: Option<bool> = row
        .try_get("controller_adopt_target")
        .map_err(|e| sql_error("decode provider controller adoptTarget", e))?;
    let ingress: Option<bool> = row
        .try_get("controller_ingress")
        .map_err(|e| sql_error("decode provider controller ingress", e))?;
    let controller_features = match (adopt_target, ingress) {
        (Some(adopt_target), Some(ingress)) => Some(ControllerFeatures {
            adopt_target,
            ingress,
        }),
        (None, None) => None,
        _ => return Err(store_message("provider controller features are incomplete")),
    };
    let record = EnvironmentProviderRecord {
        provider_id,
        display_name: row
            .try_get("display_name")
            .map_err(|e| sql_error("decode provider display_name", e))?,
        controller_endpoint: parse_id(row, "controller_endpoint", |value| {
            EnvironmentEndpoint::parse(&value)
        })?,
        metadata: json_column(row, "metadata_json")?,
        controller_features,
        data_route_base: optional_id(row, "data_route_base", DataRouteBase::new)?,
        created_at_ms: scalar(row, "created_at_ms")?,
        updated_at_ms: scalar(row, "updated_at_ms")?,
    };
    record.validate()?;
    Ok(record)
}

fn binding_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError> {
    let record = EnvironmentProviderBindingRecord {
        universe_id: row
            .try_get("universe_id")
            .map_err(|e| sql_error("decode binding universe", e))?,
        binding_id: parse_id(row, "binding_id", EnvironmentProviderBindingId::try_new)?,
        provider_id: parse_id(row, "provider_id", EnvironmentProviderId::try_new)?,
        status: binding_status_from_str(&column(row, "status")?)?,
        revision: i64_to_u64(scalar(row, "revision")?, "revision")?,
        metadata: json_column(row, "metadata_json")?,
        created_at_ms: scalar(row, "created_at_ms")?,
        updated_at_ms: scalar(row, "updated_at_ms")?,
    };
    record.validate()?;
    Ok(record)
}

fn environment_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
    let source_kind = column(row, "source_kind")?;
    let provider_id: Option<String> = row
        .try_get("provider_id")
        .map_err(|e| sql_error("decode environment provider", e))?;
    let binding_id: Option<String> = row
        .try_get("binding_id")
        .map_err(|e| sql_error("decode environment binding", e))?;
    let source = match source_kind.as_str() {
        "provisioned" => EnvironmentSource::Provisioned {
            provider_id: EnvironmentProviderId::try_new(
                provider_id.ok_or_else(|| store_message("missing provider id"))?,
            )
            .map_err(|e| store_message(format!("decode provider id: {e}")))?,
            binding_id: EnvironmentProviderBindingId::try_new(
                binding_id.ok_or_else(|| store_message("missing binding id"))?,
            )
            .map_err(|e| store_message(format!("decode binding id: {e}")))?,
        },
        "external" => EnvironmentSource::External {
            endpoint: parse_id(row, "daemon_endpoint", |value| {
                EnvironmentEndpoint::parse(&value)
            })?,
        },
        "registered" => EnvironmentSource::Registered {
            registration_key_id: parse_id(
                row,
                "registration_key_id",
                EnvironmentRegistrationKeyId::try_new,
            )?,
            daemon_id: parse_id(row, "daemon_id", EnvironmentDaemonId::try_new)?,
            daemon_public_key: column(row, "daemon_public_key")?,
            identity_mode: identity_mode_from_str(&column(row, "identity_mode")?)?,
        },
        other => {
            return Err(store_message(format!(
                "unknown environment source: {other}"
            )));
        }
    };
    let lifecycle_error = match (
        row.try_get::<Option<String>, _>("lifecycle_error")
            .map_err(|e| sql_error("decode lifecycle error", e))?,
        row.try_get::<Option<i64>, _>("lifecycle_error_first_at_ms")
            .map_err(|e| sql_error("decode lifecycle error start", e))?,
        row.try_get::<Option<i64>, _>("lifecycle_error_last_at_ms")
            .map_err(|e| sql_error("decode lifecycle error time", e))?,
    ) {
        (Some(message), Some(first_at_ms), Some(last_at_ms)) => Some(EnvironmentLifecycleError {
            message,
            first_at_ms,
            last_at_ms,
        }),
        (None, None, None) => None,
        _ => return Err(store_message("environment lifecycle error is incomplete")),
    };
    let record = EnvironmentRecord {
        environment_id: parse_id(row, "environment_id", EnvironmentId::try_new)?,
        request_id: parse_id(row, "request_id", EnvironmentProvisionRequestId::try_new)?,
        source,
        display_name: row
            .try_get("display_name")
            .map_err(|e| sql_error("decode environment display", e))?,
        status: environment_status_from_str(&column(row, "status")?)?,
        desired_power: power_state_from_str(&column(row, "desired_power")?)?,
        idle_policy: optional_json_column(row, "idle_policy_json")?,
        incarnation: EnvironmentIncarnationRecord {
            incarnation_id: parse_id(row, "incarnation_id", EnvironmentIncarnationId::try_new)?,
            provision_request_id: optional_id(
                row,
                "provision_request_id",
                EnvironmentProvisionRequestId::try_new,
            )?,
            provider_target_id: optional_id(row, "provider_target_id", ProviderTargetId::new)?,
            template_id: optional_id(row, "template_id", EnvironmentTemplateId::try_new)?,
            adoption_source_target: row
                .try_get("adoption_source_target")
                .map_err(|e| sql_error("decode adoption source target", e))?,
            power_states: json_column(row, "power_states_json")?,
            created_at_ms: scalar(row, "incarnation_created_at_ms")?,
            updated_at_ms: scalar(row, "incarnation_updated_at_ms")?,
        },
        public_ingress_enabled: row
            .try_get("public_ingress_enabled")
            .map_err(|e| sql_error("decode public ingress enabled", e))?,
        public_endpoint: row
            .try_get("public_endpoint")
            .map_err(|e| sql_error("decode public endpoint", e))?,
        metadata: json_column(row, "metadata_json")?,
        last_seen_at_ms: row
            .try_get("last_seen_at_ms")
            .map_err(|e| sql_error("decode last seen", e))?,
        lifecycle_error,
        created_at_ms: scalar(row, "created_at_ms")?,
        updated_at_ms: scalar(row, "updated_at_ms")?,
    };
    record.validate()?;
    Ok(record)
}

pub(crate) fn identity_mode_from_str(
    value: &str,
) -> Result<RegisteredIdentityMode, EnvironmentRegistryError> {
    RegisteredIdentityMode::parse(value)
        .ok_or_else(|| store_message(format!("unknown identity mode: {value}")))
}

fn credential_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<EnvironmentCredentialRecord, EnvironmentRegistryError> {
    let kind = column(row, "source_kind")?;
    let grant: Option<String> = row
        .try_get("grant_id")
        .map_err(|e| sql_error("decode grant id", e))?;
    let provider: Option<String> = row
        .try_get("auth_provider_id")
        .map_err(|e| sql_error("decode auth provider id", e))?;
    let secret: Option<String> = row
        .try_get("secret_id")
        .map_err(|e| sql_error("decode secret id", e))?;
    let source = match kind.as_str() {
        "auth_grant" => EnvironmentCredentialSource::AuthGrant {
            grant_id: AuthGrantId::try_new(grant.ok_or_else(|| store_message("missing grant id"))?)
                .map_err(|e| store_message(format!("decode grant id: {e}")))?,
        },
        "auth_provider_credential" => EnvironmentCredentialSource::AuthProviderCredential {
            provider_id: AuthProviderId::try_new(
                provider.ok_or_else(|| store_message("missing auth provider id"))?,
            )
            .map_err(|e| store_message(format!("decode auth provider id: {e}")))?,
        },
        "direct_secret" => EnvironmentCredentialSource::DirectSecret {
            secret_id: SecretId::try_new(secret.ok_or_else(|| store_message("missing secret id"))?)
                .map_err(|e| store_message(format!("decode secret id: {e}")))?,
        },
        other => return Err(store_message(format!("unknown credential source: {other}"))),
    };
    let record = EnvironmentCredentialRecord {
        environment_id: parse_id(row, "environment_id", EnvironmentId::try_new)?,
        env_name: column(row, "env_name")?,
        source,
        created_at_ms: scalar(row, "created_at_ms")?,
        updated_at_ms: scalar(row, "updated_at_ms")?,
    };
    record.validate()?;
    Ok(record)
}

fn credential_source_columns(
    source: &EnvironmentCredentialSource,
) -> (&'static str, Option<&str>, Option<&str>, Option<&str>) {
    match source {
        EnvironmentCredentialSource::AuthGrant { grant_id } => {
            ("auth_grant", Some(grant_id.as_str()), None, None)
        }
        EnvironmentCredentialSource::AuthProviderCredential { provider_id } => (
            "auth_provider_credential",
            None,
            Some(provider_id.as_str()),
            None,
        ),
        EnvironmentCredentialSource::DirectSecret { secret_id } => {
            ("direct_secret", None, None, Some(secret_id.as_str()))
        }
    }
}

fn binding_status_to_str(value: EnvironmentProviderBindingStatus) -> &'static str {
    match value {
        EnvironmentProviderBindingStatus::Enabled => "enabled",
        EnvironmentProviderBindingStatus::Disabled => "disabled",
    }
}
fn binding_status_from_str(
    value: &str,
) -> Result<EnvironmentProviderBindingStatus, EnvironmentRegistryError> {
    match value {
        "enabled" => Ok(EnvironmentProviderBindingStatus::Enabled),
        "disabled" => Ok(EnvironmentProviderBindingStatus::Disabled),
        other => Err(store_message(format!("unknown binding status: {other}"))),
    }
}
fn power_state_from_str(value: &str) -> Result<PowerState, EnvironmentRegistryError> {
    PowerState::ALL
        .into_iter()
        .find(|state| state.as_str() == value)
        .ok_or_else(|| store_message(format!("unknown environment power state: {value}")))
}

fn optional_json_column<T: serde::de::DeserializeOwned>(
    row: &sqlx::postgres::PgRow,
    name: &str,
) -> Result<Option<T>, EnvironmentRegistryError> {
    let value: Option<serde_json::Value> = row
        .try_get(name)
        .map_err(|e| sql_error("decode json column", e))?;
    value
        .map(|value| {
            serde_json::from_value(value).map_err(|e| store_message(format!("decode {name}: {e}")))
        })
        .transpose()
}

fn environment_status_to_str(value: EnvironmentStatus) -> &'static str {
    match value {
        EnvironmentStatus::Provisioning => "provisioning",
        EnvironmentStatus::Booting => "booting",
        EnvironmentStatus::Ready => "ready",
        EnvironmentStatus::Paused => "paused",
        EnvironmentStatus::Suspended => "suspended",
        EnvironmentStatus::Offline => "offline",
        EnvironmentStatus::Closing => "closing",
        EnvironmentStatus::Closed => "closed",
        EnvironmentStatus::Failed => "failed",
        EnvironmentStatus::Unknown => "unknown",
    }
}
fn environment_status_from_str(value: &str) -> Result<EnvironmentStatus, EnvironmentRegistryError> {
    match value {
        "provisioning" => Ok(EnvironmentStatus::Provisioning),
        "booting" => Ok(EnvironmentStatus::Booting),
        "ready" => Ok(EnvironmentStatus::Ready),
        "paused" => Ok(EnvironmentStatus::Paused),
        "suspended" => Ok(EnvironmentStatus::Suspended),
        "offline" => Ok(EnvironmentStatus::Offline),
        "closing" => Ok(EnvironmentStatus::Closing),
        "closed" => Ok(EnvironmentStatus::Closed),
        "failed" => Ok(EnvironmentStatus::Failed),
        "unknown" => Ok(EnvironmentStatus::Unknown),
        other => Err(store_message(format!(
            "unknown environment status: {other}"
        ))),
    }
}

fn validate_ingress_request_time(value: i64) -> Result<(), EnvironmentRegistryError> {
    if value < 0 {
        invalid("updated_at_ms must be nonnegative")
    } else {
        Ok(())
    }
}

fn invalid_store<T>(message: impl Into<String>) -> Result<T, EnvironmentRegistryError> {
    invalid(message)
}

fn parse_id<T, E>(
    row: &sqlx::postgres::PgRow,
    name: &str,
    parse: impl FnOnce(String) -> Result<T, E>,
) -> Result<T, EnvironmentRegistryError>
where
    E: std::fmt::Display,
{
    parse(column(row, name)?).map_err(|e| store_message(format!("decode {name}: {e}")))
}
fn optional_id<T, E>(
    row: &sqlx::postgres::PgRow,
    name: &str,
    parse: impl FnOnce(String) -> Result<T, E>,
) -> Result<Option<T>, EnvironmentRegistryError>
where
    E: std::fmt::Display,
{
    let value: Option<String> = row
        .try_get(name)
        .map_err(|e| sql_error("decode optional id", e))?;
    value
        .map(parse)
        .transpose()
        .map_err(|e| store_message(format!("decode {name}: {e}")))
}
fn column(row: &sqlx::postgres::PgRow, name: &str) -> Result<String, EnvironmentRegistryError> {
    row.try_get(name)
        .map_err(|e| sql_error("decode environment column", e))
}
fn scalar(row: &sqlx::postgres::PgRow, name: &str) -> Result<i64, EnvironmentRegistryError> {
    row.try_get(name)
        .map_err(|e| sql_error("decode integer column", e))
}
fn json_value<T: serde::Serialize>(
    action: &str,
    value: &T,
) -> Result<serde_json::Value, EnvironmentRegistryError> {
    serde_json::to_value(value).map_err(|e| store_message(format!("{action}: {e}")))
}
fn json_column<T: serde::de::DeserializeOwned>(
    row: &sqlx::postgres::PgRow,
    name: &str,
) -> Result<T, EnvironmentRegistryError> {
    let value: serde_json::Value = row
        .try_get(name)
        .map_err(|e| sql_error("decode json column", e))?;
    serde_json::from_value(value).map_err(|e| store_message(format!("decode {name}: {e}")))
}
fn i64_to_u64(value: i64, name: &str) -> Result<u64, EnvironmentRegistryError> {
    value
        .try_into()
        .map_err(|_| store_message(format!("{name} is negative")))
}
pub(crate) fn not_found(kind: &'static str, id: &impl ToString) -> EnvironmentRegistryError {
    EnvironmentRegistryError::NotFound {
        kind,
        id: id.to_string(),
    }
}
pub(crate) fn invalid<T>(message: impl Into<String>) -> Result<T, EnvironmentRegistryError> {
    Err(invalid_error(message))
}
fn invalid_error(message: impl Into<String>) -> EnvironmentRegistryError {
    EnvironmentRegistryError::InvalidInput {
        message: message.into(),
    }
}
fn store_error(action: &str, error: crate::PgStoreError) -> EnvironmentRegistryError {
    store_message(format!("{action}: {error}"))
}
pub(crate) fn sql_error(action: &str, error: sqlx::Error) -> EnvironmentRegistryError {
    store_message(format!("{action}: {error}"))
}
pub(crate) fn store_message(message: impl Into<String>) -> EnvironmentRegistryError {
    EnvironmentRegistryError::Store {
        message: message.into(),
    }
}
fn map_environment_insert_error(error: sqlx::Error) -> EnvironmentRegistryError {
    if let Some(db) = error.as_database_error()
        && db.constraint() == Some("environments_pkey")
    {
        return EnvironmentRegistryError::AlreadyExists {
            kind: "environment",
            id: "duplicate environment id".to_owned(),
        };
    }
    sql_error("insert environment", error)
}
fn map_registered_insert_error(
    error: sqlx::Error,
    daemon_id: &EnvironmentDaemonId,
) -> EnvironmentRegistryError {
    if let Some(db) = error.as_database_error()
        && db.constraint() == Some("environments_daemon_public_key_idx")
    {
        return EnvironmentRegistryError::AlreadyExists {
            kind: "daemon_identity",
            id: daemon_id.to_string(),
        };
    }
    map_environment_insert_error(error)
}
fn map_binding_write_error(action: &str, error: sqlx::Error) -> EnvironmentRegistryError {
    if let Some(db) = error.as_database_error()
        && db.constraint() == Some("environment_provider_bindings_universe_id_provider_id_key")
    {
        return EnvironmentRegistryError::AlreadyExists {
            kind: "environment_provider_binding",
            id: "universe/provider".to_owned(),
        };
    }
    sql_error(action, error)
}

fn map_provider_delete_error(error: sqlx::Error) -> EnvironmentRegistryError {
    if error
        .as_database_error()
        .is_some_and(|database| database.code().as_deref() == Some("23503"))
    {
        return invalid_error("environment provider is referenced by a universe binding");
    }
    sql_error("delete environment provider", error)
}

#[allow(dead_code)]
async fn _transaction_marker(_: &mut Transaction<'_, Postgres>) {}

/// Decode every row that still decodes. A row that does not is left out
/// and logged, so one bad row never hides the rest of a listing or stalls
/// a reconciler pass over them.
pub(crate) fn decode_rows<T>(
    rows: &[sqlx::postgres::PgRow],
    decode: impl Fn(&sqlx::postgres::PgRow) -> Result<T, EnvironmentRegistryError>,
    kind: &'static str,
) -> Vec<T> {
    rows.iter()
        .filter_map(|row| decoded_or_logged(decode(row), kind))
        .collect()
}

fn decoded_or_logged<T>(
    result: Result<T, EnvironmentRegistryError>,
    kind: &'static str,
) -> Option<T> {
    result
        .inspect_err(|error| tracing::error!(%error, kind, "skipping undecodable registry row"))
        .ok()
}

/// A string map as the jsonb object the `@>` containment filter compares
/// against; building the value directly cannot fail.
fn metadata_filter_json(
    metadata: &std::collections::BTreeMap<String, String>,
) -> serde_json::Value {
    serde_json::Value::Object(
        metadata
            .iter()
            .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
            .collect(),
    )
}
