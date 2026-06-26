//! Remote MCP server registry contracts.
//!
//! This crate owns provider-independent control-plane models and store traits
//! for remote MCP server catalogs. Concrete persistence adapters, such as
//! `store-pg`, implement these traits outside this crate.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use engine::{StringIdError, validate_general_string_id};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

macro_rules! mcp_string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                let value = value.into();
                Self::try_new(value)
                    .unwrap_or_else(|error| panic!("invalid {}: {error}", stringify!($name)))
            }

            pub fn try_new(value: impl Into<String>) -> Result<Self, StringIdError> {
                let value = value.into();
                validate_general_string_id(stringify!($name), &value)?;
                Ok(Self(value))
            }

            pub fn parse(value: impl Into<String>) -> Result<Self, StringIdError> {
                Self::try_new(value)
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = StringIdError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = StringIdError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl FromStr for $name {
            type Err = StringIdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::try_new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::try_new(value).map_err(de::Error::custom)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

mcp_string_id!(McpServerId);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum McpRegistryError {
    #[error("mcp registry server already exists: {server_id}")]
    AlreadyExists { server_id: McpServerId },

    #[error("mcp registry server not found: {server_id}")]
    NotFound { server_id: McpServerId },

    #[error("invalid mcp registry request: {message}")]
    InvalidInput { message: String },

    #[error("mcp registry store failure: {message}")]
    Store { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerRecord {
    pub server_id: McpServerId,
    pub display_name: Option<String>,
    pub server_url: String,
    pub transport: RemoteMcpTransport,
    pub default_server_label: String,
    pub description: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    pub approval_default: McpApprovalPolicy,
    pub defer_loading_default: Option<bool>,
    pub auth_policy: McpServerAuthPolicy,
    pub status: McpServerStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl McpServerRecord {
    pub fn validate(&self) -> Result<(), McpRegistryError> {
        validate_nonempty_optional("display_name", self.display_name.as_deref())?;
        validate_remote_mcp_server_url(&self.server_url)?;
        validate_remote_mcp_server_label(&self.default_server_label)?;
        validate_nonempty_optional("description", self.description.as_deref())?;
        validate_allowed_tools(self.allowed_tools.as_deref())?;
        self.auth_policy.validate()?;
        validate_nonnegative_i64(self.created_at_ms, "created_at_ms")?;
        validate_nonnegative_i64(self.updated_at_ms, "updated_at_ms")?;
        if self.updated_at_ms < self.created_at_ms {
            return Err(McpRegistryError::InvalidInput {
                message: format!(
                    "updated_at_ms {} must be >= created_at_ms {}",
                    self.updated_at_ms, self.created_at_ms
                ),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateMcpServerRecord {
    pub server_id: McpServerId,
    pub display_name: Option<String>,
    pub server_url: String,
    pub transport: RemoteMcpTransport,
    pub default_server_label: String,
    pub description: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    pub approval_default: McpApprovalPolicy,
    pub defer_loading_default: Option<bool>,
    pub auth_policy: McpServerAuthPolicy,
    pub status: McpServerStatus,
    pub created_at_ms: i64,
}

impl CreateMcpServerRecord {
    pub fn into_record(self) -> McpServerRecord {
        McpServerRecord {
            server_id: self.server_id,
            display_name: self.display_name,
            server_url: self.server_url,
            transport: self.transport,
            default_server_label: self.default_server_label,
            description: self.description,
            allowed_tools: self.allowed_tools,
            approval_default: self.approval_default,
            defer_loading_default: self.defer_loading_default,
            auth_policy: self.auth_policy,
            status: self.status,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.created_at_ms,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListMcpServers {
    pub status: Option<McpServerStatus>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteMcpTransport {
    StreamableHttp,
    Sse,
    #[default]
    Auto,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpApprovalPolicy {
    #[default]
    ProviderDefault,
    Always,
    Never,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpServerAuthPolicy {
    #[default]
    None,
    OptionalBearer,
    RequiredBearer,
    OptionalOAuth {
        resource: String,
        scopes_default: Vec<String>,
        protected_resource_metadata_url: Option<String>,
        authorization_server: Option<String>,
    },
    RequiredOAuth {
        resource: String,
        scopes_default: Vec<String>,
        protected_resource_metadata_url: Option<String>,
        authorization_server: Option<String>,
    },
}

impl McpServerAuthPolicy {
    pub fn validate(&self) -> Result<(), McpRegistryError> {
        match self {
            Self::None | Self::OptionalBearer | Self::RequiredBearer => Ok(()),
            Self::OptionalOAuth {
                resource,
                scopes_default,
                protected_resource_metadata_url,
                authorization_server,
            }
            | Self::RequiredOAuth {
                resource,
                scopes_default,
                protected_resource_metadata_url,
                authorization_server,
            } => {
                validate_nonempty_string("oauth resource", resource)?;
                validate_scope_defaults(scopes_default)?;
                if let Some(url) = protected_resource_metadata_url {
                    validate_remote_mcp_server_url(url)?;
                }
                if let Some(url) = authorization_server {
                    validate_remote_mcp_server_url(url)?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpServerStatus {
    #[default]
    Active,
    NeedsAuthConfig,
    Unverified,
    Disabled,
}

#[async_trait]
pub trait McpRegistryStore: Send + Sync {
    async fn create_server(
        &self,
        record: CreateMcpServerRecord,
    ) -> Result<McpServerRecord, McpRegistryError>;

    async fn read_server(
        &self,
        server_id: &McpServerId,
    ) -> Result<McpServerRecord, McpRegistryError>;

    async fn list_servers(
        &self,
        request: ListMcpServers,
    ) -> Result<Vec<McpServerRecord>, McpRegistryError>;

    async fn delete_server(
        &self,
        server_id: &McpServerId,
    ) -> Result<McpServerRecord, McpRegistryError>;
}

#[derive(Clone, Default)]
pub struct InMemoryMcpRegistryStore {
    inner: Arc<RwLock<BTreeMap<McpServerId, McpServerRecord>>>,
}

impl InMemoryMcpRegistryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl McpRegistryStore for InMemoryMcpRegistryStore {
    async fn create_server(
        &self,
        record: CreateMcpServerRecord,
    ) -> Result<McpServerRecord, McpRegistryError> {
        let record = record.into_record();
        record.validate()?;
        let mut inner = self.inner.write().map_err(|_| McpRegistryError::Store {
            message: "mcp registry write lock poisoned".to_owned(),
        })?;
        if inner.contains_key(&record.server_id) {
            return Err(McpRegistryError::AlreadyExists {
                server_id: record.server_id,
            });
        }
        inner.insert(record.server_id.clone(), record.clone());
        Ok(record)
    }

    async fn read_server(
        &self,
        server_id: &McpServerId,
    ) -> Result<McpServerRecord, McpRegistryError> {
        let inner = self.inner.read().map_err(|_| McpRegistryError::Store {
            message: "mcp registry read lock poisoned".to_owned(),
        })?;
        inner
            .get(server_id)
            .cloned()
            .ok_or_else(|| McpRegistryError::NotFound {
                server_id: server_id.clone(),
            })
    }

    async fn list_servers(
        &self,
        request: ListMcpServers,
    ) -> Result<Vec<McpServerRecord>, McpRegistryError> {
        let inner = self.inner.read().map_err(|_| McpRegistryError::Store {
            message: "mcp registry read lock poisoned".to_owned(),
        })?;
        Ok(inner
            .values()
            .filter(|record| request.status.is_none_or(|status| record.status == status))
            .cloned()
            .collect())
    }

    async fn delete_server(
        &self,
        server_id: &McpServerId,
    ) -> Result<McpServerRecord, McpRegistryError> {
        let mut inner = self.inner.write().map_err(|_| McpRegistryError::Store {
            message: "mcp registry write lock poisoned".to_owned(),
        })?;
        inner
            .remove(server_id)
            .ok_or_else(|| McpRegistryError::NotFound {
                server_id: server_id.clone(),
            })
    }
}

const REMOTE_MCP_URL_MAX_LEN: usize = 2048;
const REMOTE_MCP_COMPONENT_MAX_LEN: usize = 128;

fn validate_remote_mcp_server_label(value: &str) -> Result<(), McpRegistryError> {
    validate_mcp_component(
        "remote MCP server label",
        value,
        "ASCII letters, digits, '_', '-'",
        |ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'),
    )
}

fn validate_allowed_tools(values: Option<&[String]>) -> Result<(), McpRegistryError> {
    let Some(values) = values else {
        return Ok(());
    };
    if values.is_empty() {
        return Err(McpRegistryError::InvalidInput {
            message: "allowed_tools must not be empty when present".to_owned(),
        });
    }
    let mut seen = std::collections::BTreeSet::new();
    for value in values {
        validate_nonempty_string("allowed tool", value)?;
        if value.trim() != value || value.chars().any(char::is_whitespace) {
            return Err(McpRegistryError::InvalidInput {
                message: format!("allowed tool {value:?} must not contain whitespace"),
            });
        }
        if value.chars().any(|ch| ch.is_control()) {
            return Err(McpRegistryError::InvalidInput {
                message: format!("allowed tool {value:?} must not contain control characters"),
            });
        }
        if !seen.insert(value.as_str()) {
            return Err(McpRegistryError::InvalidInput {
                message: format!("duplicate allowed tool {value}"),
            });
        }
    }
    Ok(())
}

fn validate_scope_defaults(values: &[String]) -> Result<(), McpRegistryError> {
    let mut seen = std::collections::BTreeSet::new();
    for value in values {
        validate_nonempty_string("oauth default scope", value)?;
        if value.trim() != value || value.chars().any(char::is_whitespace) {
            return Err(McpRegistryError::InvalidInput {
                message: format!("oauth default scope {value:?} must not contain whitespace"),
            });
        }
        if value.chars().any(|ch| ch.is_control()) {
            return Err(McpRegistryError::InvalidInput {
                message: format!(
                    "oauth default scope {value:?} must not contain control characters"
                ),
            });
        }
        if !seen.insert(value.as_str()) {
            return Err(McpRegistryError::InvalidInput {
                message: format!("duplicate oauth default scope {value}"),
            });
        }
    }
    Ok(())
}

fn validate_remote_mcp_server_url(value: &str) -> Result<(), McpRegistryError> {
    if value.is_empty() {
        return Err(McpRegistryError::InvalidInput {
            message: "remote MCP server URL must not be empty".to_owned(),
        });
    }
    if value.len() > REMOTE_MCP_URL_MAX_LEN {
        return Err(McpRegistryError::InvalidInput {
            message: format!(
                "remote MCP server URL is too long: {} bytes, max {}",
                value.len(),
                REMOTE_MCP_URL_MAX_LEN
            ),
        });
    }
    if value.chars().any(char::is_whitespace) || value.chars().any(|ch| ch.is_control()) {
        return Err(McpRegistryError::InvalidInput {
            message: "remote MCP server URL must not contain whitespace or control characters"
                .to_owned(),
        });
    }
    if value.contains('#') {
        return Err(McpRegistryError::InvalidInput {
            message: "remote MCP server URL must not contain a fragment".to_owned(),
        });
    }
    let Some((scheme, rest)) = value.split_once("://") else {
        return Err(McpRegistryError::InvalidInput {
            message: "remote MCP server URL must include http:// or https:// scheme".to_owned(),
        });
    };
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(McpRegistryError::InvalidInput {
            message: format!("remote MCP server URL scheme {scheme:?} is not supported"),
        });
    }
    let authority_end = rest
        .find(|ch| matches!(ch, '/' | '?' | '#'))
        .unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() {
        return Err(McpRegistryError::InvalidInput {
            message: "remote MCP server URL host must not be empty".to_owned(),
        });
    }
    if authority.contains('@') {
        return Err(McpRegistryError::InvalidInput {
            message: "remote MCP server URL must not include credentials".to_owned(),
        });
    }
    Ok(())
}

fn validate_nonempty_optional(
    name: &'static str,
    value: Option<&str>,
) -> Result<(), McpRegistryError> {
    if let Some(value) = value {
        validate_nonempty_string(name, value)?;
    }
    Ok(())
}

fn validate_nonempty_string(name: &'static str, value: &str) -> Result<(), McpRegistryError> {
    if value.is_empty() {
        return Err(McpRegistryError::InvalidInput {
            message: format!("{name} must not be empty"),
        });
    }
    Ok(())
}

fn validate_nonnegative_i64(value: i64, name: &'static str) -> Result<(), McpRegistryError> {
    if value < 0 {
        return Err(McpRegistryError::InvalidInput {
            message: format!("{name} must be nonnegative: {value}"),
        });
    }
    Ok(())
}

fn validate_mcp_component(
    kind: &'static str,
    value: &str,
    allowed: &'static str,
    allowed_char: impl Fn(char) -> bool,
) -> Result<(), McpRegistryError> {
    if value.is_empty() {
        return Err(McpRegistryError::InvalidInput {
            message: format!("{kind} must not be empty"),
        });
    }
    if value.len() > REMOTE_MCP_COMPONENT_MAX_LEN {
        return Err(McpRegistryError::InvalidInput {
            message: format!(
                "{kind} is too long: {} bytes, max {}",
                value.len(),
                REMOTE_MCP_COMPONENT_MAX_LEN
            ),
        });
    }
    let Some(first) = value.chars().next() else {
        return Err(McpRegistryError::InvalidInput {
            message: format!("{kind} must not be empty"),
        });
    };
    if !first.is_ascii_alphanumeric() {
        return Err(McpRegistryError::InvalidInput {
            message: format!("{kind} must start with an ASCII letter or digit"),
        });
    }
    for (index, ch) in value.char_indices() {
        if !allowed_char(ch) {
            return Err(McpRegistryError::InvalidInput {
                message: format!(
                    "{kind} contains invalid character {ch:?} at byte {index}; allowed: {allowed}"
                ),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_request(server_id: &str, status: McpServerStatus) -> CreateMcpServerRecord {
        CreateMcpServerRecord {
            server_id: McpServerId::new(server_id),
            display_name: Some("Echo".to_owned()),
            server_url: "https://echo.example.com/mcp".to_owned(),
            transport: RemoteMcpTransport::Auto,
            default_server_label: "echo".to_owned(),
            description: Some("Echo MCP server".to_owned()),
            allowed_tools: Some(vec!["hello".to_owned()]),
            approval_default: McpApprovalPolicy::Never,
            defer_loading_default: Some(true),
            auth_policy: McpServerAuthPolicy::None,
            status,
            created_at_ms: 10,
        }
    }

    #[test]
    fn records_validate_remote_mcp_shape() {
        let record = create_request("echo", McpServerStatus::Active).into_record();

        record.validate().expect("valid MCP server record");
    }

    #[test]
    fn records_reject_credentials_in_urls() {
        let mut record = create_request("echo", McpServerStatus::Active).into_record();
        record.server_url = "https://user:secret@echo.example.com/mcp".to_owned();

        let error = record
            .validate()
            .expect_err("URL credentials must be rejected");

        assert!(matches!(error, McpRegistryError::InvalidInput { .. }));
    }

    #[test]
    fn oauth_auth_policy_allows_empty_scope_defaults() {
        let mut record = create_request("echo", McpServerStatus::Active).into_record();
        record.auth_policy = McpServerAuthPolicy::RequiredOAuth {
            resource: "https://echo.example.com".to_owned(),
            scopes_default: Vec::new(),
            protected_resource_metadata_url: None,
            authorization_server: None,
        };

        record.validate().expect("empty OAuth scopes are valid");
    }

    #[tokio::test]
    async fn in_memory_store_creates_lists_reads_and_deletes_servers() {
        let store = InMemoryMcpRegistryStore::new();

        let created = store
            .create_server(create_request("echo", McpServerStatus::Active))
            .await
            .expect("create server");
        assert_eq!(created.server_id.as_str(), "echo");

        let listed = store
            .list_servers(ListMcpServers::default())
            .await
            .expect("list servers");
        assert_eq!(listed, vec![created.clone()]);

        let active = store
            .list_servers(ListMcpServers {
                status: Some(McpServerStatus::Active),
            })
            .await
            .expect("list active servers");
        assert_eq!(active, vec![created.clone()]);

        let disabled = store
            .list_servers(ListMcpServers {
                status: Some(McpServerStatus::Disabled),
            })
            .await
            .expect("list disabled servers");
        assert!(disabled.is_empty());

        let read = store
            .read_server(&McpServerId::new("echo"))
            .await
            .expect("read server");
        assert_eq!(read, created);

        let deleted = store
            .delete_server(&McpServerId::new("echo"))
            .await
            .expect("delete server");
        assert_eq!(deleted, created);

        assert!(matches!(
            store.read_server(&McpServerId::new("echo")).await,
            Err(McpRegistryError::NotFound { .. })
        ));
    }
}
