use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};

use crate::api_client::HttpAgentApi;

#[derive(Args, Debug, Clone)]
pub(crate) struct SkillsArgs {
    #[command(subcommand)]
    command: SkillsCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum SkillsCommand {
    /// List skills available to a session.
    List(SkillsListArgs),
    /// List currently active skills for a session.
    Active(SkillsActiveArgs),
    /// Activate a skill for the next run or the session.
    Activate(SkillsActivateArgs),
    /// Deactivate an active skill.
    Deactivate(SkillsDeactivateArgs),
}

#[derive(Args, Debug, Clone)]
struct SkillsListArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    /// Emit the skill list as JSON.
    #[arg(long)]
    json: bool,
    /// Session id to inspect.
    #[arg(long)]
    session: String,
}

#[derive(Args, Debug, Clone)]
struct SkillsActiveArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    /// Emit active skills as JSON.
    #[arg(long)]
    json: bool,
    /// Session id to inspect.
    #[arg(long)]
    session: String,
}

#[derive(Args, Debug, Clone)]
struct SkillsActivateArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    /// Emit activation result as JSON.
    #[arg(long)]
    json: bool,
    /// Session id to change.
    #[arg(long)]
    session: String,
    /// Activation retention scope.
    #[arg(long, default_value_t = SkillScopeArg::Run)]
    scope: SkillScopeArg,
    /// Skill id to activate.
    skill_id: String,
}

#[derive(Args, Debug, Clone)]
struct SkillsDeactivateArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    /// Emit deactivation result as JSON.
    #[arg(long)]
    json: bool,
    /// Session id to change.
    #[arg(long)]
    session: String,
    /// Skill id to deactivate.
    skill_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum SkillScopeArg {
    Run,
    Session,
}

impl std::fmt::Display for SkillScopeArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Run => "run",
            Self::Session => "session",
        })
    }
}

impl From<SkillScopeArg> for api::SkillActivationScope {
    fn from(value: SkillScopeArg) -> Self {
        match value {
            SkillScopeArg::Run => Self::Run,
            SkillScopeArg::Session => Self::Session,
        }
    }
}

pub(crate) async fn handle(args: SkillsArgs) -> Result<()> {
    match args.command {
        SkillsCommand::List(args) => list(args).await,
        SkillsCommand::Active(args) => active(args).await,
        SkillsCommand::Activate(args) => activate(args).await,
        SkillsCommand::Deactivate(args) => deactivate(args).await,
    }
}

async fn list(args: SkillsListArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .list_skills(api::SkillListParams {
            session_id: args.session,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    print_skill_list(response, args.json)
}

async fn active(args: SkillsActiveArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .active_skills(api::SkillActiveParams {
            session_id: args.session,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    print_active_skills(response, args.json)
}

async fn activate(args: SkillsActivateArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .activate_skill(api::SkillActivateParams {
            session_id: args.session,
            skill_id: args.skill_id,
            scope: args.scope.into(),
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    println!("activated {}", response.activation.skill_id);
    print_activation(&response.activation);
    println!("activeCount {}", response.active.len());
    Ok(())
}

async fn deactivate(args: SkillsDeactivateArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .deactivate_skill(api::SkillDeactivateParams {
            session_id: args.session,
            skill_id: args.skill_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    println!("deactivated {}", response.skill_id);
    println!("activeCount {}", response.active.len());
    Ok(())
}

fn print_skill_list(response: api::SkillListResponse, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    print_catalog_ref(response.catalog_ref.as_deref());
    if response.skills.is_empty() {
        println!("skills 0");
        return Ok(());
    }
    for skill in &response.skills {
        let active = if skill.active { "active" } else { "inactive" };
        let enabled = if skill.enabled { "enabled" } else { "disabled" };
        println!("{} {} {} {}", skill.skill_id, active, enabled, skill.name);
        println!("  {}", skill.description);
        if let Some(short_description) = &skill.short_description {
            println!("  short {}", short_description);
        }
    }
    Ok(())
}

fn print_active_skills(response: api::SkillActiveResponse, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    print_catalog_ref(response.catalog_ref.as_deref());
    if response.activations.is_empty() {
        println!("active 0");
        return Ok(());
    }
    for activation in &response.activations {
        print_activation(activation);
    }
    Ok(())
}

fn print_activation(activation: &api::SkillActivationView) {
    let name = activation.name.as_deref().unwrap_or("-");
    println!(
        "{} {} {} {}",
        activation.skill_id,
        activation_scope(activation.scope),
        activation_source(&activation.source),
        name
    );
    if let Some(description) = &activation.description {
        println!("  {}", description);
    }
    println!("  catalogRef {}", activation.catalog_ref);
}

fn print_catalog_ref(catalog_ref: Option<&str>) {
    println!("catalogRef {}", catalog_ref.unwrap_or("-"));
}

fn activation_scope(scope: api::SkillActivationScope) -> &'static str {
    match scope {
        api::SkillActivationScope::Run => "run",
        api::SkillActivationScope::Session => "session",
    }
}

fn activation_source(source: &api::SkillActivationSource) -> String {
    match source {
        api::SkillActivationSource::ToolResult { call_id } => format!("toolResult:{call_id}"),
        api::SkillActivationSource::DirectContext { context_ref } => {
            format!("directContext:{context_ref}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_source_formats_source_details() {
        assert_eq!(
            activation_source(&api::SkillActivationSource::ToolResult {
                call_id: "call_1".to_owned()
            }),
            "toolResult:call_1"
        );
        assert_eq!(
            activation_source(&api::SkillActivationSource::DirectContext {
                context_ref: "sha256:abc".to_owned()
            }),
            "directContext:sha256:abc"
        );
    }

    #[test]
    fn skill_scope_arg_maps_to_api_scope() {
        assert_eq!(
            api::SkillActivationScope::from(SkillScopeArg::Run),
            api::SkillActivationScope::Run
        );
        assert_eq!(
            api::SkillActivationScope::from(SkillScopeArg::Session),
            api::SkillActivationScope::Session
        );
    }
}
