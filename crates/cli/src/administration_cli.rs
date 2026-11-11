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
    /// Show the active universe's UUID and current handle.
    Status,
    /// Persist this connection's universe selection.
    Use { target: String },
    /// Create a runtime universe; does not change the selected universe.
    Create {
        #[arg(long)]
        id: Option<uuid::Uuid>,
        /// The universe's deployment-unique name; derived from the UUID when
        /// omitted.
        #[arg(long)]
        handle: Option<String>,
    },
    /// Read a universe and its resource counts.
    Read { target: String },
    /// Rename a universe. Requests and URLs that use the previous handle
    /// stop reaching it.
    SetHandle { target: String, handle: String },
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
            // The selection may be a handle; the listing gives its UUID.
            let selected = active.active_universe();
            let is_active = |universe: &api::DeploymentUniverseView| {
                selected.as_deref() == Some(universe.universe_id.as_str())
                    || selected.as_deref() == Some(universe.handle.as_str())
            };
            let active_id = response
                .universes
                .iter()
                .find(|universe| is_active(universe))
                .map(|universe| universe.universe_id.clone())
                .or_else(|| {
                    selected
                        .clone()
                        .filter(|s| uuid::Uuid::parse_str(s).is_ok())
                });
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
                        active: is_active(&universe),
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
                    println!("  UUID                                  HANDLE");
                }
                for u in response.universes {
                    println!(
                        "{} {}  {}",
                        if is_active(&u) { "*" } else { " " },
                        u.universe_id,
                        u.handle
                    );
                }
            }
        }
        UniverseCommand::Create { id, handle } => {
            let response: api::DeploymentUniverseCreateResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_UNIVERSES_CREATE,
                    api::DeploymentUniverseCreateParams {
                        universe_id: id.unwrap_or_else(uuid::Uuid::new_v4).to_string(),
                        handle,
                    },
                )
                .await?
                .result;
            if args.json {
                print_json(&response)?;
            } else {
                println!(
                    "{}  {}",
                    response.universe.universe_id, response.universe.handle
                );
            }
        }
        UniverseCommand::SetHandle { target, handle } => {
            let response: api::DeploymentUniverseReadResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_UNIVERSES_HANDLE_PUT,
                    api::DeploymentUniverseHandlePutParams {
                        universe: target,
                        handle,
                    },
                )
                .await?
                .result;
            if args.json {
                print_json(&response)?;
            } else {
                println!(
                    "{}  {}",
                    response.universe.universe_id, response.universe.handle
                );
            }
        }
        UniverseCommand::Read { target } => {
            let response: api::DeploymentUniverseReadResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_UNIVERSES_READ,
                    api::DeploymentUniverseReadParams { universe: target },
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
/// The scope a key belongs to: the deployment, or one universe.
#[derive(Debug, Clone, Args)]
struct KeyScopeArgs {
    /// A deployment key.
    #[arg(
        long,
        required_unless_present = "universe_id",
        conflicts_with = "universe_id"
    )]
    deployment: bool,
    /// A key of this universe.
    #[arg(long)]
    universe_id: Option<uuid::Uuid>,
}

impl KeyScopeArgs {
    fn scope(&self) -> api::AccessScope {
        self.universe_id
            .map_or(api::AccessScope::Deployment, |universe_id| {
                api::AccessScope::Universe { universe_id }
            })
    }
}

#[derive(Debug, Subcommand)]
enum ApiKeyCommand {
    List,
    /// Create a key with a generated secret, which is printed once.
    Create {
        #[arg(long)]
        name: String,
        #[command(flatten)]
        scope: KeyScopeArgs,
        /// The key's id within its scope; allocated when omitted.
        #[arg(long)]
        key_id: Option<String>,
        #[arg(long = "group")]
        groups: Vec<String>,
        #[arg(long)]
        assert_actor: bool,
    },
    /// Declare a key with a secret you supply: afterwards the key of this id
    /// exists, answers to exactly that secret, and holds these groups.
    /// Repeating the command changes nothing; a different secret replaces
    /// the previous one at once. Generate one with `lightspeed generate
    /// secret api-key`.
    Put {
        #[arg(long)]
        key_id: String,
        #[arg(long)]
        name: String,
        #[command(flatten)]
        scope: KeyScopeArgs,
        /// A method group the key may call (repeatable); omitted grants
        /// every group its scope allows.
        #[arg(long = "group")]
        groups: Vec<String>,
        #[arg(long)]
        assert_actor: bool,
        #[command(flatten)]
        secret: crate::key_secrets::SecretSourceArgs,
    },
    /// Replace an active key's secret with a generated one immediately;
    /// prints the new secret once.
    Rotate {
        key_id: String,
        #[command(flatten)]
        scope: KeyScopeArgs,
    },
    Revoke {
        key_id: String,
        #[command(flatten)]
        scope: KeyScopeArgs,
    },
}

fn parse_groups(groups: &[String]) -> Result<Option<Vec<api::MethodGroup>>> {
    if groups.is_empty() {
        return Ok(None);
    }
    groups
        .iter()
        .map(|g| api::MethodGroup::parse(g).with_context(|| format!("unknown method group {g}")))
        .collect::<Result<Vec<_>>>()
        .map(Some)
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
                    println!("KEY ID  PREFIX  STATUS  SCOPE  NAME");
                }
                for key in response.api_keys {
                    println!(
                        "{}  {}  {}  {}  {}",
                        key.key_id,
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
            scope,
            key_id,
            groups,
            assert_actor,
        } => {
            let response: api::DeploymentApiKeyCreateResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_API_KEYS_CREATE,
                    api::DeploymentApiKeyCreateParams {
                        scope: scope.scope(),
                        key_id,
                        groups: parse_groups(&groups)?,
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
        ApiKeyCommand::Put {
            key_id,
            name,
            scope,
            groups,
            assert_actor,
            secret,
        } => {
            let response: api::DeploymentApiKeyPutResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_API_KEYS_PUT,
                    api::DeploymentApiKeyPutParams {
                        scope: scope.scope(),
                        key_id,
                        groups: parse_groups(&groups)?,
                        assert_actor,
                        display_name: name,
                        secret: secret.read()?,
                    },
                )
                .await?
                .result;
            if !args.json {
                println!(
                    "{}",
                    match (response.created, response.secret_replaced) {
                        (true, _) => "API key created with the supplied secret.",
                        (false, true) =>
                            "API key now answers to the supplied secret. The previous secret no longer works.",
                        (false, false) => "API key is as declared.",
                    }
                );
            }
            crate::output::show(args.json, &response)?;
        }
        ApiKeyCommand::Rotate { key_id, scope } => {
            let response: api::DeploymentApiKeyCreateResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_API_KEYS_ROTATE,
                    api::DeploymentApiKeyRotateParams {
                        scope: scope.scope(),
                        key_id,
                    },
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
        ApiKeyCommand::Revoke { key_id, scope } => {
            let response: api::DeploymentApiKeyRevokeResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_API_KEYS_REVOKE,
                    api::DeploymentApiKeyRevokeParams {
                        scope: scope.scope(),
                        key_id,
                    },
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
