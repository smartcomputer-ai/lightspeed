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
    /// List environments bound to a session.
    List(EnvListArgs),
    /// Read one session environment.
    Read(EnvReadArgs),
    /// Attach a provider target to a session.
    Attach(EnvAttachArgs),
    /// Activate a ready session environment.
    Activate(EnvActivateArgs),
    /// Deactivate the current session environment.
    Deactivate(EnvDeactivateArgs),
    /// Close or detach a session environment.
    Close(EnvCloseArgs),
}

#[derive(Args, Debug, Clone)]
struct EnvListArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    /// Emit environments as JSON.
    #[arg(long)]
    json: bool,
    /// Session id to inspect.
    #[arg(long)]
    session: String,
}

#[derive(Args, Debug, Clone)]
struct EnvReadArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    /// Emit the environment as JSON.
    #[arg(long)]
    json: bool,
    /// Session id to inspect.
    #[arg(long)]
    session: String,
    /// Environment id to read.
    env_id: String,
}

#[derive(Args, Debug, Clone)]
struct EnvAttachArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    /// Emit the attach response as JSON.
    #[arg(long)]
    json: bool,
    /// Session id to attach into.
    #[arg(long)]
    session: String,
    /// Environment id to create for this session binding.
    #[arg(long = "env-id")]
    env_id: Option<String>,
    /// Registered environment provider id.
    #[arg(long = "provider-id")]
    provider_id: String,
    /// Provider target id to attach. `host-bridge` defaults to `local`.
    #[arg(long = "target-id", default_value = "local")]
    target_id: String,
    /// Activate the environment immediately after attaching it.
    #[arg(long)]
    activate: bool,
}

#[derive(Args, Debug, Clone)]
struct EnvActivateArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    /// Emit the activation response as JSON.
    #[arg(long)]
    json: bool,
    /// Session id to change.
    #[arg(long)]
    session: String,
    /// Environment id to activate.
    env_id: String,
}

#[derive(Args, Debug, Clone)]
struct EnvDeactivateArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    /// Emit the deactivation response as JSON.
    #[arg(long)]
    json: bool,
    /// Session id to change.
    #[arg(long)]
    session: String,
}

#[derive(Args, Debug, Clone)]
struct EnvCloseArgs {
    /// JSON-RPC agent API URL.
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL")]
    api_url: String,
    /// Emit the close response as JSON.
    #[arg(long)]
    json: bool,
    /// Session id to change.
    #[arg(long)]
    session: String,
    /// Force provider-side target close when closing a target.
    #[arg(long)]
    force: bool,
    /// Close the provider target as well as the session binding.
    #[arg(long = "close-target", conflicts_with = "detach_only")]
    close_target: bool,
    /// Detach only; do not call provider closeTarget.
    #[arg(long = "detach-only", conflicts_with = "close_target")]
    detach_only: bool,
    /// Environment id to close.
    env_id: String,
}

pub(crate) async fn handle(args: EnvArgs) -> Result<()> {
    match args.command {
        EnvCommand::List(args) => list(args).await,
        EnvCommand::Read(args) => read(args).await,
        EnvCommand::Attach(args) => attach(args).await,
        EnvCommand::Activate(args) => activate(args).await,
        EnvCommand::Deactivate(args) => deactivate(args).await,
        EnvCommand::Close(args) => close(args).await,
    }
}

async fn list(args: EnvListArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .list_session_environments(api::SessionEnvironmentListParams {
            session_id: args.session,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    if response.environments.is_empty() {
        println!("environments 0");
        return Ok(());
    }
    if let Some(active) = response.active_env_id {
        println!("active {active}");
    }
    for environment in &response.environments {
        print_environment_summary(environment);
    }
    Ok(())
}

async fn read(args: EnvReadArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let environment = api
        .read_session_environment(api::SessionEnvironmentReadParams {
            session_id: args.session,
            env_id: args.env_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result
        .environment;
    print_environment(&environment, args.json)
}

async fn attach(args: EnvAttachArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .attach_session_environment(api::SessionEnvironmentAttachParams {
            session_id: args.session,
            env_id: args.env_id,
            provider_id: args.provider_id,
            request: api::HostTargetAttachRequestView::Target {
                target_id: args.target_id,
            },
            activate: args.activate,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    println!("attached {}", response.environment.env_id);
    if let Some(active) = response.active_env_id {
        println!("active {active}");
    }
    print_environment(&response.environment, false)
}

async fn activate(args: EnvActivateArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .activate_session_environment(api::SessionEnvironmentActivateParams {
            session_id: args.session,
            env_id: args.env_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    println!("active {}", response.environment.env_id);
    print_environment(&response.environment, false)
}

async fn deactivate(args: EnvDeactivateArgs) -> Result<()> {
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .deactivate_session_environment(api::SessionEnvironmentDeactivateParams {
            session_id: args.session,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    println!("active -");
    println!("environmentCount {}", response.environments.len());
    Ok(())
}

async fn close(args: EnvCloseArgs) -> Result<()> {
    let close_target = match (args.close_target, args.detach_only) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        (false, false) => None,
        (true, true) => unreachable!("clap conflicts prevent both close flags"),
    };
    let api = HttpAgentApi::new(args.api_url);
    let response = api
        .close_session_environment(api::SessionEnvironmentCloseParams {
            session_id: args.session,
            env_id: args.env_id,
            force: args.force,
            close_target,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    println!("closed {}", response.environment.env_id);
    if let Some(active) = response.active_env_id {
        println!("active {active}");
    } else {
        println!("active -");
    }
    print_environment(&response.environment, false)
}

fn print_environment(environment: &api::SessionEnvironmentView, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&environment)?);
        return Ok(());
    }

    print_environment_summary(&environment);
    println!("kind {}", kind_label(environment.kind));
    println!("status {}", status_label(environment.status));
    println!("active {}", environment.active);
    println!(
        "capabilities fsRead={} fsWrite={} processExec={} processStdin={} network={} persistent={}",
        environment.capabilities.fs_read,
        environment.capabilities.fs_write,
        environment.capabilities.process_exec,
        environment.capabilities.process_stdin,
        environment.capabilities.network,
        environment.capabilities.persistent
    );
    if let Some(cwd) = &environment.cwd {
        println!("cwd {cwd}");
    }
    if let Some(target) = &environment.exec_target {
        println!("execTarget {}:{}", target.namespace, target.id);
    }
    Ok(())
}

fn print_environment_summary(environment: &api::SessionEnvironmentView) {
    let active = if environment.active { " active" } else { "" };
    let cwd = environment.cwd.as_deref().unwrap_or("-");
    println!(
        "{} {} {} cwd={}{}",
        environment.env_id,
        kind_label(environment.kind),
        status_label(environment.status),
        cwd,
        active
    );
}

fn kind_label(kind: api::SessionEnvironmentKindView) -> &'static str {
    match kind {
        api::SessionEnvironmentKindView::Sandbox => "sandbox",
        api::SessionEnvironmentKindView::AttachedHost => "attachedHost",
    }
}

fn status_label(status: api::SessionEnvironmentStatusView) -> &'static str {
    match status {
        api::SessionEnvironmentStatusView::Attaching => "attaching",
        api::SessionEnvironmentStatusView::Ready => "ready",
        api::SessionEnvironmentStatusView::Degraded => "degraded",
        api::SessionEnvironmentStatusView::Detached => "detached",
    }
}
