use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use api::{
    AgentProfileInput, AgentProfileUpdatePatch, InlineAgentProfile, ProfileApplyParams,
    ProfileCreateParams, ProfileDeleteParams, ProfileId, ProfileListParams, ProfileReadParams,
    ProfileSource, ProfileUpdateParams,
};
use clap::{Args, Subcommand};
use serde::de::DeserializeOwned;

use crate::api_client::{HttpAgentApi, api_error};

#[derive(Args, Debug)]
pub(crate) struct ProfilesArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    #[command(subcommand)]
    command: ProfilesCommand,
}

#[derive(Subcommand, Debug)]
enum ProfilesCommand {
    /// List profiles.
    List,
    /// Read one profile.
    Read { profile_id: String },
    /// Create a profile from an AgentProfileInput JSON file or literal.
    Create { json: String },
    /// Patch a profile from an AgentProfileUpdatePatch JSON file or literal.
    Update {
        profile_id: String,
        json: String,
        #[arg(long = "expected-revision")]
        expected_revision: Option<u64>,
    },
    /// Delete a profile.
    Delete { profile_id: String },
    /// Apply a profile to an idle session.
    Apply {
        session_id: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long = "profile-json")]
        profile_json: Option<String>,
        #[arg(long = "expected-config-revision")]
        expected_config_revision: Option<u64>,
        #[arg(long = "expected-tools-revision")]
        expected_tools_revision: Option<u64>,
    },
}

pub(crate) async fn handle(args: ProfilesArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    match args.command {
        ProfilesCommand::List => {
            let response = api
                .list_profiles(ProfileListParams::default())
                .await
                .map_err(api_error)?;
            print_json(&response.result.profiles)
        }
        ProfilesCommand::Read { profile_id } => {
            let response = api
                .read_profile(ProfileReadParams {
                    profile_id: parse_profile_id(&profile_id)?,
                })
                .await
                .map_err(api_error)?;
            print_json(&response.result.profile)
        }
        ProfilesCommand::Create { json } => {
            let profile = read_json_arg::<AgentProfileInput>(&json)?;
            let response = api
                .create_profile(ProfileCreateParams { profile })
                .await
                .map_err(api_error)?;
            print_json(&response.result.profile)
        }
        ProfilesCommand::Update {
            profile_id,
            json,
            expected_revision,
        } => {
            let patch = read_json_arg::<AgentProfileUpdatePatch>(&json)?;
            let response = api
                .update_profile(ProfileUpdateParams {
                    profile_id: parse_profile_id(&profile_id)?,
                    expected_revision,
                    patch,
                })
                .await
                .map_err(api_error)?;
            print_json(&response.result.profile)
        }
        ProfilesCommand::Delete { profile_id } => {
            let response = api
                .delete_profile(ProfileDeleteParams {
                    profile_id: parse_profile_id(&profile_id)?,
                })
                .await
                .map_err(api_error)?;
            print_json(&response.result.profile)
        }
        ProfilesCommand::Apply {
            session_id,
            profile,
            profile_json,
            expected_config_revision,
            expected_tools_revision,
        } => {
            let profile = profile_source_from_args(profile.as_deref(), profile_json.as_deref())?;
            let response = api
                .apply_profile(ProfileApplyParams {
                    session_id,
                    profile,
                    expected_config_revision,
                    expected_tools_revision,
                })
                .await
                .map_err(api_error)?;
            print_json(&response.result)
        }
    }
}

fn parse_profile_id(value: &str) -> Result<ProfileId> {
    ProfileId::try_new(value.to_owned()).map_err(|error| anyhow!("invalid profile id: {error}"))
}

fn profile_source_from_args(
    profile: Option<&str>,
    profile_json: Option<&str>,
) -> Result<ProfileSource> {
    match (profile, profile_json) {
        (Some(_), Some(_)) => Err(anyhow!(
            "--profile and --profile-json are mutually exclusive"
        )),
        (Some(profile_id), None) => Ok(ProfileSource::Named {
            profile_id: parse_profile_id(profile_id)?,
        }),
        (None, Some(json_arg)) => {
            let profile = read_json_arg::<InlineAgentProfile>(json_arg)?;
            Ok(ProfileSource::Inline { profile })
        }
        (None, None) => Err(anyhow!("one of --profile or --profile-json is required")),
    }
}

fn read_json_arg<T: DeserializeOwned>(arg: &str) -> Result<T> {
    let path = PathBuf::from(arg);
    let json = if path.exists() {
        std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read JSON {}", path.display()))?
    } else {
        arg.to_owned()
    };
    serde_json::from_str(&json).context("failed to parse JSON")
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
