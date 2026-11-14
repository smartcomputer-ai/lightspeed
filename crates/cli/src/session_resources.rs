//! Reading a session and replacing its configuration through the typed API.
use crate::{api_client::HttpAgentApi, output};
use anyhow::{Context, Result};
use clap::{Args, Subcommand};

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
            "lifecycleStatus":session.lifecycle_status, "activity":session.activity,
            "executionFailure":session.execution_failure, "activeRun":session.active_run.as_ref().map(|run| &run.id),
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
struct SessionArgs {
    #[arg(short = 's', long)]
    session: String,
    #[arg(long)]
    json: bool,
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
