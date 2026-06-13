//! Filesystem-backed storage adapters.
//!
//! This crate keeps durable filesystem I/O outside `engine` while
//! implementing the core storage contracts.
//!
//! Project-backed stores use `.lightspeed` by convention:
//!
//! ```text
//! .lightspeed/
//!   cas/
//!     sha256/<prefix>/<digest>.bin
//!   sessions/
//!     <percent-encoded-session-id>/
//!       session.json
//!       events.jsonl
//!   vfs/
//!     snapshots/<percent-encoded-snapshot-ref>.json
//!     workspaces/<percent-encoded-workspace-id>.json
//!     mounts/<percent-encoded-session-id>/<percent-encoded-mount-path>.json
//! ```
//!
//! Session logs are append-oriented JSONL files with one committed
//! `DynamicSessionEntry` per line, which keeps replay streamable and leaves
//! `session.json` as a compact index record for listing and head checks.

mod blob;
mod session;
mod vfs;

use std::{
    io,
    path::{Path, PathBuf},
    process,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::fs;

pub use blob::FsBlobStore;
pub use session::FsSessionStore;
pub use vfs::FsVfsCatalogStore;

pub const LIGHTSPEED_DIR: &str = ".lightspeed";

#[derive(Clone)]
pub struct FsStore {
    root: Arc<PathBuf>,
    sessions: FsSessionStore,
    blobs: FsBlobStore,
    vfs: FsVfsCatalogStore,
}

impl FsStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            root: Arc::new(root.clone()),
            sessions: FsSessionStore::new(root.clone()),
            blobs: FsBlobStore::new(root.clone()),
            vfs: FsVfsCatalogStore::new(root),
        }
    }

    pub async fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let store = Self::new(root);
        ensure_layout(store.root()).await?;
        Ok(store)
    }

    pub fn for_project(project_root: impl AsRef<Path>) -> Self {
        Self::new(lightspeed_dir(project_root))
    }

    pub async fn open_project(project_root: impl AsRef<Path>) -> io::Result<Self> {
        Self::open(lightspeed_dir(project_root)).await
    }

    pub fn root(&self) -> &Path {
        self.root.as_ref().as_path()
    }

    pub fn sessions(&self) -> &FsSessionStore {
        &self.sessions
    }

    pub fn blobs(&self) -> &FsBlobStore {
        &self.blobs
    }

    pub fn vfs(&self) -> &FsVfsCatalogStore {
        &self.vfs
    }

    pub fn into_parts(self) -> (FsSessionStore, FsBlobStore) {
        (self.sessions, self.blobs)
    }
}

async fn ensure_layout(root: &Path) -> io::Result<()> {
    fs::create_dir_all(sessions_dir(root)).await?;
    fs::create_dir_all(cas_dir(root)).await?;
    fs::create_dir_all(vfs_dir(root)).await?;
    Ok(())
}

pub fn lightspeed_dir(project_root: impl AsRef<Path>) -> PathBuf {
    project_root.as_ref().join(LIGHTSPEED_DIR)
}

fn sessions_dir(root: &Path) -> PathBuf {
    root.join("sessions")
}

fn cas_dir(root: &Path) -> PathBuf {
    root.join("cas")
}

fn vfs_dir(root: &Path) -> PathBuf {
    root.join("vfs")
}

async fn path_exists(path: &Path) -> io::Result<bool> {
    match fs::metadata(path).await {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

async fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let tmp = temporary_sibling(path);
    fs::write(&tmp, bytes).await?;
    match fs::rename(&tmp, path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            match fs::remove_file(path).await {
                Ok(()) => {}
                Err(remove_error) if remove_error.kind() == io::ErrorKind::NotFound => {}
                Err(remove_error) => {
                    let _ = fs::remove_file(&tmp).await;
                    return Err(remove_error);
                }
            }
            fs::rename(&tmp, path).await
        }
        Err(error) => {
            let _ = fs::remove_file(&tmp).await;
            Err(error)
        }
    }
}

fn temporary_sibling(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("store-fs");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    path.with_file_name(format!(".{file_name}.{}.{}.tmp", process::id(), nanos))
}

fn encode_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn open_project_uses_dot_lightspeed_layout() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = FsStore::open_project(temp_dir.path())
            .await
            .expect("open project store");

        assert_eq!(store.root(), temp_dir.path().join(LIGHTSPEED_DIR).as_path());
        assert!(temp_dir.path().join(LIGHTSPEED_DIR).join("cas").is_dir());
        assert!(temp_dir.path().join(LIGHTSPEED_DIR).join("sessions").is_dir());
    }
}
