use std::{
    net::SocketAddr,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "host-bridge",
    version,
    about = "Lightspeed guest OS host bridge"
)]
pub struct BridgeArgs {
    #[arg(long, env = "LIGHTSPEED_GATEWAY_URL")]
    pub gateway_url: String,

    #[arg(long, env = "LIGHTSPEED_HOST_BRIDGE_PROVIDER_ID")]
    pub provider_id: Option<String>,

    #[arg(long, env = "LIGHTSPEED_PROVIDER_TOKEN")]
    pub provider_token: Option<String>,

    #[arg(
        long,
        env = "LIGHTSPEED_HOST_BRIDGE_TARGET_ID",
        default_value = "local"
    )]
    pub target_id: String,

    #[arg(
        long,
        env = "LIGHTSPEED_HOST_BRIDGE_LISTEN",
        default_value = "127.0.0.1:0"
    )]
    pub listen: SocketAddr,

    #[arg(long, env = "LIGHTSPEED_HOST_BRIDGE_ADVERTISE_URL")]
    pub advertise_url: Option<String>,

    #[arg(long, env = "LIGHTSPEED_HOST_BRIDGE_CWD")]
    pub cwd: Option<PathBuf>,

    #[arg(long, env = "LIGHTSPEED_HOST_BRIDGE_FS_ROOT")]
    pub fs_root: Option<PathBuf>,

    #[arg(long, default_value_t = 10_000)]
    pub heartbeat_interval_ms: u64,

    #[arg(long, default_value_t = 30_000)]
    pub lease_ttl_ms: u64,

    #[arg(long, default_value_t = false)]
    pub read_only_fs: bool,
}

#[derive(Clone, Debug)]
pub struct BridgeConfig {
    pub gateway_url: String,
    pub provider_id: String,
    pub provider_token: Option<String>,
    pub target_id: String,
    pub listen: SocketAddr,
    pub advertise_url: Option<String>,
    pub cwd: PathBuf,
    pub fs_root: PathBuf,
    pub heartbeat_interval: Duration,
    pub lease_ttl: Duration,
    pub read_only_fs: bool,
}

impl BridgeArgs {
    pub fn into_config(self) -> Result<BridgeConfig> {
        if self.gateway_url.trim().is_empty() {
            bail!("--gateway-url must not be empty");
        }
        if self.target_id.trim().is_empty() {
            bail!("--target-id must not be empty");
        }
        if self.lease_ttl_ms == 0 {
            bail!("--lease-ttl-ms must be greater than zero");
        }
        if self.heartbeat_interval_ms == 0 {
            bail!("--heartbeat-interval-ms must be greater than zero");
        }

        let cwd = match self.cwd {
            Some(cwd) => cwd,
            None => std::env::current_dir().context("read current directory")?,
        };
        let cwd = canonical_dir(cwd, "cwd")?;
        let fs_root = canonical_dir(self.fs_root.unwrap_or_else(|| cwd.clone()), "fs root")?;
        if !cwd.starts_with(&fs_root) {
            bail!(
                "cwd must be inside fs root: cwd={}, fs_root={}",
                cwd.display(),
                fs_root.display()
            );
        }

        Ok(BridgeConfig {
            gateway_url: self.gateway_url,
            provider_id: self.provider_id.unwrap_or_else(ephemeral_provider_id),
            provider_token: self.provider_token,
            target_id: self.target_id,
            listen: self.listen,
            advertise_url: self.advertise_url,
            cwd,
            fs_root,
            heartbeat_interval: Duration::from_millis(self.heartbeat_interval_ms),
            lease_ttl: Duration::from_millis(self.lease_ttl_ms),
            read_only_fs: self.read_only_fs,
        })
    }
}

impl BridgeConfig {
    pub fn advertise_base_url(&self, local_addr: SocketAddr) -> String {
        self.advertise_url
            .clone()
            .unwrap_or_else(|| format!("ws://{local_addr}"))
            .trim_end_matches('/')
            .to_owned()
    }

    pub fn lease_ttl_ms_i64(&self) -> i64 {
        self.lease_ttl.as_millis().min(i64::MAX as u128) as i64
    }

    pub fn display_name(&self) -> String {
        format!("host bridge {}", self.provider_id)
    }
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

fn ephemeral_provider_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("host-bridge-{}-{millis}", std::process::id())
}
