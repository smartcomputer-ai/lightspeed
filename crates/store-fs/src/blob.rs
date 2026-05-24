use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use agent_core::{
    BlobRef,
    storage::{BlobInfo, BlobStore, BlobStoreError},
};
use async_trait::async_trait;
use tokio::{fs, sync::Mutex};

#[derive(Clone)]
pub struct FsBlobStore {
    root: Arc<PathBuf>,
    lock: Arc<Mutex<()>>,
}

struct BlobPaths {
    dir: PathBuf,
    data: PathBuf,
}

impl FsBlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: Arc::new(root.into()),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let store = Self::new(root);
        fs::create_dir_all(store.blob_root()).await?;
        Ok(store)
    }

    pub fn for_project(project_root: impl AsRef<Path>) -> Self {
        Self::new(crate::forge_dir(project_root))
    }

    pub async fn open_project(project_root: impl AsRef<Path>) -> io::Result<Self> {
        Self::open(crate::forge_dir(project_root)).await
    }

    pub fn root(&self) -> &Path {
        self.root.as_ref().as_path()
    }

    fn blob_root(&self) -> PathBuf {
        crate::cas_dir(self.root())
    }

    fn blob_paths(&self, blob_ref: &BlobRef) -> Result<BlobPaths, BlobStoreError> {
        let digest = sha256_hex(blob_ref)?;
        let prefix = &digest[..2];
        let dir = self.blob_root().join("sha256").join(prefix);
        Ok(BlobPaths {
            data: dir.join(format!("{digest}.bin")),
            dir,
        })
    }
}

#[async_trait]
impl BlobStore for FsBlobStore {
    async fn put_bytes(&self, bytes: Vec<u8>) -> Result<BlobRef, BlobStoreError> {
        let blob_ref = BlobRef::from_bytes(&bytes);
        let paths = self.blob_paths(&blob_ref)?;

        let _guard = self.lock.lock().await;
        fs::create_dir_all(&paths.dir)
            .await
            .map_err(|error| blob_io_error("create blob directory", &paths.dir, error))?;

        if !crate::path_exists(&paths.data)
            .await
            .map_err(|error| blob_io_error("stat blob bytes", &paths.data, error))?
        {
            crate::atomic_write(&paths.data, &bytes)
                .await
                .map_err(|error| blob_io_error("write blob bytes", &paths.data, error))?;
        }

        Ok(blob_ref)
    }

    async fn read_bytes(&self, blob_ref: &BlobRef) -> Result<Vec<u8>, BlobStoreError> {
        let paths = self.blob_paths(blob_ref)?;
        let bytes = fs::read(&paths.data).await.map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                BlobStoreError::NotFound {
                    blob_ref: blob_ref.clone(),
                }
            } else {
                blob_io_error("read blob bytes", &paths.data, error)
            }
        })?;
        let actual = BlobRef::from_bytes(&bytes);
        if &actual != blob_ref {
            return Err(BlobStoreError::Store {
                message: format!("blob hash mismatch: expected {blob_ref}, got {actual}"),
            });
        }
        Ok(bytes)
    }

    async fn has_blob(&self, blob_ref: &BlobRef) -> Result<bool, BlobStoreError> {
        let paths = self.blob_paths(blob_ref)?;
        crate::path_exists(&paths.data)
            .await
            .map_err(|error| blob_io_error("stat blob bytes", &paths.data, error))
    }

    async fn stat_blob(&self, blob_ref: &BlobRef) -> Result<BlobInfo, BlobStoreError> {
        let paths = self.blob_paths(blob_ref)?;
        let data_metadata = fs::metadata(&paths.data).await.map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                BlobStoreError::NotFound {
                    blob_ref: blob_ref.clone(),
                }
            } else {
                blob_io_error("stat blob bytes", &paths.data, error)
            }
        })?;

        Ok(BlobInfo {
            blob_ref: blob_ref.clone(),
            byte_len: data_metadata.len(),
        })
    }
}

fn sha256_hex(blob_ref: &BlobRef) -> Result<&str, BlobStoreError> {
    let value = blob_ref.as_str();
    let Some(digest) = value.strip_prefix("sha256:") else {
        return Err(BlobStoreError::Store {
            message: format!("unsupported blob ref format: {blob_ref}"),
        });
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(BlobStoreError::Store {
            message: format!("unsupported blob ref format: {blob_ref}"),
        });
    }
    Ok(digest)
}

fn blob_io_error(action: &str, path: &Path, error: io::Error) -> BlobStoreError {
    BlobStoreError::Store {
        message: format!("{action} '{}': {error}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::storage::BlobStore;

    #[tokio::test(flavor = "current_thread")]
    async fn fs_blob_store_persists_bytes() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = FsBlobStore::open(temp_dir.path())
            .await
            .expect("open store");

        let blob_ref = store
            .put_bytes(b"hello".to_vec())
            .await
            .expect("write blob");
        let paths = store.blob_paths(&blob_ref).expect("blob paths");
        assert!(
            !crate::path_exists(&paths.data.with_extension("json"))
                .await
                .expect("stat blob sidecar")
        );

        let reopened = FsBlobStore::open(temp_dir.path())
            .await
            .expect("reopen store");
        assert!(reopened.has_blob(&blob_ref).await.expect("has blob"));
        assert_eq!(
            reopened.read_text(&blob_ref).await.expect("read text"),
            "hello"
        );
        assert_eq!(
            reopened.stat_blob(&blob_ref).await.expect("stat blob"),
            BlobInfo {
                blob_ref,
                byte_len: 5,
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs_blob_store_dedupes_by_content_ref() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = FsBlobStore::open(temp_dir.path())
            .await
            .expect("open store");

        let first = store
            .put_bytes(b"same".to_vec())
            .await
            .expect("write first");
        let second = store
            .put_bytes(b"same".to_vec())
            .await
            .expect("write second");

        assert_eq!(first, second);
        assert_eq!(
            store.stat_blob(&first).await.expect("stat blob"),
            BlobInfo {
                blob_ref: first,
                byte_len: 4,
            }
        );
    }
}
