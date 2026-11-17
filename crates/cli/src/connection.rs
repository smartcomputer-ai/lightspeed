//! Local deployment connections. Server authority is always discovered, never inferred from a key.
use crate::api_client::HttpAgentApi;
use anyhow::{Context, Result, bail};
use api::CallerAccess;
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{IsTerminal, Read, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
};
use uuid::Uuid;

pub static ACTIVE: OnceLock<ResolvedConnection> = OnceLock::new();

pub(crate) fn active() -> &'static ResolvedConnection {
    ACTIVE
        .get()
        .expect("online commands resolve their connection before dispatch")
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CredentialKind {
    #[default]
    None,
    RuntimeToken,
    PlatformApiKey,
    PlatformSession,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ServerKind {
    Runtime,
    Platform,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformPrincipal {
    pub kind: String,
    pub id: String,
    pub name: String,
    pub platform_admin: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Discovery {
    runtime: api::RuntimeKind,
    contract_revision: String,
    caller: CallerAccess,
    default_universe: Option<String>,
    platform: Option<PlatformDiscovery>,
}
#[derive(Deserialize)]
struct PlatformDiscovery {
    principal: PlatformPrincipal,
    credential: PlatformCredential,
}
#[derive(Deserialize)]
struct PlatformCredential {
    kind: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedConnection {
    pub endpoint: String,
    pub single: bool,
    pub credential_kind: CredentialKind,
    pub server_kind: ServerKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default)]
    pub allow_insecure_http: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_file: Option<PathBuf>,
    #[serde(default)]
    pub owned_credential: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub universe: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Connections {
    pub selected: Option<String>,
    pub connections: BTreeMap<String, SavedConnection>,
}

// Deliberately no Debug: holds plaintext only for the life of the client.
#[derive(Clone, Default)]
pub struct ResolvedConnection {
    pub name: Option<String>,
    pub endpoint: String,
    pub secret: Option<String>,
    pub credential_kind: CredentialKind,
    pub credential_file: Option<PathBuf>,
    pub owned_credential: bool,
    pub server_kind: Option<ServerKind>,
    pub principal: Option<PlatformPrincipal>,
    pub account_id: Option<String>,
    pub allow_insecure_http: bool,
    /// The universe a runtime request addresses, as its UUID or its handle.
    /// A saved selection is always the UUID; a handle here was named for
    /// this invocation and is sent as given, for the runtime to resolve.
    pub universe: Option<String>,
    pub caller: Option<CallerAccess>,
}

impl ResolvedConnection {
    pub fn is_platform(&self) -> bool {
        self.server_kind == Some(ServerKind::Platform)
    }
    pub fn selected_account(&self) -> Option<&str> {
        self.account_id.as_deref()
    }

    /// Authentication is shared by runtime resources and native Platform operations.
    pub fn auth_headers(&self) -> Result<reqwest::header::HeaderMap> {
        use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
        let mut headers = HeaderMap::new();
        let secret = if self.credential_kind == CredentialKind::PlatformSession {
            self.credential_file
                .as_deref()
                .map(read_secret)
                .transpose()?
                .or_else(|| self.secret.clone())
        } else {
            self.secret.clone()
        };
        if let Some(secret) = secret {
            let mut value = HeaderValue::from_str(&format!("Bearer {secret}"))
                .map_err(|_| anyhow::anyhow!("invalid credential header"))?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
        Ok(headers)
    }

    pub fn accept_session_rotation(&self, headers: &reqwest::header::HeaderMap) -> Result<()> {
        if self.credential_kind != CredentialKind::PlatformSession
            || !self.owned_credential
            || !headers.contains_key("set-auth-token")
        {
            return Ok(());
        }
        self.accept_session_rotation_at(headers, &config_dir()?)
    }

    fn accept_session_rotation_at(
        &self,
        headers: &reqwest::header::HeaderMap,
        dir: &Path,
    ) -> Result<()> {
        if self.credential_kind != CredentialKind::PlatformSession || !self.owned_credential {
            return Ok(());
        }
        let Some(value) = headers.get("set-auth-token") else {
            return Ok(());
        };
        let secret = value
            .to_str()
            .map_err(|_| anyhow::anyhow!("invalid renewed session credential"))?;
        if secret.is_empty() || secret.chars().any(char::is_whitespace) || secret.len() > 4096 {
            bail!("invalid renewed session credential");
        }
        let path = self
            .credential_file
            .as_deref()
            .context("CLI session has no owned credential file")?;
        if path.parent() != Some(dir.join("credentials").as_path()) {
            bail!("refusing to replace a credential outside the CLI credential directory");
        }
        private_write(path, secret.as_bytes())
    }

    /// The universe requests actually address, as a UUID or a handle,
    /// including server-bound keys and single mode even when no local
    /// selection was saved.
    pub fn active_universe(&self) -> Option<String> {
        self.universe.clone()
    }

    pub(crate) fn redact_error(&self, mut detail: String) -> String {
        if let Some(secret) = &self.secret
            && !secret.is_empty()
        {
            detail = detail.replace(secret, "[credential redacted]");
        }
        if self.credential_kind == CredentialKind::PlatformSession
            && let Some(path) = &self.credential_file
            && let Ok(secret) = read_secret(path)
        {
            detail = detail.replace(&secret, "[credential redacted]");
        }
        detail
    }

    pub fn headers(&self, method: &str) -> Result<reqwest::header::HeaderMap> {
        let headers = self.auth_headers()?;
        let scope = api::method_access(method)
            .context("unknown runtime method")?
            .scope();
        if matches!(
            scope,
            api::MethodScope::Connection | api::MethodScope::Deployment
        ) {
            return Ok(headers);
        }
        if self.universe.is_none() {
            bail!(
                "select a universe with `lightspeed universe use <uuid-or-handle>` or --universe"
            );
        }
        Ok(headers)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UniverseStatus {
    /// Both are known once the universe has been read. Otherwise only the
    /// one the selection carries is, and the reason says why.
    pub universe_id: Option<String>,
    pub handle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

impl UniverseStatus {
    pub fn print(&self, heading: &str) {
        println!("{heading}:");
        if self.universe_id.is_none() && self.handle.is_none() {
            println!("  None selected. Use `lightspeed universe use <handle-or-uuid>`.");
            return;
        }
        let shown = |value: &Option<String>| match (value, &self.unavailable_reason) {
            (Some(value), _) => value.clone(),
            (None, reason) => format!("unavailable ({})", reason.as_deref().unwrap_or("not read")),
        };
        println!("  Handle: {}", shown(&self.handle));
        println!("  UUID: {}", shown(&self.universe_id));
    }
}

/// Status must remain useful for restricted keys and stale selections. The
/// selection supplies its own half of the identity; the other half is read
/// only when permitted, with the reason given when it could not be.
pub async fn universe_status(connection: &ResolvedConnection) -> Result<UniverseStatus> {
    connection
        .caller
        .as_ref()
        .context("connection has not been verified")?;
    let mut status = UniverseStatus {
        universe_id: None,
        handle: None,
        unavailable_reason: None,
    };
    let Some(selected) = connection.active_universe() else {
        return Ok(status);
    };
    if Uuid::parse_str(&selected).is_ok() {
        status.universe_id = Some(selected.clone());
    } else {
        status.handle = Some(selected.clone());
    }
    let response = HttpAgentApi::with_connection(connection.clone())
        .request::<_, api::DeploymentUniverseReadResponse>(
            api::METHOD_DEPLOYMENT_UNIVERSES_READ,
            api::DeploymentUniverseReadParams { universe: selected },
        )
        .await;
    match response {
        Ok(response) => {
            status.universe_id = Some(response.universe.universe_id);
            status.handle = Some(response.universe.handle);
        }
        Err(error) => status.unavailable_reason = Some(error.to_string()),
    }
    Ok(status)
}

pub fn config_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("LIGHTSPEED_CONFIG_DIR") {
        return absolute_path(PathBuf::from(path));
    }
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .context("set LIGHTSPEED_CONFIG_DIR or HOME for saved CLI connections")?;
    absolute_path(root.join("lightspeed"))
}

fn absolute_path(path: PathBuf) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    })
}

pub fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("file needs a parent directory")?;
    if !parent.exists() {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(parent)?;
    }
    let temp = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

pub fn read_secret(path: &Path) -> Result<String> {
    let meta = std::fs::symlink_metadata(path)
        .with_context(|| format!("read credential file {}", path.display()))?;
    if !meta.is_file() {
        bail!("credential must be a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            bail!(
                "credential file {} must be owner-only (chmod 600)",
                path.display()
            );
        }
    }
    nonempty_secret(std::fs::read_to_string(path)?)
}

fn nonempty_secret(secret: String) -> Result<String> {
    let secret = secret.trim();
    if secret.is_empty() {
        bail!("credential is empty");
    }
    Ok(secret.to_owned())
}

pub fn load_at(dir: &Path) -> Result<Connections> {
    match std::fs::read(dir.join("connections.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("invalid saved connections"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Connections::default()),
        Err(error) => Err(error.into()),
    }
}
pub(crate) fn save_at(dir: &Path, config: &Connections) -> Result<()> {
    private_write(
        &dir.join("connections.json"),
        &serde_json::to_vec_pretty(config)?,
    )
}

pub fn endpoint(url: &str) -> Result<String> {
    endpoint_with_policy(url, false)
}

pub fn endpoint_with_policy(url: &str, allow_insecure_http: bool) -> Result<String> {
    let mut parsed = reqwest::Url::parse(url).context("invalid deployment URL")?;
    let local = parsed.host_str().is_some_and(|h| {
        h == "localhost"
            || h == "[::1]"
            || h == "::1"
            || h.parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if !(parsed.scheme() == "https"
        || (parsed.scheme() == "http" && (local || allow_insecure_http)))
    {
        bail!(
            "deployment URL requires HTTPS except on loopback; use --allow-insecure-http only for a trusted development network"
        );
    }
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!("deployment URL must not contain credentials, query parameters or a fragment");
    }
    let path = parsed.path().trim_end_matches('/');
    if !path.ends_with("/api/v1") {
        parsed.set_path(&format!("{path}/api/v1"));
    }
    Ok(parsed.to_string().trim_end_matches('/').to_owned())
}

/// Auth endpoints live next to the shared API, including deployments under a base path.
pub fn auth_url(endpoint: &str, suffix: &str) -> Result<reqwest::Url> {
    let base = endpoint
        .strip_suffix("/api/v1")
        .context("invalid deployment API base")?;
    reqwest::Url::parse(&format!("{base}/api/auth/{suffix}")).context("invalid deployment auth URL")
}

pub(crate) fn saved_resolved(name: &str, saved: &SavedConnection) -> Result<ResolvedConnection> {
    if saved.single && saved.credential_file.is_some() {
        bail!("single-mode connection must not contain a runtime token");
    }
    if !saved.single && saved.credential_file.is_none() {
        bail!("authenticated connection has no credential");
    }
    if (saved.credential_kind == CredentialKind::None) != saved.single {
        bail!("saved credential kind does not match its authentication mode");
    }
    if (saved.server_kind == ServerKind::Runtime)
        != matches!(
            saved.credential_kind,
            CredentialKind::None | CredentialKind::RuntimeToken
        )
    {
        bail!("saved credential kind does not match the deployment kind");
    }
    if saved.server_kind == ServerKind::Runtime && saved.account_id.is_some() {
        bail!("a direct runtime connection cannot select a Platform account");
    }
    if saved
        .credential_file
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        bail!("saved credential path must be absolute");
    }
    Ok(ResolvedConnection {
        name: Some(name.to_owned()),
        endpoint: endpoint_with_policy(&saved.endpoint, saved.allow_insecure_http)?,
        credential_kind: saved.credential_kind,
        credential_file: saved.credential_file.clone(),
        owned_credential: saved.owned_credential,
        account_id: saved.account_id.clone(),
        server_kind: Some(saved.server_kind),
        principal: None,
        allow_insecure_http: saved.allow_insecure_http,
        secret: saved
            .credential_file
            .as_deref()
            .map(read_secret)
            .transpose()?,
        universe: saved.universe.clone(),
        caller: None,
    })
}

pub async fn verify(mut connection: ResolvedConnection) -> Result<ResolvedConnection> {
    let client = HttpAgentApi::with_connection(connection.clone());
    let response: Discovery = client
        .request_connection(api::METHOD_INITIALIZE, serde_json::json!({}))
        .await?;
    if response.contract_revision != api::HTTP_CONTRACT_REVISION {
        bail!("deployment HTTP contract revision is incompatible with this CLI");
    }
    let discovered_kind = match response.runtime {
        api::RuntimeKind::Runtime => ServerKind::Runtime,
        api::RuntimeKind::Platform => ServerKind::Platform,
    };
    if connection
        .server_kind
        .is_some_and(|saved| saved != discovered_kind)
    {
        bail!("saved connection does not match the deployment kind");
    }
    connection.server_kind = Some(match response.runtime {
        api::RuntimeKind::Runtime => {
            if connection.credential_kind == CredentialKind::PlatformSession {
                bail!("a Platform session cannot authenticate to a direct runtime");
            }
            connection.credential_kind = if connection.secret.is_some() {
                CredentialKind::RuntimeToken
            } else {
                CredentialKind::None
            };
            if connection.account_id.is_some() {
                bail!("a direct runtime connection cannot select a Platform account");
            }
            ServerKind::Runtime
        }
        api::RuntimeKind::Platform => {
            if connection.secret.is_none() {
                bail!(
                    "Platform authentication is required; use `lightspeed connect login` or add an API-key connection"
                );
            }
            let platform = response
                .platform
                .context("Platform discovery omitted its principal")?;
            match (
                connection.credential_kind,
                platform.credential.kind.as_str(),
            ) {
                (CredentialKind::PlatformSession, "cliSession") => {}
                (CredentialKind::PlatformSession, _) => {
                    bail!("the approved credential is not a CLI login session")
                }
                (_, "personalKey" | "applicationKey") => {
                    connection.credential_kind = CredentialKind::PlatformApiKey
                }
                (_, "cliSession" | "session") => bail!(
                    "use `lightspeed connect login` for managed CLI sessions; connect add accepts a Platform API key"
                ),
                _ => bail!("unsupported Platform credential kind"),
            }
            connection.principal = Some(platform.principal);
            ServerKind::Platform
        }
    });
    if response.caller.single != connection.secret.is_none() {
        bail!("deployment authentication does not match the configured connection");
    }
    // A deployment-wide suggestion cannot undo an explicit account selection:
    // discovery does not identify the suggested universe's containing account.
    if connection.universe.is_none()
        && (!connection.is_platform() || connection.account_id.is_none())
    {
        connection.universe = response.default_universe;
    }
    connection.caller = Some(response.caller);
    Ok(connection)
}

/// The UUID of the universe a UUID or handle names, for a selection that is
/// saved: stored as the UUID, a later rename cannot move it to another
/// universe. Resolving a handle needs permission to read universes, which a
/// per-invocation selection does not.
pub async fn resolve_universe(connection: &ResolvedConnection, selected: &str) -> Result<String> {
    if let Ok(id) = Uuid::parse_str(selected) {
        return Ok(id.to_string());
    }
    let response: api::DeploymentUniverseReadResponse = HttpAgentApi::with_connection(
        connection.clone(),
    )
    .request(
        api::METHOD_DEPLOYMENT_UNIVERSES_READ,
        api::DeploymentUniverseReadParams {
            universe: selected.to_owned(),
        },
    )
    .await
    .with_context(|| {
        format!(
            "cannot resolve universe {selected} to save it; use its UUID, or pass --universe {selected} on each command if the deployment is unavailable"
        )
    })?;
    Ok(response.universe.universe_id)
}

/// Explicit connection selection is an atomic choice of endpoint and credential.
/// Environment overrides apply when no named connection is explicitly selected.
pub async fn resolve(
    name: Option<&str>,
    url: Option<&str>,
    universe: Option<&str>,
) -> Result<ResolvedConnection> {
    let config = match config_dir() {
        Ok(dir) => load_at(&dir)?,
        // Nowhere to keep saved connections, as in a container that sets no
        // home directory: the connection then comes from the environment.
        Err(_) if name.is_none() => Connections::default(),
        Err(error) => return Err(error),
    };
    let selected = name.or(config.selected.as_deref());
    let saved = selected
        .map(|n| {
            config
                .connections
                .get(n)
                .with_context(|| format!("unknown connection {n}"))
        })
        .transpose()?;
    let env_url = if name.is_none() {
        std::env::var("LIGHTSPEED_API_URL").ok()
    } else {
        None
    };
    let target = url.or(env_url.as_deref()).filter(|s| !s.is_empty());
    let explicit_secret = if name.is_none() {
        std::env::var("LIGHTSPEED_API_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(nonempty_secret)
            .transpose()?
    } else {
        None
    };
    let mut connection = if let Some(saved) = saved {
        let saved_endpoint = endpoint_with_policy(&saved.endpoint, saved.allow_insecure_http)?;
        let effective_endpoint = target
            .map(|url| endpoint_with_policy(url, saved.allow_insecure_http))
            .transpose()?
            .unwrap_or(saved_endpoint.clone());
        if effective_endpoint != saved_endpoint {
            // Never read or forward a saved credential to an overridden endpoint.
            ResolvedConnection {
                name: None,
                endpoint: endpoint(&effective_endpoint)?,
                secret: None,
                universe: None,
                caller: None,
                ..Default::default()
            }
        } else if explicit_secret.is_some() {
            ResolvedConnection {
                name: selected.map(str::to_owned),
                endpoint: saved_endpoint,
                account_id: saved.account_id.clone(),
                allow_insecure_http: saved.allow_insecure_http,
                secret: None,
                universe: saved.universe.clone(),
                caller: None,
                ..Default::default()
            }
        } else {
            saved_resolved(selected.unwrap(), saved)?
        }
    } else {
        ResolvedConnection {
            name: None,
            endpoint: endpoint(target.context(
                "no deployment connection; use `lightspeed connect dev` or `lightspeed connect add`",
            )?)?,
            secret: None,
            universe: None,
            caller: None,
            ..Default::default()
        }
    };
    if let Some(secret) = explicit_secret {
        connection.secret = Some(secret);
        connection.credential_kind = CredentialKind::RuntimeToken;
    }
    let env_universe = if name.is_none() {
        std::env::var("LIGHTSPEED_UNIVERSE").ok()
    } else {
        None
    };
    if let Some(universe) = universe
        .or(env_universe.as_deref())
        .filter(|s| !s.is_empty())
    {
        connection.universe = Some(universe.into());
    }
    verify(connection).await
}

#[derive(Debug, Args)]
pub struct ConnectArgs {
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: ConnectCommand,
}
#[derive(Debug, Subcommand)]
enum ConnectCommand {
    /// Sign in to a Platform deployment through your browser.
    Login(crate::device_login::LoginArgs),
    /// Revoke this CLI session and remove its local credential.
    Logout(crate::device_login::LogoutArgs),
    /// Verify and save a connection without selecting it.
    Add {
        name: String,
        #[arg(long)]
        url: String,
        #[arg(long, conflicts_with_all=["token_file", "token_env", "token_stdin"])]
        single: bool,
        #[arg(long, conflicts_with_all=["token_env", "token_stdin"])]
        token_file: Option<PathBuf>,
        #[arg(long, conflicts_with = "token_stdin")]
        token_env: Option<String>,
        #[arg(long)]
        token_stdin: bool,
        /// Permit credentials over plain HTTP on a trusted development network.
        #[arg(long)]
        allow_insecure_http: bool,
    },
    /// Verify and select a saved connection.
    Use { name: String },
    /// List saved connections without exposing credentials.
    List,
    /// Inspect the effective connection and current server authority.
    Status,
    /// Forget a local connection; does not revoke the remote credential.
    Remove { name: String },
    /// Import and select this checkout's development connection.
    Dev,
}

fn prompt_secret() -> Result<String> {
    if !std::io::stdin().is_terminal() {
        bail!("use --token-stdin, --token-file or --token-env for noninteractive access");
    }
    eprint!("Lightspeed runtime token or Platform API key: ");
    std::io::stderr().flush()?;
    crossterm::terminal::enable_raw_mode()?;
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
            eprintln!();
        }
    }
    let _restore = Restore;
    let mut secret = String::new();
    loop {
        use crossterm::event::{Event, KeyCode, KeyModifiers};
        if let Event::Key(key) = crossterm::event::read()? {
            match key.code {
                KeyCode::Enter => return nonempty_secret(secret),
                KeyCode::Esc => bail!("cancelled"),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    bail!("cancelled")
                }
                KeyCode::Char(c) => secret.push(c),
                KeyCode::Backspace => {
                    secret.pop();
                }
                _ => {}
            }
        }
    }
}

pub async fn handle(
    args: ConnectArgs,
    selected: Option<&str>,
    universe: Option<&str>,
    api_url: Option<&str>,
    account: Option<&str>,
) -> Result<()> {
    if let ConnectCommand::Login(args) = args.command {
        return crate::device_login::login(args, selected, api_url).await;
    }
    if let ConnectCommand::Logout(args) = args.command {
        return crate::device_login::logout(args, selected).await;
    }
    let dir = config_dir()?;
    let mut config = load_at(&dir)?;
    let json = args.json;
    match args.command {
        ConnectCommand::Login(_) | ConnectCommand::Logout(_) => unreachable!(),
        ConnectCommand::Add {
            name,
            url,
            single,
            token_file,
            token_env,
            token_stdin,
            allow_insecure_http,
        } => {
            if name.trim().is_empty() || config.connections.contains_key(&name) {
                bail!("connection name is empty or already exists; remove it before replacing");
            }
            let secret = if single {
                None
            } else if let Some(file) = &token_file {
                Some(read_secret(file)?)
            } else if let Some(env) = token_env {
                Some(nonempty_secret(
                    std::env::var(&env).with_context(|| format!("{env} is unset"))?,
                )?)
            } else if token_stdin {
                let mut input = String::new();
                std::io::stdin().read_to_string(&mut input)?;
                Some(nonempty_secret(input)?)
            } else {
                Some(prompt_secret()?)
            };
            let verified = verify(ResolvedConnection {
                name: Some(name.clone()),
                endpoint: endpoint_with_policy(&url, allow_insecure_http)?,
                allow_insecure_http,
                secret,
                universe: universe.map(str::to_owned),
                caller: None,
                ..Default::default()
            })
            .await?;
            let universe = match &verified.universe {
                Some(selected) => Some(resolve_universe(&verified, selected).await?),
                None => None,
            };
            let mut file = token_file.map(std::fs::canonicalize).transpose()?;
            let owned = file.is_none() && verified.secret.is_some();
            if owned {
                let path = dir.join("credentials").join(Uuid::new_v4().to_string());
                private_write(&path, verified.secret.as_ref().unwrap().as_bytes())?;
                file = Some(path);
            }
            config.connections.insert(
                name.clone(),
                SavedConnection {
                    endpoint: verified.endpoint,
                    single,
                    credential_kind: verified.credential_kind,
                    server_kind: verified
                        .server_kind
                        .context("verified connection has no deployment kind")?,
                    account_id: verified.account_id,
                    allow_insecure_http,
                    credential_file: file,
                    owned_credential: owned,
                    universe,
                },
            );
            save_at(&dir, &config)?;
            if json {
                crate::output::show(
                    true,
                    &serde_json::json!({"connection":name,"selected":false}),
                )?;
            } else {
                println!("Saved {name}. Select it with `lightspeed connect use {name}`.");
            }
        }
        ConnectCommand::Use { name } => {
            let saved = config
                .connections
                .get(&name)
                .context("unknown connection")?;
            let verified = verify(saved_resolved(&name, saved)?).await?;
            let saved = config
                .connections
                .get_mut(&name)
                .expect("selected connection exists");
            saved.credential_kind = verified.credential_kind;
            saved.server_kind = verified
                .server_kind
                .context("verified connection has no deployment kind")?;
            config.selected = Some(name.clone());
            save_at(&dir, &config)?;
            if json {
                crate::output::show(
                    true,
                    &serde_json::json!({"connection":name,"selected":true}),
                )?;
            } else {
                println!("Selected {name}");
            }
        }
        ConnectCommand::List => {
            let rows: Vec<_> = config.connections.iter().map(|(name, c)| serde_json::json!({"name": name, "endpoint": c.endpoint, "selected": config.selected.as_ref() == Some(name), "universe": c.universe, "accountId":c.account_id, "single": c.single, "serverKind":c.server_kind,"authentication":c.credential_kind})).collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                if rows.is_empty() {
                    println!("No saved connections. Use connect add or connect dev.");
                } else {
                    println!("  NAME  DEPLOYMENT");
                }
                for row in rows {
                    println!(
                        "{} {}  {}",
                        if row["selected"] == true { "*" } else { " " },
                        row["name"].as_str().unwrap(),
                        row["endpoint"].as_str().unwrap()
                    );
                }
            }
        }
        ConnectCommand::Status => {
            let mut c = resolve(selected, api_url, universe).await?;
            apply_account_override(&mut c, account, universe)?;
            let caller = c.caller.as_ref().unwrap();
            let status = universe_status(&c).await?;
            let value = serde_json::json!({"connection": c.name, "endpoint": c.endpoint, "selectedUniverse": c.universe, "activeUniverse": status, "caller": caller,"serverKind":c.server_kind,"principal":c.principal,"authentication":c.credential_kind,"accountId":c.account_id});
            if json {
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                println!(
                    "Connection: {}\n  Deployment: {}\n  Server: {:?}\n  Authentication: {:?}\n  Account: {}",
                    c.name.as_deref().unwrap_or("environment"),
                    c.endpoint,
                    c.server_kind.unwrap(),
                    c.credential_kind,
                    c.account_id.as_deref().unwrap_or("none selected")
                );
                if let Some(principal) = &c.principal {
                    println!(
                        "  Principal: {} ({}, {})",
                        principal.name, principal.kind, principal.id
                    );
                }
                if let Some(name) = &caller.token_name {
                    println!("  Runtime token: {name}");
                }
                println!();
                status.print("Selected universe");
            }
        }
        ConnectCommand::Remove { name } => {
            let removed = config
                .connections
                .remove(&name)
                .context("unknown connection")?;
            if config.selected.as_deref() == Some(&name) {
                config.selected = None;
            }
            save_at(&dir, &config)?;
            if removed.owned_credential
                && let Some(file) = removed.credential_file
                && file.parent() == Some(dir.join("credentials").as_path())
                && file.exists()
            {
                // Only ever unlink this CLI's owned credential directory.
                std::fs::remove_file(file)?;
            }
            if json {
                crate::output::show(
                    true,
                    &serde_json::json!({"removed":name,"keyRevoked":false}),
                )?;
            } else {
                println!("Forgot {name}; remote credential was not revoked.");
            }
        }
        ConnectCommand::Dev => {
            let cwd = std::env::current_dir()?.canonicalize()?;
            let path = cwd
                .ancestors()
                .map(|p| p.join(".lightspeed/cli/connection.json"))
                .find(|p| p.is_file())
                .context("no development connection; start ./dev.sh or ./dev.sh runtime first")?;
            let mut saved: SavedConnection = serde_json::from_slice(&std::fs::read(&path)?)?;
            saved.owned_credential = false;
            let name = format!(
                "dev-{}",
                Uuid::new_v5(&Uuid::NAMESPACE_URL, path.to_string_lossy().as_bytes())
            );
            if let Some(old) = config.connections.get(&name)
                && old.endpoint == saved.endpoint
                && old.single == saved.single
            {
                saved.universe = old.universe.clone();
            }
            let verified = verify(saved_resolved(&name, &saved)?).await?;
            saved.universe = verified.universe;
            saved.credential_kind = verified.credential_kind;
            saved.server_kind = verified
                .server_kind
                .context("verified connection has no deployment kind")?;
            config.connections.insert(name.clone(), saved);
            config.selected = Some(name.clone());
            save_at(&dir, &config)?;
            if json {
                crate::output::show(
                    true,
                    &serde_json::json!({"connection":name,"selected":true}),
                )?;
            } else {
                println!("Selected {name}");
            }
        }
    }
    Ok(())
}

pub fn select_universe(connection: &ResolvedConnection, id: String) -> Result<()> {
    let name = connection.name.as_ref().context("save and select a connection before using `universe use`; use --universe for an environment-only connection")?;
    let dir = config_dir()?;
    let mut config = load_at(&dir)?;
    config
        .connections
        .get_mut(name)
        .context("connection was removed")?
        .universe = Some(id.clone());
    save_at(&dir, &config)?;
    Ok(())
}

/// An invocation account override never persists or reuses another account's universe.
pub fn apply_account_override(
    connection: &mut ResolvedConnection,
    account: Option<&str>,
    explicit_universe: Option<&str>,
) -> Result<()> {
    let Some(account) = account else {
        return Ok(());
    };
    if !connection.is_platform() {
        bail!("--account requires a Platform deployment");
    }
    if account.trim().is_empty() {
        bail!("account must not be empty");
    }
    if connection.account_id.as_deref() != Some(account) {
        connection.universe = explicit_universe.map(str::to_owned);
        connection.account_id = Some(account.to_owned());
    }
    Ok(())
}

pub fn save_account(connection: &ResolvedConnection, account_id: &str) -> Result<()> {
    if !connection.is_platform() {
        bail!("accounts require a Platform deployment");
    }
    let name = connection.name.as_ref().context(
        "save a connection before using `platform account use`; use --account for this invocation",
    )?;
    let dir = config_dir()?;
    let mut config = load_at(&dir)?;
    let saved = config
        .connections
        .get_mut(name)
        .context("connection was removed")?;
    if saved.endpoint != connection.endpoint {
        bail!("saved connection endpoint changed");
    }
    if saved.account_id.as_deref() != Some(account_id) {
        saved.universe = None;
        saved.account_id = Some(account_id.to_owned());
    }
    save_at(&dir, &config)
}

/// Delete only a secret owned by this CLI, never an imported credential file.
pub(crate) fn remove_owned_secret(dir: &Path, saved: &SavedConnection) -> Result<()> {
    if saved.owned_credential
        && let Some(file) = &saved.credential_file
        && file.parent() == Some(dir.join("credentials").as_path())
    {
        match std::fs::remove_file(file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Retry discovery only. The deadline also bounds an in-flight HTTP request.
pub async fn resolve_wait(
    name: Option<&str>,
    url: Option<&str>,
    universe: Option<&str>,
    seconds: Option<u64>,
) -> Result<ResolvedConnection> {
    let Some(seconds) = seconds else {
        return resolve(name, url, universe).await;
    };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(seconds);
    loop {
        if tokio::time::Instant::now() >= deadline {
            bail!("Deployment did not become available within {seconds} seconds");
        }
        match tokio::time::timeout_at(deadline, resolve(name, url, universe)).await {
            Ok(Ok(connection)) => return Ok(connection),
            Ok(Err(error)) if error.is::<crate::api_client::StartupUnavailable>() => {}
            Ok(Err(error)) => return Err(error),
            Err(_) => bail!("Deployment did not become available within {seconds} seconds"),
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            bail!("Deployment did not become available within {seconds} seconds");
        }
        tokio::time::sleep_until(std::cmp::min(
            deadline,
            now + std::time::Duration::from_secs(1),
        ))
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_files_are_private_and_metadata_has_no_secret() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("credentials/key");
        private_write(&file, b"lsr_private_fixture").unwrap();
        assert_eq!(read_secret(&file).unwrap(), "lsr_private_fixture");
        let config = Connections {
            selected: Some("test".into()),
            connections: BTreeMap::from([(
                "test".into(),
                SavedConnection {
                    endpoint: "http://localhost/api/v1".into(),
                    single: false,
                    credential_kind: CredentialKind::RuntimeToken,
                    server_kind: ServerKind::Runtime,
                    credential_file: Some(file),
                    owned_credential: true,
                    account_id: None,
                    allow_insecure_http: false,
                    universe: None,
                },
            )]),
        };
        save_at(dir.path(), &config).unwrap();
        assert!(
            !std::fs::read_to_string(dir.path().join("connections.json"))
                .unwrap()
                .contains("lsr_private_fixture")
        );
        assert_eq!(
            load_at(dir.path()).unwrap().selected.as_deref(),
            Some("test")
        );
    }
    #[test]
    fn saved_connections_require_explicit_matching_credential_metadata() {
        let valid = serde_json::json!({
            "endpoint": "http://localhost/api/v1",
            "single": true,
            "credentialKind": "none",
            "serverKind": "runtime"
        });
        let saved: SavedConnection = serde_json::from_value(valid.clone()).unwrap();
        assert!(saved_resolved("local", &saved).is_ok());
        for field in ["credentialKind", "serverKind"] {
            let mut incomplete = valid.clone();
            incomplete.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<SavedConnection>(incomplete).is_err());
        }
        for (field, value) in [
            ("credentialKind", serde_json::json!("runtimeToken")),
            ("serverKind", serde_json::json!("platform")),
            ("accountId", serde_json::json!("account")),
            ("credentialFile", serde_json::json!("relative-token")),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            let saved: SavedConnection = serde_json::from_value(invalid).unwrap();
            assert!(saved_resolved("local", &saved).is_err(), "{field}");
        }
    }

    #[test]
    fn endpoint_validation_prevents_credential_urls_and_remote_plaintext() {
        for url in [
            "http://remote.example/api/v1",
            "https://key@example.org/api/v1",
            "https://example.org/api/v1?secret=x",
        ] {
            assert!(endpoint(url).is_err());
        }
        for url in [
            "https://remote.example/api/v1",
            "http://127.0.0.1:18080/api/v1",
            "http://[::1]/api/v1",
        ] {
            assert!(endpoint(url).is_ok(), "{url}");
        }
    }

    #[test]
    fn direct_tokens_select_universes_only_for_universe_methods() {
        let mut connection = ResolvedConnection {
            name: None,
            endpoint: "http://localhost/api/v1".into(),
            secret: Some("lsr_fixture".into()),
            universe: Some("team".into()),
            caller: Some(CallerAccess {
                single: false,
                token_name: Some("operator".into()),
            }),
            ..Default::default()
        };
        let headers = connection.headers("session/list").unwrap();
        assert!(!headers.contains_key("x-lightspeed-universe"));
        assert_eq!(headers["authorization"], "Bearer lsr_fixture");
        for method in ["initialize", "deployment/universes/list"] {
            assert!(
                !connection
                    .headers(method)
                    .unwrap()
                    .contains_key("x-lightspeed-universe")
            );
        }
        connection.universe = None;
        assert!(connection.headers("session/list").is_err());
        connection.secret = None;
        connection.caller = Some(CallerAccess {
            single: true,
            token_name: None,
        });
        assert!(connection.headers("session/list").is_err());
        connection.universe = Some("local".into());
        assert!(connection.headers("session/list").unwrap().is_empty());
    }

    #[test]
    fn deployment_origins_and_base_paths_normalize_without_losing_the_auth_base() {
        for (input, expected) in [
            ("https://example.test", "https://example.test/api/v1"),
            (
                "https://example.test/team/",
                "https://example.test/team/api/v1",
            ),
            (
                "https://example.test/team/api/v1/",
                "https://example.test/team/api/v1",
            ),
        ] {
            assert_eq!(endpoint(input).unwrap(), expected);
        }
        assert_eq!(
            auth_url("https://example.test/team/api/v1", "device/code")
                .unwrap()
                .as_str(),
            "https://example.test/team/api/auth/device/code"
        );
        assert!(endpoint("http://remote.example").is_err());
        assert_eq!(
            endpoint_with_policy("http://remote.example", true).unwrap(),
            "http://remote.example/api/v1"
        );
    }

    #[test]
    fn an_account_override_clears_only_the_previous_accounts_universe_selection() {
        let mut connection = ResolvedConnection {
            server_kind: Some(ServerKind::Platform),
            account_id: Some("first".into()),
            universe: Some("old-universe".into()),
            ..Default::default()
        };
        apply_account_override(&mut connection, Some("first"), None).unwrap();
        assert_eq!(connection.universe.as_deref(), Some("old-universe"));
        apply_account_override(&mut connection, Some("second"), None).unwrap();
        assert_eq!(connection.account_id.as_deref(), Some("second"));
        assert!(connection.universe.is_none());
        apply_account_override(&mut connection, Some("third"), Some("explicit-universe")).unwrap();
        assert_eq!(connection.universe.as_deref(), Some("explicit-universe"));
        connection.server_kind = Some(ServerKind::Runtime);
        assert!(apply_account_override(&mut connection, Some("team"), None).is_err());
    }

    #[test]
    fn session_rotation_updates_only_owned_files_and_is_used_by_subsequent_requests() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("credentials/session");
        private_write(&file, b"old-session").unwrap();
        let mut connection = ResolvedConnection {
            secret: Some("old-session".into()),
            credential_kind: CredentialKind::PlatformSession,
            credential_file: Some(file.clone()),
            owned_credential: true,
            ..Default::default()
        };
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("set-auth-token", "new-session".parse().unwrap());
        connection
            .accept_session_rotation_at(&headers, dir.path())
            .unwrap();
        assert_eq!(
            connection.auth_headers().unwrap()["authorization"],
            "Bearer new-session"
        );
        assert_eq!(
            connection.redact_error("old-session / new-session".into()),
            "[credential redacted] / [credential redacted]"
        );
        headers.insert("set-auth-token", "third-session".parse().unwrap());
        connection.owned_credential = false;
        connection
            .accept_session_rotation_at(&headers, dir.path())
            .unwrap();
        assert_eq!(read_secret(&file).unwrap(), "new-session");
        connection.owned_credential = true;
        connection.credential_kind = CredentialKind::PlatformApiKey;
        connection
            .accept_session_rotation_at(&headers, dir.path())
            .unwrap();
        assert_eq!(read_secret(&file).unwrap(), "new-session");
        connection.credential_kind = CredentialKind::PlatformSession;
        assert!(
            connection
                .accept_session_rotation_at(&headers, &dir.path().join("different-root"))
                .is_err()
        );
        assert_eq!(read_secret(&file).unwrap(), "new-session");
    }
}
