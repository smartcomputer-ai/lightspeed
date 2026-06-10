use std::io::Read as _;

use anyhow::{Context, Result};
use clap::{ArgGroup, Args, Subcommand, ValueEnum};

use crate::api_client::HttpAgentApi;

#[derive(Args, Debug, Clone)]
pub(crate) struct AuthArgs {
    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum AuthCommand {
    /// Manage universe-scoped auth grants.
    Grant(AuthGrantArgs),
    /// Manage OAuth client configurations.
    Client(AuthClientArgs),
    /// Run an OAuth authorization flow and store the resulting grant.
    Login(AuthLoginArgs),
}

#[derive(Args, Debug, Clone)]
struct AuthGrantArgs {
    #[command(subcommand)]
    command: AuthGrantCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum AuthGrantCommand {
    /// Import a static bearer credential as an auth grant.
    Import(AuthGrantImportArgs),
    /// List auth grants.
    List(AuthGrantListArgs),
    /// Read an auth grant.
    Read(AuthGrantReadArgs),
    /// Revoke an auth grant.
    Revoke(AuthGrantRevokeArgs),
}

#[derive(Args, Clone)]
#[command(group(ArgGroup::new("token_source").required(true)))]
struct AuthGrantImportArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Emit the imported grant as JSON.
    #[arg(long)]
    json: bool,
    /// Optional stable grant id. Generated when omitted.
    #[arg(long = "id")]
    grant_id: Option<String>,
    /// Optional provider id recorded on the grant. Defaults to "static".
    #[arg(long = "provider-id")]
    provider_id: Option<String>,
    /// Bearer token value. Prefer --token-stdin or --token-env to keep the
    /// value out of shell history.
    #[arg(long, group = "token_source")]
    token: Option<String>,
    /// Read the bearer token from this environment variable.
    #[arg(long = "token-env", group = "token_source")]
    token_env: Option<String>,
    /// Read the bearer token from stdin.
    #[arg(long = "token-stdin", group = "token_source")]
    token_stdin: bool,
    /// Optional display name.
    #[arg(long = "display-name")]
    display_name: Option<String>,
    /// Optional subject hint (for example an account or user name).
    #[arg(long = "subject-hint")]
    subject_hint: Option<String>,
    /// Optional scope entry. Repeat to record multiple.
    #[arg(long = "scope")]
    scopes: Vec<String>,
    /// Optional audience the grant is bound to (for MCP: the server URL).
    #[arg(long)]
    audience: Option<String>,
    /// Optional expiry in unix milliseconds.
    #[arg(long = "expires-at-ms")]
    expires_at_ms: Option<i64>,
}

impl std::fmt::Debug for AuthGrantImportArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthGrantImportArgs")
            .field("api_url", &self.api_url)
            .field("grant_id", &self.grant_id)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("token_env", &self.token_env)
            .field("token_stdin", &self.token_stdin)
            .field("audience", &self.audience)
            .finish_non_exhaustive()
    }
}

#[derive(Args, Debug, Clone)]
struct AuthGrantListArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Emit grants as JSON.
    #[arg(long)]
    json: bool,
    /// Optional status filter.
    #[arg(long)]
    status: Option<AuthGrantStatusArg>,
}

#[derive(Args, Debug, Clone)]
struct AuthGrantReadArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Emit the grant as JSON.
    #[arg(long)]
    json: bool,
    /// Grant id to read.
    grant_id: String,
}

#[derive(Args, Debug, Clone)]
struct AuthGrantRevokeArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Emit the revoked grant as JSON.
    #[arg(long)]
    json: bool,
    /// Grant id to revoke.
    grant_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum AuthGrantStatusArg {
    Active,
    NeedsReauth,
    Revoked,
    Failed,
}

impl From<AuthGrantStatusArg> for api::AuthGrantStatus {
    fn from(value: AuthGrantStatusArg) -> Self {
        match value {
            AuthGrantStatusArg::Active => Self::Active,
            AuthGrantStatusArg::NeedsReauth => Self::NeedsReauth,
            AuthGrantStatusArg::Revoked => Self::Revoked,
            AuthGrantStatusArg::Failed => Self::Failed,
        }
    }
}

#[derive(Args, Debug, Clone)]
struct AuthClientArgs {
    #[command(subcommand)]
    command: AuthClientCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum AuthClientCommand {
    /// Register a manually configured OAuth client.
    Add(AuthClientAddArgs),
    /// List OAuth clients.
    List(AuthClientListArgs),
    /// Read an OAuth client.
    Read(AuthClientReadArgs),
    /// Remove an OAuth client.
    Remove(AuthClientRemoveArgs),
}

#[derive(Args, Clone)]
#[command(group(ArgGroup::new("client_secret_source")))]
struct AuthClientAddArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Emit the created client as JSON.
    #[arg(long)]
    json: bool,
    /// Optional stable client id. Generated when omitted.
    #[arg(long = "id")]
    client_id: Option<String>,
    /// Optional provider id recorded on grants. Defaults to the client id.
    #[arg(long = "provider-id")]
    provider_id: Option<String>,
    /// Provider kind for grants minted through this client.
    #[arg(long = "kind", default_value = "custom-oauth")]
    kind: AuthProviderKindArg,
    /// Optional display name.
    #[arg(long = "display-name")]
    display_name: Option<String>,
    /// OAuth authorization endpoint URL.
    #[arg(long = "authorization-endpoint")]
    authorization_endpoint: String,
    /// OAuth token endpoint URL.
    #[arg(long = "token-endpoint")]
    token_endpoint: String,
    /// Client identifier issued by the authorization server.
    #[arg(long = "client-id")]
    remote_client_id: String,
    /// Client secret value. Prefer --client-secret-stdin or
    /// --client-secret-env to keep the value out of shell history.
    #[arg(long = "client-secret", group = "client_secret_source")]
    client_secret: Option<String>,
    /// Read the client secret from this environment variable.
    #[arg(long = "client-secret-env", group = "client_secret_source")]
    client_secret_env: Option<String>,
    /// Read the client secret from stdin.
    #[arg(long = "client-secret-stdin", group = "client_secret_source")]
    client_secret_stdin: bool,
    /// Token endpoint authentication method. Defaults to client-secret-basic
    /// when a secret is provided, none otherwise.
    #[arg(long = "auth-method")]
    auth_method: Option<TokenAuthMethodArg>,
    /// Default scope entry. Repeat to record multiple.
    #[arg(long = "scope")]
    scopes: Vec<String>,
    /// Default audience grants are bound to (for MCP: the server URL).
    #[arg(long)]
    audience: Option<String>,
}

impl std::fmt::Debug for AuthClientAddArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthClientAddArgs")
            .field("api_url", &self.api_url)
            .field("client_id", &self.client_id)
            .field("kind", &self.kind)
            .field("remote_client_id", &self.remote_client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("client_secret_env", &self.client_secret_env)
            .field("client_secret_stdin", &self.client_secret_stdin)
            .field("audience", &self.audience)
            .finish_non_exhaustive()
    }
}

#[derive(Args, Debug, Clone)]
struct AuthClientListArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Emit clients as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug, Clone)]
struct AuthClientReadArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Emit the client as JSON.
    #[arg(long)]
    json: bool,
    /// Client id to read.
    client_id: String,
}

#[derive(Args, Debug, Clone)]
struct AuthClientRemoveArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Emit the removed client as JSON.
    #[arg(long)]
    json: bool,
    /// Client id to remove.
    client_id: String,
}

#[derive(Args, Debug, Clone)]
struct AuthLoginArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "FORGE_API_URL")]
    api_url: String,
    /// Emit the final flow status as JSON.
    #[arg(long)]
    json: bool,
    /// OAuth client id to authorize against.
    client_id: String,
    /// Scope override. Repeat to request multiple.
    #[arg(long = "scope")]
    scopes: Vec<String>,
    /// Audience override (for MCP: the server URL).
    #[arg(long)]
    audience: Option<String>,
    /// Print the authorization URL and flow id, then exit without waiting
    /// for the callback. Check progress with auth flow status polling.
    #[arg(long = "no-wait")]
    no_wait: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum AuthProviderKindArg {
    McpOauth,
    GithubOauthApp,
    GithubAppUser,
    CustomOauth,
}

impl From<AuthProviderKindArg> for api::AuthProviderKind {
    fn from(value: AuthProviderKindArg) -> Self {
        match value {
            AuthProviderKindArg::McpOauth => Self::McpOAuth,
            AuthProviderKindArg::GithubOauthApp => Self::GitHubOAuthApp,
            AuthProviderKindArg::GithubAppUser => Self::GitHubAppUser,
            AuthProviderKindArg::CustomOauth => Self::CustomOAuth,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum TokenAuthMethodArg {
    ClientSecretBasic,
    ClientSecretPost,
    None,
}

impl From<TokenAuthMethodArg> for api::TokenEndpointAuthMethod {
    fn from(value: TokenAuthMethodArg) -> Self {
        match value {
            TokenAuthMethodArg::ClientSecretBasic => Self::ClientSecretBasic,
            TokenAuthMethodArg::ClientSecretPost => Self::ClientSecretPost,
            TokenAuthMethodArg::None => Self::None,
        }
    }
}

pub(crate) async fn handle(args: AuthArgs) -> Result<()> {
    match args.command {
        AuthCommand::Grant(args) => grant(args).await,
        AuthCommand::Client(args) => client(args).await,
        AuthCommand::Login(args) => login(args).await,
    }
}

async fn client(args: AuthClientArgs) -> Result<()> {
    match args.command {
        AuthClientCommand::Add(args) => client_add(args).await,
        AuthClientCommand::List(args) => client_list(args).await,
        AuthClientCommand::Read(args) => client_read(args).await,
        AuthClientCommand::Remove(args) => client_remove(args).await,
    }
}

async fn grant(args: AuthGrantArgs) -> Result<()> {
    match args.command {
        AuthGrantCommand::Import(args) => grant_import(args).await,
        AuthGrantCommand::List(args) => grant_list(args).await,
        AuthGrantCommand::Read(args) => grant_read(args).await,
        AuthGrantCommand::Revoke(args) => grant_revoke(args).await,
    }
}

fn resolve_token(args: &AuthGrantImportArgs) -> Result<String> {
    if let Some(token) = &args.token {
        return Ok(token.clone());
    }
    if let Some(name) = &args.token_env {
        return std::env::var(name)
            .with_context(|| format!("environment variable {name} is not set"))
            .and_then(|value| {
                if value.is_empty() {
                    anyhow::bail!("environment variable {name} is empty")
                } else {
                    Ok(value)
                }
            });
    }
    let mut token = String::new();
    std::io::stdin()
        .read_to_string(&mut token)
        .context("read token from stdin")?;
    let token = token.trim().to_owned();
    if token.is_empty() {
        anyhow::bail!("no token provided on stdin");
    }
    Ok(token)
}

async fn grant_import(args: AuthGrantImportArgs) -> Result<()> {
    let token = resolve_token(&args)?;
    let api = HttpAgentApi::new(args.api_url.clone());
    let response = api
        .import_auth_grant(api::AuthGrantImportParams {
            grant_id: args.grant_id.clone(),
            provider_id: args.provider_id.clone(),
            token,
            display_name: args.display_name.clone(),
            subject_hint: args.subject_hint.clone(),
            scopes: args.scopes.clone(),
            audience: args.audience.clone(),
            expires_at_ms: args.expires_at_ms,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    print_grant(&response.grant);
    Ok(())
}

async fn grant_list(args: AuthGrantListArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .list_auth_grants(api::AuthGrantListParams {
            status: args.status.map(Into::into),
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    if response.grants.is_empty() {
        println!("grants 0");
        return Ok(());
    }
    for grant in &response.grants {
        print_grant_summary(grant);
    }
    Ok(())
}

async fn grant_read(args: AuthGrantReadArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .read_auth_grant(api::AuthGrantReadParams {
            grant_id: args.grant_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    print_grant(&response.grant);
    Ok(())
}

async fn grant_revoke(args: AuthGrantRevokeArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .revoke_auth_grant(api::AuthGrantRevokeParams {
            grant_id: args.grant_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    print_grant(&response.grant);
    Ok(())
}

fn resolve_client_secret(args: &AuthClientAddArgs) -> Result<Option<String>> {
    if let Some(secret) = &args.client_secret {
        return Ok(Some(secret.clone()));
    }
    if let Some(name) = &args.client_secret_env {
        let value = std::env::var(name)
            .with_context(|| format!("environment variable {name} is not set"))?;
        if value.is_empty() {
            anyhow::bail!("environment variable {name} is empty");
        }
        return Ok(Some(value));
    }
    if args.client_secret_stdin {
        let mut secret = String::new();
        std::io::stdin()
            .read_to_string(&mut secret)
            .context("read client secret from stdin")?;
        let secret = secret.trim().to_owned();
        if secret.is_empty() {
            anyhow::bail!("no client secret provided on stdin");
        }
        return Ok(Some(secret));
    }
    Ok(None)
}

async fn client_add(args: AuthClientAddArgs) -> Result<()> {
    let client_secret = resolve_client_secret(&args)?;
    let api = HttpAgentApi::new(args.api_url.clone());
    let response = api
        .create_auth_client(api::AuthClientCreateParams {
            client_id: args.client_id.clone(),
            provider_id: args.provider_id.clone(),
            provider_kind: args.kind.into(),
            display_name: args.display_name.clone(),
            authorization_endpoint: args.authorization_endpoint.clone(),
            token_endpoint: args.token_endpoint.clone(),
            remote_client_id: args.remote_client_id.clone(),
            client_secret,
            token_endpoint_auth_method: args.auth_method.map(Into::into),
            scopes_default: args.scopes.clone(),
            audience: args.audience.clone(),
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    print_client(&response.client);
    Ok(())
}

async fn client_list(args: AuthClientListArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .list_auth_clients(api::AuthClientListParams {})
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    if response.clients.is_empty() {
        println!("clients 0");
        return Ok(());
    }
    for client in &response.clients {
        println!(
            "{} {} {:?} {}",
            client.client_id, client.provider_id, client.provider_kind, client.remote_client_id
        );
    }
    Ok(())
}

async fn client_read(args: AuthClientReadArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .read_auth_client(api::AuthClientReadParams {
            client_id: args.client_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    print_client(&response.client);
    Ok(())
}

async fn client_remove(args: AuthClientRemoveArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .delete_auth_client(api::AuthClientDeleteParams {
            client_id: args.client_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    print_client(&response.client);
    Ok(())
}

const LOGIN_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

async fn login(args: AuthLoginArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url.clone());
    let started = api
        .start_auth_flow(api::AuthFlowStartParams {
            client_id: args.client_id.clone(),
            scopes: if args.scopes.is_empty() {
                None
            } else {
                Some(args.scopes.clone())
            },
            audience: args.audience.clone(),
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;

    eprintln!("Open this URL in your browser to authorize:");
    println!("{}", started.authorize_url);
    eprintln!("flowId {}", started.flow_id);
    if args.no_wait {
        return Ok(());
    }
    eprintln!("Waiting for the authorization callback (ctrl-c to stop waiting)...");

    loop {
        tokio::time::sleep(LOGIN_POLL_INTERVAL).await;
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
                if args.json {
                    println!("{}", serde_json::to_string_pretty(&response)?);
                    return Ok(());
                }
                let grant_id = response.flow.grant_id.as_deref().unwrap_or("<missing>");
                println!("login complete");
                println!("grantId {grant_id}");
                return Ok(());
            }
            api::AuthFlowStatus::Failed => {
                if args.json {
                    println!("{}", serde_json::to_string_pretty(&response)?);
                }
                anyhow::bail!(
                    "authorization failed: {}",
                    response.flow.error.as_deref().unwrap_or("unknown error")
                );
            }
            api::AuthFlowStatus::Expired => {
                anyhow::bail!("authorization flow expired before the callback completed");
            }
        }
    }
}

fn print_client(client: &api::OAuthClientView) {
    println!("clientId {}", client.client_id);
    println!("providerId {}", client.provider_id);
    println!("providerKind {:?}", client.provider_kind);
    if let Some(display_name) = &client.display_name {
        println!("displayName {display_name}");
    }
    println!("authorizationEndpoint {}", client.authorization_endpoint);
    println!("tokenEndpoint {}", client.token_endpoint);
    println!("remoteClientId {}", client.remote_client_id);
    println!("hasClientSecret {}", client.has_client_secret);
    println!("authMethod {:?}", client.token_endpoint_auth_method);
    if !client.scopes_default.is_empty() {
        println!("scopesDefault {}", client.scopes_default.join(" "));
    }
    if let Some(audience) = &client.audience {
        println!("audience {audience}");
    }
    println!("createdAtMs {}", client.created_at_ms);
    println!("updatedAtMs {}", client.updated_at_ms);
}

fn print_grant_summary(grant: &api::AuthGrantView) {
    println!(
        "{} {} {:?} {:?}",
        grant.grant_id, grant.provider_id, grant.provider_kind, grant.status
    );
}

fn print_grant(grant: &api::AuthGrantView) {
    println!("grantId {}", grant.grant_id);
    println!("providerId {}", grant.provider_id);
    println!("providerKind {:?}", grant.provider_kind);
    println!("status {:?}", grant.status);
    if let Some(display_name) = &grant.display_name {
        println!("displayName {display_name}");
    }
    if let Some(subject_hint) = &grant.subject_hint {
        println!("subjectHint {subject_hint}");
    }
    if !grant.scopes.is_empty() {
        println!("scopes {}", grant.scopes.join(" "));
    }
    if let Some(audience) = &grant.audience {
        println!("audience {audience}");
    }
    println!("hasAccessToken {}", grant.has_access_token);
    if grant.has_refresh_token {
        println!("hasRefreshToken true");
    }
    if let Some(expires_at_ms) = grant.expires_at_ms {
        println!("expiresAtMs {expires_at_ms}");
    }
    println!("createdAtMs {}", grant.created_at_ms);
    println!("updatedAtMs {}", grant.updated_at_ms);
}
