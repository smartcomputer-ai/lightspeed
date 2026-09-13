//! Remote MCP server registry contracts.
//!
//! This crate owns provider-independent control-plane models and store traits
//! for remote MCP server catalogs. Concrete persistence adapters, such as
//! `store-pg`, implement these traits outside this crate.

pub mod search;

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use auth::AuthGrantId;
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

    #[error(
        "mcp registry server revision conflict for {server_id}: expected {expected}, got {actual}"
    )]
    RevisionConflict {
        server_id: McpServerId,
        expected: u64,
        actual: u64,
    },

    #[error("invalid mcp registry request: {message}")]
    InvalidInput { message: String },

    #[error("mcp registry store failure: {message}")]
    Store { message: String },
}

/// Sanitized metadata from one live MCP `tools/list` response. This is
/// request-scoped evidence, never catalog or execution state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredMcpTool {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
    pub annotations: Option<McpToolAnnotations>,
}

/// Standard MCP tool annotation hints. Values are untrusted and must not be
/// treated as authorization or approval facts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpToolAnnotations {
    pub read_only_hint: Option<bool>,
    pub destructive_hint: Option<bool>,
    pub idempotent_hint: Option<bool>,
    pub open_world_hint: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpToolDiscoveryFailureKind {
    CredentialAbsent,
    GrantNeedsReauth,
    GrantAudienceMismatch,
    Unauthorized,
    Forbidden,
    AdditionalConsentRequired,
    RemoteRateLimited,
    RemoteFailure,
    Unreachable,
    InvalidResponse,
    UnsupportedProtocol,
    PaginationLimit,
    ResponseTooLarge,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct McpToolDiscoveryFailure {
    pub kind: McpToolDiscoveryFailureKind,
    pub message: String,
    pub required_scopes: Vec<String>,
}

impl McpToolDiscoveryFailure {
    pub fn new(kind: McpToolDiscoveryFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            required_scopes: Vec::new(),
        }
    }

    pub fn with_required_scopes(mut self, scopes: Vec<String>) -> Self {
        self.required_scopes = scopes;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct McpToolDiscoveryLimits {
    pub max_pages: usize,
    pub max_tools: usize,
    pub max_response_bytes: usize,
    /// Independent response budget for `tools/call`; changing discovery
    /// inventory limits must not silently change tool execution policy.
    pub max_tool_call_response_bytes: usize,
    pub max_name_bytes: usize,
    pub max_text_bytes: usize,
    pub max_schema_bytes: usize,
    pub max_schema_depth: usize,
}

impl Default for McpToolDiscoveryLimits {
    fn default() -> Self {
        Self {
            // Enterprise MCPs commonly expose broad, paginated inventories.
            // Keep hard memory/wire bounds, but size the defaults for those
            // servers rather than for small development fixtures.
            max_pages: 64,
            max_tools: 2_048,
            max_response_bytes: 16 * 1024 * 1024,
            // Execution results have a separate context-safety envelope;
            // broadening discovery must not silently admit giant tool output.
            max_tool_call_response_bytes: 2 * 1024 * 1024,
            max_name_bytes: 128,
            max_text_bytes: 16 * 1024,
            max_schema_bytes: 512 * 1024,
            max_schema_depth: 64,
        }
    }
}

/// Truncate untrusted human-facing MCP metadata without splitting UTF-8.
/// Protocol identity and executable schemas must use validation instead.
pub fn truncate_utf8_with_ellipsis(mut value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    const ELLIPSIS: &str = "…";
    let append_ellipsis = max_bytes >= ELLIPSIS.len();
    let mut end = if append_ellipsis {
        max_bytes - ELLIPSIS.len()
    } else {
        max_bytes
    };
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    if append_ellipsis {
        value.push_str(ELLIPSIS);
    }
    value
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
    pub execution: McpExecution,
    pub exposure: McpExposure,
    pub approval: McpApprovalPolicy,
    pub defer_loading: Option<bool>,
    pub allow_private_network: bool,
    pub auth_policy: McpServerAuthPolicy,
    /// Universe-owned grant used to authenticate this configured server.
    /// Sessions select only `server_id` and never override this binding.
    pub auth_grant_id: Option<AuthGrantId>,
    pub status: McpServerStatus,
    pub revision: u64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl McpServerRecord {
    pub fn validate(&self) -> Result<(), McpRegistryError> {
        if self.revision == 0 {
            return Err(McpRegistryError::InvalidInput {
                message: "revision must be >= 1".to_owned(),
            });
        }
        validate_nonempty_optional("display_name", self.display_name.as_deref())?;
        validate_remote_mcp_server_url(&self.server_url)?;
        validate_remote_mcp_server_label(&self.default_server_label)?;
        validate_nonempty_optional("description", self.description.as_deref())?;
        validate_allowed_tools(self.allowed_tools.as_deref())?;
        validate_execution_policy(
            &self.default_server_label,
            self.execution,
            self.exposure,
            self.allowed_tools.as_deref(),
        )?;
        self.auth_policy.validate()?;
        if matches!(self.auth_policy, McpServerAuthPolicy::None) && self.auth_grant_id.is_some() {
            return Err(McpRegistryError::InvalidInput {
                message: "MCP servers with auth policy none cannot bind an auth grant".to_owned(),
            });
        }
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

/// Full-document put payload. `now_ms` becomes both `created_at_ms` and
/// `updated_at_ms` on create; on replace it stamps `updated_at_ms` while the
/// existing `created_at_ms` is preserved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutMcpServerRecord {
    pub server_id: McpServerId,
    pub display_name: Option<String>,
    pub server_url: String,
    pub transport: RemoteMcpTransport,
    pub default_server_label: String,
    pub description: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    pub execution: McpExecution,
    pub exposure: McpExposure,
    pub approval: McpApprovalPolicy,
    pub defer_loading: Option<bool>,
    pub allow_private_network: bool,
    pub auth_policy: McpServerAuthPolicy,
    pub auth_grant_id: Option<AuthGrantId>,
    pub status: McpServerStatus,
    pub now_ms: i64,
}

impl PutMcpServerRecord {
    /// Materializes a fresh record at revision 1.
    pub fn into_record(self) -> McpServerRecord {
        McpServerRecord {
            server_id: self.server_id,
            display_name: self.display_name,
            server_url: self.server_url,
            transport: self.transport,
            default_server_label: self.default_server_label,
            description: self.description,
            allowed_tools: self.allowed_tools,
            execution: self.execution,
            exposure: self.exposure,
            approval: self.approval,
            defer_loading: self.defer_loading,
            allow_private_network: self.allow_private_network,
            auth_policy: self.auth_policy,
            auth_grant_id: self.auth_grant_id,
            status: self.status,
            revision: 1,
            created_at_ms: self.now_ms,
            updated_at_ms: self.now_ms,
        }
    }

    /// Whole-document replacement of `current`: identity and `created_at_ms`
    /// are preserved, the revision bumps, and everything else comes from the
    /// payload.
    pub fn into_replacement(
        self,
        current: &McpServerRecord,
    ) -> Result<McpServerRecord, McpRegistryError> {
        if self.server_id != current.server_id {
            return Err(McpRegistryError::InvalidInput {
                message: format!(
                    "replacement server id {} does not match current {}",
                    self.server_id, current.server_id
                ),
            });
        }
        let revision =
            current
                .revision
                .checked_add(1)
                .ok_or_else(|| McpRegistryError::InvalidInput {
                    message: "mcp server revision exhausted".to_owned(),
                })?;
        Ok(McpServerRecord {
            server_id: self.server_id,
            display_name: self.display_name,
            server_url: self.server_url,
            transport: self.transport,
            default_server_label: self.default_server_label,
            description: self.description,
            allowed_tools: self.allowed_tools,
            execution: self.execution,
            exposure: self.exposure,
            approval: self.approval,
            defer_loading: self.defer_loading,
            allow_private_network: self.allow_private_network,
            auth_policy: self.auth_policy,
            auth_grant_id: self.auth_grant_id,
            status: self.status,
            revision,
            created_at_ms: current.created_at_ms,
            updated_at_ms: self.now_ms,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListMcpServers {
    pub status: Option<McpServerStatus>,
}

/// Runtime transport selected by the catalog implementation. This is kept out
/// of the public management API until Lightspeed supports more than the
/// current Streamable HTTP transport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteMcpTransport {
    #[default]
    StreamableHttp,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpExecution {
    #[default]
    Provider,
    Native,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpExposure {
    #[default]
    Inject,
    Search,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpApprovalPolicy {
    #[default]
    Never,
    Always,
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
    /// Create the server when absent (revision 1), otherwise replace the
    /// whole document and bump the revision. `expected_revision` is checked
    /// only when the record already exists; `None` replaces unconditionally.
    async fn put_server(
        &self,
        record: PutMcpServerRecord,
        expected_revision: Option<u64>,
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
    async fn put_server(
        &self,
        record: PutMcpServerRecord,
        expected_revision: Option<u64>,
    ) -> Result<McpServerRecord, McpRegistryError> {
        let mut inner = self.inner.write().map_err(|_| McpRegistryError::Store {
            message: "mcp registry write lock poisoned".to_owned(),
        })?;
        let record = match inner.get(&record.server_id) {
            Some(current) => {
                if let Some(expected) = expected_revision
                    && current.revision != expected
                {
                    return Err(McpRegistryError::RevisionConflict {
                        server_id: record.server_id,
                        expected,
                        actual: current.revision,
                    });
                }
                record.into_replacement(current)?
            }
            None => record.into_record(),
        };
        record.validate()?;
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

fn validate_execution_policy(
    server_label: &str,
    execution: McpExecution,
    exposure: McpExposure,
    allowed_tools: Option<&[String]>,
) -> Result<(), McpRegistryError> {
    if execution == McpExecution::Provider && exposure != McpExposure::Inject {
        return Err(McpRegistryError::InvalidInput {
            message: "exposure applies only to native MCP execution".to_owned(),
        });
    }
    if execution != McpExecution::Native || exposure != McpExposure::Inject {
        return Ok(());
    }
    let Some(allowed_tools) = allowed_tools else {
        return Ok(());
    };
    let prefix = native_tool_prefix(server_label);
    for remote_name in allowed_tools {
        validate_mcp_component(
            "native injected MCP tool",
            remote_name,
            "ASCII letters, digits, '_', '-'",
            |ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'),
        )?;
        let combined = format!("{prefix}{remote_name}");
        if combined.len() > 64 {
            return Err(McpRegistryError::InvalidInput {
                message: format!(
                    "native MCP tool name {combined:?} exceeds the 64-byte provider limit; shorten the server label or remote tool name, or use search exposure"
                ),
            });
        }
    }
    Ok(())
}

pub fn native_tool_prefix(server_label: &str) -> String {
    let mut prefix = String::from("mcp_");
    for ch in server_label.chars() {
        prefix.push(if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
            ch
        } else {
            '_'
        });
    }
    prefix.push_str("__");
    prefix
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

/// Validate the remote MCP URL at registry and read-only discovery boundaries.
pub fn validate_remote_mcp_server_url(value: &str) -> Result<(), McpRegistryError> {
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
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
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

    fn put_request(server_id: &str, status: McpServerStatus) -> PutMcpServerRecord {
        PutMcpServerRecord {
            server_id: McpServerId::new(server_id),
            display_name: Some("Echo".to_owned()),
            server_url: "https://echo.example.com/mcp".to_owned(),
            transport: RemoteMcpTransport::StreamableHttp,
            default_server_label: "echo".to_owned(),
            description: Some("Echo MCP server".to_owned()),
            allowed_tools: Some(vec!["hello".to_owned()]),
            execution: McpExecution::Provider,
            exposure: McpExposure::Inject,
            approval: McpApprovalPolicy::Never,
            defer_loading: Some(true),
            allow_private_network: false,
            auth_policy: McpServerAuthPolicy::None,
            auth_grant_id: None,
            status,
            now_ms: 10,
        }
    }

    #[test]
    fn records_validate_remote_mcp_shape() {
        let record = put_request("echo", McpServerStatus::Active).into_record();

        record.validate().expect("valid MCP server record");
    }

    #[test]
    fn metadata_truncation_preserves_utf8_and_byte_budget() {
        let value = format!("prefix-{}-suffix", "雪".repeat(8));
        let truncated = truncate_utf8_with_ellipsis(value, 17);
        assert!(truncated.is_char_boundary(truncated.len()));
        assert!(truncated.len() <= 17);
        assert!(truncated.ends_with('…'));
        assert_eq!(truncate_utf8_with_ellipsis("abc".to_owned(), 2), "ab");
        assert_eq!(truncate_utf8_with_ellipsis("abc".to_owned(), 3), "abc");
    }

    #[test]
    fn native_inject_selected_tools_must_fit_provider_function_names() {
        let mut record =
            put_request("durable-native-server-identifier", McpServerStatus::Active).into_record();
        record.execution = McpExecution::Native;
        record.exposure = McpExposure::Inject;
        record.default_server_label = "native".to_owned();
        record.allowed_tools = Some(vec!["lookup_customer".to_owned()]);
        record
            .validate()
            .expect("the compact server label determines the injected tool name");

        record.allowed_tools = Some(vec!["x".repeat(60)]);
        assert!(matches!(
            record.validate(),
            Err(McpRegistryError::InvalidInput { .. })
        ));

        record.allowed_tools = Some(vec!["not/a/function".to_owned()]);
        assert!(matches!(
            record.validate(),
            Err(McpRegistryError::InvalidInput { .. })
        ));

        record.exposure = McpExposure::Search;
        record
            .validate()
            .expect("search exposure transports the remote name as data");
    }

    #[test]
    fn records_reject_credentials_in_urls() {
        let mut record = put_request("echo", McpServerStatus::Active).into_record();
        record.server_url = "https://user:secret@echo.example.com/mcp".to_owned();

        let error = record
            .validate()
            .expect_err("URL credentials must be rejected");

        assert!(matches!(error, McpRegistryError::InvalidInput { .. }));
    }

    #[test]
    fn no_auth_servers_reject_grant_bindings() {
        let mut record = put_request("echo", McpServerStatus::Active).into_record();
        record.auth_grant_id = Some(auth::AuthGrantId::new("authgrant_1"));

        let error = record
            .validate()
            .expect_err("no-auth server must reject a grant binding");

        assert!(matches!(error, McpRegistryError::InvalidInput { .. }));
    }

    #[test]
    fn oauth_auth_policy_allows_empty_scope_defaults() {
        let mut record = put_request("echo", McpServerStatus::Active).into_record();
        record.auth_policy = McpServerAuthPolicy::RequiredOAuth {
            resource: "https://echo.example.com".to_owned(),
            scopes_default: Vec::new(),
            protected_resource_metadata_url: None,
            authorization_server: None,
        };

        record.validate().expect("empty OAuth scopes are valid");
    }

    #[tokio::test]
    async fn in_memory_store_put_replaces_whole_document_and_bumps_revision() {
        let store = InMemoryMcpRegistryStore::new();
        let created = store
            .put_server(put_request("echo", McpServerStatus::Active), None)
            .await
            .expect("create server");
        assert_eq!(created.revision, 1);
        assert_eq!(created.created_at_ms, 10);
        assert_eq!(created.updated_at_ms, 10);

        let mut replacement = put_request("echo", McpServerStatus::Disabled);
        replacement.server_url = "https://echo2.example.com/mcp".to_owned();
        replacement.description = None;
        replacement.now_ms = 20;
        let replaced = store
            .put_server(replacement, Some(1))
            .await
            .expect("replace server");

        assert_eq!(replaced.revision, 2);
        assert_eq!(replaced.server_url, "https://echo2.example.com/mcp");
        assert_eq!(replaced.description, None);
        assert_eq!(replaced.status, McpServerStatus::Disabled);
        // Identity and creation time are preserved; updated_at is stamped.
        assert_eq!(replaced.created_at_ms, created.created_at_ms);
        assert_eq!(replaced.updated_at_ms, 20);

        let read = store
            .read_server(&McpServerId::new("echo"))
            .await
            .expect("read server");
        assert_eq!(read, replaced);
    }

    #[tokio::test]
    async fn in_memory_store_put_checks_expected_revision_only_when_present() {
        let store = InMemoryMcpRegistryStore::new();
        // expected_revision on an absent record still creates.
        let created = store
            .put_server(put_request("echo", McpServerStatus::Active), Some(7))
            .await
            .expect("create server despite expected revision");
        assert_eq!(created.revision, 1);

        let conflict = store
            .put_server(put_request("echo", McpServerStatus::Active), Some(7))
            .await;
        assert!(matches!(
            conflict,
            Err(McpRegistryError::RevisionConflict {
                expected: 7,
                actual: 1,
                ..
            })
        ));

        // No expected revision replaces unconditionally.
        let replaced = store
            .put_server(put_request("echo", McpServerStatus::Active), None)
            .await
            .expect("unconditional replace");
        assert_eq!(replaced.revision, 2);
    }

    #[tokio::test]
    async fn in_memory_store_put_validates_and_does_not_partially_apply() {
        let store = InMemoryMcpRegistryStore::new();
        store
            .put_server(put_request("echo", McpServerStatus::Active), None)
            .await
            .expect("create server");

        let mut invalid = put_request("echo", McpServerStatus::Active);
        invalid.server_url = "not a url".to_owned();
        invalid.now_ms = 20;
        let result = store.put_server(invalid, None).await;
        assert!(matches!(result, Err(McpRegistryError::InvalidInput { .. })));

        // A failed put must not partially apply.
        let read = store
            .read_server(&McpServerId::new("echo"))
            .await
            .expect("read server");
        assert_eq!(read.server_url, "https://echo.example.com/mcp");
        assert_eq!(read.revision, 1);
    }

    #[tokio::test]
    async fn in_memory_store_creates_lists_reads_and_deletes_servers() {
        let store = InMemoryMcpRegistryStore::new();

        let created = store
            .put_server(put_request("echo", McpServerStatus::Active), None)
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
