use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use agent_core::{
    BlobRef,
    storage::{BlobInfo, BlobStore, BlobStoreError, BlobWrite},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::Mutex};

#[derive(Clone)]
pub struct FsBlobStore {
    root: Arc<PathBuf>,
    lock: Arc<Mutex<()>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct BlobMetadata {
    blob_ref: BlobRef,
    byte_len: u64,
    child_refs: Vec<BlobRef>,
}

struct BlobPaths {
    dir: PathBuf,
    data: PathBuf,
    metadata: PathBuf,
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
            metadata: dir.join(format!("{digest}.json")),
            dir,
        })
    }
}

#[async_trait]
impl BlobStore for FsBlobStore {
    async fn put_bytes(&self, write: BlobWrite) -> Result<BlobRef, BlobStoreError> {
        let blob_ref = BlobRef::from_bytes(&write.bytes);
        let paths = self.blob_paths(&blob_ref)?;
        let metadata = BlobMetadata {
            blob_ref: blob_ref.clone(),
            byte_len: write.bytes.len() as u64,
            child_refs: write.child_refs,
        };

        let _guard = self.lock.lock().await;
        fs::create_dir_all(&paths.dir)
            .await
            .map_err(|error| blob_io_error("create blob directory", &paths.dir, error))?;

        if !crate::path_exists(&paths.data)
            .await
            .map_err(|error| blob_io_error("stat blob bytes", &paths.data, error))?
        {
            crate::atomic_write(&paths.data, &write.bytes)
                .await
                .map_err(|error| blob_io_error("write blob bytes", &paths.data, error))?;
        }

        if !crate::path_exists(&paths.metadata)
            .await
            .map_err(|error| blob_io_error("stat blob metadata", &paths.metadata, error))?
        {
            write_blob_metadata(&paths.metadata, &metadata).await?;
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
        let has_bytes = crate::path_exists(&paths.data)
            .await
            .map_err(|error| blob_io_error("stat blob bytes", &paths.data, error))?;
        let has_metadata = crate::path_exists(&paths.metadata)
            .await
            .map_err(|error| blob_io_error("stat blob metadata", &paths.metadata, error))?;
        Ok(has_bytes && has_metadata)
    }

    async fn stat_blob(&self, blob_ref: &BlobRef) -> Result<BlobInfo, BlobStoreError> {
        let paths = self.blob_paths(blob_ref)?;
        let metadata = read_blob_metadata(&paths.metadata, blob_ref).await?;
        if &metadata.blob_ref != blob_ref {
            return Err(BlobStoreError::Store {
                message: format!(
                    "blob metadata '{}' references '{}', expected '{blob_ref}'",
                    paths.metadata.display(),
                    metadata.blob_ref
                ),
            });
        }

        let data_metadata = fs::metadata(&paths.data).await.map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                BlobStoreError::NotFound {
                    blob_ref: blob_ref.clone(),
                }
            } else {
                blob_io_error("stat blob bytes", &paths.data, error)
            }
        })?;
        if data_metadata.len() != metadata.byte_len {
            return Err(BlobStoreError::Store {
                message: format!(
                    "blob byte length mismatch for {blob_ref}: metadata {}, file {}",
                    metadata.byte_len,
                    data_metadata.len()
                ),
            });
        }

        Ok(BlobInfo {
            blob_ref: blob_ref.clone(),
            byte_len: metadata.byte_len,
            child_refs: metadata.child_refs,
        })
    }
}

async fn write_blob_metadata(path: &Path, metadata: &BlobMetadata) -> Result<(), BlobStoreError> {
    let bytes = serde_json::to_vec_pretty(metadata).map_err(|error| BlobStoreError::Store {
        message: format!("serialize blob metadata for '{}': {error}", path.display()),
    })?;
    crate::atomic_write(path, &bytes)
        .await
        .map_err(|error| blob_io_error("write blob metadata", path, error))
}

async fn read_blob_metadata(
    path: &Path,
    blob_ref: &BlobRef,
) -> Result<BlobMetadata, BlobStoreError> {
    let bytes = fs::read(path).await.map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            BlobStoreError::NotFound {
                blob_ref: blob_ref.clone(),
            }
        } else {
            blob_io_error("read blob metadata", path, error)
        }
    })?;
    serde_json::from_slice(&bytes).map_err(|error| BlobStoreError::Store {
        message: format!("decode blob metadata '{}': {error}", path.display()),
    })
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
    async fn fs_blob_store_persists_bytes_and_metadata() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = FsBlobStore::open(temp_dir.path())
            .await
            .expect("open store");
        let child = BlobRef::from_bytes(b"child");

        let blob_ref = store
            .put_bytes(BlobWrite {
                bytes: b"hello".to_vec(),
                child_refs: vec![child.clone()],
            })
            .await
            .expect("write blob");

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
                child_refs: vec![child],
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
            .put_bytes(BlobWrite {
                bytes: b"same".to_vec(),
                child_refs: vec![BlobRef::from_bytes(b"first-child")],
            })
            .await
            .expect("write first");
        let second = store
            .put_bytes(BlobWrite {
                bytes: b"same".to_vec(),
                child_refs: vec![BlobRef::from_bytes(b"second-child")],
            })
            .await
            .expect("write second");

        assert_eq!(first, second);
        assert_eq!(
            store.stat_blob(&first).await.expect("stat blob").child_refs,
            vec![BlobRef::from_bytes(b"first-child")]
        );
    }
}
