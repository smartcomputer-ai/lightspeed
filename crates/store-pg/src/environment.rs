use async_trait::async_trait;
use auth::{AuthGrantId, AuthProviderId, SecretId};
use environments::{
    BeginCloseEnvironment, CreateEnvironment, CreateExternalEnvironment,
    EnvironmentCredentialRecord, EnvironmentCredentialSource, EnvironmentCredentialStore,
    EnvironmentId, EnvironmentIncarnationId, EnvironmentIncarnationRecord,
    EnvironmentProviderBindingId, EnvironmentProviderBindingRecord,
    EnvironmentProviderBindingStatus, EnvironmentProviderBindingStore, EnvironmentProviderId,
    EnvironmentProviderRecord, EnvironmentProviderStore, EnvironmentProvisionRequestId,
    EnvironmentRecord, EnvironmentRegistryError, EnvironmentSource, EnvironmentStatus,
    EnvironmentStore, EnvironmentTemplateId, FailEnvironmentLifecycle, FinishCloseEnvironment,
    ListEnvironmentCredentials, ListEnvironmentProviders, ListEnvironments,
    ObserveProvisionedEnvironment, PutEnvironmentCredential, PutEnvironmentProvider,
    PutEnvironmentProviderBinding,
};
use host_protocol::shared::HostTargetId;
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::PgStore;

const PROVIDER_COLUMNS: &str = r#"
    provider_id, display_name, controller_connection_json, metadata_json,
    created_at_ms, updated_at_ms
"#;

const BINDING_COLUMNS: &str = r#"
    universe_id, binding_id, provider_id, status, revision, metadata_json,
    created_at_ms, updated_at_ms
"#;

const ENVIRONMENT_COLUMNS: &str = r#"
    e.environment_id, e.request_id, e.source_kind, e.provider_id, e.binding_id, e.daemon_connection_json,
    e.display_name, e.status, e.metadata_json,
    e.created_at_ms, e.updated_at_ms,
    i.incarnation_id, i.provision_request_id, i.provider_target_id,
    i.template_id,
    i.created_at_ms AS incarnation_created_at_ms,
    i.updated_at_ms AS incarnation_updated_at_ms
"#;

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
        let record = request.into_record()?;
        let query = format!(
            r#"
            INSERT INTO environment_providers (
                provider_id, display_name, controller_connection_json,
                metadata_json, created_at_ms, updated_at_ms
            ) VALUES ($1,$2,$3,$4,$5,$6)
            ON CONFLICT (provider_id) DO UPDATE SET
                display_name = EXCLUDED.display_name,
                controller_connection_json = EXCLUDED.controller_connection_json,
                metadata_json = EXCLUDED.metadata_json,
                updated_at_ms = EXCLUDED.updated_at_ms
            RETURNING {PROVIDER_COLUMNS}
        "#
        );
        let row = sqlx::query(&query)
            .bind(record.provider_id.as_str())
            .bind(record.display_name.as_deref())
            .bind(json_value(
                "encode provider controller connection",
                &record.controller_connection,
            )?)
            .bind(json_value("encode provider metadata", &record.metadata)?)
            .bind(record.created_at_ms)
            .bind(record.updated_at_ms)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| sql_error("put environment provider", error))?;
        provider_from_row(&row)
    }

    async fn read_provider(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        let query =
            format!("SELECT {PROVIDER_COLUMNS} FROM environment_providers WHERE provider_id = $1");
        let row = sqlx::query(&query)
            .bind(provider_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| sql_error("read environment provider", error))?
            .ok_or_else(|| not_found("environment_provider", provider_id))?;
        provider_from_row(&row)
    }

    async fn list_providers(
        &self,
        _request: ListEnvironmentProviders,
    ) -> Result<Vec<EnvironmentProviderRecord>, EnvironmentRegistryError> {
        let query =
            format!("SELECT {PROVIDER_COLUMNS} FROM environment_providers ORDER BY provider_id");
        let rows = sqlx::query(&query)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| sql_error("list environment providers", error))?;
        rows.iter().map(provider_from_row).collect()
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
        let row = sqlx::query(&query)
            .bind(provider_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_provider_delete_error)?
            .ok_or_else(|| not_found("environment_provider", provider_id))?;
        provider_from_row(&row)
    }
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
        let row = sqlx::query(&query)
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
        let row = sqlx::query(&query)
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
        let rows = sqlx::query(&query)
            .bind(universe_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| sql_error("list provider bindings", error))?;
        rows.iter().map(binding_from_row).collect()
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
        let row = sqlx::query(&query)
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
        let binding_row = sqlx::query(&query)
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
        .map_err(|error| map_environment_insert_error(error))?;
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
        let candidate = EnvironmentRecord {
            environment_id: request.environment_id.clone(),
            request_id: request.request_id.clone(),
            source: EnvironmentSource::External {
                connection: request.connection.clone(),
            },
            display_name: request.display_name.clone(),
            status: EnvironmentStatus::Ready,
            incarnation: EnvironmentIncarnationRecord {
                incarnation_id: request.incarnation_id.clone(),
                provision_request_id: None,
                provider_target_id: None,
                template_id: None,
                created_at_ms: request.created_at_ms,
                updated_at_ms: request.created_at_ms,
            },
            metadata: request.metadata.clone(),
            created_at_ms: request.created_at_ms,
            updated_at_ms: request.created_at_ms,
        };
        candidate.validate()?;
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
                universe_id, environment_id, request_id, source_kind, daemon_connection_json,
                display_name, status, current_incarnation_id, metadata_json,
                created_at_ms, updated_at_ms
            ) VALUES ($1,$2,$3,'external',$4,$5,'ready',$6,$7,$8,$8)
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.environment_id.as_str())
        .bind(request.request_id.as_str())
        .bind(json_value("encode daemon connection", &request.connection)?)
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
        }
        query.push_str(" ORDER BY e.environment_id");
        let mut sql = sqlx::query(&query).bind(self.config.universe_id);
        if let Some(id) = request.provider_id {
            sql = sql.bind(id.to_string());
        }
        if let Some(id) = request.binding_id {
            sql = sql.bind(id.to_string());
        }
        if let Some(status) = request.status {
            sql = sql.bind(environment_status_to_str(status));
        }
        let rows = sql
            .fetch_all(&self.pool)
            .await
            .map_err(|error| sql_error("list environments", error))?;
        rows.iter().map(environment_from_row).collect()
    }

    async fn list_environments_needing_reconcile(
        &self,
    ) -> Result<Vec<EnvironmentRecord>, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {ENVIRONMENT_COLUMNS} {ENVIRONMENT_JOIN} WHERE e.universe_id = $1 AND e.status IN ('provisioning','booting','closing','unknown') ORDER BY e.updated_at_ms, e.environment_id"
        );
        let rows = sqlx::query(&query)
            .bind(self.config.universe_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| sql_error("list environments needing reconcile", error))?;
        rows.iter().map(environment_from_row).collect()
    }

    async fn observe_provisioned_environment(
        &self,
        request: ObserveProvisionedEnvironment,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let current = self.read_environment(&request.environment_id).await?;
        if request.observed_at_ms < current.incarnation.updated_at_ms {
            return Ok(current);
        }
        if current
            .incarnation
            .provider_target_id
            .as_ref()
            .is_some_and(|id| id != &request.provider_target_id)
        {
            return invalid("provider target conflicts with current incarnation");
        }
        sqlx::query("UPDATE environment_incarnations SET provider_target_id=$4, updated_at_ms=$5 WHERE universe_id=$1 AND environment_id=$2 AND incarnation_id=$3")
            .bind(self.config.universe_id).bind(request.environment_id.as_str()).bind(current.incarnation.incarnation_id.as_str())
            .bind(request.provider_target_id.as_str()).bind(request.observed_at_ms)
            .execute(&self.pool).await.map_err(|error| sql_error("observe environment incarnation", error))?;
        sqlx::query("UPDATE environments SET status=$3, updated_at_ms=$4 WHERE universe_id=$1 AND environment_id=$2")
            .bind(self.config.universe_id).bind(request.environment_id.as_str()).bind(environment_status_to_str(request.status)).bind(request.observed_at_ms)
            .execute(&self.pool).await.map_err(|error| sql_error("observe environment", error))?;
        self.read_environment(&request.environment_id).await
    }

    async fn fail_environment_lifecycle(
        &self,
        request: FailEnvironmentLifecycle,
    ) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
        let mut metadata = self
            .read_environment(&request.environment_id)
            .await?
            .metadata;
        metadata.insert("lifecycleError".to_owned(), request.message);
        sqlx::query("UPDATE environments SET status='failed', metadata_json=$3, updated_at_ms=$4 WHERE universe_id=$1 AND environment_id=$2")
            .bind(self.config.universe_id).bind(request.environment_id.as_str()).bind(json_value("encode environment metadata", &metadata)?).bind(request.observed_at_ms)
            .execute(&self.pool).await.map_err(|error| sql_error("fail environment", error))?;
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
        sqlx::query("UPDATE environments SET status='closed', updated_at_ms=$3 WHERE universe_id=$1 AND environment_id=$2")
            .bind(self.config.universe_id).bind(request.environment_id.as_str()).bind(request.observed_at_ms)
            .execute(&mut *tx).await.map_err(|error| sql_error("finish environment close", error))?;
        tx.commit()
            .await
            .map_err(|error| sql_error("commit environment close", error))?;
        self.read_environment(&request.environment_id).await
    }
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
        let row = sqlx::query(&query)
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
        let rows = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(request.environment_id.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|error| sql_error("list environment credentials", error))?;
        rows.iter().map(credential_from_row).collect()
    }

    async fn unbind_credential(
        &self,
        environment_id: &EnvironmentId,
        env_name: &str,
    ) -> Result<EnvironmentCredentialRecord, EnvironmentRegistryError> {
        let query = format!(
            "DELETE FROM environment_credentials WHERE universe_id=$1 AND environment_id=$2 AND env_name=$3 RETURNING {CREDENTIAL_COLUMNS}"
        );
        let row = sqlx::query(&query)
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
    predicate: &str,
    value: &str,
    action: &str,
) -> Result<EnvironmentRecord, EnvironmentRegistryError> {
    let query = format!(
        "SELECT {ENVIRONMENT_COLUMNS} {ENVIRONMENT_JOIN} WHERE e.universe_id = $1 AND {predicate}"
    );
    let row = sqlx::query(&query)
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
    let connection: environments::HostControllerConnectionSpec =
        json_column(row, "controller_connection_json")?;
    let record = EnvironmentProviderRecord {
        provider_id,
        display_name: row
            .try_get("display_name")
            .map_err(|e| sql_error("decode provider display_name", e))?,
        controller_connection: connection,
        metadata: json_column(row, "metadata_json")?,
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
            connection: json_column(row, "daemon_connection_json")?,
        },
        other => {
            return Err(store_message(format!(
                "unknown environment source: {other}"
            )));
        }
    };
    let target: Option<String> = row
        .try_get("provider_target_id")
        .map_err(|e| sql_error("decode provider target", e))?;
    let record = EnvironmentRecord {
        environment_id: parse_id(row, "environment_id", EnvironmentId::try_new)?,
        request_id: parse_id(row, "request_id", EnvironmentProvisionRequestId::try_new)?,
        source,
        display_name: row
            .try_get("display_name")
            .map_err(|e| sql_error("decode environment display", e))?,
        status: environment_status_from_str(&column(row, "status")?)?,
        incarnation: EnvironmentIncarnationRecord {
            incarnation_id: parse_id(row, "incarnation_id", EnvironmentIncarnationId::try_new)?,
            provision_request_id: optional_id(
                row,
                "provision_request_id",
                EnvironmentProvisionRequestId::try_new,
            )?,
            provider_target_id: target.map(HostTargetId::new),
            template_id: optional_id(row, "template_id", EnvironmentTemplateId::try_new)?,
            created_at_ms: scalar(row, "incarnation_created_at_ms")?,
            updated_at_ms: scalar(row, "incarnation_updated_at_ms")?,
        },
        metadata: json_column(row, "metadata_json")?,
        created_at_ms: scalar(row, "created_at_ms")?,
        updated_at_ms: scalar(row, "updated_at_ms")?,
    };
    record.validate()?;
    Ok(record)
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
fn environment_status_to_str(value: EnvironmentStatus) -> &'static str {
    match value {
        EnvironmentStatus::Provisioning => "provisioning",
        EnvironmentStatus::Booting => "booting",
        EnvironmentStatus::Ready => "ready",
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
fn not_found(kind: &'static str, id: &impl ToString) -> EnvironmentRegistryError {
    EnvironmentRegistryError::NotFound {
        kind,
        id: id.to_string(),
    }
}
fn invalid<T>(message: impl Into<String>) -> Result<T, EnvironmentRegistryError> {
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
fn sql_error(action: &str, error: sqlx::Error) -> EnvironmentRegistryError {
    store_message(format!("{action}: {error}"))
}
fn store_message(message: impl Into<String>) -> EnvironmentRegistryError {
    EnvironmentRegistryError::Store {
        message: message.into(),
    }
}
fn map_environment_insert_error(error: sqlx::Error) -> EnvironmentRegistryError {
    if let Some(db) = error.as_database_error() {
        if db.constraint() == Some("environments_pkey") {
            return EnvironmentRegistryError::AlreadyExists {
                kind: "environment",
                id: "duplicate environment id".to_owned(),
            };
        }
    }
    sql_error("insert environment", error)
}
fn map_binding_write_error(action: &str, error: sqlx::Error) -> EnvironmentRegistryError {
    if let Some(db) = error.as_database_error() {
        if db.constraint() == Some("environment_provider_bindings_universe_id_provider_id_key") {
            return EnvironmentRegistryError::AlreadyExists {
                kind: "environment_provider_binding",
                id: "universe/provider".to_owned(),
            };
        }
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
