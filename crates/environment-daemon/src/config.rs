use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "lightspeed-envd",
    version,
    about = "Lightspeed environment execution daemon"
)]
pub struct DaemonArgs {
    /// WebSocket listener reachable by Lightspeed or an environment provider.
    #[arg(
        long,
        env = "LIGHTSPEED_ENVD_LISTEN",
        default_value = "127.0.0.1:19091"
    )]
    pub listen: SocketAddr,

    #[arg(long, env = "LIGHTSPEED_ENVD_CWD")]
    pub cwd: Option<PathBuf>,

    #[arg(long, env = "LIGHTSPEED_ENVD_FS_ROOT")]
    pub fs_root: Option<PathBuf>,

    #[arg(long, env = "LIGHTSPEED_ENVD_STATE_DIR")]
    pub state_dir: Option<PathBuf>,

    #[arg(long, default_value_t = false)]
    pub read_only_fs: bool,
}

#[derive(Clone, Debug)]
pub struct DaemonConfig {
    pub listen: SocketAddr,
    pub cwd: PathBuf,
    pub fs_root: PathBuf,
    pub state_dir: PathBuf,
    pub read_only_fs: bool,
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
        Ok(DaemonConfig {
            listen: self.listen,
            cwd,
            fs_root,
            state_dir,
            read_only_fs: self.read_only_fs,
        })
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

    #[test]
    fn listener_config_needs_no_identity_or_secret() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = DaemonArgs {
            listen: "127.0.0.1:0".parse().unwrap(),
            cwd: Some(temp.path().to_path_buf()),
            fs_root: Some(temp.path().to_path_buf()),
            state_dir: Some(temp.path().join("state")),
            read_only_fs: false,
        }
        .into_config()
        .expect("config");
        assert_eq!(config.listen.port(), 0);
    }
}
