//! Local runtime connections. Server authority is always discovered, never inferred from a key.
use crate::api_client::HttpAgentApi;
use anyhow::{Context, Result, bail};
use api::{AccessScope, CallerAccess};
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

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedConnection {
    pub endpoint: String,
    pub single: bool,
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
#[derive(Clone)]
pub struct ResolvedConnection {
    pub name: Option<String>,
    pub endpoint: String,
    pub secret: Option<String>,
    pub universe: Option<String>,
    pub caller: Option<CallerAccess>,
}

impl ResolvedConnection {
    /// The universe requests actually address, including server-bound keys
    /// and single mode even when no local selection was saved.
    pub fn active_universe_id(&self) -> Option<String> {
        match self.caller.as_ref().map(|caller| caller.scope) {
            Some(AccessScope::Universe { universe_id }) => Some(universe_id.to_string()),
            _ => self.universe.clone(),
        }
    }

    pub fn headers(&self, method: &str) -> Result<reqwest::header::HeaderMap> {
        use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
        let mut headers = HeaderMap::new();
        if let Some(secret) = &self.secret {
            let mut value = HeaderValue::from_str(&format!("Bearer {secret}"))
                .context("invalid API key header")?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
        let scope = api::method_access(method)
            .context("unknown runtime method")?
            .scope();
        if matches!(
            scope,
            api::MethodScope::Connection | api::MethodScope::Deployment
        ) {
            return Ok(headers);
        }
        if let Some(caller) = &self.caller {
            if caller.single || matches!(caller.scope, AccessScope::Universe { .. }) {
                return Ok(headers);
            }
            if self.universe.is_none() {
                bail!(
                    "select a universe with `lightspeed universe use <uuid-or-slug>` or --universe"
                );
            }
        }
        if let Some(universe) = &self.universe {
            headers.insert("x-lightspeed-universe", HeaderValue::from_str(universe)?);
        }
        Ok(headers)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UniverseStatus {
    pub universe_id: Option<String>,
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug_unavailable_reason: Option<String>,
}

impl UniverseStatus {
    pub fn print(&self, heading: &str) {
        println!("{heading}:");
        match &self.universe_id {
            None => println!("  None selected. Use `lightspeed universe use <slug-or-uuid>`."),
            Some(id) => {
                match &self.slug_unavailable_reason {
                    Some(reason) => println!("  Slug: unavailable ({reason})"),
                    None => println!("  Slug: {}", self.slug.as_deref().unwrap_or("(no slug)")),
                }
                println!("  UUID: {id}");
            }
        }
    }
}

/// Status must remain useful for restricted keys and stale selections. Fetch
/// the slug only when permitted, and distinguish unnamed from unavailable.
pub async fn universe_status(connection: &ResolvedConnection) -> Result<UniverseStatus> {
    let caller = connection
        .caller
        .as_ref()
        .context("connection has not been verified")?;
    let mut status = UniverseStatus {
        universe_id: connection.active_universe_id(),
        slug: None,
        slug_unavailable_reason: None,
    };
    let Some(id) = &status.universe_id else {
        return Ok(status);
    };
    if !caller.single
        && (caller.scope != AccessScope::Deployment
            || !caller
                .groups
                .contains(&api::MethodGroup::DeploymentUniverses))
    {
        status.slug_unavailable_reason =
            Some("requires a deployment key with deployment/universes permission".into());
        return Ok(status);
    }
    let response = HttpAgentApi::with_connection(connection.clone())
        .request::<_, api::DeploymentUniverseReadResponse>(
            api::METHOD_DEPLOYMENT_UNIVERSES_READ,
            api::DeploymentUniverseReadParams {
                universe_id: id.clone(),
            },
        )
        .await;
    match response {
        Ok(response) => status.slug = response.result.universe.slug,
        Err(error) => status.slug_unavailable_reason = Some(error.to_string()),
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
        bail!("API key is empty");
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
fn save_at(dir: &Path, config: &Connections) -> Result<()> {
    private_write(
        &dir.join("connections.json"),
        &serde_json::to_vec_pretty(config)?,
    )
}

fn endpoint(url: &str) -> Result<String> {
    let parsed = reqwest::Url::parse(url).context("invalid runtime URL")?;
    let local = parsed.host_str().is_some_and(|h| {
        h == "localhost"
            || h == "[::1]"
            || h == "::1"
            || h.parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if !(parsed.scheme() == "https" || (parsed.scheme() == "http" && local)) {
        bail!("runtime URL requires HTTPS, except on loopback");
    }
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!("runtime URL must not contain credentials, query parameters or a fragment");
    }
    Ok(parsed.to_string())
}

fn saved_resolved(name: &str, saved: &SavedConnection) -> Result<ResolvedConnection> {
    if saved.single && saved.credential_file.is_some() {
        bail!("single-mode connection must not contain an API key");
    }
    if !saved.single && saved.credential_file.is_none() {
        bail!("authenticated connection has no credential");
    }
    Ok(ResolvedConnection {
        name: Some(name.to_owned()),
        endpoint: endpoint(&saved.endpoint)?,
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
    let response: api::InitializeResponse = client
        .request(api::METHOD_INITIALIZE, api::InitializeParams::default())
        .await?
        .result;
    if response.caller.single != connection.secret.is_none() {
        bail!("runtime authentication does not match the configured connection");
    }
    connection.caller = Some(response.caller);
    if let Some(selected) = connection.universe.clone() {
        connection.universe = Some(resolve_universe(&connection, &selected).await?);
    }
    Ok(connection)
}

pub async fn resolve_universe(connection: &ResolvedConnection, selected: &str) -> Result<String> {
    let caller = connection
        .caller
        .as_ref()
        .context("connection has not been verified")?;
    if let AccessScope::Universe { universe_id } = caller.scope {
        if selected != universe_id.to_string() {
            bail!(
                "this connection is bound to universe {universe_id}; switching universes requires a deployment key"
            );
        }
        return Ok(universe_id.to_string());
    }
    if let Ok(id) = Uuid::parse_str(selected) {
        return Ok(id.to_string());
    }
    let client = HttpAgentApi::with_connection(connection.clone());
    let response: api::DeploymentUniverseListResponse = client
        .request(
            api::METHOD_DEPLOYMENT_UNIVERSES_LIST,
            api::DeploymentUniverseListParams {},
        )
        .await?
        .result;
    let matches: Vec<_> = response
        .universes
        .iter()
        .filter(|u| u.slug.as_deref() == Some(selected))
        .collect();
    match matches.as_slice() {
        [universe] => Ok(universe.universe_id.clone()),
        [] => bail!(
            "unknown universe {selected}; use an explicit UUID if this key cannot list universes"
        ),
        _ => bail!("ambiguous universe slug {selected}; use its UUID"),
    }
}

/// Explicit connection selection is an atomic choice of endpoint and credential.
/// Environment overrides apply when no named connection is explicitly selected.
pub async fn resolve(
    name: Option<&str>,
    url: Option<&str>,
    universe: Option<&str>,
) -> Result<ResolvedConnection> {
    let config = load_at(&config_dir()?)?;
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
    let explicit_secret = if name.is_none() || url.is_some() {
        std::env::var("LIGHTSPEED_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(nonempty_secret)
            .transpose()?
    } else {
        None
    };
    let mut connection = if let Some(saved) = saved {
        let saved_endpoint = endpoint(&saved.endpoint)?;
        let effective_endpoint = target
            .map(endpoint)
            .transpose()?
            .unwrap_or(saved_endpoint.clone());
        if effective_endpoint != saved_endpoint {
            // Never read or forward a saved credential to an overridden endpoint.
            ResolvedConnection {
                name: None,
                endpoint: effective_endpoint,
                secret: None,
                universe: None,
                caller: None,
            }
        } else if explicit_secret.is_some() {
            ResolvedConnection {
                name: selected.map(str::to_owned),
                endpoint: saved_endpoint,
                secret: None,
                universe: saved.universe.clone(),
                caller: None,
            }
        } else {
            saved_resolved(selected.unwrap(), saved)?
        }
    } else {
        ResolvedConnection {
            name: None,
            endpoint: endpoint(target.context(
                "no runtime connection; use `lightspeed connect dev` or `lightspeed connect add`",
            )?)?,
            secret: None,
            universe: None,
            caller: None,
        }
    };
    if let Some(secret) = explicit_secret {
        connection.secret = Some(secret);
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
    /// Verify and save a connection without selecting it.
    Add {
        name: String,
        #[arg(long)]
        url: String,
        #[arg(long, conflicts_with_all=["api_key_file", "api_key_env", "api_key_stdin"])]
        single: bool,
        #[arg(long, conflicts_with_all=["api_key_env", "api_key_stdin"])]
        api_key_file: Option<PathBuf>,
        #[arg(long, conflicts_with = "api_key_stdin")]
        api_key_env: Option<String>,
        #[arg(long)]
        api_key_stdin: bool,
    },
    /// Verify and select a saved connection.
    Use { name: String },
    /// List saved connections without exposing credentials.
    List,
    /// Inspect the effective connection and current server authority.
    Status,
    /// Forget a local connection; does not revoke the runtime key.
    Remove { name: String },
    /// Import and select this checkout's development connection.
    Dev,
}

fn prompt_secret() -> Result<String> {
    if !std::io::stdin().is_terminal() {
        bail!("use --api-key-stdin, --api-key-file or --api-key-env for noninteractive access");
    }
    eprint!("Lightspeed API key: ");
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
) -> Result<()> {
    let dir = config_dir()?;
    let mut config = load_at(&dir)?;
    let json = args.json;
    match args.command {
        ConnectCommand::Add {
            name,
            url,
            single,
            api_key_file,
            api_key_env,
            api_key_stdin,
        } => {
            if name.trim().is_empty() || config.connections.contains_key(&name) {
                bail!("connection name is empty or already exists; remove it before replacing");
            }
            let secret = if single {
                None
            } else if let Some(file) = &api_key_file {
                Some(read_secret(file)?)
            } else if let Some(env) = api_key_env {
                Some(nonempty_secret(
                    std::env::var(&env).with_context(|| format!("{env} is unset"))?,
                )?)
            } else if api_key_stdin {
                let mut input = String::new();
                std::io::stdin().read_to_string(&mut input)?;
                Some(nonempty_secret(input)?)
            } else {
                Some(prompt_secret()?)
            };
            let verified = verify(ResolvedConnection {
                name: Some(name.clone()),
                endpoint: endpoint(&url)?,
                secret,
                universe: universe.map(str::to_owned),
                caller: None,
            })
            .await?;
            let mut file = api_key_file.map(std::fs::canonicalize).transpose()?;
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
                    credential_file: file,
                    owned_credential: owned,
                    universe: verified.universe,
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
            verify(saved_resolved(&name, saved)?).await?;
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
            let rows: Vec<_> = config.connections.iter().map(|(name, c)| serde_json::json!({"name": name, "endpoint": c.endpoint, "selected": config.selected.as_ref() == Some(name), "universe": c.universe, "single": c.single})).collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                if rows.is_empty() {
                    println!("No saved connections. Use connect add or connect dev.");
                } else {
                    println!("  NAME  RUNTIME");
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
            let c = resolve(selected, api_url, universe).await?;
            let caller = c.caller.as_ref().unwrap();
            let status = universe_status(&c).await?;
            let value = serde_json::json!({"connection": c.name, "endpoint": c.endpoint, "selectedUniverse": c.universe, "activeUniverse": status, "caller": caller});
            if json {
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                println!(
                    "Connection: {}\n  Runtime: {}\n  Mode: {}\n\nAPI key:\n  Prefix: {}\n  Scope: {}\n  Groups: {}\n",
                    c.name.as_deref().unwrap_or("environment"),
                    c.endpoint,
                    if caller.single {
                        "single"
                    } else {
                        "authenticated"
                    },
                    caller.key_prefix.as_deref().unwrap_or("none"),
                    match caller.scope {
                        AccessScope::Deployment => "deployment".to_owned(),
                        AccessScope::Universe { universe_id } =>
                            format!("universe ({universe_id})"),
                    },
                    caller
                        .groups
                        .iter()
                        .map(|g| g.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
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
                println!("Forgot {name}; runtime key was not revoked.");
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

#[cfg(test)]
mod tests {
    use super::*;
    fn connection(scope: AccessScope, single: bool) -> ResolvedConnection {
        ResolvedConnection {
            name: None,
            endpoint: "http://localhost/rpc".into(),
            secret: (!single).then(|| "lsk_fixture".into()),
            universe: Some(Uuid::nil().to_string()),
            caller: Some(CallerAccess {
                scope,
                single,
                key_prefix: None,
                groups: vec![],
            }),
        }
    }
    #[test]
    fn headers_follow_method_scope_and_credential_scope() {
        let deployment = connection(AccessScope::Deployment, false);
        for method in [
            api::METHOD_INITIALIZE,
            api::METHOD_DEPLOYMENT_UNIVERSES_LIST,
        ] {
            let headers = deployment.headers(method).unwrap();
            assert!(headers.contains_key("authorization"));
            assert!(!headers.contains_key("x-lightspeed-universe"));
        }
        assert!(
            deployment
                .headers(api::METHOD_SESSION_LIST)
                .unwrap()
                .contains_key("x-lightspeed-universe")
        );
        for single in [true, false] {
            let bound = connection(
                AccessScope::Universe {
                    universe_id: Uuid::nil(),
                },
                single,
            );
            let headers = bound.headers(api::METHOD_SESSION_LIST).unwrap();
            assert!(!headers.contains_key("x-lightspeed-universe"));
            assert_eq!(headers.contains_key("authorization"), !single);
        }
        let mut missing = deployment;
        missing.universe = None;
        assert!(missing.headers(api::METHOD_SESSION_LIST).is_err());
        assert!(
            missing
                .headers(api::METHOD_DEPLOYMENT_UNIVERSES_LIST)
                .is_ok()
        );
    }
    #[test]
    fn saved_files_are_private_and_metadata_has_no_secret() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("credentials/key");
        private_write(&file, b"lsk_private_fixture").unwrap();
        assert_eq!(read_secret(&file).unwrap(), "lsk_private_fixture");
        let config = Connections {
            selected: Some("test".into()),
            connections: BTreeMap::from([(
                "test".into(),
                SavedConnection {
                    endpoint: "http://localhost/rpc".into(),
                    single: false,
                    credential_file: Some(file),
                    owned_credential: true,
                    universe: None,
                },
            )]),
        };
        save_at(dir.path(), &config).unwrap();
        assert!(
            !std::fs::read_to_string(dir.path().join("connections.json"))
                .unwrap()
                .contains("lsk_private_fixture")
        );
        assert_eq!(
            load_at(dir.path()).unwrap().selected.as_deref(),
            Some("test")
        );
    }
    #[test]
    fn endpoint_validation_prevents_credential_urls_and_remote_plaintext() {
        for url in [
            "http://remote.example/rpc",
            "https://key@example.org/rpc",
            "https://example.org/rpc?secret=x",
        ] {
            assert!(endpoint(url).is_err());
        }
        for url in [
            "https://remote.example/rpc",
            "http://127.0.0.1:18080/rpc",
            "http://[::1]/rpc",
        ] {
            assert!(endpoint(url).is_ok(), "{url}");
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn bound_connections_cannot_switch_universes() {
        let c = connection(
            AccessScope::Universe {
                universe_id: Uuid::nil(),
            },
            false,
        );
        assert_eq!(
            resolve_universe(&c, &Uuid::nil().to_string())
                .await
                .unwrap(),
            Uuid::nil().to_string()
        );
        assert!(
            resolve_universe(&c, &Uuid::new_v4().to_string())
                .await
                .is_err()
        );
    }
}
