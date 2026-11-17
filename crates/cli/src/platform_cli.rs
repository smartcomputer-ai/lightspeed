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
    /// Select and manage organizations and their members.
    Organization(OrganizationArgs),
    /// Manage service users in the selected organization.
    ServiceUser(ServiceUserArgs),
    /// Manage personal or service user API keys.
    ApiKey(ApiKeyArgs),
}

impl PlatformArgs {
    /// Whether the command runs without a connection.
    pub(crate) fn offline(&self) -> bool {
        matches!(&self.command, PlatformCommand::ApiKey(args) if matches!(args.command, ApiKeyCommand::GenerateSecret))
    }
}

pub(crate) async fn handle(args: PlatformArgs) -> Result<()> {
    if args.offline() {
        return crate::key_secrets::print_generated_secret(
            crate::key_secrets::SecretKind::ServiceUserKey,
        );
    }
    require_platform()?;
    match args.command {
        PlatformCommand::Organization(args) => organization(args).await,
        PlatformCommand::ServiceUser(args) => service_user(args).await,
        PlatformCommand::ApiKey(args) => api_key(args).await,
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OrganizationRole {
    Member,
    Creator,
    Admin,
}
impl OrganizationRole {
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
pub(crate) struct OrganizationArgs {
    #[command(subcommand)]
    command: OrganizationCommand,
}
#[derive(Debug, Subcommand)]
enum OrganizationCommand {
    /// List the organizations this connection can see.
    List,
    /// Show one organization: the selected one unless an ID is given.
    Show {
        #[arg(value_name = "ORGANIZATION_ID")]
        target: Option<String>,
    },
    /// Select the organization later commands use, for this connection.
    Use {
        #[arg(value_name = "ORGANIZATION_ID")]
        target: String,
    },
    /// Create a shared organization (Platform Admin). Its settings are kept
    /// in an organization document; see `config apply`.
    Create {
        name: String,
        /// The organization's deployment-unique handle.
        #[arg(long)]
        handle: String,
        /// The user who becomes its first Organization Admin; you by default.
        #[arg(long, value_name = "USER_ID")]
        initial_admin_user: Option<String>,
        /// Its own universe limit; without it the deployment default applies.
        #[arg(long)]
        universe_limit: Option<u32>,
    },
    /// List, add or remove the selected organization's people.
    Member {
        #[command(subcommand)]
        command: MemberCommand,
    },
}
#[derive(Debug, Subcommand)]
enum MemberCommand {
    /// List the selected organization's people and how each joined.
    List,
    /// Add a person to the selected organization, or change their role.
    /// Universe roles are given with `universe access`.
    #[command(visible_alias = "set-role")]
    Add {
        #[arg(value_name = "USER_ID")]
        user: String,
        #[arg(long, value_enum, default_value = "member")]
        role: OrganizationRole,
    },
    /// Remove a person from the selected organization, with the roles they
    /// were given in its universes.
    Remove {
        #[arg(value_name = "USER_ID")]
        user: String,
    },
}
#[derive(Debug, Args)]
pub(crate) struct ServiceUserArgs {
    #[command(subcommand)]
    command: ServiceUserCommand,
}
#[derive(Debug, Subcommand)]
enum ServiceUserCommand {
    /// List the selected organization's service users.
    List,
    /// Create a service user; issue its keys with `platform api-key create
    /// --service-user ID`.
    Create {
        /// Unique in the organization.
        name: String,
        #[arg(long, value_enum, default_value = "member")]
        role: OrganizationRole,
    },
    /// Rename a service user, change its role, or disable or enable it.
    /// Disabled, its keys stop working until it is enabled again.
    Update {
        #[arg(value_name = "SERVICE_USER_ID")]
        service_user: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, value_enum)]
        role: Option<OrganizationRole>,
        #[arg(long)]
        enabled: Option<bool>,
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
    /// Grant an organization user or service user access to the selected universe.
    Put {
        #[command(flatten)]
        principal: PrincipalArgs,
        #[arg(long, value_enum)]
        role: UniverseRole,
    },
    /// Remove a grant from the selected universe; the principal keeps what
    /// the organization's default access gives it.
    Remove {
        #[arg(value_name = "GRANT_ID")]
        grant: String,
    },
}
#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct PrincipalArgs {
    #[arg(long)]
    user: Option<String>,
    #[arg(long)]
    service_user: Option<String>,
}
impl PrincipalArgs {
    fn value(self) -> Value {
        match self.user {
            Some(id) => json!({"kind":"user","id":id}),
            None => {
                json!({"kind":"serviceUser","id":self.service_user.expect("clap requires principal")})
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
    pub(crate) owner_service_user: Option<String>,
}
impl InitialOwnerArgs {
    pub(crate) fn value(&self) -> Option<Value> {
        self.owner_user
            .as_ref()
            .map(|id| json!({"kind":"user","id":id}))
            .or_else(|| {
                self.owner_service_user
                    .as_ref()
                    .map(|id| json!({"kind":"serviceUser","id":id}))
            })
    }
}

#[derive(Debug, Args)]
pub(crate) struct ApiKeyArgs {
    /// Manage keys for this organization service user instead of personal keys.
    #[arg(long, global = true)]
    service_user: Option<String>,
    #[command(subcommand)]
    command: ApiKeyCommand,
}
#[derive(Debug, Subcommand)]
enum ApiKeyCommand {
    /// List your keys, or a service user's.
    List,
    /// Issue a key; its secret is shown once. Restrictions only narrow what
    /// its owner may do.
    Create {
        name: String,
        #[arg(long, conflicts_with = "no_expiry")]
        expires_in_days: Option<u32>,
        /// Create a key that never expires. It works until revoked; a
        /// deployment with a maximum key lifetime refuses it.
        #[arg(long)]
        no_expiry: bool,
        #[arg(long = "scope-organization")]
        organizations: Vec<String>,
        #[arg(long = "scope-universe")]
        universes: Vec<uuid::Uuid>,
        #[arg(long = "operation")]
        operations: Vec<String>,
        #[arg(long)]
        read_only: bool,
        #[arg(long)]
        allow_universe_creation: Option<bool>,
        /// Let the key declare service-user keys with supplied secrets, for
        /// a pipeline that applies organization documents. Its principal must
        /// be an Organization Admin; other key changes still need a person.
        #[arg(long)]
        allow_service_user_keys: bool,
        #[command(flatten)]
        output: SecretOutput,
    },
    /// Revoke a key: it stops working at once, for good.
    Revoke {
        #[arg(value_name = "KEY_ID")]
        key: String,
    },
    /// Give a key a new secret in place. Its name, access and expiry stay,
    /// so nothing that refers to it changes; the old secret stops working at
    /// once.
    Replace {
        #[arg(value_name = "KEY_ID")]
        key: String,
        #[command(flatten)]
        output: SecretOutput,
    },
    /// Print a new secret for a service user key declared in an organization
    /// document. Contacts nothing and saves nothing.
    GenerateSecret,
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

pub(crate) fn selected_organization() -> Result<String> {
    connection::ACTIVE
        .get()
        .and_then(|active| active.selected_organization())
        .map(str::to_owned)
        .context("no organization selected; use --organization ID or platform organization use ID")
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
fn with_organization(mut params: Value) -> Result<Value> {
    params["organizationId"] = json!(selected_organization()?);
    Ok(params)
}

pub(crate) async fn organization(args: OrganizationArgs) -> Result<()> {
    require_platform()?;
    let (operation, params) = match args.command {
        OrganizationCommand::List => ("platform/organizations/list", json!({})),
        OrganizationCommand::Show { target } => (
            "platform/organizations/read",
            json!({"organizationId": target.map(Ok).unwrap_or_else(selected_organization)?}),
        ),
        OrganizationCommand::Use {
            target: organization,
        } => {
            let result = call(
                "platform/organizations/read",
                json!({"organizationId":organization}),
            )
            .await?;
            connection::save_organization(
                connection::ACTIVE.get().context("no connection")?,
                &organization,
            )?;
            return show(&result);
        }
        OrganizationCommand::Create {
            name,
            handle,
            initial_admin_user,
            universe_limit,
        } => {
            let mut params = json!({"name":name,"handle":handle});
            if let Some(value) = initial_admin_user {
                params["initialAdminUserId"] = json!(value);
            }
            if let Some(value) = universe_limit {
                params["universeQuota"] = json!(value);
            }
            ("platform/organizations/create", params)
        }
        OrganizationCommand::Member { command } => match command {
            MemberCommand::List => (
                "platform/organizations/members/list",
                with_organization(json!({}))?,
            ),
            MemberCommand::Add { user, role } => (
                "platform/organizations/members/put",
                with_organization(json!({"userId":user,"role":role.value()}))?,
            ),
            MemberCommand::Remove { user } => (
                "platform/organizations/members/delete",
                with_organization(json!({"userId":user}))?,
            ),
        },
    };
    show(&call(operation, params).await?)
}
pub(crate) async fn service_user(args: ServiceUserArgs) -> Result<()> {
    let (operation, params) = match args.command {
        ServiceUserCommand::List => ("platform/organizations/service-users/list", json!({})),
        ServiceUserCommand::Create { name, role } => (
            "platform/organizations/service-users/create",
            json!({"name":name,"role":role.value()}),
        ),
        ServiceUserCommand::Update {
            service_user,
            name,
            role,
            enabled,
        } => {
            let mut params = json!({"serviceUserId":service_user});
            if let Some(value) = name {
                params["name"] = json!(value);
            }
            if let Some(value) = role {
                params["role"] = json!(value.value());
            }
            if let Some(value) = enabled {
                params["enabled"] = json!(value);
            }
            ("platform/organizations/service-users/update", params)
        }
    };
    show(&call(operation, with_organization(params)?).await?)
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
        "platform/credentials/create" | "platform/service-users/credentials/create"
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
    let mut params = match &args.service_user {
        Some(id) => with_organization(json!({"serviceUserId":id}))?,
        None => json!({}),
    };
    let prefix = if args.service_user.is_some() {
        "platform/service-users/credentials"
    } else {
        "platform/credentials"
    };
    match args.command {
        ApiKeyCommand::List => show(&call(&format!("{prefix}/list"), params).await?),
        ApiKeyCommand::Revoke { key } => {
            params["keyId"] = json!(key);
            show(&call(&format!("{prefix}/revoke"), params).await?)
        }
        ApiKeyCommand::Replace { key, output } => {
            params["keyId"] = json!(key);
            let file = output.prepare(true)?;
            output.finish(call(&format!("{prefix}/replace"), params).await?, file)
        }
        ApiKeyCommand::GenerateSecret => unreachable!("handled offline"),
        ApiKeyCommand::Create {
            name,
            expires_in_days,
            no_expiry,
            organizations,
            universes,
            operations,
            read_only,
            allow_universe_creation,
            allow_service_user_keys,
            output,
        } => {
            let mut restrictions = json!({});
            if !organizations.is_empty() {
                restrictions["organizationIds"] = json!(organizations);
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
            if allow_service_user_keys {
                restrictions["allowServiceUserKeys"] = json!(true);
            }
            params["name"] = json!(name);
            params["restrictions"] = restrictions;
            if no_expiry {
                params["expiresInDays"] = serde_json::Value::Null;
            } else if let Some(value) = expires_in_days {
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
