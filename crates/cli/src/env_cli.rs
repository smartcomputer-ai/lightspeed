use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api_client::HttpAgentApi;

#[derive(Args, Debug, Clone)]
pub(crate) struct EnvArgs {
    #[command(subcommand)]
    command: EnvCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum EnvCommand {
    /// List universe environments.
    List(ResourceArgs),
    /// Read one universe environment.
    Read(EnvironmentResourceArgs),
    /// Activate a universe environment for a session.
    Activate(ActivateArgs),
    /// Clear a session's active environment.
    Deactivate(SessionArgs),
    /// Close one universe environment.
    Close(EnvironmentResourceArgs),
    /// Bind, list, or unbind universe environment credentials.
    Credentials(CredentialArgs),
}

#[derive(Args, Debug, Clone)]
struct ResourceArgs {
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug, Clone)]
struct EnvironmentResourceArgs {
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    #[arg(long)]
    json: bool,
    environment_id: String,
}

#[derive(Args, Debug, Clone)]
struct SessionArgs {
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    session: String,
}

#[derive(Args, Debug, Clone)]
struct ActivateArgs {
    #[command(flatten)]
    session: SessionArgs,
    environment_id: String,
}

#[derive(Args, Debug, Clone)]
struct CredentialArgs {
    #[command(subcommand)]
    command: CredentialCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum CredentialCommand {
    Bind(CredentialBindArgs),
    List(CredentialListArgs),
    Unbind(CredentialUnbindArgs),
}

#[derive(Args, Debug, Clone)]
struct CredentialListArgs {
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    #[arg(long)]
    json: bool,
    environment_id: String,
}

#[derive(Args, Debug, Clone)]
struct CredentialBindArgs {
    #[command(flatten)]
    common: CredentialListArgs,
    #[arg(long = "env-name")]
    env_name: String,
    #[arg(long = "grant-id", conflicts_with_all = ["provider_id", "secret_id"])]
    grant_id: Option<String>,
    #[arg(long = "provider-id", conflicts_with_all = ["grant_id", "secret_id"])]
    provider_id: Option<String>,
    #[arg(long = "secret-id", conflicts_with_all = ["grant_id", "provider_id"])]
    secret_id: Option<String>,
}

#[derive(Args, Debug, Clone)]
struct CredentialUnbindArgs {
    #[command(flatten)]
    common: CredentialListArgs,
    #[arg(long = "env-name")]
    env_name: String,
}

pub(crate) async fn handle(args: EnvArgs) -> Result<()> {
    match args.command {
        EnvCommand::List(args) => list(args).await,
        EnvCommand::Read(args) => read(args).await,
        EnvCommand::Activate(args) => activate(args).await,
        EnvCommand::Deactivate(args) => deactivate(args).await,
        EnvCommand::Close(args) => close(args).await,
        EnvCommand::Credentials(args) => credentials(args).await,
    }
}

async fn list(args: ResourceArgs) -> Result<()> {
    let response = HttpAgentApi::new(args.api_url)
        .list_environments(api::EnvironmentListParams::default())
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    print_json_or(args.json, &response, || {
        for environment in &response.environments {
            let provider = match &environment.source {
                api::EnvironmentSourceView::Provisioned { provider_id, .. } => provider_id.as_str(),
                api::EnvironmentSourceView::Enrolled => "enrolled",
            };
            println!(
                "{} {} {:?}",
                environment.environment_id, provider, environment.status
            );
        }
    })
}

async fn read(args: EnvironmentResourceArgs) -> Result<()> {
    let response = HttpAgentApi::new(args.api_url)
        .read_environment(api::EnvironmentReadParams {
            environment_id: args.environment_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    print_json_or(args.json, &response, || {
        let provider = match &response.environment.source {
            api::EnvironmentSourceView::Provisioned { provider_id, .. } => provider_id.as_str(),
            api::EnvironmentSourceView::Enrolled => "enrolled",
        };
        println!(
            "{} {} {:?}",
            response.environment.environment_id, provider, response.environment.status
        );
    })
}

async fn activate(args: ActivateArgs) -> Result<()> {
    let response = HttpAgentApi::new(args.session.api_url)
        .activate_session_environment(api::SessionEnvironmentActivateParams {
            session_id: args.session.session,
            environment_id: args.environment_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    print_json_or(args.session.json, &response, || {
        println!(
            "active {}",
            response
                .session
                .active_environment_id
                .as_deref()
                .unwrap_or("-")
        );
    })
}

async fn deactivate(args: SessionArgs) -> Result<()> {
    let response = HttpAgentApi::new(args.api_url)
        .deactivate_session_environment(api::SessionEnvironmentDeactivateParams {
            session_id: args.session,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    print_json_or(args.json, &response, || println!("active -"))
}

async fn close(args: EnvironmentResourceArgs) -> Result<()> {
    let response = HttpAgentApi::new(args.api_url)
        .close_environment(api::EnvironmentCloseParams {
            environment_id: args.environment_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    print_json_or(args.json, &response, || {
        println!("closed {}", response.environment.environment_id)
    })
}

async fn credentials(args: CredentialArgs) -> Result<()> {
    match args.command {
        CredentialCommand::Bind(args) => {
            let source = match (args.grant_id, args.provider_id, args.secret_id) {
                (Some(grant_id), None, None) => {
                    api::EnvironmentCredentialSourceView::AuthGrant { grant_id }
                }
                (None, Some(provider_id), None) => {
                    api::EnvironmentCredentialSourceView::AuthProviderCredential { provider_id }
                }
                (None, None, Some(secret_id)) => {
                    api::EnvironmentCredentialSourceView::DirectSecret { secret_id }
                }
                _ => anyhow::bail!("specify exactly one credential source"),
            };
            let response = HttpAgentApi::new(args.common.api_url)
                .bind_environment_credential(api::EnvironmentCredentialBindParams {
                    environment_id: args.common.environment_id,
                    env_name: args.env_name,
                    source,
                })
                .await
                .map_err(crate::api_client::api_error)?
                .result;
            print_json_or(args.common.json, &response, || {
                print_credential(&response.credential)
            })
        }
        CredentialCommand::List(args) => {
            let response = HttpAgentApi::new(args.api_url)
                .list_environment_credentials(api::EnvironmentCredentialListParams {
                    environment_id: args.environment_id,
                })
                .await
                .map_err(crate::api_client::api_error)?
                .result;
            print_json_or(args.json, &response, || {
                for credential in &response.credentials {
                    print_credential(credential);
                }
            })
        }
        CredentialCommand::Unbind(args) => {
            let response = HttpAgentApi::new(args.common.api_url)
                .unbind_environment_credential(api::EnvironmentCredentialUnbindParams {
                    environment_id: args.common.environment_id,
                    env_name: args.env_name,
                })
                .await
                .map_err(crate::api_client::api_error)?
                .result;
            print_json_or(args.common.json, &response, || {
                print_credential(&response.credential)
            })
        }
    }
}

fn print_credential(credential: &api::EnvironmentCredentialView) {
    println!(
        "{} {} {:?}",
        credential.environment_id, credential.env_name, credential.source
    );
}

fn print_json_or<T: serde::Serialize>(json: bool, value: &T, text: impl FnOnce()) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        text();
    }
    Ok(())
}
