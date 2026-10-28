//! Environment providers, universe bindings and environments.
//!
//! Every environment write locks its row and applies the domain request's
//! pure `apply` in one transaction, so this store and the in-memory one
//! share each transition's skip rule: a write whose expected state no longer
//! holds leaves the row as it is. The writes that run often or in bulk (the
//! liveness stamp, the deployment sweeps) are single
//! conditional statements with the same rules.

// Formatted SQL uses internal schema fragments; request values use bind parameters.
use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use auth::{AuthGrantId, AuthProviderId, SecretId, SecretValue};
use environment_protocol::{ids::ProviderTargetId, routes::DataRouteBase};
use environments::{
    BeginCloseEnvironment, CreateEnvironment, CreateExternalEnvironment,
    CreateRegisteredEnvironment, DEFAULT_EPHEMERAL_DISCONNECT_GRACE_MS, DaemonBuild,
    EndProvisionedEnvironment, EnvironmentCloseReason, EnvironmentCredentialRecord,
    EnvironmentCredentialSource, EnvironmentCredentialStore, EnvironmentEndpoint, EnvironmentId,
    EnvironmentPage, EnvironmentProviderBindingRecord, EnvironmentProviderBindingStore,
    EnvironmentProviderId, EnvironmentProviderRecord, EnvironmentProviderStore,
    EnvironmentProvisionRequestId, EnvironmentRecord, EnvironmentRegistrationKeyId,
    EnvironmentRegistrationKeyRecord, EnvironmentRegistrationKeyStore, EnvironmentRegistryError,
    EnvironmentSource, EnvironmentState, EnvironmentStore, EnvironmentTemplateId, FailEnvironment,
    FinishCloseEnvironment, ListEnvironmentCredentials, ListEnvironments, MarkRegisteredConnected,
    MarkRegisteredDisconnected, ObservedTarget, OpenEnvironment, PowerState,
    ProviderCredentialChange, ProvisionedSource, PutEnvironmentCredential, PutEnvironmentProvider,
    PutEnvironmentProviderBinding, REGISTERED_LIVENESS_STAMP_MS, REGISTERED_STALE_AFTER_MS,
    RecordTargetObservation, RegisteredIdentityMode, RegisteredSource, RegisteredSweep,
    ScheduleEnvironmentAttempt, StampRegisteredSeen, TargetPhase, UpdateEnvironment,
    sweep_registered_environment, validate_daemon_public_key,
};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::{
    PgStore, PgStoreError,
    auth::{LOCAL_KEY_ID, open_secret, seal_secret},
};

const PROVIDER_COLUMNS: &str = r#"
    provider_id, display_name, controller_endpoint, data_route_base,
    credential_key_id IS NOT NULL AS has_credential, created_at_ms, updated_at_ms
"#;

const BINDING_COLUMNS: &str = "universe_id, provider_id, enabled, created_at_ms, updated_at_ms";

/// Columns of `environments e` plus the identity mode of a registered row's
/// key `k` (see [`ENVIRONMENT_FROM`]).
const ENVIRONMENT_COLUMNS: &str = r#"
    e.environment_id, e.source_kind, e.state, e.state_message, e.close_reason,
    e.state_changed_at_ms, e.request_id, e.display_name, e.metadata_json,
    e.daemon_version, e.daemon_git_sha, e.daemon_protocol_version,
    e.provider_id, e.template_id, e.create_idle_json, e.provider_target_id,
    e.next_attempt_at_ms, e.attempts,
    e.observed_phase, e.observed_power, e.observed_message, e.observed_at_ms,
    e.public_endpoint, e.daemon_endpoint,
    e.registration_key_id, e.daemon_public_key, e.connected,
    e.connection_changed_at_ms, e.last_seen_at_ms,
    e.created_at_ms, e.updated_at_ms, e.closed_at_ms,
    k.identity_mode
"#;

/// Registered rows read their identity mode from the key that admitted them.
const ENVIRONMENT_FROM: &str = r#"
    FROM environments e
    LEFT JOIN environment_registration_keys k
      ON k.universe_id = e.universe_id AND k.registration_key_id = e.registration_key_id
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
        // A row that no longer decodes is replaced as if it were absent.
        let existing = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(request.provider_id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| sql_error("lock environment provider", error))?
            .and_then(|row| decoded_or_logged(provider_from_row(&row), "environment provider"));
        let credential = request.credential.clone();
        let record = request.into_record(existing.as_ref())?;
        let (keep, key_id, nonce, ciphertext) = match credential {
            ProviderCredentialChange::Keep => (true, None, None, None),
            ProviderCredentialChange::Set(credential) => {
                let (key_id, nonce, ciphertext) =
                    self.seal_provider_credential(&record.provider_id, &credential)?;
                (false, Some(key_id), Some(nonce), Some(ciphertext))
            }
            ProviderCredentialChange::Clear => (false, None, None, None),
        };
        let query = format!(
            r#"
            INSERT INTO environment_providers (
                provider_id, display_name, controller_endpoint, data_route_base,
                credential_key_id, credential_nonce, credential_ciphertext,
                created_at_ms, updated_at_ms
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
            ON CONFLICT (provider_id) DO UPDATE SET
                display_name = EXCLUDED.display_name,
                controller_endpoint = EXCLUDED.controller_endpoint,
                data_route_base = EXCLUDED.data_route_base,
                credential_key_id = CASE WHEN $10::boolean
                    THEN environment_providers.credential_key_id ELSE EXCLUDED.credential_key_id END,
                credential_nonce = CASE WHEN $10::boolean
                    THEN environment_providers.credential_nonce ELSE EXCLUDED.credential_nonce END,
                credential_ciphertext = CASE WHEN $10::boolean
                    THEN environment_providers.credential_ciphertext
                    ELSE EXCLUDED.credential_ciphertext END,
                created_at_ms = EXCLUDED.created_at_ms,
                updated_at_ms = EXCLUDED.updated_at_ms
            RETURNING {PROVIDER_COLUMNS}
            "#
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(record.provider_id.as_str())
            .bind(record.display_name.as_deref())
            .bind(record.controller_endpoint.as_str())
            .bind(record.data_route_base.as_ref().map(DataRouteBase::as_str))
            .bind(key_id)
            .bind(nonce)
            .bind(ciphertext)
            .bind(record.created_at_ms)
            .bind(record.updated_at_ms)
            .bind(keep)
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| sql_error("put environment provider", error))?;
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
    ) -> Result<Option<SecretValue>, EnvironmentRegistryError> {
        let row = sqlx::query(
            "SELECT credential_key_id, credential_nonce, credential_ciphertext FROM environment_providers WHERE provider_id = $1",
        )
        .bind(provider_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| sql_error("read environment provider credential", error))?
        .ok_or_else(|| not_found("environment_provider", provider_id))?;
        let key_id: Option<String> = row
            .try_get("credential_key_id")
            .map_err(|error| sql_error("decode provider credential key id", error))?;
        let nonce: Option<Vec<u8>> = row
            .try_get("credential_nonce")
            .map_err(|error| sql_error("decode provider credential nonce", error))?;
        let ciphertext: Option<Vec<u8>> = row
            .try_get("credential_ciphertext")
            .map_err(|error| sql_error("decode provider credential", error))?;
        let (key_id, nonce, ciphertext) = match (key_id, nonce, ciphertext) {
            (None, None, None) => return Ok(None),
            (Some(key_id), Some(nonce), Some(ciphertext)) => (key_id, nonce, ciphertext),
            _ => return Err(store_message("provider credential columns are incomplete")),
        };
        if key_id != LOCAL_KEY_ID {
            return Err(store_message(format!(
                "unsupported provider credential key id '{key_id}'"
            )));
        }
        let (cipher, _) = self
            .secrets_cipher()
            .map_err(|error| store_message(error.to_string()))?;
        open_secret(
            &cipher,
            &provider_credential_aad(provider_id),
            &nonce,
            &ciphertext,
        )
        .map(Some)
        .map_err(|error| store_message(format!("open provider credential: {error}")))
    }

    async fn list_providers(
        &self,
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
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin environment provider delete", error))?;
        let query = format!(
            "SELECT {PROVIDER_COLUMNS} FROM environment_providers WHERE provider_id = $1 FOR UPDATE"
        );
        sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(provider_id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| sql_error("lock environment provider", error))?
            .ok_or_else(|| not_found("environment_provider", provider_id))?;
        let bindings: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM environment_provider_bindings WHERE provider_id = $1",
        )
        .bind(provider_id.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| sql_error("count environment provider bindings", error))?;
        if bindings != 0 {
            return invalid("environment provider is referenced by a universe binding");
        }
        let active: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM environments WHERE provider_id = $1 AND state <> 'closed'",
        )
        .bind(provider_id.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| sql_error("count environment provider environments", error))?;
        if active != 0 {
            return invalid("environment provider is referenced by a non-closed environment");
        }
        let query = format!(
            "DELETE FROM environment_providers WHERE provider_id = $1 RETURNING {PROVIDER_COLUMNS}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(provider_id.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(map_provider_delete_error)?;
        tx.commit()
            .await
            .map_err(|error| sql_error("commit environment provider delete", error))?;
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
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin provider binding put", error))?;
        let provider: Option<String> = sqlx::query_scalar(
            "SELECT provider_id FROM environment_providers WHERE provider_id = $1 FOR SHARE",
        )
        .bind(request.provider_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| sql_error("lock binding provider", error))?;
        if provider.is_none() {
            return Err(not_found("environment_provider", &request.provider_id));
        }
        let query = format!(
            "SELECT {BINDING_COLUMNS} FROM environment_provider_bindings WHERE universe_id = $1 AND provider_id = $2 FOR UPDATE"
        );
        let existing = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(request.universe_id)
            .bind(request.provider_id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| sql_error("lock provider binding", error))?
            .and_then(|row| decoded_or_logged(binding_from_row(&row), "provider binding"));
        let record = request.into_record(existing.as_ref())?;
        let query = format!(
            r#"
            INSERT INTO environment_provider_bindings (
                universe_id, provider_id, enabled, created_at_ms, updated_at_ms
            ) VALUES ($1,$2,$3,$4,$5)
            ON CONFLICT (universe_id, provider_id) DO UPDATE SET
                enabled = EXCLUDED.enabled,
                created_at_ms = EXCLUDED.created_at_ms,
                updated_at_ms = EXCLUDED.updated_at_ms
            RETURNING {BINDING_COLUMNS}
            "#
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(record.universe_id)
            .bind(record.provider_id.as_str())
            .bind(record.enabled)
            .bind(record.created_at_ms)
            .bind(record.updated_at_ms)
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| map_binding_write_error(error, record.universe_id))?;
        tx.commit()
            .await
            .map_err(|error| sql_error("commit provider binding", error))?;
        binding_from_row(&row)
    }

    async fn read_provider_binding(
        &self,
        universe_id: Uuid,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {BINDING_COLUMNS} FROM environment_provider_bindings WHERE universe_id = $1 AND provider_id = $2"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(universe_id)
            .bind(provider_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| sql_error("read provider binding", error))?
            .ok_or_else(|| not_found("environment_provider_binding", provider_id))?;
        binding_from_row(&row)
    }

    async fn list_provider_bindings(
        &self,
        universe_id: Uuid,
    ) -> Result<Vec<EnvironmentProviderBindingRecord>, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {BINDING_COLUMNS} FROM environment_provider_bindings WHERE universe_id = $1 ORDER BY provider_id"
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
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin provider binding delete", error))?;
        let query = format!(
            "SELECT {BINDING_COLUMNS} FROM environment_provider_bindings WHERE universe_id = $1 AND provider_id = $2 FOR UPDATE"
        );
        sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(universe_id)
            .bind(provider_id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| sql_error("lock provider binding", error))?
            .ok_or_else(|| not_found("environment_provider_binding", provider_id))?;
        let active: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM environments WHERE universe_id = $1 AND provider_id = $2 AND state <> 'closed'",
        )
        .bind(universe_id)
        .bind(provider_id.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| sql_error("count binding environments", error))?;
        if active != 0 {
            return invalid("provider binding is referenced by a non-closed environment");
        }
        let query = format!(
            "DELETE FROM environment_provider_bindings WHERE universe_id = $1 AND provider_id = $2 RETURNING {BINDING_COLUMNS}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(universe_id)
            .bind(provider_id.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| sql_error("delete provider binding", error))?;
        tx.commit()
            .await
            .map_err(|error| sql_error("commit provider binding delete", error))?;
        binding_from_row(&row)
    }
}

impl PgStore {
    /// Apply one write to one environment: the row is locked, `change` runs
    /// on a copy, and the copy is validated and written only when `change`
    /// reports a change. Returns the current record and whether it changed.
    async fn change_environment<F>(
        &self,
        environment_id: &EnvironmentId,
        action: &'static str,
        change: F,
    ) -> Result<(EnvironmentRecord, bool), EnvironmentRegistryError>
    where
        F: FnOnce(&mut EnvironmentRecord) -> Result<bool, EnvironmentRegistryError> + Send,
    {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error(action, error))?;
        let current = lock_environment(&mut tx, self.config.universe_id, environment_id).await?;
        let mut candidate = current.clone();
        if !change(&mut candidate)? {
            return Ok((current, false));
        }
        candidate.validate()?;
        let columns = EnvironmentColumns::of(&candidate)?;
        update_environment_row(&mut tx, self.config.universe_id, &candidate, columns)
            .await
            .map_err(|error| map_environment_write_error(action, error))?;
        tx.commit()
            .await
            .map_err(|error| sql_error(action, error))?;
        Ok((candidate, true))
    }

    async fn find_environment_by_request_id(
        &self,
        request_id: &EnvironmentProvisionRequestId,
    ) -> Result<Option<EnvironmentRecord>, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {ENVIRONMENT_COLUMNS} {ENVIRONMENT_FROM} WHERE e.universe_id = $1 AND e.request_id = $2"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(request_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| sql_error("read environment by request id", error))?;
        row.as_ref().map(environment_from_row).transpose()
    }

    /// The stored row a retried create with `request_id` converges on:
    /// itself when `matches` holds, otherwise `AlreadyExists`.
    async fn converge_create(
        &self,
        request_id: &EnvironmentProvisionRequestId,
        matches: impl FnOnce(&EnvironmentRecord) -> bool,
    ) -> Result<Option<EnvironmentRecord>, EnvironmentRegistryError> {
        match self.find_environment_by_request_id(request_id).await? {
            None => Ok(None),
            Some(existing) if matches(&existing) => Ok(Some(existing)),
            Some(_) => Err(EnvironmentRegistryError::AlreadyExists {
                kind: "environment_request",
                id: request_id.to_string(),
            }),
        }
    }

    /// Insert a provisioned or external row unless its request id is
    /// already taken; a concurrent create with the same request id converges
    /// like a retry.
    async fn insert_requested_environment(
        &self,
        mut tx: Transaction<'_, Postgres>,
        record: EnvironmentRecord,
        matches: impl FnOnce(&EnvironmentRecord) -> bool,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let columns = EnvironmentColumns::of(&record)?;
        let inserted = insert_environment_row(&mut tx, self.config.universe_id, &record, columns)
            .await
            .map_err(|error| map_environment_write_error("insert environment", error))?;
        if inserted {
            tx.commit()
                .await
                .map_err(|error| sql_error("commit environment create", error))?;
            return Ok(record);
        }
        drop(tx);
        let request_id = record
            .request_id
            .as_ref()
            .ok_or_else(|| store_message("environment insert conflicted without a request id"))?;
        self.converge_create(request_id, matches)
            .await?
            .ok_or_else(|| store_message("conflicting environment request is gone"))
    }
}

#[async_trait]
impl EnvironmentStore for PgStore {
    async fn create_environment(
        &self,
        request: CreateEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let record = request.record()?;
        self.ensure_universe()
            .await
            .map_err(|error| store_error("ensure universe", error))?;
        if let Some(existing) = self
            .converge_create(&request.request_id, |existing| request.matches(existing))
            .await?
        {
            return Ok(existing);
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin environment create", error))?;
        // Share-lock the binding so it cannot be deleted or disabled while
        // the row that references it is inserted.
        let enabled: Option<bool> = sqlx::query_scalar(
            "SELECT enabled FROM environment_provider_bindings WHERE universe_id = $1 AND provider_id = $2 FOR SHARE",
        )
        .bind(self.config.universe_id)
        .bind(request.provider_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| sql_error("lock provider binding", error))?;
        match enabled {
            None => {
                return Err(not_found(
                    "environment_provider_binding",
                    &request.provider_id,
                ));
            }
            Some(false) => return invalid("environment provider binding is disabled"),
            Some(true) => {}
        }
        self.insert_requested_environment(tx, record, |existing| request.matches(existing))
            .await
    }

    async fn create_external_environment(
        &self,
        request: CreateExternalEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let record = request.record()?;
        self.ensure_universe()
            .await
            .map_err(|error| store_error("ensure universe", error))?;
        if let Some(existing) = self
            .converge_create(&request.request_id, |existing| request.matches(existing))
            .await?
        {
            return Ok(existing);
        }
        let tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin external environment create", error))?;
        self.insert_requested_environment(tx, record, |existing| request.matches(existing))
            .await
    }

    async fn create_registered_environment(
        &self,
        request: CreateRegisteredEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        if request.created_at_ms < 0 {
            return invalid("created_at_ms must be nonnegative");
        }
        validate_daemon_public_key(&request.daemon_public_key)?;
        self.ensure_universe()
            .await
            .map_err(|error| store_error("ensure universe", error))?;
        // A known identity converges on its row, closed included: the
        // identity is spent, and the caller refuses a closed one.
        if let Some(existing) = self
            .read_environment_by_daemon_public_key(&request.daemon_public_key)
            .await?
        {
            return Ok(existing);
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin registered environment create", error))?;
        // Lock the key row: concurrent registrations against one key
        // serialize here, so the count below is exact. Rotation takes the
        // same lock, so the secret check below sees every committed one.
        let key = crate::environment_registration::lock_registration_key(
            &mut tx,
            self.config.universe_id,
            &request.registration_key_id,
        )
        .await?;
        key.check_admits(request.created_at_ms)?;
        let live: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM environment_registration_key_secrets
                WHERE universe_id = $1 AND registration_key_id = $2
                  AND secret_hash = $3 AND retired_at_ms IS NULL
            )
            "#,
        )
        .bind(self.config.universe_id)
        .bind(key.registration_key_id.as_str())
        .bind(&request.secret_hash)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| sql_error("check registration key secret", error))?;
        if !live {
            return Err(request.secret_retired());
        }
        if let Some(limit) = key.max_active_environments {
            let active: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM environments WHERE universe_id = $1 AND registration_key_id = $2 AND state <> 'closed'",
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
        let record = request.record(&key)?;
        let columns = EnvironmentColumns::of(&record)?;
        match insert_environment_row(&mut tx, self.config.universe_id, &record, columns).await {
            Ok(_) => {
                tx.commit()
                    .await
                    .map_err(|error| sql_error("commit registered environment create", error))?;
                Ok(record)
            }
            Err(error) if constraint_of(&error) == Some("environments_daemon_public_key_idx") => {
                drop(tx);
                // A concurrent admission of the same identity in this
                // universe won; elsewhere the identity belongs to another
                // universe.
                self.read_environment_by_daemon_public_key(&request.daemon_public_key)
                    .await?
                    .ok_or_else(|| EnvironmentRegistryError::AlreadyExists {
                        kind: "daemon_identity",
                        id: request
                            .daemon_id()
                            .map(|daemon_id| daemon_id.to_string())
                            .unwrap_or_else(|_| request.daemon_public_key.clone()),
                    })
            }
            Err(error) => Err(map_environment_write_error(
                "insert registered environment",
                error,
            )),
        }
    }

    async fn read_environment(
        &self,
        environment_id: &EnvironmentId,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {ENVIRONMENT_COLUMNS} {ENVIRONMENT_FROM} WHERE e.universe_id = $1 AND e.environment_id = $2"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(environment_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| sql_error("read environment", error))?
            .ok_or_else(|| not_found("environment", environment_id))?;
        environment_from_row(&row)
    }

    async fn read_environment_by_daemon_public_key(
        &self,
        daemon_public_key: &str,
    ) -> Result<Option<EnvironmentRecord>, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {ENVIRONMENT_COLUMNS} {ENVIRONMENT_FROM} WHERE e.universe_id = $1 AND e.daemon_public_key = $2"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(daemon_public_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| sql_error("read environment by daemon key", error))?;
        row.as_ref().map(environment_from_row).transpose()
    }

    async fn list_environments(
        &self,
        request: ListEnvironments,
    ) -> Result<EnvironmentPage, EnvironmentRegistryError> {
        request.validate()?;
        let mut query =
            format!("SELECT {ENVIRONMENT_COLUMNS} {ENVIRONMENT_FROM} WHERE e.universe_id = $1");
        let mut next = 2;
        let mut parameter = |query: &mut String, predicate: &str| {
            query.push_str(&format!(" AND {predicate}${next}"));
            next += 1;
        };
        if request.provider_id.is_some() {
            parameter(&mut query, "e.provider_id = ");
        }
        if request.registration_key_id.is_some() {
            parameter(&mut query, "e.registration_key_id = ");
        }
        if request.state.is_some() {
            parameter(&mut query, "e.state = ");
        }
        // Appended only when present so containment can use the GIN index.
        if !request.metadata.is_empty() {
            parameter(&mut query, "e.metadata_json @> ");
        }
        if request.after.is_some() {
            parameter(&mut query, "e.environment_id > ");
        }
        if !request.include_closed && request.state != Some(EnvironmentState::Closed) {
            query.push_str(" AND e.state <> 'closed'");
        }
        query.push_str(&format!(" ORDER BY e.environment_id LIMIT ${next}"));
        let mut sql = sqlx::query(sqlx::AssertSqlSafe(query)).bind(self.config.universe_id);
        if let Some(id) = &request.provider_id {
            sql = sql.bind(id.as_str().to_owned());
        }
        if let Some(id) = &request.registration_key_id {
            sql = sql.bind(id.as_str().to_owned());
        }
        if let Some(state) = request.state {
            sql = sql.bind(state.as_str());
        }
        if !request.metadata.is_empty() {
            sql = sql.bind(metadata_filter_json(&request.metadata));
        }
        if let Some(after) = &request.after {
            sql = sql.bind(after.as_str().to_owned());
        }
        let limit = request.limit as usize;
        // One row past the page tells whether another page follows.
        let mut rows = sql
            .bind(i64::from(request.limit) + 1)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| sql_error("list environments", error))?;
        let next_after = if rows.len() > limit {
            rows.truncate(limit);
            rows.last()
                .map(|row| parse_id(row, "environment_id", EnvironmentId::try_new))
                .transpose()?
        } else {
            None
        };
        Ok(EnvironmentPage {
            environments: decode_rows(&rows, environment_from_row, "environment"),
            next_after,
        })
    }

    async fn update_environment(
        &self,
        request: UpdateEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change_environment(&request.environment_id, "update environment", |record| {
            request.apply(record)
        })
        .await
        .map(|(record, _)| record)
    }

    async fn begin_close_environment(
        &self,
        request: BeginCloseEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change_environment(
            &request.environment_id,
            "begin environment close",
            |record| request.apply(record),
        )
        .await
        .map(|(record, _)| record)
    }

    async fn finish_close_environment(
        &self,
        request: FinishCloseEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change_environment(
            &request.environment_id,
            "finish environment close",
            |record| request.apply(record),
        )
        .await
        .map(|(record, _)| record)
    }

    async fn mark_registered_connected(
        &self,
        request: MarkRegisteredConnected,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change_environment(
            &request.environment_id,
            "mark registered environment connected",
            |record| request.apply(record),
        )
        .await
        .map(|(record, _)| record)
    }

    async fn mark_registered_disconnected(
        &self,
        request: MarkRegisteredDisconnected,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change_environment(
            &request.environment_id,
            "mark registered environment disconnected",
            |record| request.apply(record),
        )
        .await
        .map(|(record, _)| record)
    }

    async fn stamp_registered_seen(
        &self,
        request: StampRegisteredSeen,
    ) -> Result<bool, EnvironmentRegistryError> {
        if request.at_ms < 0 {
            return invalid("at_ms must be nonnegative");
        }
        // One round trip: the stamp is conditional, and the row's kind
        // tells a skipped stamp from a missing or non-registered row.
        let row = sqlx::query(
            r#"
            WITH target AS (
                SELECT source_kind FROM environments
                WHERE universe_id = $1 AND environment_id = $2
            ), stamped AS (
                UPDATE environments SET
                    last_seen_at_ms = $3,
                    updated_at_ms = GREATEST(updated_at_ms, $3)
                WHERE universe_id = $1 AND environment_id = $2
                  AND source_kind = 'registered' AND state = 'open' AND connected
                  AND last_seen_at_ms <= $4
                RETURNING 1
            )
            SELECT (SELECT source_kind FROM target) AS source_kind,
                   EXISTS (SELECT 1 FROM stamped) AS stamped
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.environment_id.as_str())
        .bind(request.at_ms)
        .bind(request.at_ms.saturating_sub(REGISTERED_LIVENESS_STAMP_MS))
        .fetch_one(&self.pool)
        .await
        .map_err(|error| sql_error("stamp registered environment", error))?;
        let source_kind: Option<String> = row
            .try_get("source_kind")
            .map_err(|error| sql_error("decode stamped source kind", error))?;
        match source_kind.as_deref() {
            None => Err(not_found("environment", &request.environment_id)),
            Some("registered") => row
                .try_get("stamped")
                .map_err(|error| sql_error("decode stamp result", error)),
            Some(_) => invalid("connection writes apply only to registered environments"),
        }
    }

    async fn sweep_registered_environments(
        &self,
        now_ms: i64,
    ) -> Result<RegisteredSweep, EnvironmentRegistryError> {
        // Only rows one of the three sweep rules can move; the rules
        // themselves run in the domain, under the row lock. A connected row
        // can close only after rule 1 marks it disconnected, so rule 1
        // covers it.
        let candidates: Vec<(String, String)> = sqlx::query_as(
            r#"
            SELECT e.environment_id, e.registration_key_id
            FROM environments e
            JOIN environment_registration_keys k
              ON k.universe_id = e.universe_id AND k.registration_key_id = e.registration_key_id
            WHERE e.universe_id = $1 AND e.source_kind = 'registered' AND e.state = 'open'
              AND (
                (e.connected AND e.last_seen_at_ms < $2 - $3)
                OR (
                    NOT e.connected AND k.identity_mode = 'ephemeral'
                    AND e.connection_changed_at_ms < $2 - COALESCE(k.ephemeral_disconnect_grace_ms, $4)
                )
                OR (
                    NOT e.connected AND k.identity_mode = 'persistent'
                    AND k.offline_close_after_ms IS NOT NULL
                    AND e.connection_changed_at_ms < $2 - k.offline_close_after_ms
                )
              )
            ORDER BY e.environment_id
            "#,
        )
        .bind(self.config.universe_id)
        .bind(now_ms)
        .bind(REGISTERED_STALE_AFTER_MS)
        .bind(i64::try_from(DEFAULT_EPHEMERAL_DISCONNECT_GRACE_MS).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| sql_error("list registered sweep candidates", error))?;
        let mut keys: HashMap<String, EnvironmentRegistrationKeyRecord> = HashMap::new();
        let mut sweep = RegisteredSweep::default();
        for (environment_id, registration_key_id) in candidates {
            let key = match keys.get(&registration_key_id) {
                Some(key) => key.clone(),
                None => {
                    let id = EnvironmentRegistrationKeyId::try_new(registration_key_id.clone())
                        .map_err(|error| {
                            store_message(format!("decode registration key id: {error}"))
                        })?;
                    let key = self.read_registration_key(&id).await?;
                    keys.insert(registration_key_id, key.clone());
                    key
                }
            };
            let environment_id = EnvironmentId::try_new(environment_id)
                .map_err(|error| store_message(format!("decode environment id: {error}")))?;
            let mut outcome = (false, None);
            let changed = self
                .change_environment(&environment_id, "sweep registered environment", |record| {
                    outcome = sweep_registered_environment(record, &key, now_ms);
                    Ok(outcome.0 || outcome.1.is_some())
                })
                .await;
            if let Err(error) = changed {
                tracing::error!(%error, environment_id = %environment_id, "registered sweep skipped a row");
                continue;
            }
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
        if now_ms < 0 {
            return invalid("now_ms must be nonnegative");
        }
        repair_connections(&self.pool, Some(self.config.universe_id), now_ms)
            .await
            .map_err(|error| sql_error("repair registered connections", error))
    }

    async fn list_environment_work(
        &self,
        now_ms: i64,
        limit: u32,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {ENVIRONMENT_COLUMNS} {ENVIRONMENT_FROM} WHERE e.universe_id = $1 AND e.next_attempt_at_ms <= $2 ORDER BY e.next_attempt_at_ms, e.environment_id LIMIT $3"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(now_ms)
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(|error| sql_error("list environment work", error))?;
        Ok(decode_rows(&rows, environment_from_row, "environment"))
    }

    async fn list_open_provisioned_environments(
        &self,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {ENVIRONMENT_COLUMNS} {ENVIRONMENT_FROM} WHERE e.universe_id = $1 AND e.source_kind = 'provisioned' AND e.state = 'open' ORDER BY e.environment_id"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| sql_error("list open provisioned environments", error))?;
        Ok(decode_rows(&rows, environment_from_row, "environment"))
    }

    async fn record_target_observation(
        &self,
        request: RecordTargetObservation,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change_environment(
            &request.environment_id,
            "record target observation",
            |record| request.apply(record),
        )
        .await
        .map(|(record, _)| record)
    }

    async fn schedule_environment_attempt(
        &self,
        request: ScheduleEnvironmentAttempt,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change_environment(
            &request.environment_id,
            "schedule environment attempt",
            |record| request.apply(record),
        )
        .await
        .map(|(record, _)| record)
    }

    async fn open_environment(
        &self,
        request: OpenEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change_environment(&request.environment_id, "open environment", |record| {
            request.apply(record)
        })
        .await
        .map(|(record, _)| record)
    }

    async fn fail_environment(
        &self,
        request: FailEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change_environment(&request.environment_id, "fail environment", |record| {
            request.apply(record)
        })
        .await
        .map(|(record, _)| record)
    }

    async fn end_provisioned_environment(
        &self,
        request: EndProvisionedEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        self.change_environment(
            &request.environment_id,
            "end provisioned environment",
            |record| request.apply(record),
        )
        .await
        .map(|(record, _)| record)
    }
}

/// Connected registered rows become disconnected as of `now_ms`, in one
/// universe or in all of them.
async fn repair_connections(
    pool: &PgPool,
    universe_id: Option<Uuid>,
    now_ms: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r#"
        UPDATE environments SET
            connected = false,
            connection_changed_at_ms = $2,
            updated_at_ms = GREATEST(updated_at_ms, $2)
        WHERE ($1::uuid IS NULL OR universe_id = $1)
          AND source_kind = 'registered' AND connected
        "#,
    )
    .bind(universe_id)
    .bind(now_ms)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

// ---------------------------------------------------------------------------
// Deployment-level functions: the environment-gateway role's loops run
// these across universes before they pick a universe-scoped store.
// ---------------------------------------------------------------------------

/// Universes holding provisioned rows whose next attempt is due at `now_ms`.
pub async fn list_universes_with_environment_work(
    pool: &PgPool,
    now_ms: i64,
) -> Result<Vec<Uuid>, PgStoreError> {
    list_universes(
        pool,
        "SELECT DISTINCT universe_id FROM environments WHERE next_attempt_at_ms <= $1 ORDER BY universe_id",
        Some(now_ms),
    )
    .await
}

/// Universes holding open provisioned rows: the display sweep's scan set.
pub async fn list_universes_with_open_provisioned_environments(
    pool: &PgPool,
) -> Result<Vec<Uuid>, PgStoreError> {
    list_universes(
        pool,
        "SELECT DISTINCT universe_id FROM environments WHERE source_kind = 'provisioned' AND state = 'open' ORDER BY universe_id",
        None,
    )
    .await
}

/// Universes holding open registered rows (a registered row is only ever
/// `open` or `closed`): the registered sweep's scan set.
pub async fn list_universes_with_registered_environments(
    pool: &PgPool,
) -> Result<Vec<Uuid>, PgStoreError> {
    list_universes(
        pool,
        "SELECT DISTINCT universe_id FROM environments WHERE source_kind = 'registered' AND state = 'open' ORDER BY universe_id",
        None,
    )
    .await
}

async fn list_universes(
    pool: &PgPool,
    query: &'static str,
    now_ms: Option<i64>,
) -> Result<Vec<Uuid>, PgStoreError> {
    let mut sql = sqlx::query_as::<_, (Uuid,)>(query);
    if let Some(now_ms) = now_ms {
        sql = sql.bind(now_ms);
    }
    let rows = sql.fetch_all(pool).await?;
    Ok(rows.into_iter().map(|(universe_id,)| universe_id).collect())
}

/// At gateway startup, in every universe: connected registered rows become
/// disconnected with `connection_changed_at_ms = now_ms`, since the
/// connections the previous gateway held ended now, and persistent rows get
/// their full reconnect wait while their daemons reconnect.
/// `last_seen_at_ms` is kept. Assumes a single gateway replica.
pub async fn repair_registered_connections(
    pool: &PgPool,
    now_ms: i64,
) -> Result<u64, PgStoreError> {
    Ok(repair_connections(pool, None, now_ms).await?)
}

/// Of `keys` (universe, environment id), those whose live routes must end:
/// the row is gone or not `open`, or it is provisioned and its universe's
/// binding of the provider is missing or disabled.
pub async fn list_revoked_environment_routes(
    pool: &PgPool,
    keys: &[(Uuid, String)],
) -> Result<Vec<(Uuid, String)>, PgStoreError> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let (universes, environments): (Vec<Uuid>, Vec<String>) = keys.iter().cloned().unzip();
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        r#"
        SELECT route.universe_id, route.environment_id
        FROM unnest($1::uuid[], $2::text[]) AS route (universe_id, environment_id)
        LEFT JOIN environments e
          ON e.universe_id = route.universe_id AND e.environment_id = route.environment_id
        LEFT JOIN environment_provider_bindings b
          ON b.universe_id = e.universe_id AND b.provider_id = e.provider_id
        WHERE e.environment_id IS NULL
           OR e.state <> 'open'
           OR (e.source_kind = 'provisioned' AND (b.provider_id IS NULL OR NOT b.enabled))
        ORDER BY route.universe_id, route.environment_id
        "#,
    )
    .bind(universes)
    .bind(environments)
    .fetch_all(pool)
    .await?;
    Ok(rows)
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

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

async fn lock_environment(
    tx: &mut Transaction<'_, Postgres>,
    universe_id: Uuid,
    environment_id: &EnvironmentId,
) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
    let query = format!(
        "SELECT {ENVIRONMENT_COLUMNS} {ENVIRONMENT_FROM} WHERE e.universe_id = $1 AND e.environment_id = $2 FOR UPDATE OF e"
    );
    let row = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind(universe_id)
        .bind(environment_id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| sql_error("lock environment", error))?
        .ok_or_else(|| not_found("environment", environment_id))?;
    environment_from_row(&row)
}

/// Every stored column of an environment record, as bound to SQL.
struct EnvironmentColumns {
    source_kind: &'static str,
    state: &'static str,
    state_message: Option<String>,
    close_reason: Option<&'static str>,
    state_changed_at_ms: i64,
    request_id: Option<String>,
    display_name: Option<String>,
    metadata_json: serde_json::Value,
    daemon_version: Option<String>,
    daemon_git_sha: Option<String>,
    daemon_protocol_version: Option<i32>,
    provider_id: Option<String>,
    template_id: Option<String>,
    create_idle_json: Option<serde_json::Value>,
    provider_target_id: Option<String>,
    next_attempt_at_ms: Option<i64>,
    attempts: i32,
    observed_phase: Option<&'static str>,
    observed_power: Option<&'static str>,
    observed_message: Option<String>,
    observed_at_ms: Option<i64>,
    public_endpoint: Option<String>,
    daemon_endpoint: Option<String>,
    registration_key_id: Option<String>,
    daemon_public_key: Option<String>,
    connected: Option<bool>,
    connection_changed_at_ms: Option<i64>,
    last_seen_at_ms: Option<i64>,
    created_at_ms: i64,
    updated_at_ms: i64,
    closed_at_ms: Option<i64>,
}

impl EnvironmentColumns {
    fn of(record: &EnvironmentRecord) -> Result<Self, EnvironmentRegistryError> {
        let mut columns = Self {
            source_kind: record.source.kind_str(),
            state: record.state.as_str(),
            state_message: record.state_message.clone(),
            close_reason: record.close_reason.map(EnvironmentCloseReason::as_str),
            state_changed_at_ms: record.state_changed_at_ms,
            request_id: record.request_id.as_ref().map(|id| id.as_str().to_owned()),
            display_name: record.display_name.clone(),
            metadata_json: metadata_filter_json(&record.metadata),
            daemon_version: None,
            daemon_git_sha: None,
            daemon_protocol_version: None,
            provider_id: None,
            template_id: None,
            create_idle_json: None,
            provider_target_id: None,
            next_attempt_at_ms: None,
            attempts: 0,
            observed_phase: None,
            observed_power: None,
            observed_message: None,
            observed_at_ms: None,
            public_endpoint: None,
            daemon_endpoint: None,
            registration_key_id: None,
            daemon_public_key: None,
            connected: None,
            connection_changed_at_ms: None,
            last_seen_at_ms: None,
            created_at_ms: record.created_at_ms,
            updated_at_ms: record.updated_at_ms,
            closed_at_ms: record.closed_at_ms,
        };
        if let Some(build) = &record.daemon_build {
            columns.daemon_version = build.version.clone();
            columns.daemon_git_sha = build.git_sha.clone();
            columns.daemon_protocol_version = Some(
                i32::try_from(build.protocol_version)
                    .map_err(|_| invalid_error("daemon protocol version is out of range"))?,
            );
        }
        match &record.source {
            EnvironmentSource::Provisioned(source) => {
                columns.provider_id = Some(source.provider_id.as_str().to_owned());
                columns.template_id = Some(source.template_id.as_str().to_owned());
                columns.create_idle_json = source
                    .idle
                    .as_ref()
                    .map(|policy| json_value("encode create idle policy", policy))
                    .transpose()?;
                columns.provider_target_id = source
                    .provider_target_id
                    .as_ref()
                    .map(|id| id.as_str().to_owned());
                columns.next_attempt_at_ms = source.next_attempt_at_ms;
                columns.attempts = i32::try_from(source.attempts).unwrap_or(i32::MAX);
                if let Some(observed) = &source.observed {
                    columns.observed_phase = Some(target_phase_str(observed.phase));
                    columns.observed_power = observed.power.map(PowerState::as_str);
                    columns.observed_message = observed.message.clone();
                    columns.observed_at_ms = Some(observed.observed_at_ms);
                }
                columns.public_endpoint = source.public_endpoint.clone();
            }
            EnvironmentSource::External { endpoint } => {
                columns.daemon_endpoint = Some(endpoint.as_str().to_owned());
            }
            EnvironmentSource::Registered(source) => {
                columns.registration_key_id = Some(source.registration_key_id.as_str().to_owned());
                columns.daemon_public_key = Some(source.daemon_public_key.clone());
                columns.connected = Some(source.connected);
                columns.connection_changed_at_ms = Some(source.connection_changed_at_ms);
                columns.last_seen_at_ms = Some(source.last_seen_at_ms);
            }
        }
        Ok(columns)
    }
}

/// Insert a new row. Returns false, inserting nothing, when the request id
/// is already taken in the universe.
async fn insert_environment_row(
    tx: &mut Transaction<'_, Postgres>,
    universe_id: Uuid,
    record: &EnvironmentRecord,
    columns: EnvironmentColumns,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        r#"
        INSERT INTO environments (
            universe_id, environment_id, source_kind, state, state_message, close_reason,
            state_changed_at_ms, request_id, display_name, metadata_json,
            daemon_version, daemon_git_sha, daemon_protocol_version,
            provider_id, template_id, create_idle_json, provider_target_id,
            next_attempt_at_ms, attempts,
            observed_phase, observed_power, observed_message, observed_at_ms,
            public_endpoint, daemon_endpoint,
            registration_key_id, daemon_public_key, connected,
            connection_changed_at_ms, last_seen_at_ms,
            created_at_ms, updated_at_ms, closed_at_ms
        ) VALUES (
            $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,
            $18,$19,$20,$21,$22,$23,$24,$25,$26,$27,$28,$29,$30,$31,$32,$33
        )
        ON CONFLICT (universe_id, request_id) WHERE request_id IS NOT NULL DO NOTHING
        "#,
    )
    .bind(universe_id)
    .bind(record.environment_id.as_str())
    .bind(columns.source_kind)
    .bind(columns.state)
    .bind(columns.state_message)
    .bind(columns.close_reason)
    .bind(columns.state_changed_at_ms)
    .bind(columns.request_id)
    .bind(columns.display_name)
    .bind(columns.metadata_json)
    .bind(columns.daemon_version)
    .bind(columns.daemon_git_sha)
    .bind(columns.daemon_protocol_version)
    .bind(columns.provider_id)
    .bind(columns.template_id)
    .bind(columns.create_idle_json)
    .bind(columns.provider_target_id)
    .bind(columns.next_attempt_at_ms)
    .bind(columns.attempts)
    .bind(columns.observed_phase)
    .bind(columns.observed_power)
    .bind(columns.observed_message)
    .bind(columns.observed_at_ms)
    .bind(columns.public_endpoint)
    .bind(columns.daemon_endpoint)
    .bind(columns.registration_key_id)
    .bind(columns.daemon_public_key)
    .bind(columns.connected)
    .bind(columns.connection_changed_at_ms)
    .bind(columns.last_seen_at_ms)
    .bind(columns.created_at_ms)
    .bind(columns.updated_at_ms)
    .bind(columns.closed_at_ms)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Write the mutable columns of a locked row. The kind, its identity
/// columns and its create inputs never change after the insert.
async fn update_environment_row(
    tx: &mut Transaction<'_, Postgres>,
    universe_id: Uuid,
    record: &EnvironmentRecord,
    columns: EnvironmentColumns,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE environments SET
            state = $3, state_message = $4, close_reason = $5, state_changed_at_ms = $6,
            display_name = $7, metadata_json = $8,
            daemon_version = $9, daemon_git_sha = $10, daemon_protocol_version = $11,
            provider_target_id = $12, next_attempt_at_ms = $13, attempts = $14,
            observed_phase = $15, observed_power = $16, observed_message = $17,
            observed_at_ms = $18, public_endpoint = $19,
            connected = $20, connection_changed_at_ms = $21, last_seen_at_ms = $22,
            updated_at_ms = $23, closed_at_ms = $24
        WHERE universe_id = $1 AND environment_id = $2
        "#,
    )
    .bind(universe_id)
    .bind(record.environment_id.as_str())
    .bind(columns.state)
    .bind(columns.state_message)
    .bind(columns.close_reason)
    .bind(columns.state_changed_at_ms)
    .bind(columns.display_name)
    .bind(columns.metadata_json)
    .bind(columns.daemon_version)
    .bind(columns.daemon_git_sha)
    .bind(columns.daemon_protocol_version)
    .bind(columns.provider_target_id)
    .bind(columns.next_attempt_at_ms)
    .bind(columns.attempts)
    .bind(columns.observed_phase)
    .bind(columns.observed_power)
    .bind(columns.observed_message)
    .bind(columns.observed_at_ms)
    .bind(columns.public_endpoint)
    .bind(columns.connected)
    .bind(columns.connection_changed_at_ms)
    .bind(columns.last_seen_at_ms)
    .bind(columns.updated_at_ms)
    .bind(columns.closed_at_ms)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn provider_from_row(row: &PgRow) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
    let record = EnvironmentProviderRecord {
        provider_id: parse_id(row, "provider_id", EnvironmentProviderId::try_new)?,
        display_name: optional_string(row, "display_name")?,
        controller_endpoint: parse_id(row, "controller_endpoint", |value| {
            EnvironmentEndpoint::parse(&value)
        })?,
        data_route_base: optional_id(row, "data_route_base", DataRouteBase::new)?,
        has_credential: row
            .try_get("has_credential")
            .map_err(|e| sql_error("decode provider credential presence", e))?,
        created_at_ms: scalar(row, "created_at_ms")?,
        updated_at_ms: scalar(row, "updated_at_ms")?,
    };
    record.validate()?;
    Ok(record)
}

fn binding_from_row(
    row: &PgRow,
) -> Result<EnvironmentProviderBindingRecord, EnvironmentRegistryError> {
    let record = EnvironmentProviderBindingRecord {
        universe_id: row
            .try_get("universe_id")
            .map_err(|e| sql_error("decode binding universe", e))?,
        provider_id: parse_id(row, "provider_id", EnvironmentProviderId::try_new)?,
        enabled: row
            .try_get("enabled")
            .map_err(|e| sql_error("decode binding enabled", e))?,
        created_at_ms: scalar(row, "created_at_ms")?,
        updated_at_ms: scalar(row, "updated_at_ms")?,
    };
    record.validate()?;
    Ok(record)
}

fn environment_from_row(row: &PgRow) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
    let source_kind = column(row, "source_kind")?;
    let source = match source_kind.as_str() {
        "provisioned" => EnvironmentSource::Provisioned(ProvisionedSource {
            provider_id: parse_id(row, "provider_id", EnvironmentProviderId::try_new)?,
            template_id: parse_id(row, "template_id", EnvironmentTemplateId::try_new)?,
            idle: optional_json_column(row, "create_idle_json")?,
            provider_target_id: optional_id(row, "provider_target_id", ProviderTargetId::new)?,
            observed: observed_from_row(row)?,
            public_endpoint: optional_string(row, "public_endpoint")?,
            next_attempt_at_ms: optional_i64(row, "next_attempt_at_ms")?,
            attempts: u32::try_from(
                row.try_get::<i32, _>("attempts")
                    .map_err(|e| sql_error("decode attempts", e))?,
            )
            .map_err(|_| store_message("attempts is negative"))?,
        }),
        "external" => EnvironmentSource::External {
            endpoint: parse_id(row, "daemon_endpoint", |value| {
                EnvironmentEndpoint::parse(&value)
            })?,
        },
        "registered" => EnvironmentSource::Registered(RegisteredSource {
            registration_key_id: parse_id(
                row,
                "registration_key_id",
                EnvironmentRegistrationKeyId::try_new,
            )?,
            daemon_public_key: column(row, "daemon_public_key")?,
            identity_mode: identity_mode_from_str(&column(row, "identity_mode")?)?,
            connected: row
                .try_get::<Option<bool>, _>("connected")
                .map_err(|e| sql_error("decode connected", e))?
                .ok_or_else(|| store_message("registered environment without connected"))?,
            connection_changed_at_ms: required_i64(row, "connection_changed_at_ms")?,
            last_seen_at_ms: required_i64(row, "last_seen_at_ms")?,
        }),
        other => {
            return Err(store_message(format!(
                "unknown environment source: {other}"
            )));
        }
    };
    let daemon_build = row
        .try_get::<Option<i32>, _>("daemon_protocol_version")
        .map_err(|e| sql_error("decode daemon protocol version", e))?
        .map(
            |protocol_version| -> Result<DaemonBuild, EnvironmentRegistryError> {
                Ok(DaemonBuild {
                    version: optional_string(row, "daemon_version")?,
                    git_sha: optional_string(row, "daemon_git_sha")?,
                    protocol_version: u32::try_from(protocol_version)
                        .map_err(|_| store_message("daemon protocol version is negative"))?,
                })
            },
        )
        .transpose()?;
    let state = column(row, "state")?;
    let record = EnvironmentRecord {
        environment_id: parse_id(row, "environment_id", EnvironmentId::try_new)?,
        request_id: optional_id(row, "request_id", EnvironmentProvisionRequestId::try_new)?,
        source,
        display_name: optional_string(row, "display_name")?,
        metadata: json_column(row, "metadata_json")?,
        state: EnvironmentState::parse(&state)
            .ok_or_else(|| store_message(format!("unknown environment state: {state}")))?,
        state_message: optional_string(row, "state_message")?,
        close_reason: optional_string(row, "close_reason")?
            .map(|reason| {
                EnvironmentCloseReason::parse(&reason)
                    .ok_or_else(|| store_message(format!("unknown close reason: {reason}")))
            })
            .transpose()?,
        state_changed_at_ms: scalar(row, "state_changed_at_ms")?,
        daemon_build,
        created_at_ms: scalar(row, "created_at_ms")?,
        updated_at_ms: scalar(row, "updated_at_ms")?,
        closed_at_ms: optional_i64(row, "closed_at_ms")?,
    };
    record.validate()?;
    Ok(record)
}

fn observed_from_row(row: &PgRow) -> Result<Option<ObservedTarget>, EnvironmentRegistryError> {
    let Some(phase) = optional_string(row, "observed_phase")? else {
        return Ok(None);
    };
    Ok(Some(ObservedTarget {
        phase: target_phase_from_str(&phase)?,
        power: optional_string(row, "observed_power")?
            .map(|power| power_state_from_str(&power))
            .transpose()?,
        message: optional_string(row, "observed_message")?,
        observed_at_ms: required_i64(row, "observed_at_ms")?,
    }))
}

pub(crate) fn identity_mode_from_str(
    value: &str,
) -> Result<RegisteredIdentityMode, EnvironmentRegistryError> {
    RegisteredIdentityMode::parse(value)
        .ok_or_else(|| store_message(format!("unknown identity mode: {value}")))
}

fn target_phase_str(phase: TargetPhase) -> &'static str {
    match phase {
        TargetPhase::Provisioning => "provisioning",
        TargetPhase::Active => "active",
        TargetPhase::Closing => "closing",
        TargetPhase::Closed => "closed",
        TargetPhase::Failed => "failed",
        TargetPhase::Unknown => "unknown",
    }
}

fn target_phase_from_str(value: &str) -> Result<TargetPhase, EnvironmentRegistryError> {
    [
        TargetPhase::Provisioning,
        TargetPhase::Active,
        TargetPhase::Closing,
        TargetPhase::Closed,
        TargetPhase::Failed,
        TargetPhase::Unknown,
    ]
    .into_iter()
    .find(|phase| target_phase_str(*phase) == value)
    .ok_or_else(|| store_message(format!("unknown target phase: {value}")))
}

fn power_state_from_str(value: &str) -> Result<PowerState, EnvironmentRegistryError> {
    PowerState::ALL
        .into_iter()
        .find(|state| state.as_str() == value)
        .ok_or_else(|| store_message(format!("unknown environment power state: {value}")))
}

fn credential_from_row(
    row: &PgRow,
) -> Result<EnvironmentCredentialRecord, EnvironmentRegistryError> {
    let kind = column(row, "source_kind")?;
    let grant = optional_string(row, "grant_id")?;
    let provider = optional_string(row, "auth_provider_id")?;
    let secret = optional_string(row, "secret_id")?;
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

// ---------------------------------------------------------------------------
// Decoding and error helpers
// ---------------------------------------------------------------------------

fn optional_json_column<T: serde::de::DeserializeOwned>(
    row: &PgRow,
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

fn parse_id<T, E>(
    row: &PgRow,
    name: &str,
    parse: impl FnOnce(String) -> Result<T, E>,
) -> Result<T, EnvironmentRegistryError>
where
    E: std::fmt::Display,
{
    parse(column(row, name)?).map_err(|e| store_message(format!("decode {name}: {e}")))
}

fn optional_id<T, E>(
    row: &PgRow,
    name: &str,
    parse: impl FnOnce(String) -> Result<T, E>,
) -> Result<Option<T>, EnvironmentRegistryError>
where
    E: std::fmt::Display,
{
    optional_string(row, name)?
        .map(parse)
        .transpose()
        .map_err(|e| store_message(format!("decode {name}: {e}")))
}

fn optional_string(row: &PgRow, name: &str) -> Result<Option<String>, EnvironmentRegistryError> {
    row.try_get(name)
        .map_err(|e| sql_error(&format!("decode {name}"), e))
}

fn optional_i64(row: &PgRow, name: &str) -> Result<Option<i64>, EnvironmentRegistryError> {
    row.try_get(name)
        .map_err(|e| sql_error(&format!("decode {name}"), e))
}

fn required_i64(row: &PgRow, name: &str) -> Result<i64, EnvironmentRegistryError> {
    optional_i64(row, name)?.ok_or_else(|| store_message(format!("missing {name}")))
}

fn column(row: &PgRow, name: &str) -> Result<String, EnvironmentRegistryError> {
    row.try_get(name)
        .map_err(|e| sql_error(&format!("decode {name}"), e))
}

fn scalar(row: &PgRow, name: &str) -> Result<i64, EnvironmentRegistryError> {
    row.try_get(name)
        .map_err(|e| sql_error(&format!("decode {name}"), e))
}

fn json_value<T: serde::Serialize>(
    action: &str,
    value: &T,
) -> Result<serde_json::Value, EnvironmentRegistryError> {
    serde_json::to_value(value).map_err(|e| store_message(format!("{action}: {e}")))
}

fn json_column<T: serde::de::DeserializeOwned>(
    row: &PgRow,
    name: &str,
) -> Result<T, EnvironmentRegistryError> {
    let value: serde_json::Value = row
        .try_get(name)
        .map_err(|e| sql_error("decode json column", e))?;
    serde_json::from_value(value).map_err(|e| store_message(format!("decode {name}: {e}")))
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

fn store_error(action: &str, error: PgStoreError) -> EnvironmentRegistryError {
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

pub(crate) fn constraint_of(error: &sqlx::Error) -> Option<&str> {
    error.as_database_error().and_then(|db| db.constraint())
}

fn map_environment_write_error(action: &str, error: sqlx::Error) -> EnvironmentRegistryError {
    match constraint_of(&error) {
        Some("environments_pkey") => EnvironmentRegistryError::AlreadyExists {
            kind: "environment",
            id: "duplicate environment id".to_owned(),
        },
        Some("environments_provider_target_idx") => EnvironmentRegistryError::AlreadyExists {
            kind: "provider_target",
            id: "target recorded for another open environment".to_owned(),
        },
        Some("environments_registration_key_fk") => {
            invalid_error("environment registration key does not exist")
        }
        _ => sql_error(action, error),
    }
}

fn map_binding_write_error(error: sqlx::Error, universe_id: Uuid) -> EnvironmentRegistryError {
    if error
        .as_database_error()
        .is_some_and(|database| database.code().as_deref() == Some("23503"))
    {
        return not_found("universe", &universe_id);
    }
    sql_error("put provider binding", error)
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

/// Decode every row that still decodes. A row that does not is left out
/// and logged, so one bad row never hides the rest of a listing or stalls
/// a reconciler pass over them.
pub(crate) fn decode_rows<T>(
    rows: &[PgRow],
    decode: impl Fn(&PgRow) -> Result<T, EnvironmentRegistryError>,
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

/// A string map as a jsonb object: the stored caller metadata and the `@>`
/// containment filter compared against it. Building the value directly
/// cannot fail.
fn metadata_filter_json(metadata: &BTreeMap<String, String>) -> serde_json::Value {
    serde_json::Value::Object(
        metadata
            .iter()
            .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
            .collect(),
    )
}
