use crate::{api_client::HttpAgentApi, connection};
use anyhow::{Context, Result};
use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct UniverseArgs {
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: UniverseCommand,
}
#[derive(Debug, Subcommand)]
enum UniverseCommand {
    List,
    /// Persist this connection's universe selection.
    Use {
        target: String,
    },
    /// Create a runtime universe; does not change the selected universe.
    Create {
        #[arg(long)]
        id: Option<uuid::Uuid>,
        #[arg(long)]
        slug: Option<String>,
    },
    Read {
        target: String,
    },
}
pub async fn universe(args: UniverseArgs) -> Result<()> {
    let active = connection::ACTIVE.get().context("no connection")?;
    let client = HttpAgentApi::new("");
    match args.command {
        UniverseCommand::Use { target } => {
            let id = connection::resolve_universe(active, &target).await?;
            connection::select_universe(active, id)?;
        }
        UniverseCommand::List => {
            let response: api::DeploymentUniverseListResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_UNIVERSES_LIST,
                    api::DeploymentUniverseListParams {},
                )
                .await?
                .result;
            if args.json {
                print_json(&response)?;
            } else {
                for u in response.universes {
                    println!(
                        "{}  {}",
                        u.universe_id,
                        u.slug.as_deref().unwrap_or("(no slug)")
                    );
                }
            }
        }
        UniverseCommand::Create { id, slug } => {
            let response: api::DeploymentUniverseCreateResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_UNIVERSES_CREATE,
                    api::DeploymentUniverseCreateParams {
                        universe_id: id.unwrap_or_else(uuid::Uuid::new_v4).to_string(),
                        slug,
                    },
                )
                .await?
                .result;
            if args.json {
                print_json(&response)?;
            } else {
                println!(
                    "{}  {}",
                    response.universe.universe_id,
                    response.universe.slug.as_deref().unwrap_or("(no slug)")
                );
            }
        }
        UniverseCommand::Read { target } => {
            let id = connection::resolve_universe(active, &target).await?;
            let response: api::DeploymentUniverseReadResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_UNIVERSES_READ,
                    api::DeploymentUniverseReadParams { universe_id: id },
                )
                .await?
                .result;
            print_json(&response)?;
        }
    }
    Ok(())
}

#[derive(Debug, Args)]
pub struct ApiKeyArgs {
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: ApiKeyCommand,
}
#[derive(Debug, Subcommand)]
enum ApiKeyCommand {
    List,
    /// Create a key; the secret is printed once.
    Create {
        #[arg(long)]
        name: String,
        #[arg(
            long,
            required_unless_present = "universe_id",
            conflicts_with = "universe_id"
        )]
        deployment: bool,
        #[arg(long)]
        universe_id: Option<uuid::Uuid>,
        #[arg(long = "group")]
        groups: Vec<String>,
        #[arg(long)]
        assert_actor: bool,
    },
    Revoke {
        key_prefix: String,
    },
}
pub async fn api_key(args: ApiKeyArgs) -> Result<()> {
    let client = HttpAgentApi::new("");
    match args.command {
        ApiKeyCommand::List => {
            let response: api::DeploymentApiKeyListResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_API_KEYS_LIST,
                    api::DeploymentApiKeyListParams::default(),
                )
                .await?
                .result;
            if args.json {
                print_json(&response)?;
            } else {
                for key in response.api_keys {
                    println!(
                        "{}  {}  {}  {}",
                        key.key_prefix,
                        if key.revoked_at_ms.is_some() {
                            "revoked"
                        } else {
                            "active"
                        },
                        serde_json::to_string(&key.scope)?,
                        key.display_name.as_deref().unwrap_or("")
                    );
                }
            }
        }
        ApiKeyCommand::Create {
            name,
            deployment: _,
            universe_id,
            groups,
            assert_actor,
        } => {
            let groups = if groups.is_empty() {
                None
            } else {
                Some(
                    groups
                        .iter()
                        .map(|g| {
                            api::MethodGroup::parse(g)
                                .with_context(|| format!("unknown method group {g}"))
                        })
                        .collect::<Result<Vec<_>>>()?,
                )
            };
            let scope = universe_id.map_or(api::AccessScope::Deployment, |universe_id| {
                api::AccessScope::Universe { universe_id }
            });
            let response: api::DeploymentApiKeyCreateResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_API_KEYS_CREATE,
                    api::DeploymentApiKeyCreateParams {
                        scope,
                        groups,
                        assert_actor,
                        display_name: name,
                    },
                )
                .await?
                .result;
            // This operation intentionally returns the new secret once.
            print_json(&response)?;
        }
        ApiKeyCommand::Revoke { key_prefix } => {
            let response: api::DeploymentApiKeyRevokeResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_API_KEYS_REVOKE,
                    api::DeploymentApiKeyRevokeParams { key_prefix },
                )
                .await?
                .result;
            print_json(&response)?;
        }
    }
    Ok(())
}
#[derive(Debug, Args)]
pub struct ModelsArgs {
    #[command(subcommand)]
    command: ModelsCommand,
}
#[derive(Debug, Subcommand)]
enum ModelsCommand {
    List {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        all: bool,
    },
}
pub async fn models(args: ModelsArgs) -> Result<()> {
    let ModelsCommand::List { json, all } = args.command;
    let response = HttpAgentApi::new("")
        .list_models(api::ModelListParams {
            selectable_only: !all,
        })
        .await?
        .result;
    if json {
        print_json(&response)?;
    } else {
        for model in &response.models {
            println!("{}  {}  {}", model.provider_id, model.api_kind, model.model);
        }
        for provider in &response.providers {
            if let Some(error) = &provider.error {
                eprintln!("{}: {error}", provider.provider_id);
            }
        }
    }
    Ok(())
}
fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
