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
    /// List universes; * marks the active universe for this invocation.
    List,
    /// Show the active universe's UUID and current runtime slug.
    Status,
    /// Persist this connection's universe selection.
    Use { target: String },
    /// Create a runtime universe; does not change the selected universe.
    Create {
        #[arg(long)]
        id: Option<uuid::Uuid>,
        #[arg(long)]
        slug: Option<String>,
    },
    /// Read a universe and its resource counts.
    Read { target: String },
    /// Assign or change a runtime slug. Existing URLs may stop working.
    SetSlug { target: String, slug: String },
}
pub async fn universe(args: UniverseArgs) -> Result<()> {
    let active = connection::ACTIVE.get().context("no connection")?;
    let client = HttpAgentApi::new("");
    match args.command {
        UniverseCommand::Use { target } => {
            let id = connection::resolve_universe(active, &target).await?;
            connection::select_universe(active, id.clone())?;
            if args.json {
                print_json(&serde_json::json!({"universeId":id,"connection":active.name}))?;
            } else {
                println!("Selected universe {id}.");
            }
        }
        UniverseCommand::Status => {
            let status = connection::universe_status(active).await?;
            if args.json {
                print_json(&status)?;
            } else {
                status.print("Universe");
            }
        }
        UniverseCommand::List => {
            let response: api::DeploymentUniverseListResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_UNIVERSES_LIST,
                    api::DeploymentUniverseListParams {},
                )
                .await?
                .result;
            let active_id = active.active_universe_id();
            if args.json {
                #[derive(serde::Serialize)]
                struct ListedUniverse {
                    #[serde(flatten)]
                    universe: api::DeploymentUniverseView,
                    active: bool,
                }
                let universes: Vec<_> = response
                    .universes
                    .into_iter()
                    .map(|universe| ListedUniverse {
                        active: active_id.as_deref() == Some(universe.universe_id.as_str()),
                        universe,
                    })
                    .collect();
                print_json(
                    &serde_json::json!({ "universes": universes, "activeUniverseId": active_id }),
                )?;
            } else {
                if response.universes.is_empty() {
                    println!("No universes found.");
                } else {
                    println!("  UUID                                  SLUG");
                }
                for u in response.universes {
                    println!(
                        "{} {}  {}",
                        if active_id.as_deref() == Some(u.universe_id.as_str()) {
                            "*"
                        } else {
                            " "
                        },
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
        UniverseCommand::SetSlug { target, slug } => {
            let id = connection::resolve_universe(active, &target).await?;
            let response: api::DeploymentUniverseReadResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_UNIVERSES_SLUG_PUT,
                    api::DeploymentUniverseSlugPutParams {
                        universe_id: id,
                        slug,
                        only_if_unset: false,
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
            crate::output::show(args.json, &response.universe)?;
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
    /// Replace an active key secret immediately; prints the new secret once.
    Rotate {
        key_prefix: String,
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
                if response.api_keys.is_empty() {
                    println!("No API keys found.");
                } else {
                    println!("PREFIX  STATUS  SCOPE  NAME");
                }
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
            if !args.json {
                println!("API key created. Save the secret now; it is shown only once.");
            }
            crate::output::show(args.json, &response)?;
        }
        ApiKeyCommand::Rotate { key_prefix } => {
            let response: api::DeploymentApiKeyCreateResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_API_KEYS_ROTATE,
                    api::DeploymentApiKeyRotateParams { key_prefix },
                )
                .await?
                .result;
            if !args.json {
                println!(
                    "API key rotated. The old secret no longer works. Save the new secret now."
                );
            }
            crate::output::show(args.json, &response)?;
        }
        ApiKeyCommand::Revoke { key_prefix } => {
            let response: api::DeploymentApiKeyRevokeResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_API_KEYS_REVOKE,
                    api::DeploymentApiKeyRevokeParams { key_prefix },
                )
                .await?
                .result;
            crate::output::show(args.json, &response)?;
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
    /// Inspect, set, or clear universe model defaults.
    Defaults(crate::model_defaults_cli::ModelDefaultsArgs),
    /// Configure provider endpoints and their API-key or OAuth credentials.
    #[command(visible_alias = "providers")]
    Provider(crate::auth_cli::AuthModelArgs),
    List {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        all: bool,
    },
}
pub async fn models(args: ModelsArgs) -> Result<()> {
    let (json, all) = match args.command {
        ModelsCommand::Defaults(args) => return crate::model_defaults_cli::run(args).await,
        ModelsCommand::Provider(args) => return crate::auth_cli::model(args).await,
        ModelsCommand::List { json, all } => (json, all),
    };
    let response = HttpAgentApi::new("")
        .list_models(api::ModelListParams {
            selectable_only: !all,
        })
        .await?
        .result;
    if json {
        print_json(&response)?;
    } else {
        crate::output::table(
            &response.models,
            &[
                ("providerId", "PROVIDER"),
                ("apiKind", "API"),
                ("model", "MODEL"),
            ],
            "No models available. Configure a connection with model provider add.",
        )?;
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
