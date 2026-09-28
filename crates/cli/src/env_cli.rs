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
    /// List universe environments, optionally filtered by metadata.
    List(EnvListArgs),
    /// Read one universe environment.
    Read(EnvironmentResourceArgs),
    /// Activate a universe environment for a session.
    Activate(ActivateArgs),
    /// Clear a session's active environment.
    Deactivate(SessionArgs),
    /// Close one universe environment.
    Close(EnvironmentResourceArgs),
    /// Set the desired power state (running, paused, suspended, stopped) of
    /// a provisioned environment; the runtime converges it asynchronously and
    /// a powered-down environment wakes on its next use.
    Power(PowerArgs),
    /// Replace or clear the staged idle policy of a provisioned environment.
    IdlePolicy(IdlePolicyArgs),
    /// Bind, list, or unbind universe environment credentials.
    Credentials(CredentialArgs),
    /// Mint, list, read, or revoke registration keys that let outbound
    /// `lightspeed-envd` daemons register as environments.
    RegistrationKeys(RegistrationKeyArgs),
}

#[derive(Args, Debug, Clone)]
struct RegistrationKeyArgs {
    #[command(subcommand)]
    command: RegistrationKeyCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum RegistrationKeyCommand {
    /// Mint a key. The secret is printed exactly once.
    Create(RegistrationKeyCreateArgs),
    /// List this universe's registration keys with their environment counts.
    List(ResourceArgs),
    /// Read one registration key.
    Read(RegistrationKeyResourceArgs),
    /// Stop a key from admitting new daemons; optionally close its
    /// environments.
    Revoke(RegistrationKeyRevokeArgs),
}

#[derive(Args, Debug, Clone)]
struct RegistrationKeyCreateArgs {
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL", default_value = "")]
    api_url: String,
    #[arg(long)]
    json: bool,
    /// Group name shown for every environment this key admits.
    #[arg(long)]
    name: String,
    /// persistent: environments stay offline until closed; ephemeral: they
    /// close once their daemon has been away longer than the grace.
    #[arg(long, value_enum, default_value_t = IdentityModeArg::Persistent)]
    mode: IdentityModeArg,
    /// Non-closed environments the key may have at once.
    #[arg(long = "max-active")]
    max_active: Option<u32>,
    /// Ephemeral disconnect grace in minutes (default: 5).
    #[arg(long = "grace-min")]
    grace_min: Option<u64>,
    /// Stop admitting new daemons after this many hours.
    #[arg(long = "expires-in-hours")]
    expires_in_hours: Option<u64>,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
enum IdentityModeArg {
    Persistent,
    Ephemeral,
}

#[derive(Args, Debug, Clone)]
struct RegistrationKeyResourceArgs {
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL", default_value = "")]
    api_url: String,
    #[arg(long)]
    json: bool,
    registration_key_id: String,
}

#[derive(Args, Debug, Clone)]
struct RegistrationKeyRevokeArgs {
    #[command(flatten)]
    common: RegistrationKeyResourceArgs,
    /// Also close every non-closed environment the key admitted.
    #[arg(long = "close-environments")]
    close_environments: bool,
}

#[derive(Args, Debug, Clone)]
struct PowerArgs {
    #[command(flatten)]
    common: EnvironmentResourceArgs,
    /// One of: running, paused, suspended, stopped.
    #[arg(value_enum)]
    power: PowerArg,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
enum PowerArg {
    Running,
    Paused,
    Suspended,
    Stopped,
}

impl From<PowerArg> for api::EnvironmentPowerStateView {
    fn from(value: PowerArg) -> Self {
        match value {
            PowerArg::Running => Self::Running,
            PowerArg::Paused => Self::Paused,
            PowerArg::Suspended => Self::Suspended,
            PowerArg::Stopped => Self::Stopped,
        }
    }
}

#[derive(Args, Debug, Clone)]
struct IdlePolicyArgs {
    #[command(flatten)]
    common: EnvironmentResourceArgs,
    /// Pause after this many minutes idle.
    #[arg(long = "pause-after-min")]
    pause_after_min: Option<u64>,
    /// Suspend after this many minutes idle (providers that support it).
    #[arg(long = "suspend-after-min")]
    suspend_after_min: Option<u64>,
    /// Stop after this many minutes idle.
    #[arg(long = "stop-after-min")]
    stop_after_min: Option<u64>,
    /// Close after this many minutes idle.
    #[arg(long = "close-after-min")]
    close_after_min: Option<u64>,
    /// Remove the idle policy entirely.
    #[arg(long, conflicts_with_all = ["pause_after_min", "suspend_after_min", "stop_after_min", "close_after_min"])]
    clear: bool,
}

#[derive(Args, Debug, Clone)]
struct ResourceArgs {
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL", default_value = "")]
    api_url: String,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug, Clone)]
struct EnvListArgs {
    #[command(flatten)]
    common: ResourceArgs,
    #[command(flatten)]
    metadata: crate::session_cli::MetadataPairs,
}

#[derive(Args, Debug, Clone)]
struct EnvironmentResourceArgs {
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL", default_value = "")]
    api_url: String,
    #[arg(long)]
    json: bool,
    environment_id: String,
}

#[derive(Args, Debug, Clone)]
struct SessionArgs {
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL", default_value = "")]
    api_url: String,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    session: String,
}

#[derive(Args, Debug, Clone)]
struct ActivateArgs {
    #[command(flatten)]
    session: SessionArgs,
    environment_id: String,
}

#[derive(Args, Debug, Clone)]
struct CredentialArgs {
    #[command(subcommand)]
    command: CredentialCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum CredentialCommand {
    Bind(CredentialBindArgs),
    List(CredentialListArgs),
    Unbind(CredentialUnbindArgs),
}

#[derive(Args, Debug, Clone)]
struct CredentialListArgs {
    #[arg(long = "api-url", env = "LIGHTSPEED_API_URL", default_value = "")]
    api_url: String,
    #[arg(long)]
    json: bool,
    environment_id: String,
}

#[derive(Args, Debug, Clone)]
struct CredentialBindArgs {
    #[command(flatten)]
    common: CredentialListArgs,
    #[arg(long = "env-name")]
    env_name: String,
    #[arg(long = "grant-id", conflicts_with_all = ["provider_id", "secret_id"])]
    grant_id: Option<String>,
    #[arg(long = "provider-id", conflicts_with_all = ["grant_id", "secret_id"])]
    provider_id: Option<String>,
    #[arg(long = "secret-id", conflicts_with_all = ["grant_id", "provider_id"])]
    secret_id: Option<String>,
}

#[derive(Args, Debug, Clone)]
struct CredentialUnbindArgs {
    #[command(flatten)]
    common: CredentialListArgs,
    #[arg(long = "env-name")]
    env_name: String,
}

pub(crate) async fn handle(args: EnvArgs) -> Result<()> {
    match args.command {
        EnvCommand::List(args) => list(args).await,
        EnvCommand::Read(args) => read(args).await,
        EnvCommand::Activate(args) => activate(args).await,
        EnvCommand::Deactivate(args) => deactivate(args).await,
        EnvCommand::Close(args) => close(args).await,
        EnvCommand::Power(args) => power(args).await,
        EnvCommand::IdlePolicy(args) => idle_policy(args).await,
        EnvCommand::Credentials(args) => credentials(args).await,
        EnvCommand::RegistrationKeys(args) => registration_keys(args).await,
    }
}

async fn registration_keys(args: RegistrationKeyArgs) -> Result<()> {
    match args.command {
        RegistrationKeyCommand::Create(args) => {
            let now_ms = i64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_millis(),
            )?;
            let response = HttpAgentApi::new(args.api_url)
                .create_environment_registration_key(api::EnvironmentRegistrationKeyCreateParams {
                    display_name: args.name,
                    identity_mode: match args.mode {
                        IdentityModeArg::Persistent => api::EnvironmentIdentityModeView::Persistent,
                        IdentityModeArg::Ephemeral => api::EnvironmentIdentityModeView::Ephemeral,
                    },
                    max_active_environments: args.max_active,
                    ephemeral_disconnect_grace_ms: args
                        .grace_min
                        .map(|minutes| minutes.saturating_mul(60_000)),
                    expires_at_ms: args.expires_in_hours.map(|hours| {
                        now_ms.saturating_add(
                            i64::try_from(hours)
                                .unwrap_or(i64::MAX)
                                .saturating_mul(3_600_000),
                        )
                    }),
                })
                .await
                .map_err(crate::api_client::api_error)?
                .result;
            print_json_or(args.json, &response, || {
                print_registration_key(&response.registration_key);
                println!();
                println!("Registration key (shown once, store it now):");
                println!("  {}", response.secret.0);
                println!();
                println!("Bootstrap a daemon with:");
                println!(
                    "  LIGHTSPEED_ENVD_GATEWAY_URL=wss://<gateway>/environment-gateway/connect"
                );
                println!("  LIGHTSPEED_ENVD_REGISTRATION_KEY_FILE=<file holding the key>");
                println!("  lightspeed-envd");
            })
        }
        RegistrationKeyCommand::List(args) => {
            let response = HttpAgentApi::new(args.api_url)
                .list_environment_registration_keys(api::EnvironmentRegistrationKeyListParams {})
                .await
                .map_err(crate::api_client::api_error)?
                .result;
            print_json_or(args.json, &response, || {
                for key in &response.registration_keys {
                    print_registration_key(key);
                }
            })
        }
        RegistrationKeyCommand::Read(args) => {
            let response = HttpAgentApi::new(args.api_url)
                .read_environment_registration_key(api::EnvironmentRegistrationKeyReadParams {
                    registration_key_id: args.registration_key_id,
                })
                .await
                .map_err(crate::api_client::api_error)?
                .result;
            print_json_or(args.json, &response, || {
                print_registration_key(&response.registration_key)
            })
        }
        RegistrationKeyCommand::Revoke(args) => {
            let response = HttpAgentApi::new(args.common.api_url)
                .revoke_environment_registration_key(api::EnvironmentRegistrationKeyRevokeParams {
                    registration_key_id: args.common.registration_key_id,
                    close_environments: args.close_environments,
                })
                .await
                .map_err(crate::api_client::api_error)?
                .result;
            print_json_or(args.common.json, &response, || {
                print_registration_key(&response.registration_key);
                for environment_id in &response.closed_environment_ids {
                    println!("closing {environment_id}");
                }
            })
        }
    }
}

fn print_registration_key(key: &api::EnvironmentRegistrationKeyView) {
    println!(
        "{} {:?} {} {:?} active={} total={} max={}",
        key.registration_key_id,
        key.status,
        key.display_name,
        key.identity_mode,
        key.active_environment_count,
        key.registered_environment_count,
        key.max_active_environments
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unlimited".to_owned())
    );
}

async fn power(args: PowerArgs) -> Result<()> {
    let response = HttpAgentApi::new(args.common.api_url)
        .put_environment_power(api::EnvironmentPowerPutParams {
            environment_id: args.common.environment_id,
            power: args.power.into(),
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    print_json_or(args.common.json, &response, || {
        println!(
            "{} desired {:?} (observed {:?})",
            response.environment.environment_id,
            response.environment.desired_power,
            response.environment.status
        )
    })
}

async fn idle_policy(args: IdlePolicyArgs) -> Result<()> {
    let minutes = |value: Option<u64>| value.map(|minutes| minutes.saturating_mul(60_000));
    let idle_policy = if args.clear {
        None
    } else {
        let policy = api::EnvironmentIdlePolicyView {
            pause_after_ms: minutes(args.pause_after_min),
            suspend_after_ms: minutes(args.suspend_after_min),
            stop_after_ms: minutes(args.stop_after_min),
            close_after_ms: minutes(args.close_after_min),
        };
        if policy == api::EnvironmentIdlePolicyView::default() {
            anyhow::bail!("set at least one --*-after-min stage or pass --clear");
        }
        Some(policy)
    };
    let response = HttpAgentApi::new(args.common.api_url)
        .put_environment_idle_policy(api::EnvironmentIdlePolicyPutParams {
            environment_id: args.common.environment_id,
            idle_policy,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    print_json_or(args.common.json, &response, || {
        match &response.environment.idle_policy {
            Some(policy) => println!(
                "{} idle policy pause={:?} suspend={:?} stop={:?} close={:?} (ms)",
                response.environment.environment_id,
                policy.pause_after_ms,
                policy.suspend_after_ms,
                policy.stop_after_ms,
                policy.close_after_ms
            ),
            None => println!(
                "{} idle policy cleared",
                response.environment.environment_id
            ),
        }
    })
}

async fn list(args: EnvListArgs) -> Result<()> {
    let response = HttpAgentApi::new(args.common.api_url)
        .list_environments(api::EnvironmentListParams {
            metadata: args.metadata.map(),
            ..Default::default()
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    print_json_or(args.common.json, &response, || {
        for environment in &response.environments {
            println!(
                "{} {} {:?}",
                environment.environment_id,
                source_label(&environment.source),
                environment.status
            );
        }
    })
}

async fn read(args: EnvironmentResourceArgs) -> Result<()> {
    let response = HttpAgentApi::new(args.api_url)
        .read_environment(api::EnvironmentReadParams {
            environment_id: args.environment_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    print_json_or(args.json, &response, || {
        println!(
            "{} {} {:?}",
            response.environment.environment_id,
            source_label(&response.environment.source),
            response.environment.status
        );
    })
}

/// One-word origin of an environment for table output: the provider id, the
/// registration key it was admitted by, or `external`.
fn source_label(source: &api::EnvironmentSourceView) -> String {
    match source {
        api::EnvironmentSourceView::Provisioned { provider_id, .. } => provider_id.clone(),
        api::EnvironmentSourceView::External { .. } => "external".to_owned(),
        api::EnvironmentSourceView::Registered {
            registration_key_id,
            ..
        } => format!("registered:{registration_key_id}"),
    }
}

async fn activate(args: ActivateArgs) -> Result<()> {
    let response = HttpAgentApi::new(args.session.api_url)
        .activate_session_environment(api::SessionEnvironmentActivateParams {
            session_id: args.session.session,
            environment_id: args.environment_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    print_json_or(args.session.json, &response, || {
        println!(
            "active {}",
            response
                .session
                .active_environment_id
                .as_deref()
                .unwrap_or("-")
        );
    })
}

async fn deactivate(args: SessionArgs) -> Result<()> {
    let response = HttpAgentApi::new(args.api_url)
        .deactivate_session_environment(api::SessionEnvironmentDeactivateParams {
            session_id: args.session,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    print_json_or(args.json, &response, || println!("active -"))
}

async fn close(args: EnvironmentResourceArgs) -> Result<()> {
    let response = HttpAgentApi::new(args.api_url)
        .close_environment(api::EnvironmentCloseParams {
            environment_id: args.environment_id,
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    print_json_or(args.json, &response, || {
        println!("closed {}", response.environment.environment_id)
    })
}

async fn credentials(args: CredentialArgs) -> Result<()> {
    match args.command {
        CredentialCommand::Bind(args) => {
            let source = match (args.grant_id, args.provider_id, args.secret_id) {
                (Some(grant_id), None, None) => {
                    api::EnvironmentCredentialSourceView::AuthGrant { grant_id }
                }
                (None, Some(provider_id), None) => {
                    api::EnvironmentCredentialSourceView::AuthProviderCredential { provider_id }
                }
                (None, None, Some(secret_id)) => {
                    api::EnvironmentCredentialSourceView::DirectSecret { secret_id }
                }
                _ => anyhow::bail!("specify exactly one credential source"),
            };
            let response = HttpAgentApi::new(args.common.api_url)
                .bind_environment_credential(api::EnvironmentCredentialBindParams {
                    environment_id: args.common.environment_id,
                    env_name: args.env_name,
                    source,
                })
                .await
                .map_err(crate::api_client::api_error)?
                .result;
            print_json_or(args.common.json, &response, || {
                print_credential(&response.credential)
            })
        }
        CredentialCommand::List(args) => {
            let response = HttpAgentApi::new(args.api_url)
                .list_environment_credentials(api::EnvironmentCredentialListParams {
                    environment_id: args.environment_id,
                })
                .await
                .map_err(crate::api_client::api_error)?
                .result;
            print_json_or(args.json, &response, || {
                for credential in &response.credentials {
                    print_credential(credential);
                }
            })
        }
        CredentialCommand::Unbind(args) => {
            let response = HttpAgentApi::new(args.common.api_url)
                .unbind_environment_credential(api::EnvironmentCredentialUnbindParams {
                    environment_id: args.common.environment_id,
                    env_name: args.env_name,
                })
                .await
                .map_err(crate::api_client::api_error)?
                .result;
            print_json_or(args.common.json, &response, || {
                print_credential(&response.credential)
            })
        }
    }
}

fn print_credential(credential: &api::EnvironmentCredentialView) {
    println!(
        "{} {} {:?}",
        credential.environment_id, credential.env_name, credential.source
    );
}

pub(crate) fn print_json_or<T: serde::Serialize>(
    json: bool,
    value: &T,
    text: impl FnOnce(),
) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        text();
    }
    Ok(())
}
