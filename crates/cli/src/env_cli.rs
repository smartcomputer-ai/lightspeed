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
    /// Administer deployment-wide environment provider controllers (deployment key).
    #[command(visible_alias = "providers")]
    Provider(ProviderArgs),
    /// Provision an environment using an immutable template on a bound provider.
    Create(CreateArgs),
    /// Register an existing, Lightspeed-reachable envd WebSocket endpoint.
    Register(RegisterArgs),
    /// Inspect the current universe's routing bindings to environment providers.
    #[command(visible_alias = "bindings")]
    Binding(BindingArgs),
    /// Browse immutable templates offered by bound environment providers.
    #[command(visible_alias = "templates")]
    Template(TemplateArgs),
    /// Enable or disable provider-authorized public HTTPS ingress.
    Ingress(IngressArgs),
    /// List universe environments, optionally filtered by metadata.
    List(EnvListArgs),
    /// Read one universe environment.
    Read(EnvironmentResourceArgs),
    /// Close one universe environment.
    Close(EnvironmentResourceArgs),
    /// Set the desired power state (running, paused, suspended, stopped) of
    /// a provisioned environment; the runtime converges it asynchronously and
    /// a powered-down environment wakes on its next use.
    Power(PowerArgs),
    /// Replace or clear the staged idle policy of a provisioned environment.
    IdlePolicy(IdlePolicyArgs),
    /// Bind, list, or unbind universe environment credentials.
    #[command(name = "credential", visible_alias = "credentials")]
    Credentials(CredentialArgs),
    /// Mint, list, read, or revoke registration keys that let outbound
    /// `lightspeed-envd` daemons register as environments.
    #[command(name = "registration-key", visible_alias = "registration-keys")]
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
    #[arg(skip)]
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
    #[arg(skip)]
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
    #[arg(skip)]
    api_url: String,
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug, Clone)]
struct EnvListArgs {
    #[arg(long)]
    provider: Option<String>,
    #[arg(long)]
    binding: Option<String>,
    #[arg(long)]
    registration_key: Option<String>,
    #[command(flatten)]
    common: ResourceArgs,
    #[command(flatten)]
    metadata: crate::session_cli::MetadataPairs,
}

#[derive(Args, Debug, Clone)]
struct EnvironmentResourceArgs {
    #[arg(skip)]
    api_url: String,
    #[arg(long)]
    json: bool,
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
    #[arg(skip)]
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
        EnvCommand::Provider(args) => provider(args).await,
        EnvCommand::Create(args) => create(args).await,
        EnvCommand::Register(args) => register(args).await,
        EnvCommand::Binding(args) => binding(args).await,
        EnvCommand::Template(args) => template(args).await,
        EnvCommand::Ingress(args) => ingress(args).await,
        EnvCommand::List(args) => list(args).await,
        EnvCommand::Read(args) => read(args).await,
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
                if response.registration_keys.is_empty() {
                    println!("No environment registration keys found.");
                }
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
    crate::output::show(false, key).expect("serializable registration key metadata");
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
    if args.common.json {
        crate::output::show(true, &response)
    } else {
        println!("Power change requested; the runtime converges it asynchronously.");
        crate::output::show(
            false,
            &serde_json::json!({"environmentId":response.environment.environment_id,"desiredPower":response.environment.desired_power,"observedStatus":response.environment.status}),
        )
    }
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
    if args.common.json {
        crate::output::show(true, &response)
    } else {
        crate::output::show(
            false,
            &serde_json::json!({"environmentId":response.environment.environment_id,"idlePolicy":response.environment.idle_policy}),
        )
    }
}

async fn list(args: EnvListArgs) -> Result<()> {
    let response = HttpAgentApi::new(args.common.api_url)
        .list_environments(api::EnvironmentListParams {
            metadata: args.metadata.map(),
            provider_id: args.provider,
            binding_id: args.binding,
            registration_key_id: args.registration_key,
            ..Default::default()
        })
        .await
        .map_err(crate::api_client::api_error)?
        .result;
    if args.common.json {
        crate::output::show(true, &response)
    } else {
        crate::output::table(
            &response.environments,
            &[
                ("environmentId", "ID"),
                ("displayName", "NAME"),
                ("status", "STATUS"),
                ("desiredPower", "DESIRED POWER"),
            ],
            "No environments found.",
        )
    }
}

async fn read(args: EnvironmentResourceArgs) -> Result<()> {
    let response = HttpAgentApi::new(args.api_url)
        .read_environment(api::EnvironmentReadParams {
            environment_id: args.environment_id,
        })
        .await?
        .result;
    crate::output::show(args.json, &response.environment)
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
        println!(
            "Close requested for {}. Use environment read to inspect completion.",
            response.environment.environment_id
        )
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
                if response.credentials.is_empty() {
                    println!("No credentials bound to this environment.");
                }
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
    crate::output::show(false, credential).expect("serializable credential binding");
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

#[derive(Args, Debug, Clone)]
struct CreateArgs {
    #[command(flatten)]
    common: ResourceArgs,
    #[arg(long)]
    binding: String,
    #[arg(long)]
    template: String,
    /// Supply the same request ID when retrying an uncertain creation.
    #[arg(long)]
    request_id: String,
    #[arg(long)]
    display_name: Option<String>,
    #[command(flatten)]
    metadata: crate::session_cli::MetadataPairs,
}
#[derive(Args, Debug, Clone)]
struct RegisterArgs {
    #[command(flatten)]
    common: ResourceArgs,
    /// A ws:// or wss:// endpoint reachable from the runtime.
    endpoint: String,
    /// Supply the same request ID when retrying registration.
    #[arg(long)]
    request_id: String,
    #[arg(long)]
    display_name: Option<String>,
    #[command(flatten)]
    metadata: crate::session_cli::MetadataPairs,
}
#[derive(Args, Debug, Clone)]
struct BindingArgs {
    #[command(subcommand)]
    command: BindingCommand,
}
#[derive(Subcommand, Debug, Clone)]
enum BindingCommand {
    /// Create or replace a universe binding from DeploymentProviderBindingPutParams JSON (deployment key).
    Put(DocumentArgs),
    /// Delete a binding from the selected universe (deployment key).
    Delete {
        binding_id: String,
        #[arg(long)]
        json: bool,
    },
    List(ResourceArgs),
    Read {
        binding_id: String,
        #[arg(long)]
        json: bool,
    },
}
#[derive(Args, Debug, Clone)]
struct TemplateArgs {
    #[command(subcommand)]
    command: TemplateCommand,
}
#[derive(Subcommand, Debug, Clone)]
enum TemplateCommand {
    List {
        #[arg(long)]
        binding: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Read {
        template_id: String,
        #[arg(long)]
        binding: String,
        #[arg(long)]
        json: bool,
    },
}
#[derive(Args, Debug, Clone)]
struct IngressArgs {
    #[command(flatten)]
    common: EnvironmentResourceArgs,
    #[arg(long, conflicts_with = "disable", required_unless_present = "disable")]
    enable: bool,
    #[arg(long)]
    disable: bool,
}
async fn create(args: CreateArgs) -> Result<()> {
    let response: api::EnvironmentCreateResponse = HttpAgentApi::new(args.common.api_url)
        .request(
            api::METHOD_ENVIRONMENTS_CREATE,
            api::EnvironmentCreateParams {
                request_id: args.request_id,
                binding_id: args.binding,
                template_id: args.template,
                display_name: args.display_name,
                metadata: args.metadata.map(),
                idle_policy: None,
            },
        )
        .await?
        .result;
    if !args.common.json {
        println!("Provisioning requested; use environment read to inspect progress.");
    }
    crate::output::show(args.common.json, &response.environment)
}
async fn register(args: RegisterArgs) -> Result<()> {
    let endpoint = reqwest::Url::parse(&args.endpoint)?;
    anyhow::ensure!(
        matches!(endpoint.scheme(), "ws" | "wss"),
        "envd endpoint must use ws:// or wss://"
    );
    let response: api::EnvironmentExternalCreateResponse = HttpAgentApi::new(args.common.api_url)
        .request(
            api::METHOD_ENVIRONMENTS_EXTERNAL_CREATE,
            api::EnvironmentExternalCreateParams {
                request_id: args.request_id,
                connection: api::EnvironmentConnectionView {
                    endpoint: args.endpoint,
                    transport: api::EnvironmentConnectionTransportView::WebSocket,
                },
                display_name: args.display_name,
                metadata: args.metadata.map(),
            },
        )
        .await?
        .result;
    crate::output::show(args.common.json, &response.environment)
}
async fn binding(args: BindingArgs) -> Result<()> {
    let client = HttpAgentApi::new("");
    match args.command {
        BindingCommand::Put(args) => {
            let params: api::DeploymentProviderBindingPutParams = read_document(&args.file)?;
            let response: api::DeploymentProviderBindingPutResponse = client
                .request(api::METHOD_DEPLOYMENT_PROVIDER_BINDINGS_PUT, params)
                .await?
                .result;
            crate::output::show(args.json, &response.binding)
        }
        BindingCommand::Delete { binding_id, json } => {
            let universe_id = crate::connection::ACTIVE
                .get()
                .and_then(|c| c.active_universe_id())
                .ok_or_else(|| anyhow::anyhow!("select a universe before deleting a binding"))?;
            let response: api::DeploymentProviderBindingDeleteResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_PROVIDER_BINDINGS_DELETE,
                    api::DeploymentProviderBindingDeleteParams {
                        universe_id,
                        binding_id,
                    },
                )
                .await?
                .result;
            if !json {
                println!("Deleted environment provider binding.");
            }
            crate::output::show(json, &response.binding)
        }
        BindingCommand::List(args) => {
            let response: api::EnvironmentProviderBindingListResponse = client
                .request(
                    api::METHOD_ENVIRONMENTS_PROVIDER_BINDINGS_LIST,
                    api::EnvironmentProviderBindingListParams {},
                )
                .await?
                .result;
            if args.json {
                crate::output::show(true, &response)
            } else {
                crate::output::table(
                    &response.bindings,
                    &[
                        ("bindingId", "ID"),
                        ("providerId", "PROVIDER"),
                        ("status", "STATUS"),
                        ("revision", "REVISION"),
                    ],
                    "No environment provider bindings available.",
                )
            }
        }
        BindingCommand::Read { binding_id, json } => {
            let response: api::EnvironmentProviderBindingReadResponse = client
                .request(
                    api::METHOD_ENVIRONMENTS_PROVIDER_BINDINGS_READ,
                    api::EnvironmentProviderBindingReadParams { binding_id },
                )
                .await?
                .result;
            crate::output::show(json, &response.binding)
        }
    }
}
async fn template(args: TemplateArgs) -> Result<()> {
    let client = HttpAgentApi::new("");
    match args.command {
        TemplateCommand::List { binding, json } => {
            let response: api::EnvironmentTemplateListResponse = client
                .request(
                    api::METHOD_ENVIRONMENTS_TEMPLATES_LIST,
                    api::EnvironmentTemplateListParams {
                        binding_id: binding,
                    },
                )
                .await?
                .result;
            if json {
                crate::output::show(true, &response)
            } else {
                crate::output::table(
                    &response.templates,
                    &[
                        ("templateId", "ID"),
                        ("displayName", "NAME"),
                        ("bindingId", "BINDING"),
                        ("deprecated", "DEPRECATED"),
                    ],
                    "No environment templates available.",
                )
            }
        }
        TemplateCommand::Read {
            template_id,
            binding,
            json,
        } => {
            let response: api::EnvironmentTemplateReadResponse = client
                .request(
                    api::METHOD_ENVIRONMENTS_TEMPLATES_READ,
                    api::EnvironmentTemplateReadParams {
                        binding_id: binding,
                        template_id,
                    },
                )
                .await?
                .result;
            crate::output::show(json, &response.template)
        }
    }
}
async fn ingress(args: IngressArgs) -> Result<()> {
    let response: api::EnvironmentIngressPutResponse = HttpAgentApi::new(args.common.api_url)
        .request(
            api::METHOD_ENVIRONMENTS_INGRESS_PUT,
            api::EnvironmentIngressPutParams {
                environment_id: args.common.environment_id,
                enabled: args.enable,
            },
        )
        .await?
        .result;
    crate::output::show(args.common.json, &response.environment)
}

#[derive(Args, Debug, Clone)]
struct DocumentArgs {
    /// Typed API request JSON document. See the public API reference for fields.
    #[arg(long)]
    file: std::path::PathBuf,
    #[arg(long)]
    json: bool,
}
fn read_document<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Result<T> {
    use anyhow::Context;
    serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )
    .context("invalid API request document")
}
#[derive(Args, Debug, Clone)]
struct ProviderArgs {
    #[command(subcommand)]
    command: ProviderCommand,
}
#[derive(Subcommand, Debug, Clone)]
enum ProviderCommand {
    List {
        #[arg(long)]
        json: bool,
    },
    Read {
        provider_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Create or replace a controller using DeploymentEnvironmentProviderPutParams JSON.
    Put(DocumentArgs),
    Delete {
        provider_id: String,
        #[arg(long)]
        json: bool,
    },
}
async fn provider(args: ProviderArgs) -> Result<()> {
    let client = HttpAgentApi::new("");
    match args.command {
        ProviderCommand::List { json } => {
            let response: api::DeploymentEnvironmentProviderListResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_LIST,
                    api::DeploymentEnvironmentProviderListParams {},
                )
                .await?
                .result;
            if json {
                crate::output::show(true, &response)
            } else {
                crate::output::table(
                    &response.providers,
                    &[
                        ("providerId", "ID"),
                        ("displayName", "NAME"),
                        ("controllerConnection", "CONTROLLER"),
                    ],
                    "No environment provider controllers configured.",
                )
            }
        }
        ProviderCommand::Read { provider_id, json } => {
            let response: api::DeploymentEnvironmentProviderReadResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_READ,
                    api::DeploymentEnvironmentProviderReadParams { provider_id },
                )
                .await?
                .result;
            crate::output::show(json, &response.provider)
        }
        ProviderCommand::Put(args) => {
            let params: api::DeploymentEnvironmentProviderPutParams = read_document(&args.file)?;
            let response: api::DeploymentEnvironmentProviderPutResponse = client
                .request(api::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_PUT, params)
                .await?
                .result;
            crate::output::show(args.json, &response.provider)
        }
        ProviderCommand::Delete { provider_id, json } => {
            let response: api::DeploymentEnvironmentProviderDeleteResponse = client
                .request(
                    api::METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_DELETE,
                    api::DeploymentEnvironmentProviderDeleteParams { provider_id },
                )
                .await?
                .result;
            if !json {
                println!("Deleted environment provider controller.");
            }
            crate::output::show(json, &response.provider)
        }
    }
}
