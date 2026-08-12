use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use host_protocol::gateway::decode_enrollment_token;
use serde::{Deserialize, Serialize};

pub const STATE_FILE: &str = "identity.json";

#[derive(Parser, Debug)]
#[command(
    name = "lightspeed-envd",
    version,
    about = "Lightspeed environment execution daemon"
)]
pub struct DaemonArgs {
    /// Gateway URL used during first enrollment. It is persisted afterwards.
    #[arg(long, env = "LIGHTSPEED_ENVD_GATEWAY_URL")]
    pub gateway_url: Option<String>,

    /// Self-locating one-time enrollment token. It is never persisted.
    #[arg(long, env = "LIGHTSPEED_ENVD_ENROLLMENT_TOKEN")]
    pub enrollment_token: Option<String>,

    #[arg(long, env = "LIGHTSPEED_ENVD_CWD")]
    pub cwd: Option<PathBuf>,

    #[arg(long, env = "LIGHTSPEED_ENVD_FS_ROOT")]
    pub fs_root: Option<PathBuf>,

    #[arg(long, env = "LIGHTSPEED_ENVD_STATE_DIR")]
    pub state_dir: Option<PathBuf>,

    #[arg(long, default_value_t = false)]
    pub read_only_fs: bool,

    #[arg(long, env = "LIGHTSPEED_ENVD_RECONNECT_MS", default_value_t = 1_000)]
    pub reconnect_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonIdentityBinding {
    pub gateway_url: String,
    pub universe_id: String,
    pub environment_id: String,
    pub incarnation_id: String,
}

#[derive(Clone, Debug)]
pub struct DaemonConfig {
    pub identity: DaemonIdentityBinding,
    pub enrollment_token: Option<String>,
    pub cwd: PathBuf,
    pub fs_root: PathBuf,
    pub state_dir: PathBuf,
    pub read_only_fs: bool,
    pub reconnect_ms: u64,
}

impl DaemonArgs {
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
        let state_dir = match self.state_dir {
            Some(path) if path.is_absolute() => path,
            Some(path) => cwd.join(path),
            None => cwd.join(".lightspeed-envd"),
        };
        let persisted = read_persisted_identity(&state_dir)?;
        let identity = match self.enrollment_token.as_deref() {
            Some(token) => {
                let claims = decode_enrollment_token(token).map_err(anyhow::Error::msg)?;
                let gateway_url = self
                    .gateway_url
                    .as_deref()
                    .or_else(|| persisted.as_ref().map(|state| state.gateway_url.as_str()))
                    .ok_or_else(|| {
                        anyhow::anyhow!("--gateway-url is required for the first enrollment only")
                    })?;
                DaemonIdentityBinding {
                    gateway_url: normalize_gateway_url(gateway_url)?,
                    universe_id: claims.universe_id,
                    environment_id: claims.environment_id,
                    incarnation_id: claims.incarnation_id,
                }
            }
            None => persisted.clone().ok_or_else(|| {
                anyhow::anyhow!(
                    "--enrollment-token and --gateway-url are required for first enrollment"
                )
            })?,
        };
        if let Some(persisted) = persisted
            && persisted != identity
        {
            bail!("envd state directory is bound to a different gateway or environment");
        }
        if let Some(gateway_url) = self.gateway_url.as_deref()
            && normalize_gateway_url(gateway_url)? != identity.gateway_url
        {
            bail!("--gateway-url does not match the persisted envd identity");
        }
        Ok(DaemonConfig {
            identity,
            enrollment_token: self.enrollment_token,
            cwd,
            fs_root,
            state_dir,
            read_only_fs: self.read_only_fs,
            reconnect_ms: self.reconnect_ms.max(100),
        })
    }
}

fn read_persisted_identity(state_dir: &Path) -> Result<Option<DaemonIdentityBinding>> {
    let path = state_dir.join(STATE_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("decode {}", path.display()))
            .map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn normalize_gateway_url(value: &str) -> Result<String> {
    let value = value.trim().trim_end_matches('/');
    if value.is_empty()
        || !(value.starts_with("http://")
            || value.starts_with("https://")
            || value.starts_with("ws://")
            || value.starts_with("wss://"))
    {
        bail!("gateway URL must use http, https, ws, or wss");
    }
    Ok(value.to_owned())
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
    use crate::identity::DaemonIdentity;

    fn args(
        root: &Path,
        gateway_url: Option<&str>,
        enrollment_token: Option<String>,
    ) -> DaemonArgs {
        DaemonArgs {
            gateway_url: gateway_url.map(ToOwned::to_owned),
            enrollment_token,
            cwd: Some(root.to_path_buf()),
            fs_root: Some(root.to_path_buf()),
            state_dir: Some(root.join("state")),
            read_only_fs: false,
            reconnect_ms: 100,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn first_run_decodes_token_and_restart_uses_only_persisted_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let token = host_protocol::gateway::encode_enrollment_token(
            "universe-a",
            "environment-a",
            "incarnation-a",
            &[7; 32],
        )
        .expect("token");
        let first = args(temp.path(), Some("https://gateway.example/"), Some(token))
            .into_config()
            .expect("first-run config");
        assert_eq!(first.identity.gateway_url, "https://gateway.example");
        assert_eq!(first.identity.universe_id, "universe-a");
        DaemonIdentity::load_or_create(&first.state_dir, &first.identity)
            .await
            .expect("persist identity");

        let restarted = args(temp.path(), None, None)
            .into_config()
            .expect("restart config");
        assert_eq!(restarted.identity, first.identity);
        assert!(restarted.enrollment_token.is_none());
    }

    #[test]
    fn fresh_state_requires_gateway_and_token() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(args(temp.path(), None, None).into_config().is_err());
        let token = host_protocol::gateway::encode_enrollment_token(
            "universe-a",
            "environment-a",
            "incarnation-a",
            &[7; 32],
        )
        .expect("token");
        assert!(args(temp.path(), None, Some(token)).into_config().is_err());
    }
}
