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
    List {
        /// Show permanent records of deleted universes.
        #[arg(long)]
        deleted: bool,
    },
    /// Fence the universe immediately and remove its resources in the background.
    Delete {
        target: String,
        /// Wait for completion, optionally up to a duration (default: 5m).
        #[arg(long, num_args = 0..=1, default_missing_value = "5m", value_parser = crate::session_cli::parse_duration_ms)]
        wait: Option<u64>,
        /// Skip typing the universe handle to confirm deletion.
        #[arg(long)]
        yes: bool,
    },
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
        UniverseCommand::List { deleted: true } => {
            let mut after = None;
            let mut universes = Vec::new();
            loop {
                let response: api::DeploymentDeletedUniversesListResponse = client
                    .request(
                        api::METHOD_DEPLOYMENT_UNIVERSES_DELETED_LIST,
                        api::DeploymentDeletedUniversesListParams { after },
                    )
                    .await?;
                universes.extend(response.universes);
                after = response.next_after;
                if after.is_none() {
                    break;
                }
            }
            if args.json {
                print_json(&serde_json::json!({ "universes": universes }))?;
            } else {
                for universe in universes {
                    println!(
                        "{}  {}  deleted {}",
                        universe.universe_id, universe.handle, universe.deleted_at_ms
                    );
                }
            }
        }
        UniverseCommand::Delete { target, wait, yes } => {
            let read = client
                .request::<_, api::DeploymentUniverseReadResponse>(
                    api::METHOD_DEPLOYMENT_UNIVERSES_READ,
                    api::DeploymentUniverseReadParams {
                        universe: target.clone(),
                    },
                )
                .await;
            let (id, handle) = match read {
                Ok(response) => (response.universe.universe_id, response.universe.handle),
                Err(error) if error.kind == api::AgentApiErrorKind::Gone => {
                    let record = error
                        .deleted_universe
                        .context("gone response lacks deleted universe record")?;
                    let response = api::DeploymentUniverseDeleteResponse {
                        universe_id: record.universe_id,
                        state: api::UniverseDeletionState::Deleted,
                    };
                    crate::output::show(args.json, &response)?;
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            };
            if !yes {
                use std::io::{IsTerminal as _, Write as _};
                anyhow::ensure!(
                    std::io::stdin().is_terminal(),
                    "universe deletion requires --yes when stdin is not a terminal"
                );
                eprint!("Type {handle} to permanently delete this universe: ");
                std::io::stderr().flush()?;
                let mut confirmation = String::new();
                std::io::stdin().read_line(&mut confirmation)?;
                anyhow::ensure!(
                    confirmation.trim() == handle,
                    "universe deletion cancelled: handle did not match"
                );
            }
            let mut response: api::DeploymentUniverseDeleteResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_UNIVERSES_DELETE,
                    api::DeploymentUniverseDeleteParams {
                        universe: id.clone(),
                    },
                )
                .await?;
            if let Some(wait_ms) = wait {
                let deadline =
                    tokio::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
                while response.state != api::UniverseDeletionState::Deleted {
                    match client
                        .request::<_, api::DeploymentUniverseReadResponse>(
                            api::METHOD_DEPLOYMENT_UNIVERSES_READ,
                            api::DeploymentUniverseReadParams {
                                universe: id.clone(),
                            },
                        )
                        .await
                    {
                        Err(error) if error.kind == api::AgentApiErrorKind::Gone => {
                            response.state = api::UniverseDeletionState::Deleted;
                            break;
                        }
                        Err(error) => return Err(error.into()),
                        Ok(_) => {}
                    }
                    anyhow::ensure!(
                        tokio::time::Instant::now() < deadline,
                        "universe {id} is still deleting; timed out waiting for completion"
                    );
                    tokio::time::sleep_until(
                        deadline
                            .min(tokio::time::Instant::now() + std::time::Duration::from_secs(1)),
                    )
                    .await;
                }
            }
            crate::output::show(args.json, &response)?;
        }
        UniverseCommand::List { deleted: false } => {
            let response: api::DeploymentUniverseListResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_UNIVERSES_LIST,
                    api::DeploymentUniverseListParams {},
                )
                .await?;
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
                        "{} {}  {}{}",
                        if is_active(&u) { "*" } else { " " },
                        u.universe_id,
                        u.handle,
                        if u.deletion.is_some() {
                            "  (deleting)"
                        } else {
                            ""
                        }
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
                .await?;
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
                .await?;
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
                .await?;
            crate::output::show(args.json, &response.universe)?;
        }
    }
    Ok(())
}

#[derive(Debug, Args)]
pub struct TokenArgs {
    #[command(subcommand)]
    command: TokenCommand,
}

#[derive(Debug, Subcommand)]
enum TokenCommand {
    /// Generate a runtime token offline. Configure it on the runtime before use.
    GenerateSecret,
}

pub fn token(_args: TokenArgs) -> Result<()> {
    crate::key_secrets::print_generated_secret(crate::key_secrets::SecretKind::RuntimeToken)
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
    Provider(crate::credentials_cli::CredentialModelArgs),
    /// List the models the universe's providers offer.
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
        ModelsCommand::Provider(args) => return crate::credentials_cli::model(args).await,
        ModelsCommand::List { json, all } => (json, all),
    };
    let response = HttpAgentApi::new("")
        .list_models(api::ModelListParams {
            selectable_only: !all,
        })
        .await?;
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
            "No models available. Configure a connection with model provider put.",
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct CommandLine {
        #[command(flatten)]
        universe: UniverseArgs,
    }

    #[test]
    fn universe_delete_wait_has_a_default_and_accepts_duration() {
        let args =
            CommandLine::try_parse_from(["universe", "delete", "demo", "--wait", "--yes"]).unwrap();
        assert!(matches!(
            args.universe.command,
            UniverseCommand::Delete {
                wait: Some(300_000),
                yes: true,
                ..
            }
        ));
        let args =
            CommandLine::try_parse_from(["universe", "delete", "demo", "--wait", "2m"]).unwrap();
        assert!(matches!(
            args.universe.command,
            UniverseCommand::Delete {
                wait: Some(120_000),
                yes: false,
                ..
            }
        ));
        assert!(
            CommandLine::try_parse_from(["universe", "delete", "demo", "--wait", "0"]).is_err()
        );
    }

    #[test]
    fn universe_list_selects_deleted_inventory() {
        let args = CommandLine::try_parse_from(["universe", "list", "--deleted"]).unwrap();
        assert!(matches!(
            args.universe.command,
            UniverseCommand::List { deleted: true }
        ));
    }
}
