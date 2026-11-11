use ::mcp::{
    ListMcpServers, McpApprovalPolicy, McpExecution, McpExposure, McpRegistryError,
    McpRegistryStore, McpServerAuthPolicy, McpServerHeaderValue, McpServerHeaders, McpServerId,
    McpServerRecord, McpServerStatus, PutMcpServerRecord, RemoteMcpTransport,
};
use async_trait::async_trait;
use sqlx::Row;

use crate::PgStore;

#[async_trait]
impl McpRegistryStore for PgStore {
    async fn put_server(
        &self,
        record: PutMcpServerRecord,
        expected_revision: Option<u64>,
    ) -> Result<McpServerRecord, McpRegistryError> {
        self.ensure_universe()
            .await
            .map_err(|error| mcp_store_error("ensure universe", error))?;
        // A concurrent writer between the read and the write loses exactly one
        // retry; the recheck still enforces `expected_revision` against fresh
        // state, so the retry never bypasses the caller's guard.
        let mut attempt = 0;
        loop {
            attempt += 1;
            let current = match self.read_server(&record.server_id).await {
                Ok(current) => Some(current),
                Err(McpRegistryError::NotFound { .. }) => None,
                Err(error) => return Err(error),
            };
            let Some(current) = current else {
                match self.insert_server(record.clone()).await {
                    Ok(created) => return Ok(created),
                    Err(McpRegistryError::AlreadyExists { .. }) if attempt < 2 => continue,
                    Err(error) => return Err(error),
                }
            };
            if let Some(expected) = expected_revision
                && current.revision != expected
            {
                return Err(McpRegistryError::RevisionConflict {
                    server_id: record.server_id,
                    expected,
                    actual: current.revision,
                });
            }
            let guard_revision = current.revision;
            let replaced = record.clone().into_replacement(&current)?;
            replaced.validate()?;
            match self.cas_write_server(&replaced, guard_revision).await? {
                Some(written) => return Ok(written),
                None if attempt < 2 => continue,
                None => {
                    let actual = self.read_server(&record.server_id).await?.revision;
                    return Err(McpRegistryError::RevisionConflict {
                        server_id: record.server_id,
                        expected: guard_revision,
                        actual,
                    });
                }
            }
        }
    }

    async fn read_server(
        &self,
        server_id: &McpServerId,
    ) -> Result<McpServerRecord, McpRegistryError> {
        select_server(&self.pool, self.config.universe_id, server_id, false)
            .await?
            .ok_or_else(|| McpRegistryError::NotFound {
                server_id: server_id.clone(),
            })
    }

    async fn list_servers(
        &self,
        request: ListMcpServers,
    ) -> Result<Vec<McpServerRecord>, McpRegistryError> {
        let query = format!(
            r#"
            SELECT {SERVER_COLUMNS}
            FROM mcp_servers AS s
            WHERE s.universe_id = $1 AND ($2::text IS NULL OR s.status = $2)
            ORDER BY s.server_id
            "#
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(request.status.map(status_to_str))
            .fetch_all(&self.pool)
            .await
            .map_err(|error| mcp_sql_error("list mcp servers", error))?;

        rows.iter().map(server_record_from_row).collect()
    }

    async fn delete_server(
        &self,
        server_id: &McpServerId,
    ) -> Result<McpServerRecord, McpRegistryError> {
        self.ensure_universe()
            .await
            .map_err(|error| mcp_store_error("ensure universe", error))?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| mcp_sql_error("begin mcp server delete", error))?;
        let Some(record) =
            select_server(&mut *tx, self.config.universe_id, server_id, true).await?
        else {
            return Err(McpRegistryError::NotFound {
                server_id: server_id.clone(),
            });
        };
        // Header rows cascade with the server.
        sqlx::query("DELETE FROM mcp_servers WHERE universe_id = $1 AND server_id = $2")
            .bind(self.config.universe_id)
            .bind(server_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| mcp_sql_error("delete mcp server", error))?;
        tx.commit()
            .await
            .map_err(|error| mcp_sql_error("commit mcp server delete", error))?;
        Ok(record)
    }
}

impl PgStore {
    /// Insert a fresh record at revision 1; `AlreadyExists` when the id is
    /// taken.
    async fn insert_server(
        &self,
        record: PutMcpServerRecord,
    ) -> Result<McpServerRecord, McpRegistryError> {
        let record = record.into_record();
        record.validate()?;
        let (auth_policy, auth_metadata_json) = auth_policy_columns(&record.auth_policy)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| mcp_sql_error("begin mcp server insert", error))?;
        let inserted = sqlx::query(
            r#"
            INSERT INTO mcp_servers (
                universe_id,
                server_id,
                display_name,
                server_url,
                transport,
                default_server_label,
                description,
                allowed_tools,
                execution,
                exposure,
                approval_default,
                defer_loading_default,
                allow_private_network,
                auth_policy,
                auth_metadata_json,
                auth_grant_id,
                status,
                revision,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)
            ON CONFLICT (universe_id, server_id) DO NOTHING
            RETURNING server_id
            "#,
        )
        .bind(self.config.universe_id)
        .bind(record.server_id.as_str())
        .bind(record.display_name.as_deref())
        .bind(&record.server_url)
        .bind(transport_to_str(record.transport))
        .bind(&record.default_server_label)
        .bind(record.description.as_deref())
        .bind(record.allowed_tools.as_deref())
        .bind(execution_to_str(record.execution))
        .bind(exposure_to_str(record.exposure))
        .bind(approval_policy_to_str(record.approval))
        .bind(record.defer_loading)
        .bind(record.allow_private_network)
        .bind(auth_policy)
        .bind(auth_metadata_json)
        .bind(
            record
                .auth_grant_id
                .as_ref()
                .map(|grant_id| grant_id.as_str()),
        )
        .bind(status_to_str(record.status))
        .bind(u64_to_i64(record.revision, "revision")?)
        .bind(record.created_at_ms)
        .bind(record.updated_at_ms)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| mcp_sql_error("insert mcp server", error))?;

        if inserted.is_none() {
            return Err(McpRegistryError::AlreadyExists {
                server_id: record.server_id,
            });
        }
        let written = self.finish_server_write(tx, &record).await?;
        Ok(written)
    }

    /// Write `replaced` over the row currently at `guard_revision`. Returns
    /// `None` when the guard no longer matches (a concurrent writer won).
    async fn cas_write_server(
        &self,
        replaced: &McpServerRecord,
        guard_revision: u64,
    ) -> Result<Option<McpServerRecord>, McpRegistryError> {
        let (auth_policy, auth_metadata_json) = auth_policy_columns(&replaced.auth_policy)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| mcp_sql_error("begin mcp server write", error))?;
        let updated = sqlx::query(
            r#"
            UPDATE mcp_servers SET
                display_name = $3,
                server_url = $4,
                transport = $5,
                default_server_label = $6,
                description = $7,
                allowed_tools = $8,
                execution = $9,
                exposure = $10,
                approval_default = $11,
                defer_loading_default = $12,
                allow_private_network = $13,
                auth_policy = $14,
                auth_metadata_json = $15,
                auth_grant_id = $16,
                status = $17,
                revision = $18,
                updated_at_ms = $19
            WHERE universe_id = $1 AND server_id = $2 AND revision = $20
            RETURNING server_id
            "#,
        )
        .bind(self.config.universe_id)
        .bind(replaced.server_id.as_str())
        .bind(replaced.display_name.as_deref())
        .bind(&replaced.server_url)
        .bind(transport_to_str(replaced.transport))
        .bind(&replaced.default_server_label)
        .bind(replaced.description.as_deref())
        .bind(replaced.allowed_tools.as_deref())
        .bind(execution_to_str(replaced.execution))
        .bind(exposure_to_str(replaced.exposure))
        .bind(approval_policy_to_str(replaced.approval))
        .bind(replaced.defer_loading)
        .bind(replaced.allow_private_network)
        .bind(auth_policy)
        .bind(auth_metadata_json)
        .bind(
            replaced
                .auth_grant_id
                .as_ref()
                .map(|grant_id| grant_id.as_str()),
        )
        .bind(status_to_str(replaced.status))
        .bind(u64_to_i64(replaced.revision, "revision")?)
        .bind(replaced.updated_at_ms)
        .bind(u64_to_i64(guard_revision, "revision")?)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| mcp_sql_error("write mcp server", error))?;
        if updated.is_none() {
            return Ok(None);
        }
        self.finish_server_write(tx, replaced).await.map(Some)
    }

    /// Replace the server's header rows, read the whole record back inside
    /// the same transaction, and commit.
    async fn finish_server_write(
        &self,
        mut tx: sqlx::Transaction<'_, sqlx::Postgres>,
        record: &McpServerRecord,
    ) -> Result<McpServerRecord, McpRegistryError> {
        let universe_id = self.config.universe_id;
        sqlx::query("DELETE FROM mcp_server_headers WHERE universe_id = $1 AND server_id = $2")
            .bind(universe_id)
            .bind(record.server_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| mcp_sql_error("clear mcp server headers", error))?;
        if !record.headers.is_empty() {
            let mut names = Vec::with_capacity(record.headers.len());
            let mut literals = Vec::with_capacity(record.headers.len());
            let mut grants = Vec::with_capacity(record.headers.len());
            for (name, value) in &record.headers {
                names.push(name.clone());
                match value {
                    McpServerHeaderValue::Literal { value } => {
                        literals.push(Some(value.clone()));
                        grants.push(None);
                    }
                    McpServerHeaderValue::Credential { grant_id } => {
                        literals.push(None);
                        grants.push(Some(grant_id.as_str().to_owned()));
                    }
                }
            }
            sqlx::query(
                r#"
                INSERT INTO mcp_server_headers (universe_id, server_id, name, literal_value, grant_id)
                SELECT $1, $2, header.name, header.literal_value, header.grant_id
                FROM UNNEST($3::text[], $4::text[], $5::text[])
                    AS header (name, literal_value, grant_id)
                "#,
            )
            .bind(universe_id)
            .bind(record.server_id.as_str())
            .bind(&names)
            .bind(&literals)
            .bind(&grants)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                if foreign_key_constraint(&error) == Some(HEADER_GRANT_FK) {
                    McpRegistryError::InvalidInput {
                        message: "a custom header references a credential that does not exist"
                            .to_owned(),
                    }
                } else {
                    mcp_sql_error("write mcp server headers", error)
                }
            })?;
        }
        let written = select_server(&mut *tx, universe_id, &record.server_id, false)
            .await?
            .ok_or_else(|| McpRegistryError::Store {
                message: format!("mcp server {} vanished during write", record.server_id),
            })?;
        tx.commit()
            .await
            .map_err(|error| mcp_sql_error("commit mcp server write", error))?;
        Ok(written)
    }
}

const HEADER_GRANT_FK: &str = "mcp_server_headers_grant_fk";

/// Every column of a server row plus its header rows folded into one JSON
/// array, so reads, lists and post-write reads decode the whole document.
const SERVER_COLUMNS: &str = r#"
    s.server_id,
    s.display_name,
    s.server_url,
    s.transport,
    s.default_server_label,
    s.description,
    s.allowed_tools,
    s.execution,
    s.exposure,
    s.approval_default,
    s.defer_loading_default,
    s.allow_private_network,
    s.auth_policy,
    s.auth_metadata_json,
    s.auth_grant_id,
    s.status,
    s.revision,
    s.created_at_ms,
    s.updated_at_ms,
    COALESCE(
        (
            SELECT jsonb_agg(
                jsonb_build_object(
                    'name', h.name,
                    'literal_value', h.literal_value,
                    'grant_id', h.grant_id
                )
                ORDER BY h.name
            )
            FROM mcp_server_headers AS h
            WHERE h.universe_id = s.universe_id AND h.server_id = s.server_id
        ),
        '[]'::jsonb
    ) AS headers_json
"#;

async fn select_server<'e, E>(
    executor: E,
    universe_id: uuid::Uuid,
    server_id: &McpServerId,
    for_update: bool,
) -> Result<Option<McpServerRecord>, McpRegistryError>
where
    E: sqlx::PgExecutor<'e>,
{
    let lock = if for_update { "FOR UPDATE OF s" } else { "" };
    let query = format!(
        r#"
        SELECT {SERVER_COLUMNS}
        FROM mcp_servers AS s
        WHERE s.universe_id = $1 AND s.server_id = $2
        {lock}
        "#
    );
    let row = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind(universe_id)
        .bind(server_id.as_str())
        .fetch_optional(executor)
        .await
        .map_err(|error| mcp_sql_error("read mcp server", error))?;
    row.as_ref().map(server_record_from_row).transpose()
}

fn foreign_key_constraint(error: &sqlx::Error) -> Option<&str> {
    let database = error.as_database_error()?;
    if database.code().as_deref() != Some("23503") {
        return None;
    }
    database.constraint()
}

#[derive(serde::Deserialize)]
struct HeaderRow {
    name: String,
    literal_value: Option<String>,
    grant_id: Option<String>,
}

fn headers_from_json(value: serde_json::Value) -> Result<McpServerHeaders, McpRegistryError> {
    let rows: Vec<HeaderRow> =
        serde_json::from_value(value).map_err(|error| McpRegistryError::Store {
            message: format!("decode mcp server headers: {error}"),
        })?;
    rows.into_iter()
        .map(|row| {
            let value = match (row.literal_value, row.grant_id) {
                (Some(value), None) => McpServerHeaderValue::Literal { value },
                (None, Some(grant_id)) => McpServerHeaderValue::Credential {
                    grant_id: auth::AuthGrantId::try_new(grant_id).map_err(|error| {
                        McpRegistryError::Store {
                            message: format!("decode mcp header grant id: {error}"),
                        }
                    })?,
                },
                _ => {
                    return Err(McpRegistryError::Store {
                        message: format!("mcp header {} has no single value source", row.name),
                    });
                }
            };
            Ok((row.name, value))
        })
        .collect()
}

fn server_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<McpServerRecord, McpRegistryError> {
    let server_id: String = row
        .try_get("server_id")
        .map_err(|error| mcp_sql_error("decode mcp server id", error))?;
    let transport: String = row
        .try_get("transport")
        .map_err(|error| mcp_sql_error("decode mcp transport", error))?;
    let approval: String = row
        .try_get("approval_default")
        .map_err(|error| mcp_sql_error("decode mcp approval default", error))?;
    let execution: String = row
        .try_get("execution")
        .map_err(|error| mcp_sql_error("decode mcp execution", error))?;
    let exposure: String = row
        .try_get("exposure")
        .map_err(|error| mcp_sql_error("decode mcp exposure", error))?;
    let auth_policy: String = row
        .try_get("auth_policy")
        .map_err(|error| mcp_sql_error("decode mcp auth policy", error))?;
    let auth_metadata_json: serde_json::Value = row
        .try_get("auth_metadata_json")
        .map_err(|error| mcp_sql_error("decode mcp auth metadata", error))?;
    let auth_grant_id: Option<String> = row
        .try_get("auth_grant_id")
        .map_err(|error| mcp_sql_error("decode mcp auth grant id", error))?;
    let status: String = row
        .try_get("status")
        .map_err(|error| mcp_sql_error("decode mcp status", error))?;
    let revision: i64 = row
        .try_get("revision")
        .map_err(|error| mcp_sql_error("decode mcp revision", error))?;
    let headers_json: serde_json::Value = row
        .try_get("headers_json")
        .map_err(|error| mcp_sql_error("decode mcp headers", error))?;

    let record = McpServerRecord {
        server_id: McpServerId::try_new(server_id).map_err(|error| McpRegistryError::Store {
            message: format!("decode mcp server id: {error}"),
        })?,
        display_name: row
            .try_get("display_name")
            .map_err(|error| mcp_sql_error("decode mcp display name", error))?,
        server_url: row
            .try_get("server_url")
            .map_err(|error| mcp_sql_error("decode mcp server url", error))?,
        transport: transport_from_str(&transport)?,
        default_server_label: row
            .try_get("default_server_label")
            .map_err(|error| mcp_sql_error("decode mcp default server label", error))?,
        description: row
            .try_get("description")
            .map_err(|error| mcp_sql_error("decode mcp description", error))?,
        allowed_tools: row
            .try_get("allowed_tools")
            .map_err(|error| mcp_sql_error("decode mcp allowed tools", error))?,
        execution: execution_from_str(&execution)?,
        exposure: exposure_from_str(&exposure)?,
        approval: approval_policy_from_str(&approval)?,
        defer_loading: row
            .try_get("defer_loading_default")
            .map_err(|error| mcp_sql_error("decode mcp defer loading default", error))?,
        allow_private_network: row
            .try_get("allow_private_network")
            .map_err(|error| mcp_sql_error("decode mcp private-network flag", error))?,
        auth_policy: auth_policy_from_columns(&auth_policy, auth_metadata_json)?,
        auth_grant_id: auth_grant_id
            .map(auth::AuthGrantId::try_new)
            .transpose()
            .map_err(|error| McpRegistryError::Store {
                message: format!("decode mcp auth grant id: {error}"),
            })?,
        headers: headers_from_json(headers_json)?,
        status: status_from_str(&status)?,
        revision: i64_to_u64(revision, "revision")?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|error| mcp_sql_error("decode mcp created_at_ms", error))?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|error| mcp_sql_error("decode mcp updated_at_ms", error))?,
    };
    record.validate()?;
    Ok(record)
}

#[derive(serde::Deserialize, serde::Serialize)]
struct OAuthAuthMetadata {
    resource: String,
    #[serde(default)]
    scopes_default: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    protected_resource_metadata_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authorization_server: Option<String>,
}

fn auth_policy_columns(
    policy: &McpServerAuthPolicy,
) -> Result<(&'static str, serde_json::Value), McpRegistryError> {
    match policy {
        McpServerAuthPolicy::None => Ok(("none", serde_json::json!({}))),
        McpServerAuthPolicy::OptionalBearer => Ok(("optional_bearer", serde_json::json!({}))),
        McpServerAuthPolicy::RequiredBearer => Ok(("required_bearer", serde_json::json!({}))),
        McpServerAuthPolicy::OptionalOAuth {
            resource,
            scopes_default,
            protected_resource_metadata_url,
            authorization_server,
        } => Ok((
            "optional_oauth",
            serde_json::to_value(OAuthAuthMetadata {
                resource: resource.clone(),
                scopes_default: scopes_default.clone(),
                protected_resource_metadata_url: protected_resource_metadata_url.clone(),
                authorization_server: authorization_server.clone(),
            })
            .map_err(|error| McpRegistryError::Store {
                message: format!("encode mcp OAuth metadata: {error}"),
            })?,
        )),
        McpServerAuthPolicy::RequiredOAuth {
            resource,
            scopes_default,
            protected_resource_metadata_url,
            authorization_server,
        } => Ok((
            "required_oauth",
            serde_json::to_value(OAuthAuthMetadata {
                resource: resource.clone(),
                scopes_default: scopes_default.clone(),
                protected_resource_metadata_url: protected_resource_metadata_url.clone(),
                authorization_server: authorization_server.clone(),
            })
            .map_err(|error| McpRegistryError::Store {
                message: format!("encode mcp OAuth metadata: {error}"),
            })?,
        )),
    }
}

fn auth_policy_from_columns(
    auth_policy: &str,
    metadata: serde_json::Value,
) -> Result<McpServerAuthPolicy, McpRegistryError> {
    match auth_policy {
        "none" => Ok(McpServerAuthPolicy::None),
        "optional_bearer" => Ok(McpServerAuthPolicy::OptionalBearer),
        "required_bearer" => Ok(McpServerAuthPolicy::RequiredBearer),
        "optional_oauth" => {
            let metadata: OAuthAuthMetadata =
                serde_json::from_value(metadata).map_err(|error| McpRegistryError::Store {
                    message: format!("decode optional OAuth metadata: {error}"),
                })?;
            Ok(McpServerAuthPolicy::OptionalOAuth {
                resource: metadata.resource,
                scopes_default: metadata.scopes_default,
                protected_resource_metadata_url: metadata.protected_resource_metadata_url,
                authorization_server: metadata.authorization_server,
            })
        }
        "required_oauth" => {
            let metadata: OAuthAuthMetadata =
                serde_json::from_value(metadata).map_err(|error| McpRegistryError::Store {
                    message: format!("decode required OAuth metadata: {error}"),
                })?;
            Ok(McpServerAuthPolicy::RequiredOAuth {
                resource: metadata.resource,
                scopes_default: metadata.scopes_default,
                protected_resource_metadata_url: metadata.protected_resource_metadata_url,
                authorization_server: metadata.authorization_server,
            })
        }
        other => Err(McpRegistryError::Store {
            message: format!("unsupported MCP auth policy '{other}'"),
        }),
    }
}

fn transport_to_str(value: RemoteMcpTransport) -> &'static str {
    match value {
        RemoteMcpTransport::StreamableHttp => "streamable_http",
    }
}

fn transport_from_str(value: &str) -> Result<RemoteMcpTransport, McpRegistryError> {
    match value {
        "streamable_http" => Ok(RemoteMcpTransport::StreamableHttp),
        other => Err(McpRegistryError::Store {
            message: format!("unsupported MCP transport '{other}'"),
        }),
    }
}

fn approval_policy_to_str(value: McpApprovalPolicy) -> &'static str {
    match value {
        McpApprovalPolicy::Always => "always",
        McpApprovalPolicy::Never => "never",
    }
}

fn execution_to_str(value: McpExecution) -> &'static str {
    match value {
        McpExecution::Provider => "provider",
        McpExecution::Runtime => "runtime",
    }
}

fn execution_from_str(value: &str) -> Result<McpExecution, McpRegistryError> {
    match value {
        "provider" => Ok(McpExecution::Provider),
        "runtime" => Ok(McpExecution::Runtime),
        other => Err(McpRegistryError::Store {
            message: format!("unsupported MCP execution mode '{other}'"),
        }),
    }
}

fn exposure_to_str(value: McpExposure) -> &'static str {
    match value {
        McpExposure::Inject => "inject",
        McpExposure::Search => "search",
    }
}

fn exposure_from_str(value: &str) -> Result<McpExposure, McpRegistryError> {
    match value {
        "inject" => Ok(McpExposure::Inject),
        "search" => Ok(McpExposure::Search),
        other => Err(McpRegistryError::Store {
            message: format!("unsupported MCP exposure mode '{other}'"),
        }),
    }
}

fn approval_policy_from_str(value: &str) -> Result<McpApprovalPolicy, McpRegistryError> {
    match value {
        "always" => Ok(McpApprovalPolicy::Always),
        "never" => Ok(McpApprovalPolicy::Never),
        other => Err(McpRegistryError::Store {
            message: format!("unsupported MCP approval policy '{other}'"),
        }),
    }
}

fn status_to_str(value: McpServerStatus) -> &'static str {
    match value {
        McpServerStatus::Active => "active",
        McpServerStatus::NeedsAuthConfig => "needs_auth_config",
        McpServerStatus::Unverified => "unverified",
        McpServerStatus::Disabled => "disabled",
    }
}

fn status_from_str(value: &str) -> Result<McpServerStatus, McpRegistryError> {
    match value {
        "active" => Ok(McpServerStatus::Active),
        "needs_auth_config" => Ok(McpServerStatus::NeedsAuthConfig),
        "unverified" => Ok(McpServerStatus::Unverified),
        "disabled" => Ok(McpServerStatus::Disabled),
        other => Err(McpRegistryError::Store {
            message: format!("unsupported MCP server status '{other}'"),
        }),
    }
}

fn u64_to_i64(value: u64, name: &'static str) -> Result<i64, McpRegistryError> {
    i64::try_from(value).map_err(|_| McpRegistryError::InvalidInput {
        message: format!("{name} exceeds i64::MAX"),
    })
}

fn i64_to_u64(value: i64, name: &'static str) -> Result<u64, McpRegistryError> {
    u64::try_from(value).map_err(|_| McpRegistryError::Store {
        message: format!("{name} is negative"),
    })
}

fn mcp_store_error(action: &str, error: crate::PgStoreError) -> McpRegistryError {
    McpRegistryError::Store {
        message: format!("{action}: {error}"),
    }
}

fn mcp_sql_error(action: &str, error: sqlx::Error) -> McpRegistryError {
    McpRegistryError::Store {
        message: format!("{action}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIGRATION: &str = include_str!("../migrations/014_runtime_and_lifecycle_vocabulary.sql");

    #[test]
    fn stored_execution_values_round_trip_and_pass_the_check() {
        for execution in [McpExecution::Provider, McpExecution::Runtime] {
            let stored = execution_to_str(execution);
            assert_eq!(execution_from_str(stored).unwrap(), execution);
            assert!(MIGRATION.contains(&format!("'{stored}'")), "{stored}");
        }
        assert!(execution_from_str("native").is_err());
    }
}
