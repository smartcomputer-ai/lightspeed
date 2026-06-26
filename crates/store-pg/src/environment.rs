use async_trait::async_trait;
use auth::{AuthGrantId, AuthProviderId, SecretId};
use engine::SessionId;
use environments::{
    CreateSessionEnvironmentBinding, CreateSessionEnvironmentCredential, EnvironmentId,
    EnvironmentProviderHeartbeat, EnvironmentProviderId, EnvironmentProviderKind,
    EnvironmentProviderRecord, EnvironmentProviderStatus, EnvironmentProviderStore,
    EnvironmentRegistryError, EnvironmentTargetRecord, EnvironmentTargetStore,
    ListEnvironmentProviders, ListEnvironmentTargets, ListSessionEnvironmentCredentials,
    RegisterEnvironmentProvider, SessionEnvironmentBindingRecord, SessionEnvironmentBindingStatus,
    SessionEnvironmentBindingStore, SessionEnvironmentCredentialRecord,
    SessionEnvironmentCredentialSource, SessionEnvironmentCredentialStore, SessionEnvironmentKind,
    UpdateEnvironmentProviderStatus, UpdateEnvironmentTargetStatus,
    UpdateSessionEnvironmentBindingStatus, UpsertEnvironmentTargetRecord,
};
use host_protocol::{
    control::targets::HostTargetStatus,
    shared::{HostPath, HostTargetId},
};
use sqlx::Row;

use crate::PgStore;

const PROVIDER_COLUMNS: &str = r#"
    provider_id,
    provider_kind,
    display_name,
    status,
    controller_connection_json,
    capabilities_json,
    implementation_json,
    last_seen_ms,
    lease_expires_ms,
    metadata_json,
    created_at_ms,
    updated_at_ms
"#;

const TARGET_COLUMNS: &str = r#"
    provider_id,
    target_id,
    display_name,
    status,
    scope_json,
    capabilities_json,
    default_cwd,
    metadata_json,
    observed_at_ms
"#;

const BINDING_COLUMNS: &str = r#"
    session_id,
    env_id,
    provider_id,
    target_id,
    exec_target_json,
    kind,
    status,
    capabilities_json,
    connection_json,
    cwd,
    fs_routes_json,
    created_at_ms,
    updated_at_ms
"#;

const CREDENTIAL_COLUMNS: &str = r#"
    session_id,
    env_id,
    env_name,
    source_kind,
    grant_id,
    auth_provider_id,
    secret_id,
    created_at_ms,
    updated_at_ms
"#;

#[async_trait]
impl EnvironmentProviderStore for PgStore {
    async fn register_provider(
        &self,
        record: RegisterEnvironmentProvider,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        self.ensure_universe()
            .await
            .map_err(|error| environment_store_error("ensure universe", error))?;
        let record = record.into_record()?;
        let controller_connection_json = json_value(
            "encode provider controller connection",
            &record.controller_connection,
        )?;
        let capabilities_json = json_value("encode provider capabilities", &record.capabilities)?;
        let implementation_json =
            json_value("encode provider implementation", &record.implementation)?;
        let metadata_json = json_value("encode provider metadata", &record.metadata)?;
        let query = format!(
            r#"
            INSERT INTO environment_providers (
                universe_id,
                provider_id,
                provider_kind,
                display_name,
                status,
                controller_connection_json,
                capabilities_json,
                implementation_json,
                last_seen_ms,
                lease_expires_ms,
                metadata_json,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            ON CONFLICT (universe_id, provider_id) DO UPDATE SET
                provider_kind = EXCLUDED.provider_kind,
                display_name = EXCLUDED.display_name,
                status = EXCLUDED.status,
                controller_connection_json = EXCLUDED.controller_connection_json,
                capabilities_json = EXCLUDED.capabilities_json,
                implementation_json = EXCLUDED.implementation_json,
                last_seen_ms = EXCLUDED.last_seen_ms,
                lease_expires_ms = EXCLUDED.lease_expires_ms,
                metadata_json = EXCLUDED.metadata_json,
                updated_at_ms = EXCLUDED.updated_at_ms
            RETURNING {PROVIDER_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(record.provider_id.as_str())
            .bind(provider_kind_to_str(record.provider_kind))
            .bind(record.display_name.as_deref())
            .bind(provider_status_to_str(record.status))
            .bind(controller_connection_json)
            .bind(capabilities_json)
            .bind(implementation_json)
            .bind(record.last_seen_ms)
            .bind(record.lease_expires_ms)
            .bind(metadata_json)
            .bind(record.created_at_ms)
            .bind(record.updated_at_ms)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| environment_sql_error("register environment provider", error))?;
        provider_from_row(&row)
    }

    async fn read_provider(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        let query = format!(
            r#"
            SELECT {PROVIDER_COLUMNS}
            FROM environment_providers
            WHERE universe_id = $1 AND provider_id = $2
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(provider_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| environment_sql_error("read environment provider", error))?;
        let Some(row) = row else {
            return Err(provider_not_found(provider_id));
        };
        provider_from_row(&row)
    }

    async fn list_providers(
        &self,
        request: ListEnvironmentProviders,
    ) -> Result<Vec<EnvironmentProviderRecord>, EnvironmentRegistryError> {
        let mut query = format!(
            r#"
            SELECT {PROVIDER_COLUMNS}
            FROM environment_providers
            WHERE universe_id = $1
            "#
        );
        if request.status.is_some() {
            query.push_str(" AND status = $2");
        }
        if request.provider_kind.is_some() {
            query.push_str(if request.status.is_some() {
                " AND provider_kind = $3"
            } else {
                " AND provider_kind = $2"
            });
        }
        query.push_str(" ORDER BY provider_id");
        let mut sql = sqlx::query(&query).bind(self.config.universe_id);
        if let Some(status) = request.status {
            sql = sql.bind(provider_status_to_str(status));
        }
        if let Some(kind) = request.provider_kind {
            sql = sql.bind(provider_kind_to_str(kind));
        }
        let rows = sql
            .fetch_all(&self.pool)
            .await
            .map_err(|error| environment_sql_error("list environment providers", error))?;
        rows.iter().map(provider_from_row).collect()
    }

    async fn update_provider_heartbeat(
        &self,
        heartbeat: EnvironmentProviderHeartbeat,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(heartbeat.observed_at_ms, "observed_at_ms")?;
        if let Some(ttl) = heartbeat.lease_ttl_ms {
            validate_positive_i64(ttl, "lease_ttl_ms")?;
        }
        let current = self.read_provider(&heartbeat.provider_id).await?;
        let ttl = heartbeat.lease_ttl_ms.unwrap_or_else(|| {
            current
                .lease_expires_ms
                .saturating_sub(current.last_seen_ms)
        });
        validate_positive_i64(ttl, "lease_ttl_ms")?;
        let lease_expires_ms = heartbeat.observed_at_ms.checked_add(ttl).ok_or_else(|| {
            EnvironmentRegistryError::InvalidInput {
                message: "lease expiry timestamp overflowed".to_owned(),
            }
        })?;
        let query = format!(
            r#"
            UPDATE environment_providers
            SET status = 'online',
                last_seen_ms = $3,
                lease_expires_ms = $4,
                updated_at_ms = $3
            WHERE universe_id = $1 AND provider_id = $2
            RETURNING {PROVIDER_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(heartbeat.provider_id.as_str())
            .bind(heartbeat.observed_at_ms)
            .bind(lease_expires_ms)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| environment_sql_error("heartbeat environment provider", error))?;
        provider_from_row(&row)
    }

    async fn update_provider_status(
        &self,
        request: UpdateEnvironmentProviderStatus,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.updated_at_ms, "updated_at_ms")?;
        let query = format!(
            r#"
            UPDATE environment_providers
            SET status = $3,
                updated_at_ms = $4
            WHERE universe_id = $1 AND provider_id = $2
            RETURNING {PROVIDER_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(request.provider_id.as_str())
            .bind(provider_status_to_str(request.status))
            .bind(request.updated_at_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| environment_sql_error("update environment provider status", error))?;
        let Some(row) = row else {
            return Err(provider_not_found(&request.provider_id));
        };
        provider_from_row(&row)
    }

    async fn delete_provider(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
        let query = format!(
            r#"
            DELETE FROM environment_providers
            WHERE universe_id = $1 AND provider_id = $2
            RETURNING {PROVIDER_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(provider_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| environment_sql_error("delete environment provider", error))?;
        let Some(row) = row else {
            return Err(provider_not_found(provider_id));
        };
        provider_from_row(&row)
    }
}

#[async_trait]
impl EnvironmentTargetStore for PgStore {
    async fn upsert_target(
        &self,
        record: UpsertEnvironmentTargetRecord,
    ) -> Result<EnvironmentTargetRecord, EnvironmentRegistryError> {
        self.ensure_universe()
            .await
            .map_err(|error| environment_store_error("ensure universe", error))?;
        let record = record.into_record();
        record.validate()?;
        let scope_json = json_value("encode target scope", &record.scope)?;
        let capabilities_json = json_value("encode target capabilities", &record.capabilities)?;
        let metadata_json = json_value("encode target metadata", &record.metadata)?;
        let query = format!(
            r#"
            INSERT INTO environment_targets (
                universe_id,
                provider_id,
                target_id,
                display_name,
                status,
                scope_json,
                capabilities_json,
                default_cwd,
                metadata_json,
                observed_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            ON CONFLICT (universe_id, provider_id, target_id) DO UPDATE SET
                display_name = EXCLUDED.display_name,
                status = EXCLUDED.status,
                scope_json = EXCLUDED.scope_json,
                capabilities_json = EXCLUDED.capabilities_json,
                default_cwd = EXCLUDED.default_cwd,
                metadata_json = EXCLUDED.metadata_json,
                observed_at_ms = EXCLUDED.observed_at_ms
            RETURNING {TARGET_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(record.provider_id.as_str())
            .bind(record.target_id.as_str())
            .bind(record.display_name.as_deref())
            .bind(host_target_status_to_str(record.status))
            .bind(scope_json)
            .bind(capabilities_json)
            .bind(record.default_cwd.as_ref().map(HostPath::as_str))
            .bind(metadata_json)
            .bind(record.observed_at_ms)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| environment_sql_error("upsert environment target", error))?;
        target_from_row(&row)
    }

    async fn read_target(
        &self,
        provider_id: &EnvironmentProviderId,
        target_id: &HostTargetId,
    ) -> Result<EnvironmentTargetRecord, EnvironmentRegistryError> {
        let query = format!(
            r#"
            SELECT {TARGET_COLUMNS}
            FROM environment_targets
            WHERE universe_id = $1 AND provider_id = $2 AND target_id = $3
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(provider_id.as_str())
            .bind(target_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| environment_sql_error("read environment target", error))?;
        let Some(row) = row else {
            return Err(target_not_found(provider_id, target_id));
        };
        target_from_row(&row)
    }

    async fn list_targets(
        &self,
        request: ListEnvironmentTargets,
    ) -> Result<Vec<EnvironmentTargetRecord>, EnvironmentRegistryError> {
        let mut query = format!(
            r#"
            SELECT {TARGET_COLUMNS}
            FROM environment_targets
            WHERE universe_id = $1
            "#
        );
        if request.provider_id.is_some() {
            query.push_str(" AND provider_id = $2");
        }
        if request.status.is_some() {
            query.push_str(if request.provider_id.is_some() {
                " AND status = $3"
            } else {
                " AND status = $2"
            });
        }
        query.push_str(" ORDER BY provider_id, target_id");
        let mut sql = sqlx::query(&query).bind(self.config.universe_id);
        if let Some(provider_id) = request.provider_id {
            sql = sql.bind(provider_id.as_str().to_owned());
        }
        if let Some(status) = request.status {
            sql = sql.bind(host_target_status_to_str(status));
        }
        let rows = sql
            .fetch_all(&self.pool)
            .await
            .map_err(|error| environment_sql_error("list environment targets", error))?;
        rows.iter().map(target_from_row).collect()
    }

    async fn update_target_status(
        &self,
        request: UpdateEnvironmentTargetStatus,
    ) -> Result<EnvironmentTargetRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.observed_at_ms, "observed_at_ms")?;
        let query = format!(
            r#"
            UPDATE environment_targets
            SET status = $4,
                observed_at_ms = $5
            WHERE universe_id = $1 AND provider_id = $2 AND target_id = $3
            RETURNING {TARGET_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(request.provider_id.as_str())
            .bind(request.target_id.as_str())
            .bind(host_target_status_to_str(request.status))
            .bind(request.observed_at_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| environment_sql_error("update environment target status", error))?;
        let Some(row) = row else {
            return Err(target_not_found(&request.provider_id, &request.target_id));
        };
        target_from_row(&row)
    }
}

#[async_trait]
impl SessionEnvironmentBindingStore for PgStore {
    async fn create_binding(
        &self,
        record: CreateSessionEnvironmentBinding,
    ) -> Result<SessionEnvironmentBindingRecord, EnvironmentRegistryError> {
        self.ensure_universe()
            .await
            .map_err(|error| environment_store_error("ensure universe", error))?;
        let record = record.into_record();
        record.validate()?;
        let exec_target_json = json_value("encode binding exec target", &record.exec_target)?;
        let capabilities_json = json_value("encode binding capabilities", &record.capabilities)?;
        let connection_json = json_value("encode binding connection", &record.connection)?;
        let fs_routes_json = json_value("encode binding fs routes", &record.fs_routes)?;
        let query = format!(
            r#"
            INSERT INTO session_environment_bindings (
                universe_id,
                session_id,
                env_id,
                provider_id,
                target_id,
                exec_target_json,
                kind,
                status,
                capabilities_json,
                connection_json,
                cwd,
                fs_routes_json,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $13)
            ON CONFLICT (universe_id, session_id, env_id) DO NOTHING
            RETURNING {BINDING_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(record.session_id.as_str())
            .bind(record.env_id.as_str())
            .bind(record.provider_id.as_str())
            .bind(record.target_id.as_str())
            .bind(exec_target_json)
            .bind(binding_kind_to_str(record.kind))
            .bind(binding_status_to_str(record.status))
            .bind(capabilities_json)
            .bind(connection_json)
            .bind(record.cwd.as_ref().map(HostPath::as_str))
            .bind(fs_routes_json)
            .bind(record.created_at_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| environment_sql_error("create session environment binding", error))?;
        let Some(row) = row else {
            return Err(EnvironmentRegistryError::AlreadyExists {
                kind: "session_environment_binding",
                id: format!("{}/{}", record.session_id, record.env_id.as_str()),
            });
        };
        binding_from_row(&row)
    }

    async fn read_binding(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
    ) -> Result<SessionEnvironmentBindingRecord, EnvironmentRegistryError> {
        let query = format!(
            r#"
            SELECT {BINDING_COLUMNS}
            FROM session_environment_bindings
            WHERE universe_id = $1 AND session_id = $2 AND env_id = $3
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(session_id.as_str())
            .bind(env_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| environment_sql_error("read session environment binding", error))?;
        let Some(row) = row else {
            return Err(binding_not_found(session_id, env_id));
        };
        binding_from_row(&row)
    }

    async fn list_bindings_for_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<SessionEnvironmentBindingRecord>, EnvironmentRegistryError> {
        let query = format!(
            r#"
            SELECT {BINDING_COLUMNS}
            FROM session_environment_bindings
            WHERE universe_id = $1 AND session_id = $2
            ORDER BY env_id
            "#
        );
        let rows = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(session_id.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|error| environment_sql_error("list session environment bindings", error))?;
        rows.iter().map(binding_from_row).collect()
    }

    async fn update_binding_status(
        &self,
        request: UpdateSessionEnvironmentBindingStatus,
    ) -> Result<SessionEnvironmentBindingRecord, EnvironmentRegistryError> {
        validate_nonnegative_i64(request.updated_at_ms, "updated_at_ms")?;
        let query = format!(
            r#"
            UPDATE session_environment_bindings
            SET status = $4,
                updated_at_ms = $5
            WHERE universe_id = $1 AND session_id = $2 AND env_id = $3
            RETURNING {BINDING_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(request.session_id.as_str())
            .bind(request.env_id.as_str())
            .bind(binding_status_to_str(request.status))
            .bind(request.updated_at_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| environment_sql_error("update session environment binding", error))?;
        let Some(row) = row else {
            return Err(binding_not_found(&request.session_id, &request.env_id));
        };
        binding_from_row(&row)
    }

    async fn delete_binding(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
    ) -> Result<SessionEnvironmentBindingRecord, EnvironmentRegistryError> {
        let query = format!(
            r#"
            DELETE FROM session_environment_bindings
            WHERE universe_id = $1 AND session_id = $2 AND env_id = $3
            RETURNING {BINDING_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(session_id.as_str())
            .bind(env_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| environment_sql_error("delete session environment binding", error))?;
        let Some(row) = row else {
            return Err(binding_not_found(session_id, env_id));
        };
        binding_from_row(&row)
    }
}

#[async_trait]
impl SessionEnvironmentCredentialStore for PgStore {
    async fn bind_credential(
        &self,
        record: CreateSessionEnvironmentCredential,
    ) -> Result<SessionEnvironmentCredentialRecord, EnvironmentRegistryError> {
        let record = record.into_record();
        record.validate()?;
        let (source_kind, grant_id, auth_provider_id, secret_id) =
            credential_source_columns(&record.source);
        let query = format!(
            r#"
            INSERT INTO session_environment_credentials (
                universe_id,
                session_id,
                env_id,
                env_name,
                source_kind,
                grant_id,
                auth_provider_id,
                secret_id,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9)
            ON CONFLICT (universe_id, session_id, env_id, env_name) DO UPDATE SET
                source_kind = EXCLUDED.source_kind,
                grant_id = EXCLUDED.grant_id,
                auth_provider_id = EXCLUDED.auth_provider_id,
                secret_id = EXCLUDED.secret_id,
                updated_at_ms = EXCLUDED.updated_at_ms
            RETURNING {CREDENTIAL_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(record.session_id.as_str())
            .bind(record.env_id.as_str())
            .bind(&record.env_name)
            .bind(source_kind)
            .bind(grant_id)
            .bind(auth_provider_id)
            .bind(secret_id)
            .bind(record.created_at_ms)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| environment_sql_error("bind session environment credential", error))?;
        credential_from_row(&row)
    }

    async fn list_credentials(
        &self,
        request: ListSessionEnvironmentCredentials,
    ) -> Result<Vec<SessionEnvironmentCredentialRecord>, EnvironmentRegistryError> {
        SessionEnvironmentBindingStore::read_binding(self, &request.session_id, &request.env_id)
            .await?;
        let query = format!(
            r#"
            SELECT {CREDENTIAL_COLUMNS}
            FROM session_environment_credentials
            WHERE universe_id = $1 AND session_id = $2 AND env_id = $3
            ORDER BY env_name
            "#
        );
        let rows = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(request.session_id.as_str())
            .bind(request.env_id.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|error| {
                environment_sql_error("list session environment credentials", error)
            })?;
        rows.iter().map(credential_from_row).collect()
    }

    async fn unbind_credential(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
        env_name: &str,
    ) -> Result<SessionEnvironmentCredentialRecord, EnvironmentRegistryError> {
        let query = format!(
            r#"
            DELETE FROM session_environment_credentials
            WHERE universe_id = $1 AND session_id = $2 AND env_id = $3 AND env_name = $4
            RETURNING {CREDENTIAL_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(session_id.as_str())
            .bind(env_id.as_str())
            .bind(env_name)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| {
                environment_sql_error("unbind session environment credential", error)
            })?;
        let Some(row) = row else {
            return Err(EnvironmentRegistryError::NotFound {
                kind: "session_environment_credential",
                id: format!("{session_id}/{}/{env_name}", env_id.as_str()),
            });
        };
        credential_from_row(&row)
    }
}

fn provider_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<EnvironmentProviderRecord, EnvironmentRegistryError> {
    let provider_id: String = row
        .try_get("provider_id")
        .map_err(|error| environment_sql_error("decode provider id", error))?;
    let provider_kind: String = row
        .try_get("provider_kind")
        .map_err(|error| environment_sql_error("decode provider kind", error))?;
    let status: String = row
        .try_get("status")
        .map_err(|error| environment_sql_error("decode provider status", error))?;
    let record = EnvironmentProviderRecord {
        provider_id: EnvironmentProviderId::try_new(provider_id).map_err(|error| {
            EnvironmentRegistryError::Store {
                message: format!("decode provider id: {error}"),
            }
        })?,
        provider_kind: provider_kind_from_str(&provider_kind)?,
        display_name: row
            .try_get("display_name")
            .map_err(|error| environment_sql_error("decode provider display name", error))?,
        status: provider_status_from_str(&status)?,
        controller_connection: json_column(row, "controller_connection_json")?,
        capabilities: json_column(row, "capabilities_json")?,
        implementation: json_column(row, "implementation_json")?,
        last_seen_ms: row
            .try_get("last_seen_ms")
            .map_err(|error| environment_sql_error("decode provider last_seen_ms", error))?,
        lease_expires_ms: row
            .try_get("lease_expires_ms")
            .map_err(|error| environment_sql_error("decode provider lease_expires_ms", error))?,
        metadata: json_column(row, "metadata_json")?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|error| environment_sql_error("decode provider created_at_ms", error))?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|error| environment_sql_error("decode provider updated_at_ms", error))?,
    };
    record.validate()?;
    Ok(record)
}

fn target_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<EnvironmentTargetRecord, EnvironmentRegistryError> {
    let provider_id: String = row
        .try_get("provider_id")
        .map_err(|error| environment_sql_error("decode target provider id", error))?;
    let target_id: String = row
        .try_get("target_id")
        .map_err(|error| environment_sql_error("decode target id", error))?;
    let status: String = row
        .try_get("status")
        .map_err(|error| environment_sql_error("decode target status", error))?;
    let default_cwd: Option<String> = row
        .try_get("default_cwd")
        .map_err(|error| environment_sql_error("decode target default cwd", error))?;
    let record = EnvironmentTargetRecord {
        provider_id: EnvironmentProviderId::try_new(provider_id).map_err(|error| {
            EnvironmentRegistryError::Store {
                message: format!("decode target provider id: {error}"),
            }
        })?,
        target_id: HostTargetId::new(target_id),
        display_name: row
            .try_get("display_name")
            .map_err(|error| environment_sql_error("decode target display name", error))?,
        status: host_target_status_from_str(&status)?,
        scope: json_column(row, "scope_json")?,
        capabilities: json_column(row, "capabilities_json")?,
        default_cwd: default_cwd
            .as_deref()
            .map(HostPath::new)
            .transpose()
            .map_err(|error| EnvironmentRegistryError::Store {
                message: format!("decode target default cwd: {error}"),
            })?,
        metadata: json_column(row, "metadata_json")?,
        observed_at_ms: row
            .try_get("observed_at_ms")
            .map_err(|error| environment_sql_error("decode target observed_at_ms", error))?,
    };
    record.validate()?;
    Ok(record)
}

fn binding_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<SessionEnvironmentBindingRecord, EnvironmentRegistryError> {
    let session_id: String = row
        .try_get("session_id")
        .map_err(|error| environment_sql_error("decode binding session id", error))?;
    let env_id: String = row
        .try_get("env_id")
        .map_err(|error| environment_sql_error("decode binding env id", error))?;
    let provider_id: String = row
        .try_get("provider_id")
        .map_err(|error| environment_sql_error("decode binding provider id", error))?;
    let target_id: String = row
        .try_get("target_id")
        .map_err(|error| environment_sql_error("decode binding target id", error))?;
    let kind: String = row
        .try_get("kind")
        .map_err(|error| environment_sql_error("decode binding kind", error))?;
    let status: String = row
        .try_get("status")
        .map_err(|error| environment_sql_error("decode binding status", error))?;
    let cwd: Option<String> = row
        .try_get("cwd")
        .map_err(|error| environment_sql_error("decode binding cwd", error))?;
    let record = SessionEnvironmentBindingRecord {
        session_id: SessionId::try_new(session_id).map_err(|error| {
            EnvironmentRegistryError::Store {
                message: format!("decode binding session id: {error}"),
            }
        })?,
        env_id: EnvironmentId::try_new(env_id).map_err(|error| {
            EnvironmentRegistryError::Store {
                message: format!("decode binding env id: {error}"),
            }
        })?,
        provider_id: EnvironmentProviderId::try_new(provider_id).map_err(|error| {
            EnvironmentRegistryError::Store {
                message: format!("decode binding provider id: {error}"),
            }
        })?,
        target_id: HostTargetId::new(target_id),
        exec_target: json_column(row, "exec_target_json")?,
        kind: binding_kind_from_str(&kind)?,
        status: binding_status_from_str(&status)?,
        capabilities: json_column(row, "capabilities_json")?,
        connection: json_column(row, "connection_json")?,
        cwd: cwd
            .as_deref()
            .map(HostPath::new)
            .transpose()
            .map_err(|error| EnvironmentRegistryError::Store {
                message: format!("decode binding cwd: {error}"),
            })?,
        fs_routes: json_column(row, "fs_routes_json")?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|error| environment_sql_error("decode binding created_at_ms", error))?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|error| environment_sql_error("decode binding updated_at_ms", error))?,
    };
    record.validate()?;
    Ok(record)
}

fn credential_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<SessionEnvironmentCredentialRecord, EnvironmentRegistryError> {
    let session_id: String = row
        .try_get("session_id")
        .map_err(|error| environment_sql_error("decode credential session id", error))?;
    let env_id: String = row
        .try_get("env_id")
        .map_err(|error| environment_sql_error("decode credential env id", error))?;
    let source_kind: String = row
        .try_get("source_kind")
        .map_err(|error| environment_sql_error("decode credential source kind", error))?;
    let grant_id: Option<String> = row
        .try_get("grant_id")
        .map_err(|error| environment_sql_error("decode credential grant id", error))?;
    let auth_provider_id: Option<String> = row
        .try_get("auth_provider_id")
        .map_err(|error| environment_sql_error("decode credential auth provider id", error))?;
    let secret_id: Option<String> = row
        .try_get("secret_id")
        .map_err(|error| environment_sql_error("decode credential secret id", error))?;
    let source =
        credential_source_from_columns(&source_kind, grant_id, auth_provider_id, secret_id)?;
    let record = SessionEnvironmentCredentialRecord {
        session_id: SessionId::try_new(session_id).map_err(|error| {
            EnvironmentRegistryError::Store {
                message: format!("decode credential session id: {error}"),
            }
        })?,
        env_id: EnvironmentId::try_new(env_id).map_err(|error| {
            EnvironmentRegistryError::Store {
                message: format!("decode credential env id: {error}"),
            }
        })?,
        env_name: row
            .try_get("env_name")
            .map_err(|error| environment_sql_error("decode credential env name", error))?,
        source,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|error| environment_sql_error("decode credential created_at_ms", error))?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|error| environment_sql_error("decode credential updated_at_ms", error))?,
    };
    record.validate()?;
    Ok(record)
}

fn json_value(
    action: &'static str,
    value: &impl serde::Serialize,
) -> Result<serde_json::Value, EnvironmentRegistryError> {
    serde_json::to_value(value).map_err(|error| EnvironmentRegistryError::Store {
        message: format!("{action}: {error}"),
    })
}

fn credential_source_columns(
    source: &SessionEnvironmentCredentialSource,
) -> (&'static str, Option<&str>, Option<&str>, Option<&str>) {
    match source {
        SessionEnvironmentCredentialSource::AuthGrant { grant_id } => {
            ("auth_grant", Some(grant_id.as_str()), None, None)
        }
        SessionEnvironmentCredentialSource::AuthProviderCredential { provider_id } => (
            "auth_provider_credential",
            None,
            Some(provider_id.as_str()),
            None,
        ),
        SessionEnvironmentCredentialSource::DirectSecret { secret_id } => {
            ("direct_secret", None, None, Some(secret_id.as_str()))
        }
    }
}

fn credential_source_from_columns(
    source_kind: &str,
    grant_id: Option<String>,
    auth_provider_id: Option<String>,
    secret_id: Option<String>,
) -> Result<SessionEnvironmentCredentialSource, EnvironmentRegistryError> {
    match source_kind {
        "auth_grant" => {
            let Some(grant_id) = grant_id else {
                return Err(EnvironmentRegistryError::Store {
                    message: "credential source auth_grant missing grant_id".to_owned(),
                });
            };
            if auth_provider_id.is_some() || secret_id.is_some() {
                return Err(EnvironmentRegistryError::Store {
                    message: "credential source auth_grant has extra source columns".to_owned(),
                });
            }
            Ok(SessionEnvironmentCredentialSource::AuthGrant {
                grant_id: AuthGrantId::try_new(grant_id).map_err(|error| {
                    EnvironmentRegistryError::Store {
                        message: format!("decode credential grant id: {error}"),
                    }
                })?,
            })
        }
        "auth_provider_credential" => {
            let Some(provider_id) = auth_provider_id else {
                return Err(EnvironmentRegistryError::Store {
                    message: "credential source auth_provider_credential missing provider_id"
                        .to_owned(),
                });
            };
            if grant_id.is_some() || secret_id.is_some() {
                return Err(EnvironmentRegistryError::Store {
                    message: "credential source auth_provider_credential has extra source columns"
                        .to_owned(),
                });
            }
            Ok(SessionEnvironmentCredentialSource::AuthProviderCredential {
                provider_id: AuthProviderId::try_new(provider_id).map_err(|error| {
                    EnvironmentRegistryError::Store {
                        message: format!("decode credential provider id: {error}"),
                    }
                })?,
            })
        }
        "direct_secret" => {
            let Some(secret_id) = secret_id else {
                return Err(EnvironmentRegistryError::Store {
                    message: "credential source direct_secret missing secret_id".to_owned(),
                });
            };
            if grant_id.is_some() || auth_provider_id.is_some() {
                return Err(EnvironmentRegistryError::Store {
                    message: "credential source direct_secret has extra source columns".to_owned(),
                });
            }
            Ok(SessionEnvironmentCredentialSource::DirectSecret {
                secret_id: SecretId::try_new(secret_id).map_err(|error| {
                    EnvironmentRegistryError::Store {
                        message: format!("decode credential secret id: {error}"),
                    }
                })?,
            })
        }
        other => Err(EnvironmentRegistryError::Store {
            message: format!("unsupported credential source kind '{other}'"),
        }),
    }
}

fn json_column<T: serde::de::DeserializeOwned>(
    row: &sqlx::postgres::PgRow,
    column: &'static str,
) -> Result<T, EnvironmentRegistryError> {
    let value: serde_json::Value = row
        .try_get(column)
        .map_err(|error| environment_sql_error(&format!("decode {column}"), error))?;
    serde_json::from_value(value).map_err(|error| EnvironmentRegistryError::Store {
        message: format!("decode {column}: {error}"),
    })
}

fn provider_kind_to_str(value: EnvironmentProviderKind) -> &'static str {
    match value {
        EnvironmentProviderKind::Sandbox => "sandbox",
        EnvironmentProviderKind::Bridge => "bridge",
        EnvironmentProviderKind::Custom => "custom",
    }
}

fn provider_kind_from_str(
    value: &str,
) -> Result<EnvironmentProviderKind, EnvironmentRegistryError> {
    match value {
        "sandbox" => Ok(EnvironmentProviderKind::Sandbox),
        "bridge" => Ok(EnvironmentProviderKind::Bridge),
        "custom" => Ok(EnvironmentProviderKind::Custom),
        other => Err(EnvironmentRegistryError::Store {
            message: format!("unsupported environment provider kind '{other}'"),
        }),
    }
}

fn provider_status_to_str(value: EnvironmentProviderStatus) -> &'static str {
    match value {
        EnvironmentProviderStatus::Registering => "registering",
        EnvironmentProviderStatus::Online => "online",
        EnvironmentProviderStatus::Stale => "stale",
        EnvironmentProviderStatus::Offline => "offline",
        EnvironmentProviderStatus::Disabled => "disabled",
    }
}

fn provider_status_from_str(
    value: &str,
) -> Result<EnvironmentProviderStatus, EnvironmentRegistryError> {
    match value {
        "registering" => Ok(EnvironmentProviderStatus::Registering),
        "online" => Ok(EnvironmentProviderStatus::Online),
        "stale" => Ok(EnvironmentProviderStatus::Stale),
        "offline" => Ok(EnvironmentProviderStatus::Offline),
        "disabled" => Ok(EnvironmentProviderStatus::Disabled),
        other => Err(EnvironmentRegistryError::Store {
            message: format!("unsupported environment provider status '{other}'"),
        }),
    }
}

fn host_target_status_to_str(value: HostTargetStatus) -> &'static str {
    match value {
        HostTargetStatus::Creating => "creating",
        HostTargetStatus::Starting => "starting",
        HostTargetStatus::Ready => "ready",
        HostTargetStatus::Stopped => "stopped",
        HostTargetStatus::Closing => "closing",
        HostTargetStatus::Closed => "closed",
        HostTargetStatus::Failed => "failed",
        HostTargetStatus::Unknown => "unknown",
    }
}

fn host_target_status_from_str(value: &str) -> Result<HostTargetStatus, EnvironmentRegistryError> {
    match value {
        "creating" => Ok(HostTargetStatus::Creating),
        "starting" => Ok(HostTargetStatus::Starting),
        "ready" => Ok(HostTargetStatus::Ready),
        "stopped" => Ok(HostTargetStatus::Stopped),
        "closing" => Ok(HostTargetStatus::Closing),
        "closed" => Ok(HostTargetStatus::Closed),
        "failed" => Ok(HostTargetStatus::Failed),
        "unknown" => Ok(HostTargetStatus::Unknown),
        other => Err(EnvironmentRegistryError::Store {
            message: format!("unsupported host target status '{other}'"),
        }),
    }
}

fn binding_kind_to_str(value: SessionEnvironmentKind) -> &'static str {
    match value {
        SessionEnvironmentKind::Sandbox => "sandbox",
        SessionEnvironmentKind::AttachedHost => "attached_host",
    }
}

fn binding_kind_from_str(value: &str) -> Result<SessionEnvironmentKind, EnvironmentRegistryError> {
    match value {
        "sandbox" => Ok(SessionEnvironmentKind::Sandbox),
        "attached_host" => Ok(SessionEnvironmentKind::AttachedHost),
        other => Err(EnvironmentRegistryError::Store {
            message: format!("unsupported session environment kind '{other}'"),
        }),
    }
}

fn binding_status_to_str(value: SessionEnvironmentBindingStatus) -> &'static str {
    match value {
        SessionEnvironmentBindingStatus::Attaching => "attaching",
        SessionEnvironmentBindingStatus::Ready => "ready",
        SessionEnvironmentBindingStatus::Degraded => "degraded",
        SessionEnvironmentBindingStatus::Detached => "detached",
    }
}

fn binding_status_from_str(
    value: &str,
) -> Result<SessionEnvironmentBindingStatus, EnvironmentRegistryError> {
    match value {
        "attaching" => Ok(SessionEnvironmentBindingStatus::Attaching),
        "ready" => Ok(SessionEnvironmentBindingStatus::Ready),
        "degraded" => Ok(SessionEnvironmentBindingStatus::Degraded),
        "detached" => Ok(SessionEnvironmentBindingStatus::Detached),
        other => Err(EnvironmentRegistryError::Store {
            message: format!("unsupported session environment binding status '{other}'"),
        }),
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
        id: format!("{session_id}/{}", env_id.as_str()),
    }
}

fn environment_store_error(action: &str, error: crate::PgStoreError) -> EnvironmentRegistryError {
    EnvironmentRegistryError::Store {
        message: format!("{action}: {error}"),
    }
}

fn environment_sql_error(action: &str, error: sqlx::Error) -> EnvironmentRegistryError {
    EnvironmentRegistryError::Store {
        message: format!("{action}: {error}"),
    }
}

fn validate_nonnegative_i64(
    value: i64,
    name: &'static str,
) -> Result<(), EnvironmentRegistryError> {
    if value < 0 {
        return Err(EnvironmentRegistryError::InvalidInput {
            message: format!("{name} must be nonnegative: {value}"),
        });
    }
    Ok(())
}

fn validate_positive_i64(value: i64, name: &'static str) -> Result<(), EnvironmentRegistryError> {
    if value <= 0 {
        return Err(EnvironmentRegistryError::InvalidInput {
            message: format!("{name} must be positive: {value}"),
        });
    }
    Ok(())
}
