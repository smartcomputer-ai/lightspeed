use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};

use crate::api_client::HttpAgentApi;

#[derive(Debug, Args)]
pub struct ModelDefaultsArgs {
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: ModelDefaultsCommand,
}

#[derive(Debug, Subcommand)]
enum ModelDefaultsCommand {
    /// Read this universe's model selections and revision.
    Read,
    /// Set a complete provider/API/model route for one use.
    Set {
        #[arg(value_enum)]
        slot: Slot,
        #[arg(long)]
        provider: String,
        #[arg(long)]
        api_kind: String,
        #[arg(long)]
        model: String,
        /// Use this revision; otherwise read it immediately before updating.
        #[arg(long)]
        expected_revision: Option<u64>,
    },
    /// Clear one default; existing sessions keep their model.
    Clear {
        #[arg(value_enum)]
        slot: Slot,
        #[arg(long)]
        expected_revision: Option<u64>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Slot {
    #[value(alias = "agentRun")]
    AgentRun,
    #[value(alias = "speechToText")]
    SpeechToText,
}

impl From<Slot> for api::ModelDefaultSlot {
    fn from(slot: Slot) -> Self {
        match slot {
            Slot::AgentRun => Self::AgentRun,
            Slot::SpeechToText => Self::SpeechToText,
        }
    }
}

pub async fn run(args: ModelDefaultsArgs) -> Result<()> {
    let client = HttpAgentApi::new("");
    let defaults = match args.command {
        ModelDefaultsCommand::Read => read(&client).await?,
        ModelDefaultsCommand::Set {
            slot,
            provider,
            api_kind,
            model,
            expected_revision,
        } => {
            let slot = slot.into();
            let model = api::ModelConfig {
                provider_id: provider,
                api_kind,
                model,
            };
            api::ModelDefaultSlot::validate_model(slot, &model)?;
            put(&client, slot, Some(model), expected_revision).await?
        }
        ModelDefaultsCommand::Clear {
            slot,
            expected_revision,
        } => put(&client, slot.into(), None, expected_revision).await?,
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&defaults)?);
    } else {
        println!("Model defaults (revision {})", defaults.revision);
        for slot in [
            api::ModelDefaultSlot::AgentRun,
            api::ModelDefaultSlot::SpeechToText,
        ] {
            match defaults.model(slot) {
                Some(model) => println!(
                    "{}: {} / {} / {}",
                    slot.as_str(),
                    model.provider_id,
                    model.api_kind,
                    model.model
                ),
                None => println!("{}: unset", slot.as_str()),
            }
        }
    }
    Ok(())
}

async fn read(client: &HttpAgentApi) -> Result<api::ModelDefaults> {
    let response: api::AgentApiOutcome<api::ModelDefaultsResponse> = client
        .request(
            api::METHOD_MODELS_DEFAULTS_READ,
            api::ModelDefaultsReadParams {},
        )
        .await?;
    Ok(response.result.defaults)
}

async fn put(
    client: &HttpAgentApi,
    slot: api::ModelDefaultSlot,
    model: Option<api::ModelConfig>,
    revision: Option<u64>,
) -> Result<api::ModelDefaults> {
    let expected_revision = match revision {
        Some(revision) => revision,
        None => read(client).await?.revision,
    };
    let response: api::AgentApiOutcome<api::ModelDefaultsResponse> = client
        .request(
            api::METHOD_MODELS_DEFAULTS_PUT,
            api::ModelDefaultsPutParams {
                slot,
                model,
                expected_revision,
            },
        )
        .await?;
    // A concurrent change is returned to the caller; never retry a write
    // against a newer revision without the caller reviewing that state.
    Ok(response.result.defaults)
}

#[cfg(test)]
mod tests {
    use crate::Cli;
    use clap::Parser;

    #[test]
    fn defaults_commands_require_complete_routes_and_accept_revision_guards() {
        for args in [
            vec!["lightspeed", "model", "defaults", "read", "--json"],
            vec![
                "lightspeed",
                "model",
                "defaults",
                "set",
                "agent-run",
                "--provider",
                "custom",
                "--api-kind",
                "openai:completions",
                "--model",
                "model",
                "--expected-revision",
                "0",
            ],
            vec![
                "lightspeed",
                "models",
                "defaults",
                "clear",
                "speech-to-text",
                "--json",
            ],
        ] {
            Cli::try_parse_from(args).unwrap();
        }
        assert!(
            Cli::try_parse_from([
                "lightspeed",
                "model",
                "defaults",
                "set",
                "agent-run",
                "--model",
                "model"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from(["lightspeed", "model", "defaults", "clear", "unknown"]).is_err()
        );
    }
}
