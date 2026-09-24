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
    Server(Box<McpServerArgs>),
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
    /// Create or replace a remote MCP server record (full document).
    Put(Box<McpServerPutArgs>),
    /// List remote MCP server records.
    List(McpServerListArgs),
    /// Read a remote MCP server record.
    Read(McpServerReadArgs),
    /// Delete a remote MCP server record.
    Delete(McpServerDeleteArgs),
    /// Bind or clear the universe credential for a configured server.
    Auth(McpServerAuthArgs),
    /// Run MCP OAuth login and bind the resulting grant to this server.
    Login(McpServerLoginArgs),
}

#[derive(Args, Debug, Clone)]
struct McpServerAuthArgs {
    #[command(subcommand)]
    command: McpServerAuthCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum McpServerAuthCommand {
    /// Bind an existing universe auth grant.
    Set(McpServerAuthSetArgs),
    /// Remove the server's current grant binding.
    Clear(McpServerAuthClearArgs),
}

#[derive(Args, Debug, Clone)]
struct McpServerAuthSetArgs {
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    #[arg(long)]
    json: bool,
    /// Configured MCP server id.
    server_id: String,
    /// Existing universe auth grant id.
    #[arg(long = "grant")]
    grant_id: String,
}

#[derive(Args, Debug, Clone)]
struct McpServerAuthClearArgs {
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    #[arg(long)]
    json: bool,
    /// Configured MCP server id.
    server_id: String,
}

#[derive(Args, Debug, Clone)]
struct McpServerLoginArgs {
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    #[arg(long)]
    json: bool,
    /// Configured OAuth MCP server id.
    server_id: String,
    /// Scope override. Repeat to request multiple.
    #[arg(long = "scope")]
    scopes: Vec<String>,
    /// Audience override. Defaults to the server OAuth resource.
    #[arg(long)]
    audience: Option<String>,
}

#[derive(Args, Debug, Clone)]
struct McpServerPutArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    /// Emit the stored server as JSON.
    #[arg(long)]
    json: bool,
    /// Replace only when the current record is at this revision; absent
    /// creates or replaces unconditionally.
    #[arg(long = "expected-revision")]
    expected_revision: Option<u64>,
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
    /// Optional provider-side MCP tool allowlist entry. Repeat to allow multiple.
    #[arg(long = "allowed-tool")]
    allowed_tools: Vec<String>,
    /// Who connects to and executes this MCP server's tools.
    #[arg(long, default_value_t = RemoteMcpExecutionArg::Provider)]
    execution: RemoteMcpExecutionArg,
    /// How native tools are exposed to the model.
    #[arg(long, default_value_t = RemoteMcpExposureArg::Inject)]
    exposure: RemoteMcpExposureArg,
    /// Default remote MCP approval behavior.
    #[arg(long, default_value_t = RemoteMcpApprovalArg::Never)]
    approval: RemoteMcpApprovalArg,
    /// Enable provider-side deferred MCP tool loading by default.
    #[arg(long = "defer-loading", conflicts_with = "no_defer_loading")]
    defer_loading: bool,
    /// Disable provider-side deferred MCP tool loading by default.
    #[arg(long = "no-defer-loading", conflicts_with = "defer_loading")]
    no_defer_loading: bool,
    /// Permit this record to use the deployment's private MCP egress allowlist.
    #[arg(long)]
    allow_private_network: bool,
    /// Server status to record.
    #[arg(long, default_value_t = McpServerStatusArg::Active)]
    status: McpServerStatusArg,
    /// Auth requirement for this server.
    #[arg(long = "auth-policy", default_value_t = McpAuthPolicyArg::None)]
    auth_policy: McpAuthPolicyArg,
    /// Universe auth grant bound to this configured server.
    #[arg(long = "auth-grant-id")]
    auth_grant_id: Option<String>,
    /// Canonical OAuth resource URL (RFC 8707). Defaults to the server URL.
    /// Only valid with an OAuth auth policy.
    #[arg(long = "oauth-resource")]
    oauth_resource: Option<String>,
    /// Default OAuth scope entry. Repeat to record multiple. Only valid with
    /// an OAuth auth policy.
    #[arg(long = "oauth-scope")]
    oauth_scopes: Vec<String>,
    /// Explicit protected resource metadata URL (RFC 9728), tried before the
    /// derived well-known locations. Only valid with an OAuth auth policy.
    #[arg(long = "oauth-metadata-url")]
    oauth_metadata_url: Option<String>,
    /// Preferred authorization server when the resource metadata lists
    /// several. Only valid with an OAuth auth policy.
    #[arg(long = "oauth-authorization-server")]
    oauth_authorization_server: Option<String>,
    /// Remote MCP endpoint URL.
    server_url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum McpAuthPolicyArg {
    None,
    OptionalBearer,
    RequiredBearer,
    OptionalOauth,
    RequiredOauth,
}

impl std::fmt::Display for McpAuthPolicyArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::None => "none",
            Self::OptionalBearer => "optional-bearer",
            Self::RequiredBearer => "required-bearer",
            Self::OptionalOauth => "optional-oauth",
            Self::RequiredOauth => "required-oauth",
        })
    }
}

fn auth_policy_from_args(args: &McpServerPutArgs) -> Result<api::McpServerAuthPolicy> {
    let oauth_flags_used = args.oauth_resource.is_some()
        || !args.oauth_scopes.is_empty()
        || args.oauth_metadata_url.is_some()
        || args.oauth_authorization_server.is_some();
    let oauth_fields = || {
        (
            args.oauth_resource
                .clone()
                .unwrap_or_else(|| args.server_url.clone()),
            args.oauth_scopes.clone(),
            args.oauth_metadata_url.clone(),
            args.oauth_authorization_server.clone(),
        )
    };
    match args.auth_policy {
        McpAuthPolicyArg::None
        | McpAuthPolicyArg::OptionalBearer
        | McpAuthPolicyArg::RequiredBearer
            if oauth_flags_used =>
        {
            anyhow::bail!(
                "--oauth-* options require --auth-policy optional-oauth or required-oauth"
            )
        }
        McpAuthPolicyArg::None => Ok(api::McpServerAuthPolicy::None),
        McpAuthPolicyArg::OptionalBearer => Ok(api::McpServerAuthPolicy::OptionalBearer),
        McpAuthPolicyArg::RequiredBearer => Ok(api::McpServerAuthPolicy::RequiredBearer),
        McpAuthPolicyArg::OptionalOauth => {
            let (resource, scopes_default, protected_resource_metadata_url, authorization_server) =
                oauth_fields();
            Ok(api::McpServerAuthPolicy::OptionalOAuth {
                resource,
                scopes_default,
                protected_resource_metadata_url,
                authorization_server,
            })
        }
        McpAuthPolicyArg::RequiredOauth => {
            let (resource, scopes_default, protected_resource_metadata_url, authorization_server) =
                oauth_fields();
            Ok(api::McpServerAuthPolicy::RequiredOAuth {
                resource,
                scopes_default,
                protected_resource_metadata_url,
                authorization_server,
            })
        }
    }
}

#[derive(Args, Debug, Clone)]
struct McpServerListArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
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
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
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
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
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
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    /// Emit the link response as JSON.
    #[arg(long)]
    json: bool,
    /// Session id to change.
    #[arg(long)]
    session: String,
    /// Registered MCP server id to link.
    server_id: String,
    /// Comma-separated subset of the server's allowed tools to expose;
    /// omitted exposes the record's full allowlist.
    #[arg(long, value_delimiter = ',')]
    tools: Vec<String>,
}

#[derive(Args, Debug, Clone)]
struct McpUnlinkArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    /// Emit the unlink response as JSON.
    #[arg(long)]
    json: bool,
    /// Session id to change.
    #[arg(long)]
    session: String,
    /// Declared MCP server id to remove from the session config.
    server_id: String,
}

#[derive(Args, Debug, Clone)]
struct McpListArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    /// Emit links as JSON.
    #[arg(long)]
    json: bool,
    /// Session id to inspect.
    #[arg(long)]
    session: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum RemoteMcpApprovalArg {
    Always,
    Never,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum RemoteMcpExecutionArg {
    #[default]
    Provider,
    Native,
}

impl std::fmt::Display for RemoteMcpExecutionArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Provider => "provider",
            Self::Native => "native",
        })
    }
}

impl From<RemoteMcpExecutionArg> for api::RemoteMcpExecution {
    fn from(value: RemoteMcpExecutionArg) -> Self {
        match value {
            RemoteMcpExecutionArg::Provider => Self::Provider,
            RemoteMcpExecutionArg::Native => Self::Native,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum RemoteMcpExposureArg {
    #[default]
    Inject,
    Search,
}

impl std::fmt::Display for RemoteMcpExposureArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Inject => "inject",
            Self::Search => "search",
        })
    }
}

impl From<RemoteMcpExposureArg> for api::RemoteMcpExposure {
    fn from(value: RemoteMcpExposureArg) -> Self {
        match value {
            RemoteMcpExposureArg::Inject => Self::Inject,
            RemoteMcpExposureArg::Search => Self::Search,
        }
    }
}

impl std::fmt::Display for RemoteMcpApprovalArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Always => "always",
            Self::Never => "never",
        })
    }
}

impl From<RemoteMcpApprovalArg> for api::RemoteMcpApprovalPolicy {
    fn from(value: RemoteMcpApprovalArg) -> Self {
        match value {
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
        McpCommand::Server(args) => server(*args).await,
        McpCommand::Link(args) => link(args).await,
        McpCommand::Unlink(args) => unlink(args).await,
        McpCommand::List(args) => list(args).await,
    }
}

async fn server(args: McpServerArgs) -> Result<()> {
    match args.command {
        McpServerCommand::Put(args) => server_put(*args).await,
        McpServerCommand::List(args) => server_list(args).await,
        McpServerCommand::Read(args) => server_read(args).await,
        McpServerCommand::Delete(args) => server_delete(args).await,
        McpServerCommand::Auth(args) => server_auth(args).await,
        McpServerCommand::Login(args) => server_login(args).await,
    }
}

async fn server_auth(args: McpServerAuthArgs) -> Result<()> {
    match args.command {
        McpServerAuthCommand::Set(args) => {
            replace_server_credential(args.api_url, args.server_id, Some(args.grant_id), args.json)
                .await
        }
        McpServerAuthCommand::Clear(args) => {
            replace_server_credential(args.api_url, args.server_id, None, args.json).await
        }
    }
}

async fn replace_server_credential(
    api_url: String,
    server_id: String,
    grant_id: Option<String>,
    json: bool,
) -> Result<()> {
    let api = HttpAgentApi::new(api_url);
    let current = api
        .read_mcp_server(api::McpServerReadParams {
            server_id: server_id.clone(),
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result
        .server;
    put_server_credential(&api, current, grant_id, json).await
}

async fn put_server_credential(
    api: &HttpAgentApi,
    current: api::McpServerView,
    grant_id: Option<String>,
    json: bool,
) -> Result<()> {
    let mut server = server_input_from_view(&current);
    server.credential = grant_id.map(|grant_id| api::McpServerCredential::AuthGrant { grant_id });
    let required = matches!(
        server.auth_policy,
        api::McpServerAuthPolicy::RequiredBearer | api::McpServerAuthPolicy::RequiredOAuth { .. }
    );
    if server.credential.is_some() && server.status == api::McpServerStatus::NeedsAuthConfig {
        server.status = api::McpServerStatus::Active;
    } else if server.credential.is_none() && required {
        server.status = api::McpServerStatus::NeedsAuthConfig;
    }
    let response = api
        .put_mcp_server(api::McpServerPutParams {
            access: None,
            server,
            expected_revision: Some(current.revision),
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        print_server(&response.server);
    }
    Ok(())
}

fn server_input_from_view(server: &api::McpServerView) -> api::McpServerInput {
    api::McpServerInput {
        server_id: server.server_id.clone(),
        display_name: server.display_name.clone(),
        server_url: server.server_url.clone(),
        default_server_label: server.default_server_label.clone(),
        description: server.description.clone(),
        allowed_tools: server.allowed_tools.clone(),
        execution: server.execution,
        exposure: server.exposure,
        approval: server.approval,
        defer_loading: server.defer_loading,
        allow_private_network: server.allow_private_network,
        auth_policy: server.auth_policy.clone(),
        credential: server.credential.clone(),
        status: server.status,
    }
}

async fn server_login(args: McpServerLoginArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url.clone());
    let current = api
        .read_mcp_server(api::McpServerReadParams {
            server_id: args.server_id.clone(),
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result
        .server;
    if !matches!(
        &current.auth_policy,
        api::McpServerAuthPolicy::OptionalOAuth { .. }
            | api::McpServerAuthPolicy::RequiredOAuth { .. }
    ) {
        anyhow::bail!("MCP server {} does not use OAuth", current.server_id);
    }
    let started = api
        .start_auth_flow(api::AuthFlowStartParams {
            exposure: api::AuthGrantExposure::Brokered,
            client_id: format!("mcp:{}", args.server_id),
            scopes: (!args.scopes.is_empty()).then_some(args.scopes),
            audience: args.audience,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    eprintln!("Open this URL in your browser to authorize:");
    println!("{}", started.authorize_url);
    eprintln!("flowId {}", started.flow_id);
    eprintln!("Waiting for the authorization callback (ctrl-c to stop waiting)...");
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let response = api
            .read_auth_flow_status(api::AuthFlowStatusParams {
                flow_id: started.flow_id.clone(),
            })
            .await
            .map_err(crate::api_client::api_error)?
            .result;
        match response.flow.status {
            api::AuthFlowStatus::Pending => continue,
            api::AuthFlowStatus::Completed => {
                let grant_id = response
                    .flow
                    .grant_id
                    .ok_or_else(|| anyhow::anyhow!("completed MCP login returned no grant id"))?;
                return put_server_credential(&api, current, Some(grant_id), args.json).await;
            }
            api::AuthFlowStatus::Failed => anyhow::bail!(
                "authorization failed: {}",
                response.flow.error.as_deref().unwrap_or("unknown error")
            ),
            api::AuthFlowStatus::Expired => {
                anyhow::bail!("authorization flow expired before the callback completed")
            }
        }
    }
}

async fn server_put(args: McpServerPutArgs) -> Result<()> {
    let auth_policy = auth_policy_from_args(&args)?;
    let required_auth = matches!(
        auth_policy,
        api::McpServerAuthPolicy::RequiredBearer | api::McpServerAuthPolicy::RequiredOAuth { .. }
    );
    let mut status: api::McpServerStatus = args.status.into();
    if required_auth && args.auth_grant_id.is_none() && status == api::McpServerStatus::Active {
        status = api::McpServerStatus::NeedsAuthConfig;
    }
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .put_mcp_server(api::McpServerPutParams {
            access: None,
            server: api::McpServerInput {
                server_id: args.server_id,
                display_name: args.display_name,
                server_url: args.server_url,
                default_server_label: args.default_server_label,
                description: args.description,
                allowed_tools: nonempty_vec(args.allowed_tools),
                execution: args.execution.into(),
                exposure: args.exposure.into(),
                approval: args.approval.into(),
                defer_loading: defer_loading_arg(args.defer_loading, args.no_defer_loading),
                allow_private_network: args.allow_private_network,
                auth_policy,
                credential: args
                    .auth_grant_id
                    .map(|grant_id| api::McpServerCredential::AuthGrant { grant_id }),
                status,
            },
            expected_revision: args.expected_revision,
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

/// MCP links are declarative session config: link/unlink are sugar that
/// read-modify-put the config document's `features.mcp` block.
async fn link(args: McpLinkArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let session = api
        .read_session(api::SessionReadParams {
            session_id: args.session.clone(),
            run_limit: None,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result
        .session;
    let mut config = session.config.unwrap_or_default();
    let mut features = config.features.take().unwrap_or_default();
    let mut mcp = features.mcp.take().unwrap_or(api::McpFeature {
        version: api::CURRENT_FEATURE_VERSION,
        servers: Vec::new(),
    });
    mcp.servers.retain(|link| link.server_id != args.server_id);
    mcp.servers.push(api::McpServerAttachment {
        server_id: args.server_id.clone(),
        tools: (!args.tools.is_empty()).then_some(args.tools.clone()),
    });
    features.mcp = Some(mcp);
    config.features = Some(features);
    let response = api
        .put_session_config(api::SessionConfigPutParams {
            session_id: args.session.clone(),
            expected_config_revision: Some(session.config_revision),
            config,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    println!("linked {}", args.server_id);
    print_session_links(&api, &args.session).await
}

async fn unlink(args: McpUnlinkArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let session = api
        .read_session(api::SessionReadParams {
            session_id: args.session.clone(),
            run_limit: None,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result
        .session;
    let mut config = session.config.unwrap_or_default();
    let mut features = config.features.take().unwrap_or_default();
    if let Some(mut mcp) = features.mcp.take() {
        mcp.servers.retain(|link| link.server_id != args.server_id);
        if !mcp.servers.is_empty() {
            features.mcp = Some(mcp);
        }
    }
    config.features = Some(features);
    let response = api
        .put_session_config(api::SessionConfigPutParams {
            session_id: args.session.clone(),
            expected_config_revision: Some(session.config_revision),
            config,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    println!("unlinked {}", args.server_id);
    print_session_links(&api, &args.session).await
}

/// Materialized MCP links are the RemoteMcp entries of the session's
/// derived toolset; the declaration lives in `config.features.mcp`.
async fn session_mcp_tools(api: &HttpAgentApi, session_id: &str) -> Result<Vec<api::ToolView>> {
    let session = api
        .read_session(api::SessionReadParams {
            session_id: session_id.to_owned(),
            run_limit: None,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result
        .session;
    Ok(session
        .active_tools
        .tools
        .into_iter()
        .filter(|tool| matches!(tool.kind, api::ToolKindView::RemoteMcp { .. }))
        .collect())
}

async fn print_session_links(api: &HttpAgentApi, session_id: &str) -> Result<()> {
    let tools = session_mcp_tools(api, session_id).await?;
    println!("linkCount {}", tools.len());
    for tool in &tools {
        print_link(tool);
    }
    Ok(())
}

async fn list(args: McpListArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let tools = session_mcp_tools(&api, &args.session).await?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&tools)?);
        return Ok(());
    }
    if tools.is_empty() {
        println!("links 0");
        return Ok(());
    }
    for tool in &tools {
        print_link(tool);
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
    println!("approval {}", approval_label(server.approval));
    println!("status {}", status_label(server.status));
    println!("revision {}", server.revision);
    print_auth_policy(&server.auth_policy);
    if let Some(api::McpServerCredential::AuthGrant { grant_id }) = &server.credential {
        println!("authGrantId {grant_id}");
    }
    if let Some(display_name) = &server.display_name {
        println!("displayName {}", display_name);
    }
    if let Some(description) = &server.description {
        println!("description {}", description);
    }
    if let Some(allowed_tools) = &server.allowed_tools {
        println!("allowedTools {}", allowed_tools.join(","));
    }
    if let Some(defer_loading) = server.defer_loading {
        println!("deferLoading {}", defer_loading);
    }
}

fn print_auth_policy(policy: &api::McpServerAuthPolicy) {
    match policy {
        api::McpServerAuthPolicy::None => println!("authPolicy none"),
        api::McpServerAuthPolicy::OptionalBearer => println!("authPolicy optional-bearer"),
        api::McpServerAuthPolicy::RequiredBearer => println!("authPolicy required-bearer"),
        api::McpServerAuthPolicy::OptionalOAuth {
            resource,
            scopes_default,
            protected_resource_metadata_url,
            authorization_server,
        }
        | api::McpServerAuthPolicy::RequiredOAuth {
            resource,
            scopes_default,
            protected_resource_metadata_url,
            authorization_server,
        } => {
            let kind = if matches!(policy, api::McpServerAuthPolicy::OptionalOAuth { .. }) {
                "optional-oauth"
            } else {
                "required-oauth"
            };
            println!("authPolicy {kind}");
            println!("oauthResource {resource}");
            if !scopes_default.is_empty() {
                println!("oauthScopes {}", scopes_default.join(" "));
            }
            if let Some(url) = protected_resource_metadata_url {
                println!("oauthMetadataUrl {url}");
            }
            if let Some(issuer) = authorization_server {
                println!("oauthAuthorizationServer {issuer}");
            }
        }
    }
}

fn print_link(tool: &api::ToolView) {
    let api::ToolKindView::RemoteMcp {
        server_label,
        server_url,
        allowed_tools,
        approval,
        defer_loading,
        auth_required,
        ..
    } = &tool.kind
    else {
        return;
    };
    println!("{} {} {}", tool.tool_id, server_label, server_url);
    println!("  approval {}", approval_label(*approval));
    if let Some(allowed_tools) = allowed_tools {
        println!("  allowedTools {}", allowed_tools.join(","));
    }
    if let Some(defer_loading) = defer_loading {
        println!("  deferLoading {}", defer_loading);
    }
    println!("  authRequired {auth_required}");
}

fn approval_label(value: api::RemoteMcpApprovalPolicy) -> &'static str {
    match value {
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
