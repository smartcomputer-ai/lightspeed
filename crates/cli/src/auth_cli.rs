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

pub(crate) async fn handle(args: AuthArgs) -> Result<()> {
    match args.command {
        AuthCommand::Grant(args) => grant(args).await,
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
