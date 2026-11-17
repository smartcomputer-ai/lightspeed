use crate::{api_client::HttpAgentApi, connection};
use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};
use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

#[derive(Debug, Args)]
pub(crate) struct PlatformArgs {
    #[command(subcommand)]
    command: PlatformCommand,
}

#[derive(Debug, Subcommand)]
enum PlatformCommand {
    /// Select and manage accounts and their members.
    Account(AccountArgs),
    /// Manage applications in the selected account.
    Application(ApplicationArgs),
    /// Manage personal or application API keys.
    ApiKey(ApiKeyArgs),
}

pub(crate) async fn handle(args: PlatformArgs) -> Result<()> {
    require_platform()?;
    match args.command {
        PlatformCommand::Account(args) => account(args).await,
        PlatformCommand::Application(args) => application(args).await,
        PlatformCommand::ApiKey(args) => api_key(args).await,
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum AccountRole {
    Member,
    Creator,
    Admin,
}
impl AccountRole {
    fn value(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Creator => "creator",
            Self::Admin => "admin",
        }
    }
}
#[derive(Debug, Clone, Copy, ValueEnum)]
enum UniverseRole {
    Viewer,
    Contributor,
    Operator,
    Owner,
}
impl UniverseRole {
    fn value(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Contributor => "contributor",
            Self::Operator => "operator",
            Self::Owner => "owner",
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct AccountArgs {
    #[command(subcommand)]
    command: AccountCommand,
}
#[derive(Debug, Subcommand)]
enum AccountCommand {
    List,
    Show {
        #[arg(value_name = "ACCOUNT_ID")]
        target: Option<String>,
    },
    Use {
        #[arg(value_name = "ACCOUNT_ID")]
        target: String,
    },
    Create {
        name: String,
        #[arg(long)]
        slug: String,
        #[arg(long)]
        initial_admin_user: Option<String>,
        #[arg(long)]
        quota: Option<u32>,
    },
    /// Apply account settings from a JSON object, including membershipPolicy.
    Update {
        #[arg(long)]
        file: PathBuf,
    },
    Member {
        #[command(subcommand)]
        command: MemberCommand,
    },
}
#[derive(Debug, Subcommand)]
enum MemberCommand {
    List,
    /// Add or update an account member. Universe grants are managed with universe access.
    #[command(visible_alias = "set-role")]
    Add {
        user: String,
        #[arg(long, value_enum, default_value = "member")]
        role: AccountRole,
    },
    Remove {
        user: String,
    },
}
#[derive(Debug, Args)]
pub(crate) struct ApplicationArgs {
    #[command(subcommand)]
    command: ApplicationCommand,
}
#[derive(Debug, Subcommand)]
enum ApplicationCommand {
    List,
    Create {
        name: String,
        #[arg(long, value_enum, default_value = "member")]
        role: AccountRole,
    },
    Update {
        application: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, value_enum)]
        role: Option<AccountRole>,
        #[arg(long)]
        enabled: Option<bool>,
    },
    Disable {
        application: String,
    },
}
#[derive(Debug, Args)]
pub(crate) struct AccessArgs {
    #[command(subcommand)]
    command: AccessCommand,
}
#[derive(Debug, Subcommand)]
enum AccessCommand {
    /// List explicit grants in the selected universe.
    List,
    /// Grant an account user or application access to the selected universe.
    Put {
        #[command(flatten)]
        principal: PrincipalArgs,
        #[arg(long, value_enum)]
        role: UniverseRole,
    },
    Remove {
        grant: String,
    },
    /// Replace declared access policy with a JSON object from a file.
    PolicyPut {
        #[arg(long)]
        file: PathBuf,
    },
}
#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct PrincipalArgs {
    #[arg(long)]
    user: Option<String>,
    #[arg(long)]
    application: Option<String>,
}
impl PrincipalArgs {
    fn value(self) -> Value {
        match self.user {
            Some(id) => json!({"kind":"user","id":id}),
            None => {
                json!({"kind":"application","id":self.application.expect("clap requires principal")})
            }
        }
    }
}

#[derive(Debug, Clone, Default, Args)]
#[group(multiple = false)]
pub(crate) struct InitialOwnerArgs {
    /// Explicit initial Owner for administrative provisioning.
    #[arg(long)]
    pub(crate) owner_user: Option<String>,
    #[arg(long)]
    pub(crate) owner_application: Option<String>,
}
impl InitialOwnerArgs {
    pub(crate) fn value(&self) -> Option<Value> {
        self.owner_user
            .as_ref()
            .map(|id| json!({"kind":"user","id":id}))
            .or_else(|| {
                self.owner_application
                    .as_ref()
                    .map(|id| json!({"kind":"application","id":id}))
            })
    }
}

#[derive(Debug, Args)]
pub(crate) struct ApiKeyArgs {
    /// Manage keys for this account application instead of personal keys.
    #[arg(long, global = true)]
    application: Option<String>,
    #[command(subcommand)]
    command: ApiKeyCommand,
}
#[derive(Debug, Subcommand)]
enum ApiKeyCommand {
    List,
    Create {
        name: String,
        #[arg(long)]
        expires_in_days: Option<u32>,
        #[arg(long = "scope-account")]
        accounts: Vec<String>,
        #[arg(long = "scope-universe")]
        universes: Vec<uuid::Uuid>,
        #[arg(long = "operation")]
        operations: Vec<String>,
        #[arg(long)]
        read_only: bool,
        #[arg(long)]
        allow_universe_creation: Option<bool>,
        #[command(flatten)]
        output: SecretOutput,
    },
    Revoke {
        key: String,
    },
}
#[derive(Debug, Default, Args)]
#[group(multiple = false)]
pub(crate) struct SecretOutput {
    /// Write a newly issued secret into a new owner-only file (never overwrite).
    #[arg(long)]
    output_secret: Option<PathBuf>,
    /// Explicitly allow a newly issued secret in standard output.
    #[arg(long)]
    show_secret: bool,
}
#[derive(Debug, Args)]
pub(crate) struct ApiArgs {
    /// Canonical runtime or Platform operation ID, such as profiles/list.
    operation: String,
    /// JSON parameters file; omit for an empty object.
    #[arg(long)]
    params: Option<PathBuf>,
    #[command(flatten)]
    output: SecretOutput,
}

pub(crate) fn selected_account() -> Result<String> {
    connection::ACTIVE
        .get()
        .and_then(|active| active.selected_account())
        .map(str::to_owned)
        .context("no account selected; use --account ID or platform account use ID")
}
fn selected_universe() -> Result<String> {
    connection::ACTIVE
        .get()
        .and_then(|active| active.active_universe())
        .context("no universe selected; use --universe ID or universe use ID")
}
fn read_object(path: &Path) -> Result<Value> {
    let value: Value = serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )?;
    ensure!(value.is_object(), "parameters must be a JSON object");
    Ok(value)
}
fn require_platform() -> Result<()> {
    ensure!(
        connection::ACTIVE
            .get()
            .is_some_and(|active| active.is_platform()),
        "this command requires a Platform connection"
    );
    Ok(())
}
async fn call(operation: &str, params: Value) -> Result<Value> {
    require_platform()?;
    Ok(HttpAgentApi::from_active()
        .call_platform(operation, params)
        .await?)
}
fn show(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
fn with_account(mut params: Value) -> Result<Value> {
    params["accountId"] = json!(selected_account()?);
    Ok(params)
}

pub(crate) async fn account(args: AccountArgs) -> Result<()> {
    require_platform()?;
    let (operation, params) = match args.command {
        AccountCommand::List => ("platform/accounts/list", json!({})),
        AccountCommand::Show { target } => (
            "platform/accounts/read",
            json!({"accountId": target.map(Ok).unwrap_or_else(selected_account)?}),
        ),
        AccountCommand::Use { target: account } => {
            let result = call("platform/accounts/read", json!({"accountId":account})).await?;
            connection::save_account(connection::ACTIVE.get().context("no connection")?, &account)?;
            return show(&result);
        }
        AccountCommand::Create {
            name,
            slug,
            initial_admin_user,
            quota,
        } => {
            let mut params = json!({"name":name,"slug":slug});
            if let Some(value) = initial_admin_user {
                params["initialAdminUserId"] = json!(value);
            }
            if let Some(value) = quota {
                params["universeQuota"] = json!(value);
            }
            ("platform/accounts/create", params)
        }
        AccountCommand::Update { file } => (
            "platform/accounts/update",
            with_account(read_object(&file)?)?,
        ),
        AccountCommand::Member { command } => match command {
            MemberCommand::List => ("platform/accounts/members/list", with_account(json!({}))?),
            MemberCommand::Add { user, role } => (
                "platform/accounts/members/put",
                with_account(json!({"userId":user,"role":role.value()}))?,
            ),
            MemberCommand::Remove { user } => (
                "platform/accounts/members/delete",
                with_account(json!({"userId":user}))?,
            ),
        },
    };
    show(&call(operation, params).await?)
}
pub(crate) async fn application(args: ApplicationArgs) -> Result<()> {
    let (operation, params) = match args.command {
        ApplicationCommand::List => ("platform/accounts/applications/list", json!({})),
        ApplicationCommand::Create { name, role } => (
            "platform/accounts/applications/create",
            json!({"name":name,"role":role.value()}),
        ),
        ApplicationCommand::Update {
            application,
            name,
            role,
            enabled,
        } => {
            let mut params = json!({"applicationId":application});
            if let Some(value) = name {
                params["name"] = json!(value);
            }
            if let Some(value) = role {
                params["role"] = json!(value.value());
            }
            if let Some(value) = enabled {
                params["enabled"] = json!(value);
            }
            ("platform/accounts/applications/update", params)
        }
        ApplicationCommand::Disable { application } => (
            "platform/accounts/applications/delete",
            json!({"applicationId":application}),
        ),
    };
    show(&call(operation, with_account(params)?).await?)
}
pub(crate) async fn access(args: AccessArgs) -> Result<()> {
    require_platform()?;
    let (operation, mut params) = match args.command {
        AccessCommand::List => ("platform/universes/grants/list", json!({})),
        AccessCommand::Put { principal, role } => (
            "platform/universes/grants/put",
            json!({"principal":principal.value(),"role":role.value()}),
        ),
        AccessCommand::Remove { grant } => {
            ("platform/universes/grants/delete", json!({"grantId":grant}))
        }
        AccessCommand::PolicyPut { file } => (
            "platform/universes/access-policy/put",
            json!({"policy":read_object(&file)?}),
        ),
    };
    params["universe"] = json!(selected_universe()?);
    show(&call(operation, params).await?)
}

impl SecretOutput {
    fn prepare(&self, required: bool) -> Result<Option<std::fs::File>> {
        ensure!(
            !required || self.show_secret || self.output_secret.is_some(),
            "issuing a key requires --output-secret FILE or --show-secret"
        );
        self.output_secret
            .as_ref()
            .map(|path| {
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                options
                    .open(path)
                    .with_context(|| format!("create private secret file {}", path.display()))
            })
            .transpose()
    }
    fn finish(&self, mut value: Value, mut file: Option<std::fs::File>) -> Result<()> {
        if let Some(secret) = value.get("secret").and_then(Value::as_str) {
            if let Some(file) = file.as_mut() {
                file.write_all(secret.as_bytes())?;
                file.write_all(b"\n")?;
                file.sync_all()?;
            }
            if !self.show_secret {
                value
                    .as_object_mut()
                    .context("invalid credential response")?
                    .remove("secret");
            }
        }
        show(&value)
    }
}
fn issues_secret(operation: &str) -> bool {
    matches!(
        operation,
        "platform/credentials/create" | "platform/applications/credentials/create"
    )
}
fn ensure_json_operation(operation: &str, universe: Option<&str>, params: Value) -> Result<()> {
    let parts = api::http_request(operation, universe, params)?;
    ensure!(
        parts.transfer == api::HttpTransfer::Json,
        "generic api supports JSON operations; use the dedicated resource command for binary or streaming transfers"
    );
    Ok(())
}
pub(crate) async fn api(args: ApiArgs) -> Result<()> {
    let params = args
        .params
        .as_deref()
        .map(read_object)
        .transpose()?
        .unwrap_or_else(|| json!({}));
    let file = args.output.prepare(issues_secret(&args.operation))?;
    let client = HttpAgentApi::from_active();
    let response: Value = if args.operation.starts_with("platform/") {
        require_platform()?;
        client.call_platform(&args.operation, params).await?
    } else {
        ensure_json_operation(
            &args.operation,
            client.connection().universe.as_deref(),
            params.clone(),
        )?;
        client.request(&args.operation, params).await?
    };
    args.output.finish(response, file)
}
pub(crate) async fn api_key(args: ApiKeyArgs) -> Result<()> {
    let mut params = match &args.application {
        Some(id) => with_account(json!({"applicationId":id}))?,
        None => json!({}),
    };
    let prefix = if args.application.is_some() {
        "platform/applications/credentials"
    } else {
        "platform/credentials"
    };
    match args.command {
        ApiKeyCommand::List => show(&call(&format!("{prefix}/list"), params).await?),
        ApiKeyCommand::Revoke { key } => {
            params["keyId"] = json!(key);
            show(&call(&format!("{prefix}/revoke"), params).await?)
        }
        ApiKeyCommand::Create {
            name,
            expires_in_days,
            accounts,
            universes,
            operations,
            read_only,
            allow_universe_creation,
            output,
        } => {
            let mut restrictions = json!({});
            if !accounts.is_empty() {
                restrictions["accountIds"] = json!(accounts);
            }
            if !universes.is_empty() {
                restrictions["universeIds"] = json!(universes);
            }
            if !operations.is_empty() {
                restrictions["operations"] = json!(operations);
            }
            if read_only {
                restrictions["readOnly"] = json!(true);
            }
            if let Some(value) = allow_universe_creation {
                restrictions["allowUniverseCreation"] = json!(value);
            }
            params["name"] = json!(name);
            params["restrictions"] = restrictions;
            if let Some(value) = expires_in_days {
                params["expiresInDays"] = json!(value);
            }
            let file = output.prepare(true)?;
            output.finish(call(&format!("{prefix}/create"), params).await?, file)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generic_api_refuses_streams_and_binary_before_sending() {
        assert!(ensure_json_operation("watch", Some("u"), json!({})).is_err());
        assert!(
            ensure_json_operation("blobs/read", Some("u"), json!({"blobRef":"sha256:example"}))
                .is_err()
        );
        assert!(ensure_json_operation("profiles/list", Some("u"), json!({})).is_ok());
    }
    #[test]
    fn secret_issuance_needs_explicit_output_and_never_overwrites() {
        assert!(SecretOutput::default().prepare(true).is_err());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("key");
        let output = SecretOutput {
            output_secret: Some(path.clone()),
            show_secret: false,
        };
        output.prepare(true).unwrap();
        assert!(output.prepare(true).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
