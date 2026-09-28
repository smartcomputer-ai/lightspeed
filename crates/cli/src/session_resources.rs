//! Session configuration and environment attachments use the current typed API.
use crate::{api_client::HttpAgentApi, output};
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};

#[derive(Args, Debug, Clone)]
pub(crate) struct ReadArgs {
    #[arg(long)]
    json: bool,
    session_id: String,
}

pub(crate) async fn read(args: ReadArgs) -> Result<()> {
    let session = load(&HttpAgentApi::new(""), &args.session_id).await?;
    if args.json {
        return output::show(true, &session);
    }
    output::show(
        false,
        &serde_json::json!({
            "sessionId": session.id, "displayName": session.display_name,
            "status":session.status, "activeRun":session.active_run.as_ref().map(|run| &run.id),
            "activeEnvironment":session.active_environment_id, "configRevision":session.config_revision,
            "metadata":session.metadata, "config":session.config,
        }),
    )
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}
#[derive(Subcommand, Debug, Clone)]
enum ConfigCommand {
    /// Read the effective session configuration (use --json for a reusable document).
    Read(ReadArgs),
    /// Replace configuration from a SessionConfig JSON file, with revision checking.
    Put {
        session_id: String,
        #[arg(long)]
        file: std::path::PathBuf,
        /// Defaults to the revision read before submitting the change.
        #[arg(long)]
        expected_revision: Option<u64>,
        #[arg(long)]
        json: bool,
    },
}
pub(crate) async fn config(args: ConfigArgs) -> Result<()> {
    let api = HttpAgentApi::new("");
    match args.command {
        ConfigCommand::Read(args) => {
            output::show(args.json, &load(&api, &args.session_id).await?.config)
        }
        ConfigCommand::Put {
            session_id,
            file,
            expected_revision,
            json,
        } => {
            let config: api::SessionConfig = serde_json::from_slice(
                &std::fs::read(&file).with_context(|| format!("read {}", file.display()))?,
            )
            .context("invalid SessionConfig document")?;
            let session = load(&api, &session_id).await?;
            let response = api
                .put_session_config(api::SessionConfigPutParams {
                    session_id,
                    config,
                    expected_config_revision: Some(
                        expected_revision.unwrap_or(session.config_revision),
                    ),
                })
                .await?
                .result;
            output::show(json, &response)
        }
    }
}

#[derive(Args, Debug, Clone)]
pub(crate) struct EnvironmentArgs {
    #[command(subcommand)]
    command: EnvironmentCommand,
}
#[derive(Args, Debug, Clone)]
struct SessionArgs {
    #[arg(short = 's', long)]
    session: String,
    #[arg(long)]
    json: bool,
}
#[derive(Args, Debug, Clone)]
struct TargetArgs {
    #[command(flatten)]
    common: SessionArgs,
    environment_id: String,
}
#[derive(ValueEnum, Debug, Clone, Copy)]
enum Access {
    Read,
    Edit,
    Exec,
    Jobs,
}
impl From<Access> for api::EnvironmentAccess {
    fn from(access: Access) -> Self {
        match access {
            Access::Read => Self::Read,
            Access::Edit => Self::Edit,
            Access::Exec => Self::Exec,
            Access::Jobs => Self::Jobs,
        }
    }
}
#[derive(Subcommand, Debug, Clone)]
enum EnvironmentCommand {
    /// Attach an existing environment; activation is a separate operation.
    Attach {
        #[command(flatten)]
        target: TargetArgs,
        #[arg(long, value_enum, default_value = "read")]
        access: Access,
        #[arg(long)]
        working_directory: Option<String>,
    },
    /// Detach an environment without closing it.
    Detach(TargetArgs),
    /// List declared environment attachments and mark the active one.
    List(SessionArgs),
    /// Activate an attached environment for subsequent tool calls.
    Activate(TargetArgs),
    /// Clear the active environment without detaching or closing it.
    Deactivate(SessionArgs),
}

async fn load(api: &HttpAgentApi, id: &str) -> Result<api::SessionView> {
    Ok(api
        .read_session(api::SessionReadParams {
            session_id: id.into(),
            run_limit: Some(1),
        })
        .await?
        .result
        .session)
}

pub(crate) async fn environment(args: EnvironmentArgs) -> Result<()> {
    let api = HttpAgentApi::new("");
    match args.command {
        EnvironmentCommand::Activate(args) => {
            let response = api
                .activate_session_environment(api::SessionEnvironmentActivateParams {
                    session_id: args.common.session,
                    environment_id: args.environment_id,
                })
                .await?
                .result;
            if args.common.json {
                output::show(true, &response)
            } else {
                output::show(
                    false,
                    &serde_json::json!({"activeEnvironment":response.session.active_environment_id}),
                )
            }
        }
        EnvironmentCommand::Deactivate(args) => {
            let response = api
                .deactivate_session_environment(api::SessionEnvironmentDeactivateParams {
                    session_id: args.session,
                })
                .await?
                .result;
            if args.json {
                output::show(true, &response)
            } else {
                println!("Active environment cleared; attachments retained.");
                Ok(())
            }
        }
        EnvironmentCommand::List(args) => {
            let session = load(&api, &args.session).await?;
            let attachments = session
                .config
                .and_then(|c| c.features)
                .and_then(|f| f.environments)
                .map(|e| e.environments)
                .unwrap_or_default();
            let rows: Vec<_> = attachments
                .iter()
                .map(|attachment| {
                    let mut row =
                        serde_json::to_value(attachment).expect("serializable attachment");
                    row["active"] = serde_json::json!(
                        attachment.environment_id.is_some()
                            && attachment.environment_id == session.active_environment_id
                    );
                    row
                })
                .collect();
            if args.json {
                output::show(true, &rows)
            } else {
                output::table(
                    &rows,
                    &[
                        ("active", "ACTIVE"),
                        ("environmentId", "ENVIRONMENT"),
                        ("access", "ACCESS"),
                        ("workingDirectory", "WORKING DIRECTORY"),
                    ],
                    "No environments attached.",
                )
            }
        }
        command => {
            let (target, attachment) = match command {
                EnvironmentCommand::Attach {
                    target,
                    access,
                    working_directory,
                } => {
                    let attachment = api::EnvironmentAttachment {
                        environment_id: Some(target.environment_id.clone()),
                        inherit: false,
                        default: false,
                        access: access.into(),
                        working_directory,
                    };
                    (target, Some(attachment))
                }
                EnvironmentCommand::Detach(target) => (target, None),
                _ => unreachable!(),
            };
            let session = load(&api, &target.common.session).await?;
            let mut config = session.config.context("session has no configuration")?;
            update_environment_attachment(&mut config, &target.environment_id, attachment)?;
            let response = api
                .put_session_config(api::SessionConfigPutParams {
                    session_id: target.common.session,
                    expected_config_revision: Some(session.config_revision),
                    config,
                })
                .await?
                .result;
            if target.common.json {
                output::show(true, &response)
            } else {
                println!(
                    "Updated environment attachments (configuration revision {}).",
                    response.session.config_revision
                );
                Ok(())
            }
        }
    }
}

fn update_environment_attachment(
    config: &mut api::SessionConfig,
    id: &str,
    attachment: Option<api::EnvironmentAttachment>,
) -> Result<()> {
    let features = config.features.get_or_insert_with(Default::default);
    if attachment.is_none()
        && !features.environments.as_ref().is_some_and(|env| {
            env.environments
                .iter()
                .any(|a| a.environment_id.as_deref() == Some(id))
        })
    {
        bail!("environment {id} is not attached");
    }
    let environment = features
        .environments
        .get_or_insert(api::EnvironmentsFeature {
            version: api::CURRENT_FEATURE_VERSION,
            selection: false,
            prompts: None,
            skills: None,
            environments: vec![],
        });
    environment
        .environments
        .retain(|a| a.environment_id.as_deref() != Some(id));
    if let Some(attachment) = attachment {
        environment.environments.push(attachment);
    }
    Ok(())
}
