use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};

use crate::api_client::HttpAgentApi;

#[derive(Args, Debug, Clone)]
pub(crate) struct McpArgs {
    #[command(subcommand)]
    command: McpCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum McpCommand {
    /// Manage universe-scoped remote MCP server records.
    Server(McpServerArgs),
    /// Link a registered MCP server into a session tool profile.
    Link(McpLinkArgs),
    /// Remove a linked MCP tool from a session.
    Unlink(McpUnlinkArgs),
    /// List MCP links materialized into a session.
    List(McpListArgs),
}

#[derive(Args, Debug, Clone)]
struct McpServerArgs {
    #[command(subcommand)]
    command: McpServerCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum McpServerCommand {
    /// Add a remote MCP server record.
    Add(McpServerAddArgs),
    /// List remote MCP server records.
    List(McpServerListArgs),
    /// Read a remote MCP server record.
    Read(McpServerReadArgs),
    /// Delete a remote MCP server record.
    Delete(McpServerDeleteArgs),
}

#[derive(Args, Debug, Clone)]
struct McpServerAddArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Emit the created server as JSON.
    #[arg(long)]
    json: bool,
    /// Stable universe-scoped server id.
    #[arg(long = "id")]
    server_id: String,
    /// Default provider-facing MCP server label.
    #[arg(long = "label")]
    default_server_label: String,
    /// Optional display name.
    #[arg(long = "display-name")]
    display_name: Option<String>,
    /// Optional description.
    #[arg(long)]
    description: Option<String>,
    /// Remote MCP transport.
    #[arg(long, default_value_t = RemoteMcpTransportArg::Auto)]
    transport: RemoteMcpTransportArg,
    /// Optional provider-side MCP tool allowlist entry. Repeat to allow multiple.
    #[arg(long = "allowed-tool")]
    allowed_tools: Vec<String>,
    /// Default remote MCP approval behavior.
    #[arg(long, default_value_t = RemoteMcpApprovalArg::Never)]
    approval: RemoteMcpApprovalArg,
    /// Enable provider-side deferred MCP tool loading by default.
    #[arg(long = "defer-loading", conflicts_with = "no_defer_loading")]
    defer_loading: bool,
    /// Disable provider-side deferred MCP tool loading by default.
    #[arg(long = "no-defer-loading", conflicts_with = "defer_loading")]
    no_defer_loading: bool,
    /// Server status to record.
    #[arg(long, default_value_t = McpServerStatusArg::Active)]
    status: McpServerStatusArg,
    /// Auth requirement for this server. OAuth policies carry metadata and are
    /// configured through the API until discovery (P68 G3) lands.
    #[arg(long = "auth-policy", default_value_t = McpAuthPolicyArg::None)]
    auth_policy: McpAuthPolicyArg,
    /// Remote MCP endpoint URL.
    server_url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum McpAuthPolicyArg {
    None,
    OptionalBearer,
    RequiredBearer,
}

impl std::fmt::Display for McpAuthPolicyArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::None => "none",
            Self::OptionalBearer => "optional-bearer",
            Self::RequiredBearer => "required-bearer",
        })
    }
}

impl From<McpAuthPolicyArg> for api::McpServerAuthPolicy {
    fn from(value: McpAuthPolicyArg) -> Self {
        match value {
            McpAuthPolicyArg::None => Self::None,
            McpAuthPolicyArg::OptionalBearer => Self::OptionalBearer,
            McpAuthPolicyArg::RequiredBearer => Self::RequiredBearer,
        }
    }
}

#[derive(Args, Debug, Clone)]
struct McpServerListArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Emit servers as JSON.
    #[arg(long)]
    json: bool,
    /// Optional status filter.
    #[arg(long)]
    status: Option<McpServerStatusArg>,
}

#[derive(Args, Debug, Clone)]
struct McpServerReadArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Emit the server as JSON.
    #[arg(long)]
    json: bool,
    /// Server id to read.
    server_id: String,
}

#[derive(Args, Debug, Clone)]
struct McpServerDeleteArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Emit the deleted server as JSON.
    #[arg(long)]
    json: bool,
    /// Server id to delete.
    server_id: String,
}

#[derive(Args, Debug, Clone)]
struct McpLinkArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Emit the link response as JSON.
    #[arg(long)]
    json: bool,
    /// Session id to change.
    #[arg(long)]
    session: String,
    /// Optional engine tool id. Defaults to mcp_<server-id> with unsupported characters replaced.
    #[arg(long = "tool-id")]
    tool_id: Option<String>,
    /// Optional provider-facing MCP server label override.
    #[arg(long = "label")]
    server_label: Option<String>,
    /// Optional provider-side MCP tool allowlist entry. Repeat to allow multiple.
    #[arg(long = "allowed-tool")]
    allowed_tools: Vec<String>,
    /// Remote MCP approval behavior for this session link.
    #[arg(long)]
    approval: Option<RemoteMcpApprovalArg>,
    /// Enable provider-side deferred MCP tool loading for this session link.
    #[arg(long = "defer-loading", conflicts_with = "no_defer_loading")]
    defer_loading: bool,
    /// Disable provider-side deferred MCP tool loading for this session link.
    #[arg(long = "no-defer-loading", conflicts_with = "defer_loading")]
    no_defer_loading: bool,
    /// Optional opaque P69 auth grant id to materialize as auth_ref.
    #[arg(long = "auth-grant-id")]
    auth_grant_id: Option<String>,
    /// Registered MCP server id to link.
    server_id: String,
}

#[derive(Args, Debug, Clone)]
struct McpUnlinkArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Emit the unlink response as JSON.
    #[arg(long)]
    json: bool,
    /// Session id to change.
    #[arg(long)]
    session: String,
    /// Materialized MCP tool id to remove.
    tool_id: String,
}

#[derive(Args, Debug, Clone)]
struct McpListArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Emit links as JSON.
    #[arg(long)]
    json: bool,
    /// Session id to inspect.
    #[arg(long)]
    session: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum RemoteMcpTransportArg {
    Auto,
    StreamableHttp,
    Sse,
}

impl std::fmt::Display for RemoteMcpTransportArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::StreamableHttp => "streamable-http",
            Self::Sse => "sse",
        })
    }
}

impl From<RemoteMcpTransportArg> for api::RemoteMcpTransport {
    fn from(value: RemoteMcpTransportArg) -> Self {
        match value {
            RemoteMcpTransportArg::Auto => Self::Auto,
            RemoteMcpTransportArg::StreamableHttp => Self::StreamableHttp,
            RemoteMcpTransportArg::Sse => Self::Sse,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum RemoteMcpApprovalArg {
    ProviderDefault,
    Always,
    Never,
}

impl std::fmt::Display for RemoteMcpApprovalArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ProviderDefault => "provider-default",
            Self::Always => "always",
            Self::Never => "never",
        })
    }
}

impl From<RemoteMcpApprovalArg> for api::RemoteMcpApprovalPolicy {
    fn from(value: RemoteMcpApprovalArg) -> Self {
        match value {
            RemoteMcpApprovalArg::ProviderDefault => Self::ProviderDefault,
            RemoteMcpApprovalArg::Always => Self::Always,
            RemoteMcpApprovalArg::Never => Self::Never,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum McpServerStatusArg {
    Active,
    NeedsAuthConfig,
    Unverified,
    Disabled,
}

impl std::fmt::Display for McpServerStatusArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Active => "active",
            Self::NeedsAuthConfig => "needs-auth-config",
            Self::Unverified => "unverified",
            Self::Disabled => "disabled",
        })
    }
}

impl From<McpServerStatusArg> for api::McpServerStatus {
    fn from(value: McpServerStatusArg) -> Self {
        match value {
            McpServerStatusArg::Active => Self::Active,
            McpServerStatusArg::NeedsAuthConfig => Self::NeedsAuthConfig,
            McpServerStatusArg::Unverified => Self::Unverified,
            McpServerStatusArg::Disabled => Self::Disabled,
        }
    }
}

pub(crate) async fn handle(args: McpArgs) -> Result<()> {
    match args.command {
        McpCommand::Server(args) => server(args).await,
        McpCommand::Link(args) => link(args).await,
        McpCommand::Unlink(args) => unlink(args).await,
        McpCommand::List(args) => list(args).await,
    }
}

async fn server(args: McpServerArgs) -> Result<()> {
    match args.command {
        McpServerCommand::Add(args) => server_add(args).await,
        McpServerCommand::List(args) => server_list(args).await,
        McpServerCommand::Read(args) => server_read(args).await,
        McpServerCommand::Delete(args) => server_delete(args).await,
    }
}

async fn server_add(args: McpServerAddArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .create_mcp_server(api::McpServerCreateParams {
            server_id: args.server_id,
            display_name: args.display_name,
            server_url: args.server_url,
            transport: args.transport.into(),
            default_server_label: args.default_server_label,
            description: args.description,
            allowed_tools: nonempty_vec(args.allowed_tools),
            approval_default: args.approval.into(),
            defer_loading_default: defer_loading_arg(args.defer_loading, args.no_defer_loading),
            auth_policy: args.auth_policy.into(),
            status: args.status.into(),
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    print_server(&response.server);
    Ok(())
}

async fn server_list(args: McpServerListArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .list_mcp_servers(api::McpServerListParams {
            status: args.status.map(Into::into),
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    if response.servers.is_empty() {
        println!("servers 0");
        return Ok(());
    }
    for server in &response.servers {
        print_server_summary(server);
    }
    Ok(())
}

async fn server_read(args: McpServerReadArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .read_mcp_server(api::McpServerReadParams {
            server_id: args.server_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    print_server(&response.server);
    Ok(())
}

async fn server_delete(args: McpServerDeleteArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .delete_mcp_server(api::McpServerDeleteParams {
            server_id: args.server_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    println!("deleted {}", response.server.server_id);
    Ok(())
}

async fn link(args: McpLinkArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .link_session_mcp(api::SessionMcpLinkParams {
            session_id: args.session,
            server_id: args.server_id,
            tool_id: args.tool_id,
            server_label: args.server_label,
            allowed_tools: nonempty_vec(args.allowed_tools),
            approval: args.approval.map(Into::into),
            defer_loading: defer_loading_arg(args.defer_loading, args.no_defer_loading),
            auth_grant_id: args.auth_grant_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    println!("linked {}", response.link.tool_id);
    print_link(&response.link);
    println!("linkCount {}", response.links.len());
    Ok(())
}

async fn unlink(args: McpUnlinkArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .unlink_session_mcp(api::SessionMcpUnlinkParams {
            session_id: args.session,
            tool_id: args.tool_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    println!("unlinked {}", response.tool_id);
    println!("linkCount {}", response.links.len());
    Ok(())
}

async fn list(args: McpListArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .list_session_mcp(api::SessionMcpListParams {
            session_id: args.session,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    if response.links.is_empty() {
        println!("links 0");
        return Ok(());
    }
    for link in &response.links {
        print_link(link);
    }
    Ok(())
}

fn nonempty_vec(values: Vec<String>) -> Option<Vec<String>> {
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

fn defer_loading_arg(defer_loading: bool, no_defer_loading: bool) -> Option<bool> {
    match (defer_loading, no_defer_loading) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

fn print_server_summary(server: &api::McpServerView) {
    println!(
        "{} {} {} {}",
        server.server_id,
        status_label(server.status),
        server.default_server_label,
        server.server_url
    );
}

fn print_server(server: &api::McpServerView) {
    println!("serverId {}", server.server_id);
    println!("serverUrl {}", server.server_url);
    println!("label {}", server.default_server_label);
    println!("transport {}", transport_label(server.transport));
    println!(
        "approvalDefault {}",
        approval_label(server.approval_default)
    );
    println!("status {}", status_label(server.status));
    if let Some(display_name) = &server.display_name {
        println!("displayName {}", display_name);
    }
    if let Some(description) = &server.description {
        println!("description {}", description);
    }
    if let Some(allowed_tools) = &server.allowed_tools {
        println!("allowedTools {}", allowed_tools.join(","));
    }
    if let Some(defer_loading) = server.defer_loading_default {
        println!("deferLoading {}", defer_loading);
    }
}

fn print_link(link: &api::SessionMcpLinkView) {
    println!("{} {} {}", link.tool_id, link.server_label, link.server_url);
    println!("  approval {}", approval_label(link.approval));
    if let Some(allowed_tools) = &link.allowed_tools {
        println!("  allowedTools {}", allowed_tools.join(","));
    }
    if let Some(defer_loading) = link.defer_loading {
        println!("  deferLoading {}", defer_loading);
    }
    if let Some(auth_ref) = &link.auth_ref {
        println!("  authRef {}:{}", auth_ref.namespace, auth_ref.id);
    }
}

fn transport_label(value: api::RemoteMcpTransport) -> &'static str {
    match value {
        api::RemoteMcpTransport::StreamableHttp => "streamable-http",
        api::RemoteMcpTransport::Sse => "sse",
        api::RemoteMcpTransport::Auto => "auto",
    }
}

fn approval_label(value: api::RemoteMcpApprovalPolicy) -> &'static str {
    match value {
        api::RemoteMcpApprovalPolicy::ProviderDefault => "provider-default",
        api::RemoteMcpApprovalPolicy::Always => "always",
        api::RemoteMcpApprovalPolicy::Never => "never",
    }
}

fn status_label(value: api::McpServerStatus) -> &'static str {
    match value {
        api::McpServerStatus::Active => "active",
        api::McpServerStatus::NeedsAuthConfig => "needs-auth-config",
        api::McpServerStatus::Unverified => "unverified",
        api::McpServerStatus::Disabled => "disabled",
    }
}
