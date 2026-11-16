use std::io::Read as _;

use anyhow::{Context, Result};
use clap::{ArgGroup, Args, Subcommand, ValueEnum};

use crate::api_client::HttpAgentApi;
use crate::request_headers::request_headers_from_args;

#[derive(Args, Debug, Clone)]
pub(crate) struct CredentialArgs {
    #[command(subcommand)]
    command: CredentialCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum CredentialCommand {
    #[command(flatten)]
    Grant(CredentialGrantCommand),
    /// Manage OAuth client configurations.
    #[command(name = "oauth-client", visible_alias = "oauth-clients")]
    Client(CredentialClientArgs),
    /// Run an OAuth authorization flow and store the resulting grant.
    Login(CredentialLoginArgs),
    /// Manage GitHub App providers and installation grants.
    Github(CredentialGithubArgs),
}

#[derive(Args, Debug, Clone)]
pub(crate) struct CredentialModelArgs {
    #[command(subcommand)]
    command: CredentialModelCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum CredentialModelCommand {
    /// Read one stored model-provider connection without revealing its credential.
    Read(CredentialModelRemoveArgs),
    /// Declare a model provider: afterwards the provider of this id exists
    /// with this endpoint and this credential. The credential is an API key
    /// (stored encrypted), a credential obtained through sign-in (`--grant`,
    /// sent as a refreshed OAuth bearer token), or none (`--no-auth`).
    /// Repeating the command changes nothing.
    Put(Box<CredentialModelPutArgs>),
    /// List stored model provider credentials.
    List(CredentialModelListArgs),
    /// Remove a stored model provider credential.
    #[command(name = "delete")]
    Remove(CredentialModelRemoveArgs),
}

#[derive(Args, Clone)]
#[command(group(ArgGroup::new("api_key_source").required(true)))]
struct CredentialModelPutArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Emit the result as JSON.
    #[arg(long)]
    json: bool,
    /// model provider id from the session model selection (e.g. "openai",
    /// "anthropic"). Stored as the `model:<provider_id>` credential provider row.
    provider_id: String,
    /// Read the API key from this file.
    #[arg(long = "api-key-file", group = "api_key_source")]
    api_key_file: Option<std::path::PathBuf>,
    /// Read the API key from this environment variable.
    #[arg(long = "api-key-env", group = "api_key_source")]
    api_key_env: Option<String>,
    /// Read the API key from stdin.
    #[arg(long = "api-key-stdin", group = "api_key_source")]
    api_key_stdin: bool,
    /// A compatible provider's API base URL.
    #[arg(long)]
    base_url: Option<String>,
    /// Supported API kind; repeat for multiple kinds. Requires --base-url.
    #[arg(long = "api-kind", requires = "base_url")]
    api_kinds: Vec<String>,
    /// Extra request header with a plain, non-secret value, as NAME=VALUE.
    /// Repeat for several. Every reader of the provider sees the value.
    #[arg(long = "header", value_name = "NAME=VALUE", requires = "base_url")]
    headers: Vec<String>,
    /// Extra request header carrying a saved static-token credential's token,
    /// sent raw, as NAME=GRANT_ID. Repeat for several. Names that carry a
    /// secret (`api-key`, `x-api-key`, `proxy-authorization`) are accepted
    /// only this way.
    #[arg(
        long = "header-credential",
        value_name = "NAME=GRANT_ID",
        requires = "base_url"
    )]
    header_credentials: Vec<String>,
    /// Register a credentialless endpoint.
    #[arg(long, group = "api_key_source", requires = "base_url")]
    no_auth: bool,
    /// Use a credential obtained through sign-in (for example from
    /// `lightspeed credential login`) instead of an API key.
    #[arg(long = "grant", group = "api_key_source")]
    grant_id: Option<String>,
    /// With --grant: the audience requested for its access token, typically
    /// the provider's API base URL. Omit only for an audience-unrestricted
    /// credential.
    #[arg(long, requires = "grant_id")]
    audience: Option<String>,
    /// Optional display name.
    #[arg(long = "display-name")]
    display_name: Option<String>,
    /// Report what would change without changing it.
    #[arg(long = "dry-run")]
    dry_run: bool,
}

impl std::fmt::Debug for CredentialModelPutArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialModelPutArgs")
            .field("api_url", &self.api_url)
            .field("provider_id", &self.provider_id)
            .field("api_key_file", &self.api_key_file)
            .field("api_key_env", &self.api_key_env)
            .field("api_key_stdin", &self.api_key_stdin)
            .finish_non_exhaustive()
    }
}

#[derive(Args, Debug, Clone)]
struct CredentialModelListArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Emit providers as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug, Clone)]
struct CredentialModelRemoveArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Emit the removed provider as JSON.
    #[arg(long)]
    json: bool,
    /// model provider id (e.g. "openai") or full `model:<provider_id>` row id.
    provider_id: String,
}

#[derive(Args, Debug, Clone)]
struct CredentialGithubArgs {
    #[command(subcommand)]
    command: CredentialGithubCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum CredentialGithubCommand {
    /// Manage GitHub App provider configurations.
    App(CredentialGithubAppArgs),
    /// Manage GitHub App installations and their grants.
    Installation(CredentialGithubInstallationArgs),
}

#[derive(Args, Debug, Clone)]
struct CredentialGithubAppArgs {
    #[command(subcommand)]
    command: CredentialGithubAppCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum CredentialGithubAppCommand {
    /// Register a GitHub App: stores the private key encrypted and the app
    /// config as a credential provider.
    #[command(name = "create")]
    Add(CredentialGithubAppAddArgs),
    /// List registered GitHub Apps.
    List(CredentialGithubAppListArgs),
    /// Read a registered GitHub App.
    Read(CredentialGithubAppReadArgs),
    /// Delete a registered GitHub App and its stored credential.
    #[command(name = "delete")]
    Remove(CredentialGithubAppRemoveArgs),
}

#[derive(Args, Clone)]
#[command(group(ArgGroup::new("private_key_source").required(true)))]
struct CredentialGithubAppAddArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Emit the created provider as JSON.
    #[arg(long)]
    json: bool,
    /// Optional stable provider id. Generated when omitted.
    #[arg(long = "id")]
    provider_id: Option<String>,
    /// GitHub's numeric app id (from the app settings page).
    #[arg(long = "app-id")]
    app_id: String,
    /// Read the private key PEM from this file.
    #[arg(long = "private-key-file", group = "private_key_source")]
    private_key_file: Option<std::path::PathBuf>,
    /// Read the private key PEM from this environment variable.
    #[arg(long = "private-key-env", group = "private_key_source")]
    private_key_env: Option<String>,
    /// Read the private key PEM from stdin.
    #[arg(long = "private-key-stdin", group = "private_key_source")]
    private_key_stdin: bool,
    /// REST API base URL; override for GitHub Enterprise Server.
    #[arg(long = "api-base-url")]
    api_base_url: Option<String>,
    /// Optional display name.
    #[arg(long = "display-name")]
    display_name: Option<String>,
}

impl std::fmt::Debug for CredentialGithubAppAddArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialGithubAppAddArgs")
            .field("api_url", &self.api_url)
            .field("provider_id", &self.provider_id)
            .field("app_id", &self.app_id)
            .field("private_key_file", &self.private_key_file)
            .field("private_key_env", &self.private_key_env)
            .field("private_key_stdin", &self.private_key_stdin)
            .field("api_base_url", &self.api_base_url)
            .finish_non_exhaustive()
    }
}

#[derive(Args, Debug, Clone)]
struct CredentialGithubAppListArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Emit providers as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug, Clone)]
struct CredentialGithubAppReadArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Emit the provider as JSON.
    #[arg(long)]
    json: bool,
    /// Provider id to read.
    provider_id: String,
}

#[derive(Args, Debug, Clone)]
struct CredentialGithubAppRemoveArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Emit the removed provider as JSON.
    #[arg(long)]
    json: bool,
    /// Provider id to remove.
    provider_id: String,
}

#[derive(Args, Debug, Clone)]
struct CredentialGithubInstallationArgs {
    #[command(subcommand)]
    command: CredentialGithubInstallationCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum CredentialGithubInstallationCommand {
    /// List the app's installations, live from GitHub.
    List(CredentialGithubInstallationListArgs),
    /// Record an installation as a grant; tokens are minted on demand.
    Grant(CredentialGithubInstallationGrantArgs),
}

#[derive(Args, Debug, Clone)]
struct CredentialGithubInstallationListArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Emit installations as JSON.
    #[arg(long)]
    json: bool,
    /// GitHub App provider id.
    #[arg(long = "app")]
    provider_id: String,
}

#[derive(Args, Debug, Clone)]
struct CredentialGithubInstallationGrantArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Emit the grant as JSON.
    #[arg(long)]
    json: bool,
    /// GitHub App provider id.
    #[arg(long = "app")]
    provider_id: String,
    /// Installation id (from `installation list`).
    #[arg(long = "installation-id")]
    installation_id: i64,
    /// Optional stable grant id. Generated when omitted.
    #[arg(long = "grant-id")]
    grant_id: Option<String>,
    /// Optional display name.
    #[arg(long = "display-name")]
    display_name: Option<String>,
    /// Let the installation's short-lived tokens be bound into environments
    /// and leased to trusted services. Decided at creation.
    #[arg(long)]
    retrievable: bool,
}

#[derive(Args, Debug, Clone)]
struct CredentialGrantArgs {
    #[command(subcommand)]
    command: CredentialGrantCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum CredentialGrantCommand {
    /// Declare a token credential: afterwards the credential of this id
    /// exists with these settings and holds exactly this token. Repeating
    /// the command changes nothing; unset flags clear their setting.
    Put(CredentialGrantPutArgs),
    /// Replace the value of a token credential, keeping its settings, its id
    /// and every server, header or environment variable that references it.
    Replace(CredentialGrantReplaceArgs),
    /// List stored external credentials.
    List(CredentialGrantListArgs),
    /// Inspect external credential metadata without returning secret values.
    Read(CredentialGrantReadArgs),
    /// Revoke an external credential.
    Revoke(CredentialGrantRevokeArgs),
}

#[derive(Args, Clone)]
#[command(group(ArgGroup::new("token_source").required(true)))]
struct CredentialGrantPutArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Emit the result as JSON.
    #[arg(long)]
    json: bool,
    /// The credential's id.
    grant_id: String,
    /// Provider id recorded on the credential. Defaults to "static" and
    /// cannot be changed afterwards.
    #[arg(long = "provider-id")]
    provider_id: Option<String>,
    /// Read the token from this file, such as a mounted secret.
    #[arg(long = "token-file", group = "token_source")]
    token_file: Option<std::path::PathBuf>,
    /// Read the token from this environment variable.
    #[arg(long = "token-env", group = "token_source")]
    token_env: Option<String>,
    /// Read the token from stdin.
    #[arg(long = "token-stdin", group = "token_source")]
    token_stdin: bool,
    #[arg(long = "display-name")]
    display_name: Option<String>,
    /// Subject hint (for example an account or user name).
    #[arg(long = "subject-hint")]
    subject_hint: Option<String>,
    /// Scope entry. Repeat to record multiple.
    #[arg(long = "scope")]
    scopes: Vec<String>,
    /// Audience the credential is bound to (for MCP: the server URL).
    #[arg(long)]
    audience: Option<String>,
    /// Expiry in unix milliseconds. Omit for a token that does not expire.
    #[arg(long = "expires-at-ms")]
    expires_at_ms: Option<i64>,
    /// Let the plain token be bound into environments and leased to trusted
    /// services. Without it the token stays inside the runtime. Decided at
    /// creation and cannot be changed afterwards.
    #[arg(long)]
    retrievable: bool,
    /// Report what would change without changing it.
    #[arg(long = "dry-run")]
    dry_run: bool,
}

impl std::fmt::Debug for CredentialGrantPutArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialGrantPutArgs")
            .field("api_url", &self.api_url)
            .field("grant_id", &self.grant_id)
            .field("token_file", &self.token_file)
            .field("token_env", &self.token_env)
            .field("token_stdin", &self.token_stdin)
            .field("audience", &self.audience)
            .finish_non_exhaustive()
    }
}

#[derive(Args, Clone)]
#[command(group(ArgGroup::new("token_source").required(true)))]
struct CredentialGrantReplaceArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Emit the updated grant as JSON.
    #[arg(long)]
    json: bool,
    /// Token credential whose value to replace.
    grant_id: String,
    /// Read the new token from this file, such as a mounted secret.
    #[arg(long = "token-file", group = "token_source")]
    token_file: Option<std::path::PathBuf>,
    /// Read the new token from this environment variable.
    #[arg(long = "token-env", group = "token_source")]
    token_env: Option<String>,
    /// Read the new token from stdin.
    #[arg(long = "token-stdin", group = "token_source")]
    token_stdin: bool,
    /// Expiry of the new token in unix milliseconds. Omit for a token that
    /// does not expire; the old token's expiry is not carried over.
    #[arg(long = "expires-at-ms")]
    expires_at_ms: Option<i64>,
}

impl std::fmt::Debug for CredentialGrantReplaceArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialGrantReplaceArgs")
            .field("api_url", &self.api_url)
            .field("grant_id", &self.grant_id)
            .field("token_file", &self.token_file)
            .field("token_env", &self.token_env)
            .field("token_stdin", &self.token_stdin)
            .finish_non_exhaustive()
    }
}

#[derive(Args, Debug, Clone)]
struct CredentialGrantListArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Emit grants as JSON.
    #[arg(long)]
    json: bool,
    /// Optional status filter.
    #[arg(long)]
    status: Option<CredentialGrantStatusArg>,
}

#[derive(Args, Debug, Clone)]
struct CredentialGrantReadArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Emit the grant as JSON.
    #[arg(long)]
    json: bool,
    /// Grant id to read.
    grant_id: String,
}

#[derive(Args, Debug, Clone)]
struct CredentialGrantRevokeArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Emit the revoked grant as JSON.
    #[arg(long)]
    json: bool,
    /// Grant id to revoke.
    grant_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum CredentialGrantStatusArg {
    Active,
    NeedsReauth,
    Revoked,
    Failed,
}

impl From<CredentialGrantStatusArg> for api::CredentialGrantStatus {
    fn from(value: CredentialGrantStatusArg) -> Self {
        match value {
            CredentialGrantStatusArg::Active => Self::Active,
            CredentialGrantStatusArg::NeedsReauth => Self::NeedsReauth,
            CredentialGrantStatusArg::Revoked => Self::Revoked,
            CredentialGrantStatusArg::Failed => Self::Failed,
        }
    }
}

#[derive(Args, Debug, Clone)]
struct CredentialClientArgs {
    #[command(subcommand)]
    command: CredentialClientCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum CredentialClientCommand {
    /// Register a manually configured OAuth client.
    #[command(name = "create")]
    Add(Box<CredentialClientAddArgs>),
    /// List OAuth clients.
    List(CredentialClientListArgs),
    /// Read an OAuth client.
    Read(CredentialClientReadArgs),
    /// Remove an OAuth client.
    #[command(name = "delete")]
    Remove(CredentialClientRemoveArgs),
}

#[derive(Args, Clone)]
#[command(group(ArgGroup::new("client_secret_source")))]
struct CredentialClientAddArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
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
    kind: CredentialProviderKindArg,
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
    /// Authorization-server issuer discovered for this MCP client.
    #[arg(long = "authorization-server-issuer")]
    authorization_server_issuer: Option<String>,
    /// Require the RFC 9207 `iss` parameter on authorization callbacks.
    #[arg(long = "require-callback-issuer")]
    authorization_response_iss_parameter_supported: bool,
    /// Authorization-server-advertised scope. Repeat to record multiple.
    #[arg(long = "authorization-server-scope")]
    authorization_server_scopes_supported: Vec<String>,
}

impl std::fmt::Debug for CredentialClientAddArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialClientAddArgs")
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
            .field(
                "authorization_server_issuer",
                &self.authorization_server_issuer,
            )
            .field(
                "authorization_response_iss_parameter_supported",
                &self.authorization_response_iss_parameter_supported,
            )
            .field(
                "authorization_server_scopes_supported",
                &self.authorization_server_scopes_supported,
            )
            .finish_non_exhaustive()
    }
}

#[derive(Args, Debug, Clone)]
struct CredentialClientListArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Emit clients as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug, Clone)]
struct CredentialClientReadArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Emit the client as JSON.
    #[arg(long)]
    json: bool,
    /// Client id to read.
    client_id: String,
}

#[derive(Args, Debug, Clone)]
struct CredentialClientRemoveArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Emit the removed client as JSON.
    #[arg(long)]
    json: bool,
    /// Client id to remove.
    client_id: String,
}

#[derive(Args, Debug, Clone)]
struct CredentialLoginArgs {
    /// JSON-RPC agent API URL.
    #[arg(skip)]
    api_url: String,
    /// Emit the final flow status as JSON.
    #[arg(long)]
    json: bool,
    /// OAuth client id to authorize against. Use `mcp:<server-id>` for a
    /// catalogued OAuth MCP server: the gateway discovers the authorization
    /// server and registers a client automatically on first login.
    client_id: String,
    /// Scope override. Repeat to request multiple.
    #[arg(long = "scope")]
    scopes: Vec<String>,
    /// Audience override (for MCP: the server URL).
    #[arg(long)]
    audience: Option<String>,
    /// Print the authorization URL and flow id, then exit without waiting
    /// for the callback. Check progress with credential flow status polling.
    #[arg(long = "no-wait")]
    no_wait: bool,
    /// Let the resulting credential's token be bound into environments and
    /// leased to trusted services. Decided at creation.
    #[arg(long)]
    retrievable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum CredentialProviderKindArg {
    McpOauth,
    CustomOauth,
}

impl From<CredentialProviderKindArg> for api::CredentialProviderKind {
    fn from(value: CredentialProviderKindArg) -> Self {
        match value {
            CredentialProviderKindArg::McpOauth => Self::McpOAuth,
            CredentialProviderKindArg::CustomOauth => Self::CustomOAuth,
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

pub(crate) async fn handle(args: CredentialArgs) -> Result<()> {
    match args.command {
        CredentialCommand::Grant(command) => grant(CredentialGrantArgs { command }).await,
        CredentialCommand::Client(args) => client(args).await,
        CredentialCommand::Login(args) => login(args).await,
        CredentialCommand::Github(args) => github(args).await,
    }
}

pub(crate) async fn model(args: CredentialModelArgs) -> Result<()> {
    match args.command {
        CredentialModelCommand::Read(args) => model_read(args).await,
        CredentialModelCommand::Put(args) => model_put(*args).await,
        CredentialModelCommand::List(args) => model_list(args).await,
        CredentialModelCommand::Remove(args) => model_remove(args).await,
    }
}

async fn github(args: CredentialGithubArgs) -> Result<()> {
    match args.command {
        CredentialGithubCommand::App(args) => match args.command {
            CredentialGithubAppCommand::Add(args) => github_app_add(args).await,
            CredentialGithubAppCommand::List(args) => github_app_list(args).await,
            CredentialGithubAppCommand::Read(args) => github_app_read(args).await,
            CredentialGithubAppCommand::Remove(args) => github_app_remove(args).await,
        },
        CredentialGithubCommand::Installation(args) => match args.command {
            CredentialGithubInstallationCommand::List(args) => github_installation_list(args).await,
            CredentialGithubInstallationCommand::Grant(args) => {
                github_installation_grant(args).await
            }
        },
    }
}

async fn client(args: CredentialClientArgs) -> Result<()> {
    match args.command {
        CredentialClientCommand::Add(args) => client_add(*args).await,
        CredentialClientCommand::List(args) => client_list(args).await,
        CredentialClientCommand::Read(args) => client_read(args).await,
        CredentialClientCommand::Remove(args) => client_remove(args).await,
    }
}

async fn grant(args: CredentialGrantArgs) -> Result<()> {
    match args.command {
        CredentialGrantCommand::Put(args) => grant_put(args).await,
        CredentialGrantCommand::Replace(args) => grant_replace(args).await,
        CredentialGrantCommand::List(args) => grant_list(args).await,
        CredentialGrantCommand::Read(args) => grant_read(args).await,
        CredentialGrantCommand::Revoke(args) => grant_revoke(args).await,
    }
}

/// A token is read from a file, an environment variable or standard input,
/// never from an argument, where other processes and the shell history
/// would see it.
fn resolve_token(
    token_file: Option<&std::path::Path>,
    token_env: Option<&String>,
    _token_stdin: bool,
) -> Result<String> {
    if let Some(path) = token_file {
        let token = std::fs::read_to_string(path)
            .with_context(|| format!("read token from {}", path.display()))?;
        let token = token.trim().to_owned();
        if token.is_empty() {
            anyhow::bail!("token file {} is empty", path.display());
        }
        return Ok(token);
    }
    if let Some(name) = token_env {
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

/// Brokered unless the command asked for a retrievable credential.
fn exposure(retrievable: bool) -> api::CredentialGrantExposure {
    if retrievable {
        api::CredentialGrantExposure::Retrievable
    } else {
        api::CredentialGrantExposure::Brokered
    }
}

async fn grant_put(args: CredentialGrantPutArgs) -> Result<()> {
    let token = resolve_token(
        args.token_file.as_deref(),
        args.token_env.as_ref(),
        args.token_stdin,
    )?;
    let response = HttpAgentApi::new(args.api_url.clone())
        .put_credential_grant(api::CredentialGrantPutParams {
            grant_id: args.grant_id.clone(),
            provider_id: args.provider_id.clone(),
            exposure: exposure(args.retrievable),
            token,
            display_name: args.display_name.clone(),
            subject_hint: args.subject_hint.clone(),
            scopes: args.scopes.clone(),
            audience: args.audience.clone(),
            expires_at_ms: args.expires_at_ms,
            metadata: None,
            dry_run: args.dry_run,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    print_put(&response, args.dry_run, args.json)
}

/// Replace only the value: the credential is declared again with the
/// settings it has and the new token.
async fn grant_replace(args: CredentialGrantReplaceArgs) -> Result<()> {
    let token = resolve_token(
        args.token_file.as_deref(),
        args.token_env.as_ref(),
        args.token_stdin,
    )?;
    let api = HttpAgentApi::new(args.api_url.clone());
    let current = api
        .read_credential_grant(api::CredentialGrantReadParams {
            grant_id: args.grant_id.clone(),
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result
        .grant;
    let response = api
        .put_credential_grant(api::CredentialGrantPutParams {
            grant_id: current.grant_id,
            provider_id: Some(current.provider_id),
            exposure: current.exposure,
            token,
            display_name: current.display_name,
            subject_hint: current.subject_hint,
            scopes: current.scopes,
            audience: current.audience,
            // The old token's expiry is not carried over.
            expires_at_ms: args.expires_at_ms,
            metadata: Some(current.metadata),
            dry_run: false,
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

fn print_put(response: &api::CredentialGrantPutResponse, dry_run: bool, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(response)?);
        return Ok(());
    }
    println!(
        "{}",
        match (dry_run, response.created, response.token_replaced) {
            (false, true, _) => "Credential created.",
            (false, false, true) => "Credential now holds the supplied token.",
            (false, false, false) => "Credential is as declared.",
            (true, true, _) => "Would create the credential.",
            (true, false, true) => "Would replace the credential's token.",
            (true, false, false) => "Would not replace the credential's token.",
        }
    );
    print_grant(&response.grant);
    Ok(())
}

async fn grant_list(args: CredentialGrantListArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .list_credential_grants(api::CredentialGrantListParams {
            status: args.status.map(Into::into),
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    crate::output::table(
        &response.grants,
        &[
            ("grantId", "ID"),
            ("displayName", "NAME"),
            ("providerKind", "KIND"),
            ("status", "STATUS"),
            ("subjectHint", "SUBJECT"),
        ],
        "No credentials found.",
    )
}

async fn grant_read(args: CredentialGrantReadArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .read_credential_grant(api::CredentialGrantReadParams {
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

async fn grant_revoke(args: CredentialGrantRevokeArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .revoke_credential_grant(api::CredentialGrantRevokeParams {
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

fn resolve_client_secret(args: &CredentialClientAddArgs) -> Result<Option<String>> {
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

async fn client_add(args: CredentialClientAddArgs) -> Result<()> {
    let client_secret = resolve_client_secret(&args)?;
    let api = HttpAgentApi::new(args.api_url.clone());
    let response = api
        .create_credential_client(api::CredentialClientCreateParams {
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
            authorization_server_issuer: args.authorization_server_issuer.clone(),
            authorization_response_iss_parameter_supported: args
                .authorization_response_iss_parameter_supported,
            authorization_server_scopes_supported: args
                .authorization_server_scopes_supported
                .clone(),
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

async fn client_list(args: CredentialClientListArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .list_credential_clients(api::CredentialClientListParams {})
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    crate::output::table(
        &response.clients,
        &[
            ("clientId", "ID"),
            ("displayName", "NAME"),
            ("providerId", "PROVIDER"),
            ("remoteClientId", "OAUTH CLIENT"),
        ],
        "No OAuth clients configured.",
    )
}

async fn client_read(args: CredentialClientReadArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .read_credential_client(api::CredentialClientReadParams {
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

async fn client_remove(args: CredentialClientRemoveArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .delete_credential_client(api::CredentialClientDeleteParams {
            client_id: args.client_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    println!("Deleted OAuth client {}.", response.client.client_id);
    Ok(())
}

const LOGIN_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

async fn login(args: CredentialLoginArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url.clone());
    let started = api
        .start_credential_flow(api::CredentialFlowStartParams {
            exposure: exposure(args.retrievable),
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
    eprintln!("{}", started.authorize_url);
    eprintln!("flowId {}", started.flow_id);
    if args.no_wait {
        return crate::output::show(args.json, &started);
    }
    eprintln!("Waiting for the authorization callback (ctrl-c to stop waiting)...");

    loop {
        tokio::time::sleep(LOGIN_POLL_INTERVAL).await;
        let response = api
            .read_credential_flow_status(api::CredentialFlowStatusParams {
                flow_id: started.flow_id.clone(),
            })
            .await
            .map_err(crate::api_client::api_error)?
            .result;
        match response.flow.status {
            api::CredentialFlowStatus::Pending => continue,
            api::CredentialFlowStatus::Completed => {
                if args.json {
                    println!("{}", serde_json::to_string_pretty(&response)?);
                    return Ok(());
                }
                let grant_id = response.flow.grant_id.as_deref().unwrap_or("<missing>");
                println!("External authorization complete. Credential: {grant_id}");
                return Ok(());
            }
            api::CredentialFlowStatus::Failed => {
                if args.json {
                    println!("{}", serde_json::to_string_pretty(&response)?);
                }
                anyhow::bail!(
                    "authorization failed: {}",
                    response.flow.error.as_deref().unwrap_or("unknown error")
                );
            }
            api::CredentialFlowStatus::Expired => {
                anyhow::bail!("authorization flow expired before the callback completed");
            }
        }
    }
}

fn resolve_private_key(args: &CredentialGithubAppAddArgs) -> Result<String> {
    if let Some(path) = &args.private_key_file {
        let key = std::fs::read_to_string(path)
            .with_context(|| format!("read private key from {}", path.display()))?;
        if key.trim().is_empty() {
            anyhow::bail!("private key file {} is empty", path.display());
        }
        return Ok(key);
    }
    if let Some(name) = &args.private_key_env {
        let key = std::env::var(name)
            .with_context(|| format!("environment variable {name} is not set"))?;
        if key.is_empty() {
            anyhow::bail!("environment variable {name} is empty");
        }
        return Ok(key);
    }
    let mut key = String::new();
    std::io::stdin()
        .read_to_string(&mut key)
        .context("read private key from stdin")?;
    if key.trim().is_empty() {
        anyhow::bail!("no private key provided on stdin");
    }
    Ok(key)
}

/// Normalize a CLI-supplied model provider id to the `model:<provider_id>` auth
/// provider row id.
fn model_provider_row_id(provider_id: &str) -> String {
    if provider_id.starts_with("model:") {
        provider_id.to_owned()
    } else {
        format!("model:{provider_id}")
    }
}

fn resolve_model_api_key(args: &CredentialModelPutArgs) -> Result<String> {
    if let Some(path) = &args.api_key_file {
        let key = std::fs::read_to_string(path)
            .with_context(|| format!("read API key from {}", path.display()))?;
        let key = key.trim().to_owned();
        if key.is_empty() {
            anyhow::bail!("API key file {} is empty", path.display());
        }
        return Ok(key);
    }
    if let Some(name) = &args.api_key_env {
        let key = std::env::var(name)
            .with_context(|| format!("environment variable {name} is not set"))?;
        if key.is_empty() {
            anyhow::bail!("environment variable {name} is empty");
        }
        return Ok(key);
    }
    let mut key = String::new();
    std::io::stdin()
        .read_to_string(&mut key)
        .context("read API key from stdin")?;
    let key = key.trim().to_owned();
    if key.is_empty() {
        anyhow::bail!("no API key provided on stdin");
    }
    Ok(key)
}

async fn model_put(args: CredentialModelPutArgs) -> Result<()> {
    let credential = if args.no_auth || args.grant_id.is_some() {
        None
    } else {
        Some(resolve_model_api_key(&args)?)
    };
    let endpoint = args
        .base_url
        .as_ref()
        .map(|url| -> Result<api::ModelEndpointConfig> {
            if args.api_kinds.is_empty() {
                anyhow::bail!("--base-url requires at least one --api-kind");
            }
            let headers = request_headers_from_args(&args.headers, &args.header_credentials)?;
            Ok(api::ModelEndpointConfig {
                base_url: url.clone(),
                headers,
                api_kinds: args.api_kinds.clone(),
            })
        })
        .transpose()?;
    let config = if let Some(grant_id) = &args.grant_id {
        api::CredentialProviderConfigInput::ModelOAuth {
            grant_id: grant_id.clone(),
            audience: args.audience.clone(),
            endpoint,
        }
    } else if args.no_auth {
        api::CredentialProviderConfigInput::ModelEndpoint {
            endpoint: endpoint.context("--no-auth requires --base-url")?,
        }
    } else {
        api::CredentialProviderConfigInput::ModelApiKey { endpoint }
    };
    let response: api::CredentialProviderPutResponse = HttpAgentApi::new(args.api_url.clone())
        .request(
            api::METHOD_CREDENTIALS_PROVIDERS_PUT,
            api::CredentialProviderPutParams {
                provider_id: model_provider_row_id(&args.provider_id),
                display_name: args.display_name.clone(),
                config,
                credential,
                dry_run: args.dry_run,
            },
        )
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    print_provider(&response.provider);
    println!();
    println!(
        "{}",
        match (response.created, response.credential_replaced, args.dry_run) {
            (true, _, false) => "Created.",
            (true, _, true) => "Would be created.",
            (false, true, false) => "Updated; the stored credential was replaced.",
            (false, true, true) => "Would be updated; the stored credential would be replaced.",
            (false, false, false) => "Declared; the credential is unchanged.",
            (false, false, true) => "Settings would be applied; the credential is unchanged.",
        }
    );
    Ok(())
}

async fn model_list(args: CredentialModelListArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .list_credential_providers(api::CredentialProviderListParams {})
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    let providers: Vec<_> = response
        .providers
        .iter()
        .filter(|provider| {
            matches!(
                provider.provider_kind,
                api::CredentialProviderKind::ModelApiKey
                    | api::CredentialProviderKind::ModelOAuth
                    | api::CredentialProviderKind::ModelEndpoint
            )
        })
        .collect();
    if args.json {
        println!("{}", serde_json::to_string_pretty(&providers)?);
        return Ok(());
    }
    crate::output::table(
        &providers,
        &[
            ("providerId", "ID"),
            ("displayName", "NAME"),
            ("providerKind", "KIND"),
            ("status", "STATUS"),
        ],
        "No model providers configured.",
    )
}

async fn model_read(args: CredentialModelRemoveArgs) -> Result<()> {
    let provider = read_model_provider(&HttpAgentApi::new(args.api_url), &args.provider_id).await?;
    crate::output::show(args.json, &provider)
}
async fn read_model_provider(
    client: &HttpAgentApi,
    id: &str,
) -> Result<api::CredentialProviderView> {
    let provider = client
        .read_credential_provider(api::CredentialProviderReadParams {
            provider_id: model_provider_row_id(id),
        })
        .await?
        .result
        .provider;
    anyhow::ensure!(
        matches!(
            provider.config,
            api::CredentialProviderConfigView::ModelApiKey { .. }
                | api::CredentialProviderConfigView::ModelOAuth { .. }
                | api::CredentialProviderConfigView::ModelEndpoint { .. }
        ),
        "{id} is not a model-provider connection"
    );
    Ok(provider)
}

async fn model_remove(args: CredentialModelRemoveArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    read_model_provider(&api, &args.provider_id).await?;
    let response = api
        .delete_credential_provider(api::CredentialProviderDeleteParams {
            provider_id: model_provider_row_id(&args.provider_id),
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    println!("Deleted model provider {}.", response.provider.provider_id);
    Ok(())
}

async fn github_app_add(args: CredentialGithubAppAddArgs) -> Result<()> {
    let private_key = resolve_private_key(&args)?;
    let api = HttpAgentApi::new(args.api_url.clone());
    let response = api
        .create_credential_provider(api::CredentialProviderCreateParams {
            provider_id: args.provider_id.clone(),
            display_name: args.display_name.clone(),
            config: api::CredentialProviderConfigInput::GitHubApp {
                app_id: args.app_id.clone(),
                api_base_url: args.api_base_url.clone(),
            },
            credential: Some(private_key),
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    print_provider(&response.provider);
    Ok(())
}

async fn github_app_list(args: CredentialGithubAppListArgs) -> Result<()> {
    let providers = HttpAgentApi::new(args.api_url)
        .list_credential_providers(api::CredentialProviderListParams {})
        .await?
        .result
        .providers;
    let providers: Vec<_> = providers
        .into_iter()
        .filter(|provider| {
            matches!(
                provider.config,
                api::CredentialProviderConfigView::GitHubApp { .. }
            )
        })
        .collect();
    if args.json {
        crate::output::show(true, &providers)
    } else {
        crate::output::table(
            &providers,
            &[
                ("providerId", "ID"),
                ("displayName", "NAME"),
                ("status", "STATUS"),
            ],
            "No GitHub Apps configured.",
        )
    }
}

async fn github_app_read(args: CredentialGithubAppReadArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let current = api
        .read_credential_provider(api::CredentialProviderReadParams {
            provider_id: args.provider_id.clone(),
        })
        .await?
        .result
        .provider;
    anyhow::ensure!(
        matches!(
            current.config,
            api::CredentialProviderConfigView::GitHubApp { .. }
        ),
        "{} is not a GitHub App provider",
        args.provider_id
    );
    let response = api
        .read_credential_provider(api::CredentialProviderReadParams {
            provider_id: args.provider_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    print_provider(&response.provider);
    Ok(())
}

async fn github_app_remove(args: CredentialGithubAppRemoveArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let current = api
        .read_credential_provider(api::CredentialProviderReadParams {
            provider_id: args.provider_id.clone(),
        })
        .await?
        .result
        .provider;
    anyhow::ensure!(
        matches!(
            current.config,
            api::CredentialProviderConfigView::GitHubApp { .. }
        ),
        "{} is not a GitHub App provider",
        args.provider_id
    );
    let response = api
        .delete_credential_provider(api::CredentialProviderDeleteParams {
            provider_id: args.provider_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    println!("Deleted GitHub App {}.", response.provider.provider_id);
    Ok(())
}

async fn github_installation_list(args: CredentialGithubInstallationListArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .list_github_installations(api::CredentialGitHubInstallationListParams {
            provider_id: args.provider_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    crate::output::table(
        &response.installations,
        &[
            ("installationId", "ID"),
            ("accountLogin", "ACCOUNT"),
            ("repositorySelection", "REPOSITORIES"),
        ],
        "No GitHub installations found.",
    )
}

async fn github_installation_grant(args: CredentialGithubInstallationGrantArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .grant_github_installation(api::CredentialGitHubInstallationGrantParams {
            exposure: exposure(args.retrievable),
            provider_id: args.provider_id,
            installation_id: args.installation_id,
            grant_id: args.grant_id,
            display_name: args.display_name,
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

fn print_provider(provider: &api::CredentialProviderView) {
    crate::output::show(false, provider).expect("serializable credential metadata");
}

fn print_client(client: &api::OAuthClientView) {
    crate::output::show(false, client).expect("serializable credential metadata");
}

fn print_grant(grant: &api::CredentialGrantView) {
    crate::output::show(false, grant).expect("serializable credential metadata");
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct GrantCommand {
        #[command(subcommand)]
        command: CredentialGrantCommand,
    }

    #[derive(Parser)]
    struct ModelCommand {
        #[command(subcommand)]
        command: CredentialModelCommand,
    }

    #[test]
    fn model_header_flags_build_literal_and_credential_headers() {
        let parsed = ModelCommand::try_parse_from([
            "models",
            "put",
            "router",
            "--no-auth",
            "--base-url",
            "https://router.example/v1",
            "--api-kind",
            "openai:completions",
            "--header",
            "X-Title=Lightspeed",
            "--header-credential",
            "X-Api-Key=authgrant_router",
        ])
        .expect("parse");
        let CredentialModelCommand::Put(put) = parsed.command else {
            panic!("expected put");
        };
        let headers =
            request_headers_from_args(&put.headers, &put.header_credentials).expect("headers");
        assert_eq!(
            headers.get("X-Title"),
            Some(&api::RequestHeader::Literal {
                value: "Lightspeed".to_owned(),
            })
        );
        assert_eq!(
            headers.get("X-Api-Key"),
            Some(&api::RequestHeader::Credential {
                grant_id: "authgrant_router".to_owned(),
            })
        );

        // Headers belong to an endpoint.
        assert!(
            ModelCommand::try_parse_from([
                "models",
                "put",
                "router",
                "--api-key-stdin",
                "--header-credential",
                "X-Api-Key=authgrant_router",
            ])
            .is_err()
        );
    }

    #[test]
    fn grant_replace_needs_one_token_source_and_takes_no_token_argument() {
        let parsed = GrantCommand::try_parse_from([
            "grants",
            "replace",
            "authgrant_upstream",
            "--token-file",
            "/run/secrets/upstream",
        ])
        .expect("parse");
        let CredentialGrantCommand::Replace(args) = parsed.command else {
            panic!("expected replace");
        };
        assert_eq!(args.grant_id, "authgrant_upstream");
        // A token is never an argument: other processes and the shell
        // history would see it.
        assert!(
            GrantCommand::try_parse_from([
                "grants",
                "replace",
                "authgrant_upstream",
                "--token",
                "plaintext",
            ])
            .is_err()
        );

        assert!(
            GrantCommand::try_parse_from(["grants", "replace", "authgrant_upstream"]).is_err(),
            "a token source is required"
        );
        assert!(
            GrantCommand::try_parse_from([
                "grants",
                "replace",
                "authgrant_upstream",
                "--token-env",
                "UPSTREAM_TOKEN",
                "--token-stdin",
            ])
            .is_err(),
            "token sources are exclusive"
        );
    }

    #[test]
    fn a_credential_is_brokered_unless_declared_retrievable() {
        let put = |extra: &[&str]| {
            let mut argv = vec!["grants", "put", "repo-token", "--token-env", "REPO_TOKEN"];
            argv.extend_from_slice(extra);
            let CredentialGrantCommand::Put(args) =
                GrantCommand::try_parse_from(argv).expect("parse").command
            else {
                panic!("expected put");
            };
            exposure(args.retrievable)
        };
        assert_eq!(put(&[]), api::CredentialGrantExposure::Brokered);
        assert_eq!(
            put(&["--retrievable"]),
            api::CredentialGrantExposure::Retrievable
        );
    }
}
