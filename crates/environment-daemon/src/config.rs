use std::{
    collections::BTreeMap,
    ffi::OsString,
    fmt,
    net::SocketAddr,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use environment_protocol::{
    bytes::SecretString,
    metadata::{DisplayName, Metadata},
    routes::{CanonicalOrigin, canonical_origin, gateway_stream_url},
};

/// Listener address when the daemon is not registering outbound.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:19091";

/// Every daemon configuration variable shares this prefix; all of them are
/// removed from the environment of every child process and job.
pub const ENV_PREFIX: &str = "LIGHTSPEED_ENVD_";
/// Internal one-exec handoff for a registration key that configuration has
/// already read and scrubbed. Never documented as operator configuration.
pub(crate) const REEXEC_REGISTRATION_KEY_ENV: &str = "LIGHTSPEED_ENVD_REEXEC_REGISTRATION_KEY";
/// Set on the re-executed replacement after an automatic upgrade: the
/// gateway protocol version the upgrade targeted. A later challenge naming
/// that same version is refused instead of upgrading again.
pub(crate) const AUTO_UPGRADE_ATTEMPTED_ENV: &str = "LIGHTSPEED_ENVD_AUTO_UPGRADE_ATTEMPTED";
/// Name of the state directory below `$XDG_STATE_HOME` (or
/// `$HOME/.local/state`).
pub const STATE_DIR_NAME: &str = "lightspeed-envd";

#[derive(Parser, Debug)]
#[command(
    name = "lightspeed-envd",
    version = release_info::LONG_VERSION,
    about = "Lightspeed environment execution daemon"
)]
pub struct DaemonArgs {
    #[command(subcommand)]
    pub command: Option<DaemonCommand>,

    /// WebSocket listener reachable by Lightspeed or an environment provider:
    /// `host:port` (or `tcp://host:port`), or `unix:///absolute/path` for a
    /// host that reaches the daemon over a local bridge (such as a VM's vsock
    /// socket relay) rather than the network. Defaults to 127.0.0.1:19091
    /// unless a gateway URL makes the daemon dial out instead.
    #[arg(long, env = "LIGHTSPEED_ENVD_LISTEN")]
    pub listen: Option<ListenAddr>,

    /// Environment-gateway connect URL to dial outbound
    /// (`wss://host[/prefix]/environment-gateway/connect`). Stream sockets
    /// are dialed on the same origin and prefix.
    #[arg(long, env = "LIGHTSPEED_ENVD_GATEWAY_URL", global = true)]
    pub gateway_url: Option<String>,

    /// Discovery document used for manual or automatic upgrades. Defaults to
    /// `/.well-known/lightspeed-envd` on the configured gateway origin.
    #[arg(long, env = "LIGHTSPEED_ENVD_DISCOVERY_URL", global = true)]
    pub discovery_url: Option<String>,

    /// Accept a plain `ws://` gateway URL toward a non-loopback host, such
    /// as a cluster-internal service inside one trusted network. The
    /// registration key and every stream then cross that network in the
    /// clear.
    #[arg(
        long,
        env = "LIGHTSPEED_ENVD_ALLOW_INSECURE_GATEWAY",
        default_value_t = false
    )]
    pub allow_insecure_gateway: bool,

    /// What the daemon does once the gateway reports its environment
    /// closed: `exit` with status 4, or `park` (stop registering and
    /// listening, then wait for `SIGTERM` or `SIGINT` and exit 0) so a
    /// supervisor that restarts on every exit does not restart a spent
    /// identity.
    #[arg(
        long,
        env = "LIGHTSPEED_ENVD_ON_CLOSED",
        value_enum,
        default_value_t = OnClosed::Exit
    )]
    pub on_closed: OnClosed,

    /// Upgrade automatically when an outbound gateway challenges with a
    /// different environment protocol version.
    #[arg(long, env = "LIGHTSPEED_ENVD_AUTO_UPGRADE", default_value_t = false)]
    pub auto_upgrade: bool,

    /// Registration key admitting this daemon as a new environment. Read
    /// once, removed from the process environment, and sent only until the
    /// first admission.
    #[arg(long, env = "LIGHTSPEED_ENVD_REGISTRATION_KEY", hide_env_values = true)]
    pub registration_key: Option<String>,

    /// File holding the registration key; the safer form for sandboxes.
    #[arg(long, env = "LIGHTSPEED_ENVD_REGISTRATION_KEY_FILE")]
    pub registration_key_file: Option<PathBuf>,

    /// Display name recorded on the environment at first registration.
    #[arg(long, env = "LIGHTSPEED_ENVD_REGISTRATION_NAME")]
    pub registration_name: Option<String>,

    /// Bounded JSON object of string correlation metadata, recorded at
    /// first registration.
    #[arg(long, env = "LIGHTSPEED_ENVD_REGISTRATION_METADATA")]
    pub registration_metadata: Option<String>,

    /// Where to write the registration receipt JSON once admitted.
    #[arg(long, env = "LIGHTSPEED_ENVD_REGISTRATION_RECEIPT")]
    pub registration_receipt: Option<PathBuf>,

    /// PEM file with additional TLS trust anchors for the gateway.
    #[arg(long, env = "LIGHTSPEED_ENVD_CA_FILE", global = true)]
    pub ca_file: Option<PathBuf>,

    #[arg(long, env = "LIGHTSPEED_ENVD_CWD")]
    pub cwd: Option<PathBuf>,

    #[arg(long, env = "LIGHTSPEED_ENVD_FS_ROOT")]
    pub fs_root: Option<PathBuf>,

    /// Daemon key, job records and transfer journal. Defaults to
    /// `$XDG_STATE_HOME/lightspeed-envd`, else
    /// `$HOME/.local/state/lightspeed-envd`. The data plane never addresses
    /// it, wherever it lives.
    #[arg(long, env = "LIGHTSPEED_ENVD_STATE_DIR")]
    pub state_dir: Option<PathBuf>,

    #[arg(long, default_value_t = false)]
    pub read_only_fs: bool,

    /// Print this binary's build facts as one JSON object and exit: name,
    /// version, git sha, target, and the environment protocol version it
    /// speaks. For orchestrators checking a downloaded daemon against a
    /// deployment's discovery document.
    #[arg(long, default_value_t = false)]
    pub print_build: bool,
}

/// What a registered daemon does once its environment is closed. Either
/// way it never reconnects: a closed identity is spent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum OnClosed {
    /// End the process with status 4.
    #[default]
    Exit,
    /// Stop registering and listening, and wait for `SIGTERM` or `SIGINT`.
    Park,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Subcommand)]
pub enum DaemonCommand {
    /// Install the envd build published by the configured gateway.
    Upgrade,
}

/// Where the passive WebSocket listener binds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListenAddr {
    Tcp(SocketAddr),
    /// Absolute path of a Unix socket.
    #[cfg(unix)]
    Unix(PathBuf),
}

impl FromStr for ListenAddr {
    type Err = anyhow::Error;

    /// Accepts `host:port`, `tcp://host:port`, `unix:///path`, or `unix:/path`.
    fn from_str(value: &str) -> Result<Self> {
        if let Some(rest) = value.strip_prefix("unix:") {
            // Checked first: `/run/envd.sock` is not absolute on Windows, which
            // would otherwise report the wrong problem.
            if cfg!(not(unix)) {
                bail!("a Unix-socket listener needs a Unix host: {value}");
            }
            let path = PathBuf::from(rest.strip_prefix("//").unwrap_or(rest));
            if !path.is_absolute() {
                bail!("Unix-socket listener path must be absolute: {value}");
            }
            #[cfg(unix)]
            return Ok(Self::Unix(path));
        }
        let addr = value.strip_prefix("tcp://").unwrap_or(value);
        addr.parse()
            .map(Self::Tcp)
            .with_context(|| format!("listener must be host:port or unix:///path: {value}"))
    }
}

impl fmt::Display for ListenAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tcp(addr) => write!(f, "{addr}"),
            #[cfg(unix)]
            Self::Unix(path) => write!(f, "unix://{}", path.display()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DaemonConfig {
    /// Passive listener, when enabled.
    pub listen: Option<ListenAddr>,
    pub cwd: PathBuf,
    pub fs_root: PathBuf,
    /// Created `0700` when missing. Never addressable through the data
    /// plane.
    pub state_dir: PathBuf,
    pub read_only_fs: bool,
    /// Outbound registration, when a gateway URL is configured.
    pub registration: Option<RegistrationConfig>,
    /// Environment variable names removed from every child process and job.
    pub scrubbed_env: Vec<String>,
}

/// Outbound registration settings. Identity mode is not here on purpose:
/// it is registration-key policy, and the daemon behaves the same either way.
#[derive(Clone)]
pub struct RegistrationConfig {
    /// The gateway connect URL; it must end with `CONNECT_PATH`.
    pub gateway_url: String,
    /// Sent with `registration/register` until the first admission, then
    /// dropped (never handed to an upgraded replacement after that).
    pub registration_key: Option<SecretString>,
    /// Read by the gateway only at first admission.
    pub display_name: Option<DisplayName>,
    /// Read by the gateway only at first admission.
    pub metadata: Metadata,
    pub receipt_path: Option<PathBuf>,
    pub ca_file: Option<PathBuf>,
    /// Absolute discovery URL reported on mismatch and used by automatic
    /// upgrade.
    pub discovery_url: String,
    /// Accept a plain `ws://` gateway URL toward a non-loopback host.
    pub allow_insecure_gateway: bool,
    /// What the daemon does once its environment is closed.
    pub on_closed: OnClosed,
    /// Present only when mismatch-triggered replacement and re-exec is opted
    /// in. The argument vector is captured before the daemon starts serving.
    pub(crate) auto_upgrade: Option<RestartInvocation>,
    /// The gateway protocol an automatic upgrade already targeted in this
    /// process lineage.
    pub(crate) upgrade_attempted_for: Option<u32>,
}

#[derive(Clone)]
pub(crate) struct RestartInvocation {
    pub(crate) executable: PathBuf,
    pub(crate) arg0: OsString,
    pub(crate) args: Vec<OsString>,
}

impl std::fmt::Debug for RegistrationConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistrationConfig")
            .field("gateway_url", &self.gateway_url)
            .field(
                "registration_key",
                &self.registration_key.as_ref().map(|_| "<redacted>"),
            )
            .field("display_name", &self.display_name)
            .field("metadata", &self.metadata)
            .field("receipt_path", &self.receipt_path)
            .field("ca_file", &self.ca_file)
            .field("discovery_url", &self.discovery_url)
            .field("allow_insecure_gateway", &self.allow_insecure_gateway)
            .field("on_closed", &self.on_closed)
            .field("auto_upgrade", &self.auto_upgrade.is_some())
            .field("upgrade_attempted_for", &self.upgrade_attempted_for)
            .finish()
    }
}

/// The URLs one gateway connect URL determines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayUrls {
    /// The connect URL as configured.
    pub connect: String,
    /// What the registration signature binds.
    pub origin: CanonicalOrigin,
    /// Where every reverse-dialed stream goes.
    pub stream: String,
}

impl RegistrationConfig {
    /// Registration that never replaces the running binary: no key, name,
    /// metadata, receipt, or CA file until the caller sets them; `wss://`
    /// (or loopback `ws://`) only; exits once closed. The CLI
    /// attaches the restart invocation for automatic upgrade itself, so it
    /// stays crate-private.
    pub fn new(gateway_url: String, discovery_url: String) -> Self {
        Self {
            gateway_url,
            registration_key: None,
            display_name: None,
            metadata: Metadata::default(),
            receipt_path: None,
            ca_file: None,
            discovery_url,
            allow_insecure_gateway: false,
            on_closed: OnClosed::Exit,
            auto_upgrade: None,
            upgrade_attempted_for: None,
        }
    }

    /// The discovery URL must be HTTPS (or HTTP toward loopback) whenever
    /// it can download a build. With an insecure gateway and no automatic
    /// upgrade it is only named in mismatch reports, so the plain `http://`
    /// URL derived from a `ws://` gateway is accepted there.
    pub fn validate(&self) -> Result<GatewayUrls> {
        let urls = gateway_urls(&self.gateway_url, self.allow_insecure_gateway)?;
        if let Some(key) = &self.registration_key
            && (key.is_empty() || key.expose().chars().any(char::is_whitespace))
        {
            bail!("registration key must be a single non-empty token");
        }
        if !self.allow_insecure_gateway || self.auto_upgrade.is_some() {
            crate::upgrade::validate_discovery_url(&self.discovery_url).with_context(|| {
                if self.allow_insecure_gateway {
                    "automatic upgrade downloads over https: set LIGHTSPEED_ENVD_DISCOVERY_URL"
                } else {
                    "discovery URL"
                }
            })?;
        }
        Ok(urls)
    }
}

/// Validate a gateway connect URL and derive what it determines. Plain
/// `ws://` is accepted toward loopback, and toward any other host only when
/// `allow_insecure` opts in; otherwise the URL must be `wss://`, so neither
/// the registration key nor any stream crosses a network in the clear. The
/// path must end with `CONNECT_PATH` (a prefix before it is kept), with no
/// query or fragment.
pub fn gateway_urls(url: &str, allow_insecure: bool) -> Result<GatewayUrls> {
    let scheme = url.split_once("://").map(|(scheme, _)| scheme);
    match scheme {
        Some("wss") => {}
        Some("ws") if allow_insecure => {}
        Some("ws") => {
            let origin = canonical_origin(url)?;
            let host = origin
                .as_str()
                .trim_start_matches("ws://")
                .rsplit_once(':')
                .filter(|(_, port)| port.bytes().all(|byte| byte.is_ascii_digit()))
                .map_or(origin.as_str().trim_start_matches("ws://"), |(host, _)| {
                    host
                })
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_owned();
            let loopback = host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback());
            if !loopback {
                bail!(
                    "gateway URL must use wss:// except toward loopback \
                     (or set LIGHTSPEED_ENVD_ALLOW_INSECURE_GATEWAY): {url}"
                );
            }
        }
        _ => bail!("gateway URL must start with wss:// (or ws:// toward loopback): {url}"),
    }
    let origin = canonical_origin(url).with_context(|| format!("gateway URL {url}"))?;
    let stream = gateway_stream_url(url).with_context(|| format!("gateway URL {url}"))?;
    Ok(GatewayUrls {
        connect: url.to_owned(),
        origin,
        stream,
    })
}

/// Names of every daemon configuration variable present in the process
/// environment, captured before any child can inherit them.
pub fn scrubbed_env_names() -> Vec<String> {
    let mut names: Vec<String> = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| name.starts_with(ENV_PREFIX))
        .collect();
    names.sort();
    names
}

impl DaemonArgs {
    pub fn upgrade_request(&self) -> Result<crate::upgrade::UpgradeRequest> {
        let discovery_url = crate::upgrade::resolve_discovery_url(
            self.gateway_url.as_deref(),
            self.discovery_url.as_deref(),
        )?;
        Ok(crate::upgrade::UpgradeRequest {
            discovery_url,
            ca_file: self.ca_file.clone(),
            install_path: started_executable().context("find the started envd executable")?,
            target: release_info::TARGET.to_owned(),
            expected_protocol: None,
            other_than: None,
        })
    }

    /// Resolve the configuration. Must run before the async runtime starts
    /// any thread: it removes secret-bearing variables from the process
    /// environment.
    pub fn into_config(self) -> Result<DaemonConfig> {
        let cwd = canonical_dir(
            self.cwd
                .unwrap_or(std::env::current_dir().context("read current directory")?),
            "cwd",
        )?;
        let fs_root = canonical_dir(
            self.fs_root.unwrap_or_else(|| native_filesystem_root(&cwd)),
            "fs root",
        )?;
        if !cwd.starts_with(&fs_root) {
            bail!(
                "cwd must be inside fs root: cwd={}, fs_root={}",
                cwd.display(),
                fs_root.display()
            );
        }
        let state_dir = resolve_state_dir(self.state_dir, &cwd, |name| std::env::var_os(name))?;
        let scrubbed_env = scrubbed_env_names();
        let registration = match self.gateway_url {
            Some(gateway_url) => Some(registration_config(
                gateway_url,
                self.discovery_url,
                self.allow_insecure_gateway,
                self.on_closed,
                self.auto_upgrade,
                self.registration_key,
                self.registration_key_file,
                self.registration_name,
                self.registration_metadata,
                self.registration_receipt,
                self.ca_file,
            )?),
            None => {
                if self.registration_key.is_some() || self.registration_key_file.is_some() {
                    bail!("a registration key needs LIGHTSPEED_ENVD_GATEWAY_URL to dial");
                }
                if self.auto_upgrade {
                    bail!("automatic upgrade needs LIGHTSPEED_ENVD_GATEWAY_URL to dial");
                }
                if self.allow_insecure_gateway {
                    bail!("an insecure gateway needs LIGHTSPEED_ENVD_GATEWAY_URL to dial");
                }
                if self.on_closed == OnClosed::Park {
                    bail!("parking on close needs LIGHTSPEED_ENVD_GATEWAY_URL to register");
                }
                None
            }
        };
        let listen = match (self.listen, registration.is_some()) {
            (Some(listen), _) => Some(listen),
            (None, false) => Some(DEFAULT_LISTEN.parse().expect("valid default listener")),
            (None, true) => None,
        };
        forget_secret_env();
        Ok(DaemonConfig {
            listen,
            cwd,
            fs_root,
            state_dir,
            read_only_fs: self.read_only_fs,
            registration,
            scrubbed_env,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn registration_config(
    gateway_url: String,
    discovery_url: Option<String>,
    allow_insecure_gateway: bool,
    on_closed: OnClosed,
    auto_upgrade: bool,
    registration_key: Option<String>,
    registration_key_file: Option<PathBuf>,
    registration_name: Option<String>,
    registration_metadata: Option<String>,
    registration_receipt: Option<PathBuf>,
    ca_file: Option<PathBuf>,
) -> Result<RegistrationConfig> {
    // An explicit discovery URL is checked here; a derived one by
    // `validate`, which knows whether this configuration can download.
    let discovery_url = match discovery_url {
        Some(url) => {
            crate::upgrade::validate_discovery_url(&url)?;
            url
        }
        None => crate::upgrade::derive_discovery_url(&gateway_url)?,
    };
    let auto_upgrade = auto_upgrade
        .then(|| -> Result<RestartInvocation> {
            Ok(RestartInvocation {
                executable: started_executable().context("find the started envd executable")?,
                arg0: std::env::args_os()
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("process has no argv[0]"))?,
                args: std::env::args_os().skip(1).collect(),
            })
        })
        .transpose()?;
    let upgrade_attempted_for = match std::env::var(AUTO_UPGRADE_ATTEMPTED_ENV) {
        Ok(value) => Some(value.trim().parse::<u32>().with_context(|| {
            format!("{AUTO_UPGRADE_ATTEMPTED_ENV} must be a protocol version: {value}")
        })?),
        Err(_) => None,
    };
    let reexec_registration_key = std::env::var(REEXEC_REGISTRATION_KEY_ENV).ok();
    let registration_key = if let Some(key) = reexec_registration_key {
        Some(SecretString::new(key))
    } else {
        match (registration_key, registration_key_file) {
            (Some(_), Some(_)) => {
                bail!("set either the registration key or the key file, not both")
            }
            (Some(key), None) => Some(SecretString::new(key.trim().to_owned())),
            (None, Some(path)) => Some(SecretString::new(
                std::fs::read_to_string(&path)
                    .with_context(|| format!("read registration key file {}", path.display()))?
                    .trim()
                    .to_owned(),
            )),
            (None, None) => None,
        }
    };
    let metadata = match registration_metadata.as_deref() {
        Some(json) if !json.trim().is_empty() => Metadata::new(
            serde_json::from_str::<BTreeMap<String, String>>(json)
                .context("registration metadata must be a JSON object of strings")?,
        )
        .context("registration metadata")?,
        _ => Metadata::default(),
    };
    let display_name = registration_name
        .filter(|name| !name.is_empty())
        .map(DisplayName::new)
        .transpose()
        .context("registration name")?;
    let registration = RegistrationConfig {
        gateway_url,
        registration_key,
        display_name,
        metadata,
        receipt_path: registration_receipt,
        ca_file,
        discovery_url,
        allow_insecure_gateway,
        on_closed,
        auto_upgrade,
        upgrade_attempted_for,
    };
    registration.validate()?;
    Ok(registration)
}

/// The state directory, created `0700` and canonical. A defaulted
/// directory must not lie inside `cwd`, the agent's working tree; an
/// explicitly configured one may (confinement refuses data-plane access to
/// it wherever it lives). A relative explicit path is taken against `cwd`.
fn resolve_state_dir(
    configured: Option<PathBuf>,
    cwd: &Path,
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf> {
    let (path, defaulted) = match configured {
        Some(path) if path.is_absolute() => (path, false),
        Some(path) => (cwd.join(path), false),
        None => (default_state_dir(env)?, true),
    };
    let state_dir = prepare_state_dir(&path)?;
    if defaulted && state_dir.starts_with(cwd) {
        bail!(
            "the default state directory {} lies inside cwd {}: set LIGHTSPEED_ENVD_STATE_DIR",
            state_dir.display(),
            cwd.display()
        );
    }
    Ok(state_dir)
}

/// `$XDG_STATE_HOME/lightspeed-envd` when that variable is an absolute
/// path, else `$HOME/.local/state/lightspeed-envd`.
fn default_state_dir(env: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf> {
    if let Some(state_home) = env("XDG_STATE_HOME").map(PathBuf::from)
        && state_home.is_absolute()
    {
        return Ok(state_home.join(STATE_DIR_NAME));
    }
    match env("HOME").map(PathBuf::from) {
        Some(home) if home.is_absolute() => {
            Ok(home.join(".local").join("state").join(STATE_DIR_NAME))
        }
        _ => bail!(
            "neither XDG_STATE_HOME nor HOME names an absolute directory: set LIGHTSPEED_ENVD_STATE_DIR"
        ),
    }
}

/// Create the state directory when missing (`0700`; parents with default
/// permissions), restrict it, and return its canonical path.
pub fn prepare_state_dir(path: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(path)
        .with_context(|| format!("create state dir {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("restrict state dir {}", path.display()))?;
    }
    canonical_dir(path.to_path_buf(), "state dir")
}

/// Resolve argv[0] once, before serving anything, so replacement and re-exec
/// use the path the operator or service manager launched rather than a later
/// PATH lookup. Preserve a symlink at that path: the atomic replacement is of
/// the configured executable entry itself.
fn started_executable() -> Result<PathBuf> {
    let argv0 = std::env::args_os()
        .next()
        .ok_or_else(|| anyhow::anyhow!("process has no argv[0]"))?;
    let path = PathBuf::from(&argv0);
    if path.is_absolute() {
        return Ok(path);
    }
    if path.components().count() > 1 {
        return Ok(std::env::current_dir()
            .context("read current directory")?
            .join(path));
    }
    if let Some(search_path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&search_path) {
            let candidate = directory.join(&path);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    std::env::current_exe().context("fall back to the running executable path")
}

/// Drop the secret-bearing and one-exec variables from this process's
/// environment as soon as they have been read. Children additionally get
/// every daemon variable removed at spawn; this narrows what an in-process
/// bug could leak and what a later re-exec could inherit.
fn forget_secret_env() {
    for name in [
        "LIGHTSPEED_ENVD_REGISTRATION_KEY",
        "LIGHTSPEED_ENVD_REGISTRATION_KEY_FILE",
        REEXEC_REGISTRATION_KEY_ENV,
        AUTO_UPGRADE_ATTEMPTED_ENV,
    ] {
        // SAFETY: called from configuration parsing on the main thread
        // before the async runtime or any other thread exists.
        unsafe { std::env::remove_var(name) };
    }
}

fn native_filesystem_root(path: &Path) -> PathBuf {
    path.ancestors()
        .last()
        .expect("an absolute canonical path has a filesystem root")
        .to_path_buf()
}

fn canonical_dir(path: PathBuf, label: &str) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("canonicalize {label}: {}", path.display()))?;
    if !canonical.is_dir() {
        bail!("{label} must be a directory: {}", canonical.display());
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(temp: &tempfile::TempDir) -> DaemonArgs {
        DaemonArgs {
            command: None,
            listen: None,
            gateway_url: None,
            discovery_url: None,
            allow_insecure_gateway: false,
            on_closed: OnClosed::Exit,
            auto_upgrade: false,
            registration_key: None,
            registration_key_file: None,
            registration_name: None,
            registration_metadata: None,
            registration_receipt: None,
            ca_file: None,
            cwd: Some(temp.path().join("work")),
            fs_root: Some(temp.path().to_path_buf()),
            state_dir: Some(temp.path().join("state")),
            read_only_fs: false,
            print_build: false,
        }
    }

    fn temp() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(temp.path().join("work")).expect("work");
        temp
    }

    #[test]
    fn listener_config_needs_no_identity_or_secret() {
        let temp = temp();
        let config = DaemonArgs {
            listen: Some("127.0.0.1:0".parse().unwrap()),
            ..args(&temp)
        }
        .into_config()
        .expect("config");
        assert_eq!(config.listen, Some("127.0.0.1:0".parse().unwrap()));
        assert!(config.registration.is_none());
        let defaulted = args(&temp).into_config().expect("config");
        assert_eq!(
            defaulted.listen,
            Some(DEFAULT_LISTEN.parse().expect("default"))
        );
    }

    #[test]
    fn listener_addresses_select_tcp_or_a_unix_socket_by_scheme() {
        let tcp = ListenAddr::Tcp("0.0.0.0:19091".parse().unwrap());
        assert_eq!("0.0.0.0:19091".parse::<ListenAddr>().unwrap(), tcp);
        assert_eq!("tcp://0.0.0.0:19091".parse::<ListenAddr>().unwrap(), tcp);
        assert_eq!(tcp.to_string(), "0.0.0.0:19091");

        #[cfg(unix)]
        {
            let unix = ListenAddr::Unix(PathBuf::from("/run/envd.sock"));
            assert_eq!("unix:///run/envd.sock".parse::<ListenAddr>().unwrap(), unix);
            assert_eq!("unix:/run/envd.sock".parse::<ListenAddr>().unwrap(), unix);
            assert_eq!(unix.to_string(), "unix:///run/envd.sock");
        }
        assert!("unix://envd.sock".parse::<ListenAddr>().is_err());
        assert!("unix:envd.sock".parse::<ListenAddr>().is_err());
        assert!("ws://127.0.0.1:19091".parse::<ListenAddr>().is_err());
        assert!("localhost".parse::<ListenAddr>().is_err());
    }

    #[test]
    fn outbound_config_reads_the_key_once_and_disables_the_listener() {
        let temp = temp();
        let key_file = temp.path().join("key");
        std::fs::write(&key_file, "lsrk_secret\n").expect("write key");
        let config = DaemonArgs {
            gateway_url: Some("wss://gateway.example/environment-gateway/connect".to_owned()),
            registration_key_file: Some(key_file),
            registration_name: Some("worker-1".to_owned()),
            registration_metadata: Some(r#"{"harbor.trialId":"t-1"}"#.to_owned()),
            ..args(&temp)
        }
        .into_config()
        .expect("config");
        assert!(config.listen.is_none());
        let registration = config.registration.expect("registration");
        assert_eq!(
            registration
                .registration_key
                .as_ref()
                .map(|key| key.expose()),
            Some("lsrk_secret")
        );
        assert_eq!(
            registration.display_name.as_ref().map(DisplayName::as_str),
            Some("worker-1")
        );
        assert_eq!(registration.metadata.get("harbor.trialId"), Some("t-1"));
        assert!(!format!("{registration:?}").contains("lsrk_secret"));

        let both = DaemonArgs {
            gateway_url: Some("wss://gateway.example/environment-gateway/connect".to_owned()),
            registration_key: Some("lsrk_a".to_owned()),
            registration_key_file: Some(temp.path().join("missing")),
            ..args(&temp)
        }
        .into_config();
        assert!(both.is_err());

        let key_without_gateway = DaemonArgs {
            registration_key: Some("lsrk_a".to_owned()),
            ..args(&temp)
        }
        .into_config();
        assert!(key_without_gateway.is_err());

        let reserved = DaemonArgs {
            gateway_url: Some("wss://gateway.example/environment-gateway/connect".to_owned()),
            registration_metadata: Some(r#"{"lightspeed.x":"y"}"#.to_owned()),
            ..args(&temp)
        }
        .into_config();
        assert!(reserved.is_err());
    }

    #[test]
    fn gateway_urls_are_loopback_only_in_the_clear_and_end_with_the_connect_path() {
        let urls = gateway_urls(
            "wss://Gateway.example/ls/environment-gateway/connect",
            false,
        )
        .unwrap();
        assert_eq!(urls.origin.as_str(), "wss://gateway.example");
        assert_eq!(
            urls.stream,
            "wss://gateway.example/ls/environment-gateway/data"
        );
        for good in [
            "ws://127.0.0.1:18080/environment-gateway/connect",
            "ws://localhost:18080/environment-gateway/connect",
            "ws://[::1]:18080/environment-gateway/connect",
        ] {
            assert!(gateway_urls(good, false).is_ok(), "{good}");
        }
        for bad in [
            "ws://gateway.example/environment-gateway/connect",
            "http://gateway.example/environment-gateway/connect",
            "wss:///environment-gateway/connect",
            "wss://gateway.example/x",
            "wss://gateway.example/environment-gateway/connect?x=1",
            "wss://user@gateway.example/environment-gateway/connect",
        ] {
            assert!(gateway_urls(bad, false).is_err(), "{bad}");
            if !bad.starts_with("ws://") {
                assert!(gateway_urls(bad, true).is_err(), "{bad} stays refused");
            }
        }
    }

    #[test]
    fn an_opt_in_accepts_a_cluster_internal_gateway_in_the_clear() {
        let url = "ws://lightspeed-gateway.lightspeed.svc:8080/environment-gateway/connect";
        let urls = gateway_urls(url, true).expect("opted in");
        assert_eq!(
            urls.origin.as_str(),
            "ws://lightspeed-gateway.lightspeed.svc:8080"
        );
        assert_eq!(
            urls.stream,
            "ws://lightspeed-gateway.lightspeed.svc:8080/environment-gateway/data"
        );

        let temp = temp();
        let refused = DaemonArgs {
            gateway_url: Some(url.to_owned()),
            ..args(&temp)
        }
        .into_config();
        assert!(refused.is_err(), "refused without the opt-in");

        let config = DaemonArgs {
            gateway_url: Some(url.to_owned()),
            allow_insecure_gateway: true,
            ..args(&temp)
        }
        .into_config()
        .expect("config");
        let registration = config.registration.expect("registration");
        assert!(registration.allow_insecure_gateway);
        assert_eq!(
            registration.discovery_url,
            "http://lightspeed-gateway.lightspeed.svc:8080/.well-known/lightspeed-envd",
            "a report-only discovery URL may be plain HTTP"
        );

        // Downloading a build still needs HTTPS discovery.
        let downloading = DaemonArgs {
            gateway_url: Some(url.to_owned()),
            allow_insecure_gateway: true,
            auto_upgrade: true,
            ..args(&temp)
        }
        .into_config();
        assert!(downloading.is_err());
        let explicit = DaemonArgs {
            gateway_url: Some(url.to_owned()),
            allow_insecure_gateway: true,
            auto_upgrade: true,
            discovery_url: Some("https://downloads.example/envd.json".to_owned()),
            ..args(&temp)
        }
        .into_config()
        .expect("https discovery");
        assert!(
            explicit
                .registration
                .expect("registration")
                .auto_upgrade
                .is_some()
        );

        let without_gateway = DaemonArgs {
            allow_insecure_gateway: true,
            ..args(&temp)
        }
        .into_config();
        assert!(without_gateway.is_err());
    }

    #[test]
    fn on_closed_defaults_to_exit_and_parking_needs_a_gateway() {
        let parsed = DaemonArgs::try_parse_from([
            "lightspeed-envd",
            "--gateway-url",
            "wss://gateway.example/environment-gateway/connect",
        ])
        .expect("args");
        assert_eq!(parsed.on_closed, OnClosed::Exit);
        assert!(!parsed.allow_insecure_gateway);
        let parked = DaemonArgs::try_parse_from([
            "lightspeed-envd",
            "--gateway-url",
            "wss://gateway.example/environment-gateway/connect",
            "--on-closed",
            "park",
            "--allow-insecure-gateway",
        ])
        .expect("args");
        assert_eq!(parked.on_closed, OnClosed::Park);
        assert!(parked.allow_insecure_gateway);
        assert!(DaemonArgs::try_parse_from(["lightspeed-envd", "--on-closed", "linger"]).is_err());

        let temp = temp();
        let config = DaemonArgs {
            gateway_url: Some("wss://gateway.example/environment-gateway/connect".to_owned()),
            on_closed: OnClosed::Park,
            ..args(&temp)
        }
        .into_config()
        .expect("config");
        assert_eq!(
            config.registration.expect("registration").on_closed,
            OnClosed::Park
        );
        let without_gateway = DaemonArgs {
            on_closed: OnClosed::Park,
            ..args(&temp)
        }
        .into_config();
        assert!(without_gateway.is_err());
    }

    #[test]
    fn the_default_state_dir_follows_xdg_and_stays_out_of_cwd() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            }
        };
        assert_eq!(
            default_state_dir(env(&[("XDG_STATE_HOME", "/xdg"), ("HOME", "/home/a")])).unwrap(),
            PathBuf::from("/xdg/lightspeed-envd")
        );
        assert_eq!(
            default_state_dir(env(&[("XDG_STATE_HOME", "relative"), ("HOME", "/home/a")])).unwrap(),
            PathBuf::from("/home/a/.local/state/lightspeed-envd")
        );
        assert!(default_state_dir(env(&[])).is_err());

        let temp = temp();
        let cwd = temp.path().join("work").canonicalize().unwrap();
        let home = cwd.clone();
        let inside = resolve_state_dir(None, &cwd, |name| {
            (name == "HOME").then(|| home.clone().into_os_string())
        });
        assert!(
            inside.is_err(),
            "a defaulted state dir inside cwd is refused"
        );

        let explicit = resolve_state_dir(Some(PathBuf::from(".envd")), &cwd, |_| None)
            .expect("an explicit state dir inside cwd is allowed");
        assert_eq!(explicit, cwd.join(".envd"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&explicit).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }

        let outside_home = temp.path().canonicalize().unwrap().join("home");
        let outside = resolve_state_dir(None, &cwd, |name| {
            (name == "HOME").then(|| outside_home.clone().into_os_string())
        })
        .expect("a default outside cwd");
        assert!(outside.ends_with(".local/state/lightspeed-envd"));
    }

    #[test]
    fn upgrade_subcommand_accepts_global_gateway_and_discovery_options() {
        let parsed = DaemonArgs::try_parse_from([
            "lightspeed-envd",
            "upgrade",
            "--gateway-url",
            "wss://gateway.example/environment-gateway/connect",
            "--discovery-url",
            "https://downloads.example/envd.json",
        ])
        .expect("upgrade args");
        assert_eq!(parsed.command, Some(DaemonCommand::Upgrade));
        assert_eq!(
            parsed.gateway_url.as_deref(),
            Some("wss://gateway.example/environment-gateway/connect")
        );
        assert_eq!(
            parsed.discovery_url.as_deref(),
            Some("https://downloads.example/envd.json")
        );
    }

    #[test]
    fn outbound_upgrade_configuration_derives_discovery_and_captures_restart() {
        let temp = temp();
        let config = DaemonArgs {
            gateway_url: Some("wss://gateway.example/ls/environment-gateway/connect".to_owned()),
            auto_upgrade: true,
            ..args(&temp)
        }
        .into_config()
        .expect("config");
        let registration = config.registration.expect("registration");
        assert_eq!(
            registration.discovery_url,
            "https://gateway.example/.well-known/lightspeed-envd"
        );
        let restart = registration.auto_upgrade.expect("automatic upgrade");
        assert!(restart.executable.is_absolute());

        let parsed = DaemonArgs::try_parse_from([
            "lightspeed-envd",
            "--gateway-url",
            "wss://gateway.example/environment-gateway/connect",
            "--auto-upgrade",
        ])
        .expect("automatic upgrade args");
        assert!(parsed.auto_upgrade);
    }
}
