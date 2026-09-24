use ::mcp::{
    ListMcpServers, McpApprovalPolicy, McpExecution, McpExposure, McpRegistryError,
    McpRegistryStore, McpServerAuthPolicy, McpServerId, McpServerRecord, McpServerStatus,
    PutMcpServerRecord, RemoteMcpTransport,
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
        let row = sqlx::query(
            r#"
            SELECT
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
            FROM mcp_servers
            WHERE universe_id = $1 AND server_id = $2
            "#,
        )
        .bind(self.config.universe_id)
        .bind(server_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| mcp_sql_error("read mcp server", error))?;

        let Some(row) = row else {
            return Err(McpRegistryError::NotFound {
                server_id: server_id.clone(),
            });
        };
        server_record_from_row(&row)
    }

    async fn list_servers(
        &self,
        request: ListMcpServers,
    ) -> Result<Vec<McpServerRecord>, McpRegistryError> {
        self.server_rows(None, request)
            .await?
            .iter()
            .map(server_record_from_row)
            .collect()
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
        let row = sqlx::query(
            r#"
            DELETE FROM mcp_servers
            WHERE universe_id = $1 AND server_id = $2
            RETURNING
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
            "#,
        )
        .bind(self.config.universe_id)
        .bind(server_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| mcp_sql_error("delete mcp server", error))?;

        let Some(row) = row else {
            return Err(McpRegistryError::NotFound {
                server_id: server_id.clone(),
            });
        };
        // The anchor goes with the server, so the id is free again.
        sqlx::query("DELETE FROM access_resources WHERE universe_id = $1 AND resource_kind = 'mcp_server' AND resource_id = $2")
            .bind(self.config.universe_id)
            .bind(server_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|error| mcp_sql_error("release mcp server anchor", error))?;
        tx.commit()
            .await
            .map_err(|error| mcp_sql_error("commit mcp server delete", error))?;
        server_record_from_row(&row)
    }
}

const SERVER_COLUMNS: &str = r#"
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
"#;

impl PgStore {
    /// `list_servers` for one reader: only servers the reader may see,
    /// decided in SQL by their access policy, each with the access summary
    /// its view carries. A server without an anchor is never listed.
    pub async fn list_servers_for(
        &self,
        reader: &crate::Reader,
        request: ListMcpServers,
    ) -> Result<Vec<(McpServerRecord, ::access::ResourceAccessSummary)>, McpRegistryError> {
        self.server_rows(Some(reader), request)
            .await?
            .iter()
            .map(|row| {
                Ok((
                    server_record_from_row(row)?,
                    crate::resources::summary_from_list_row(row).map_err(|error| {
                        McpRegistryError::Store {
                            message: format!("decode mcp server access: {error}"),
                        }
                    })?,
                ))
            })
            .collect()
    }

    /// Every server matching the request, or with a reader only those it
    /// may see, joined with their access summary.
    async fn server_rows(
        &self,
        reader: Option<&crate::Reader>,
        request: ListMcpServers,
    ) -> Result<Vec<sqlx::postgres::PgRow>, McpRegistryError> {
        let columns = crate::resources::qualify_columns(SERVER_COLUMNS, "s");
        let (access_columns, access_join, filter) = match reader {
            Some(reader) => (
                format!(", {}", crate::resources::SUMMARY_COLUMNS),
                crate::resources::summary_join("mcp_server", "s", "server_id"),
                crate::resources::operational_filter(reader, "mcp_server", "s", "server_id", 3),
            ),
            None => (String::new(), String::new(), "TRUE".to_owned()),
        };
        let (principal, root_kind, root_id) = reader.map(crate::Reader::binds).unwrap_or_default();
        sqlx::query(&format!(
            r#"
            SELECT {columns}{access_columns}
            FROM mcp_servers s
            {access_join}
            WHERE s.universe_id = $1 AND ($2::text IS NULL OR s.status = $2) AND {filter}
            ORDER BY s.server_id
            "#
        ))
        .bind(self.config.universe_id)
        .bind(request.status.map(status_to_str))
        .bind(principal)
        .bind(root_kind)
        .bind(root_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| mcp_sql_error("list mcp servers", error))
    }

    /// Insert a fresh record at revision 1; `AlreadyExists` when the id is
    /// taken.
    async fn insert_server(
        &self,
        record: PutMcpServerRecord,
    ) -> Result<McpServerRecord, McpRegistryError> {
        let record = record.into_record();
        record.validate()?;
        let (auth_policy, auth_metadata_json) = auth_policy_columns(&record.auth_policy)?;
        let row = sqlx::query(
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
            RETURNING
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
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| mcp_sql_error("insert mcp server", error))?;

        let Some(row) = row else {
            return Err(McpRegistryError::AlreadyExists {
                server_id: record.server_id,
            });
        };
        server_record_from_row(&row)
    }

    /// Write `replaced` over the row currently at `guard_revision`. Returns
    /// `None` when the guard no longer matches (a concurrent writer won).
    async fn cas_write_server(
        &self,
        replaced: &McpServerRecord,
        guard_revision: u64,
    ) -> Result<Option<McpServerRecord>, McpRegistryError> {
        let (auth_policy, auth_metadata_json) = auth_policy_columns(&replaced.auth_policy)?;
        let row = sqlx::query(
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
            RETURNING
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
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| mcp_sql_error("write mcp server", error))?;
        row.as_ref().map(server_record_from_row).transpose()
    }
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
        McpExecution::Native => "native",
    }
}

fn execution_from_str(value: &str) -> Result<McpExecution, McpRegistryError> {
    match value {
        "provider" => Ok(McpExecution::Provider),
        "native" => Ok(McpExecution::Native),
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
