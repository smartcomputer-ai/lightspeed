//! Filesystem-backed content-addressed blob store.
//!
//! This crate keeps durable filesystem I/O outside `harness` while
//! implementing its blob store contract. Blobs live under the store root:
//!
//! ```text
//! cas/
//!   sha256/<prefix>/<digest>.bin
//! ```

mod blob;

use std::{
    io,
    path::{Path, PathBuf},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::fs;

pub use blob::FsBlobStore;

fn cas_dir(root: &Path) -> PathBuf {
    root.join("cas")
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
