use ::mcp::{
    CreateMcpServerRecord, ListMcpServers, McpApprovalPolicy, McpRegistryError, McpRegistryStore,
    McpServerAuthPolicy, McpServerId, McpServerRecord, McpServerStatus, RemoteMcpTransport,
};
use async_trait::async_trait;
use sqlx::Row;

use crate::PgStore;

#[async_trait]
impl McpRegistryStore for PgStore {
    async fn create_server(
        &self,
        record: CreateMcpServerRecord,
    ) -> Result<McpServerRecord, McpRegistryError> {
        self.ensure_universe()
            .await
            .map_err(|error| mcp_store_error("ensure universe", error))?;
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
                approval_default,
                defer_loading_default,
                auth_policy,
                auth_metadata_json,
                status,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $14)
            ON CONFLICT (universe_id, server_id) DO NOTHING
            RETURNING
                server_id,
                display_name,
                server_url,
                transport,
                default_server_label,
                description,
                allowed_tools,
                approval_default,
                defer_loading_default,
                auth_policy,
                auth_metadata_json,
                status,
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
        .bind(approval_policy_to_str(record.approval_default))
        .bind(record.defer_loading_default)
        .bind(auth_policy)
        .bind(auth_metadata_json)
        .bind(status_to_str(record.status))
        .bind(record.created_at_ms)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| mcp_sql_error("create mcp server", error))?;

        let Some(row) = row else {
            return Err(McpRegistryError::AlreadyExists {
                server_id: record.server_id,
            });
        };
        server_record_from_row(&row)
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
                approval_default,
                defer_loading_default,
                auth_policy,
                auth_metadata_json,
                status,
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
        let rows = match request.status {
            Some(status) => {
                sqlx::query(
                    r#"
                    SELECT
                        server_id,
                        display_name,
                        server_url,
                        transport,
                        default_server_label,
                        description,
                        allowed_tools,
                        approval_default,
                        defer_loading_default,
                        auth_policy,
                        auth_metadata_json,
                        status,
                        created_at_ms,
                        updated_at_ms
                    FROM mcp_servers
                    WHERE universe_id = $1 AND status = $2
                    ORDER BY server_id
                    "#,
                )
                .bind(self.config.universe_id)
                .bind(status_to_str(status))
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query(
                    r#"
                    SELECT
                        server_id,
                        display_name,
                        server_url,
                        transport,
                        default_server_label,
                        description,
                        allowed_tools,
                        approval_default,
                        defer_loading_default,
                        auth_policy,
                        auth_metadata_json,
                        status,
                        created_at_ms,
                        updated_at_ms
                    FROM mcp_servers
                    WHERE universe_id = $1
                    ORDER BY server_id
                    "#,
                )
                .bind(self.config.universe_id)
                .fetch_all(&self.pool)
                .await
            }
        }
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
                approval_default,
                defer_loading_default,
                auth_policy,
                auth_metadata_json,
                status,
                created_at_ms,
                updated_at_ms
            "#,
        )
        .bind(self.config.universe_id)
        .bind(server_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| mcp_sql_error("delete mcp server", error))?;

        let Some(row) = row else {
            return Err(McpRegistryError::NotFound {
                server_id: server_id.clone(),
            });
        };
        server_record_from_row(&row)
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
    let approval_default: String = row
        .try_get("approval_default")
        .map_err(|error| mcp_sql_error("decode mcp approval default", error))?;
    let auth_policy: String = row
        .try_get("auth_policy")
        .map_err(|error| mcp_sql_error("decode mcp auth policy", error))?;
    let auth_metadata_json: serde_json::Value = row
        .try_get("auth_metadata_json")
        .map_err(|error| mcp_sql_error("decode mcp auth metadata", error))?;
    let status: String = row
        .try_get("status")
        .map_err(|error| mcp_sql_error("decode mcp status", error))?;

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
        approval_default: approval_policy_from_str(&approval_default)?,
        defer_loading_default: row
            .try_get("defer_loading_default")
            .map_err(|error| mcp_sql_error("decode mcp defer loading default", error))?,
        auth_policy: auth_policy_from_columns(&auth_policy, auth_metadata_json)?,
        status: status_from_str(&status)?,
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
        RemoteMcpTransport::Sse => "sse",
        RemoteMcpTransport::Auto => "auto",
    }
}

fn transport_from_str(value: &str) -> Result<RemoteMcpTransport, McpRegistryError> {
    match value {
        "streamable_http" => Ok(RemoteMcpTransport::StreamableHttp),
        "sse" => Ok(RemoteMcpTransport::Sse),
        "auto" => Ok(RemoteMcpTransport::Auto),
        other => Err(McpRegistryError::Store {
            message: format!("unsupported MCP transport '{other}'"),
        }),
    }
}

fn approval_policy_to_str(value: McpApprovalPolicy) -> &'static str {
    match value {
        McpApprovalPolicy::ProviderDefault => "provider_default",
        McpApprovalPolicy::Always => "always",
        McpApprovalPolicy::Never => "never",
    }
}

fn approval_policy_from_str(value: &str) -> Result<McpApprovalPolicy, McpRegistryError> {
    match value {
        "provider_default" => Ok(McpApprovalPolicy::ProviderDefault),
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
