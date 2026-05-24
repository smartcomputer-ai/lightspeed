//! Object-store-backed content-addressed blob storage.
//!
//! The public [`BlobRef`](agent_core::BlobRef) remains a logical content hash.
//! This crate stores a root record per logical blob so the physical layout can
//! be either a direct object or a byte range inside an immutable pack object.
//! Single `put_bytes` calls are written direct and become durable before
//! returning. Batch `put_many` calls pack small blobs together.
//!
//! The root record is the visibility marker. Payload objects are written first;
//! only blobs with a committed root record are readable through this store.

use std::{collections::HashSet, ops::Range, sync::Arc};

use agent_core::{
    BlobRef,
    storage::{BlobInfo, BlobStore, BlobStoreError},
};
use async_trait::async_trait;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload, path::Path as ObjectPath};
use serde::{Deserialize, Serialize};

const ROOT_RECORD_VERSION: u32 = 1;
const DEFAULT_PREFIX: &str = "forge";
const DEFAULT_PACK_THRESHOLD_BYTES: usize = 64 * 1024;
const DEFAULT_PACK_TARGET_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectBlobStoreConfig {
    pub prefix: String,
    pub pack_threshold_bytes: usize,
    pub pack_target_bytes: usize,
}

impl Default for ObjectBlobStoreConfig {
    fn default() -> Self {
        Self {
            prefix: DEFAULT_PREFIX.to_owned(),
            pack_threshold_bytes: DEFAULT_PACK_THRESHOLD_BYTES,
            pack_target_bytes: DEFAULT_PACK_TARGET_BYTES,
        }
    }
}

#[derive(Clone)]
pub struct ObjectBlobStore {
    config: ObjectBlobStoreConfig,
    store: Arc<dyn ObjectStore>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlobRootRecord {
    version: u32,
    blob_ref: BlobRef,
    byte_len: u64,
    layout: BlobLayout,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum BlobLayout {
    Direct {
        object_key: String,
    },
    Packed {
        pack_key: String,
        offset: u64,
        stored_len: u64,
    },
}

#[derive(Clone)]
struct PendingPackedBlob {
    blob_ref: BlobRef,
    bytes: Vec<u8>,
}

impl ObjectBlobStore {
    pub fn new(store: Arc<dyn ObjectStore>, config: ObjectBlobStoreConfig) -> Self {
        Self { config, store }
    }

    pub fn from_store(store: Arc<dyn ObjectStore>) -> Self {
        Self::new(store, ObjectBlobStoreConfig::default())
    }

    pub fn config(&self) -> &ObjectBlobStoreConfig {
        &self.config
    }

    pub fn object_store(&self) -> &Arc<dyn ObjectStore> {
        &self.store
    }

    async fn put_direct(&self, bytes: Vec<u8>) -> Result<BlobRef, BlobStoreError> {
        let blob_ref = BlobRef::from_bytes(&bytes);
        if self.load_root(&blob_ref).await?.is_some() {
            return Ok(blob_ref);
        }

        let object_key = direct_blob_key(&self.config, &blob_ref)?;
        self.put_object(&object_key, bytes.clone()).await?;
        self.store_root(&BlobRootRecord {
            version: ROOT_RECORD_VERSION,
            blob_ref: blob_ref.clone(),
            byte_len: bytes.len() as u64,
            layout: BlobLayout::Direct { object_key },
        })
        .await?;
        Ok(blob_ref)
    }

    async fn put_packed(&self, blobs: Vec<Vec<u8>>) -> Result<Vec<BlobRef>, BlobStoreError> {
        let mut blob_refs = Vec::with_capacity(blobs.len());
        let mut direct = Vec::new();
        let mut packed = Vec::new();
        let mut seen_in_batch = HashSet::new();

        for bytes in blobs {
            let blob_ref = BlobRef::from_bytes(&bytes);
            blob_refs.push(blob_ref.clone());
            if !seen_in_batch.insert(blob_ref.clone()) || self.load_root(&blob_ref).await?.is_some()
            {
                continue;
            }
            if bytes.len() <= self.config.pack_threshold_bytes {
                packed.push(PendingPackedBlob { blob_ref, bytes });
            } else {
                direct.push(bytes);
            }
        }

        for bytes in direct {
            self.put_direct(bytes).await?;
        }
        self.write_packed_blob_groups(packed).await?;
        Ok(blob_refs)
    }

    async fn write_packed_blob_groups(
        &self,
        blobs: Vec<PendingPackedBlob>,
    ) -> Result<(), BlobStoreError> {
        if blobs.is_empty() {
            return Ok(());
        }

        let pack_target = self.config.pack_target_bytes.max(1);
        let mut current = Vec::new();
        let mut current_size = 0usize;
        for blob in blobs {
            let next_size = current_size.saturating_add(blob.bytes.len());
            if !current.is_empty() && next_size > pack_target {
                self.flush_pack(std::mem::take(&mut current)).await?;
                current_size = 0;
            }
            current_size = current_size.saturating_add(blob.bytes.len());
            current.push(blob);
        }
        self.flush_pack(current).await
    }

    async fn flush_pack(&self, blobs: Vec<PendingPackedBlob>) -> Result<(), BlobStoreError> {
        if blobs.is_empty() {
            return Ok(());
        }

        let mut pack = Vec::new();
        let mut layouts = Vec::with_capacity(blobs.len());
        for blob in blobs {
            let offset = pack.len() as u64;
            let stored_len = blob.bytes.len() as u64;
            pack.extend_from_slice(&blob.bytes);
            layouts.push((blob.blob_ref, stored_len, offset));
        }

        let pack_key = pack_blob_key(&self.config, &BlobRef::from_bytes(&pack))?;
        self.put_object(&pack_key, pack).await?;
        for (blob_ref, stored_len, offset) in layouts {
            self.store_root(&BlobRootRecord {
                version: ROOT_RECORD_VERSION,
                blob_ref: blob_ref.clone(),
                byte_len: stored_len,
                layout: BlobLayout::Packed {
                    pack_key: pack_key.clone(),
                    offset,
                    stored_len,
                },
            })
            .await?;
        }
        Ok(())
    }

    async fn load_root(
        &self,
        blob_ref: &BlobRef,
    ) -> Result<Option<BlobRootRecord>, BlobStoreError> {
        let key = root_key(&self.config, blob_ref)?;
        let Some(bytes) = self.get_object(&key).await? else {
            return Ok(None);
        };
        let root: BlobRootRecord =
            serde_json::from_slice(&bytes).map_err(|error| BlobStoreError::Store {
                message: format!("decode blob root '{key}': {error}"),
            })?;
        if root.version != ROOT_RECORD_VERSION {
            return Err(BlobStoreError::Store {
                message: format!(
                    "unsupported blob root version for {blob_ref}: {}",
                    root.version
                ),
            });
        }
        if &root.blob_ref != blob_ref {
            return Err(BlobStoreError::Store {
                message: format!(
                    "blob root '{key}' references '{}', expected '{blob_ref}'",
                    root.blob_ref
                ),
            });
        }
        Ok(Some(root))
    }

    async fn store_root(&self, root: &BlobRootRecord) -> Result<(), BlobStoreError> {
        let key = root_key(&self.config, &root.blob_ref)?;
        if self.get_object(&key).await?.is_some() {
            return Ok(());
        }
        let bytes = serde_json::to_vec_pretty(root).map_err(|error| BlobStoreError::Store {
            message: format!("serialize blob root for '{}': {error}", root.blob_ref),
        })?;
        self.put_object(&key, bytes).await
    }

    async fn put_object(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobStoreError> {
        self.store
            .put(&ObjectPath::from(key), PutPayload::from(bytes))
            .await
            .map(|_| ())
            .map_err(|error| object_store_error("put object", key, error))
    }

    async fn get_object(&self, key: &str) -> Result<Option<Vec<u8>>, BlobStoreError> {
        match self.store.get(&ObjectPath::from(key)).await {
            Ok(result) => result
                .bytes()
                .await
                .map(|bytes| Some(bytes.to_vec()))
                .map_err(|error| object_store_error("read object body", key, error)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(error) => Err(object_store_error("get object", key, error)),
        }
    }

    async fn get_object_range(
        &self,
        key: &str,
        range: Range<u64>,
    ) -> Result<Vec<u8>, BlobStoreError> {
        self.store
            .get_range(&ObjectPath::from(key), range)
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| object_store_error("get object range", key, error))
    }
}

#[async_trait]
impl BlobStore for ObjectBlobStore {
    async fn put_bytes(&self, bytes: Vec<u8>) -> Result<BlobRef, BlobStoreError> {
        self.put_direct(bytes).await
    }

    async fn put_many(&self, blobs: Vec<Vec<u8>>) -> Result<Vec<BlobRef>, BlobStoreError> {
        self.put_packed(blobs).await
    }

    async fn read_bytes(&self, blob_ref: &BlobRef) -> Result<Vec<u8>, BlobStoreError> {
        let root = self
            .load_root(blob_ref)
            .await?
            .ok_or_else(|| BlobStoreError::NotFound {
                blob_ref: blob_ref.clone(),
            })?;
        let bytes = match root.layout {
            BlobLayout::Direct { object_key } => {
                self.get_object(&object_key)
                    .await?
                    .ok_or_else(|| BlobStoreError::NotFound {
                        blob_ref: blob_ref.clone(),
                    })?
            }
            BlobLayout::Packed {
                pack_key,
                offset,
                stored_len,
            } => {
                self.get_object_range(&pack_key, offset..offset.saturating_add(stored_len))
                    .await?
            }
        };
        let actual = BlobRef::from_bytes(&bytes);
        if &actual != blob_ref {
            return Err(BlobStoreError::Store {
                message: format!("blob hash mismatch: expected {blob_ref}, got {actual}"),
            });
        }
        Ok(bytes)
    }

    async fn has_blob(&self, blob_ref: &BlobRef) -> Result<bool, BlobStoreError> {
        Ok(self.load_root(blob_ref).await?.is_some())
    }

    async fn stat_blob(&self, blob_ref: &BlobRef) -> Result<BlobInfo, BlobStoreError> {
        let root = self
            .load_root(blob_ref)
            .await?
            .ok_or_else(|| BlobStoreError::NotFound {
                blob_ref: blob_ref.clone(),
            })?;
        Ok(BlobInfo {
            blob_ref: blob_ref.clone(),
            byte_len: root.byte_len,
        })
    }
}

fn root_key(config: &ObjectBlobStoreConfig, blob_ref: &BlobRef) -> Result<String, BlobStoreError> {
    let digest = sha256_hex(blob_ref)?;
    Ok(prefixed_key(
        config,
        &format!("cas/roots/sha256/{digest}.json"),
    ))
}

fn direct_blob_key(
    config: &ObjectBlobStoreConfig,
    blob_ref: &BlobRef,
) -> Result<String, BlobStoreError> {
    let digest = sha256_hex(blob_ref)?;
    let prefix = &digest[..2];
    Ok(prefixed_key(
        config,
        &format!("cas/blobs/sha256/{prefix}/{digest}.bin"),
    ))
}

fn pack_blob_key(
    config: &ObjectBlobStoreConfig,
    pack_ref: &BlobRef,
) -> Result<String, BlobStoreError> {
    let digest = sha256_hex(pack_ref)?;
    let prefix = &digest[..2];
    Ok(prefixed_key(
        config,
        &format!("cas/packs/sha256/{prefix}/{digest}.bin"),
    ))
}

fn prefixed_key(config: &ObjectBlobStoreConfig, suffix: &str) -> String {
    let prefix = config.prefix.trim_matches('/');
    if prefix.is_empty() {
        suffix.to_owned()
    } else {
        format!("{prefix}/{suffix}")
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

fn object_store_error(action: &str, key: &str, error: object_store::Error) -> BlobStoreError {
    BlobStoreError::Store {
        message: format!("{action} '{key}': {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;

    fn test_store() -> ObjectBlobStore {
        ObjectBlobStore::new(
            Arc::new(InMemory::new()),
            ObjectBlobStoreConfig {
                prefix: "test".to_owned(),
                pack_threshold_bytes: 128,
                pack_target_bytes: 512,
            },
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn put_bytes_writes_direct_even_for_small_blobs() {
        let store = test_store();
        let blob_ref = store.put_bytes(b"hello".to_vec()).await.expect("put blob");

        assert_eq!(
            store.read_text(&blob_ref).await.expect("read blob"),
            "hello"
        );
        let root = store
            .load_root(&blob_ref)
            .await
            .expect("load root")
            .expect("root exists");
        assert!(matches!(root.layout, BlobLayout::Direct { .. }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn put_many_packs_small_blobs_and_restores_ranges() {
        let store = test_store();
        let refs = store
            .put_many(vec![
                b"alpha snapshot bytes".to_vec(),
                b"beta snapshot bytes".to_vec(),
            ])
            .await
            .expect("put many");

        assert_eq!(
            store.read_bytes(&refs[0]).await.expect("read alpha"),
            b"alpha snapshot bytes".to_vec()
        );
        assert_eq!(
            store.read_bytes(&refs[1]).await.expect("read beta"),
            b"beta snapshot bytes".to_vec()
        );
        let alpha_root = store
            .load_root(&refs[0])
            .await
            .expect("load alpha root")
            .expect("alpha root exists");
        let beta_root = store
            .load_root(&refs[1])
            .await
            .expect("load beta root")
            .expect("beta root exists");
        assert!(matches!(alpha_root.layout, BlobLayout::Packed { .. }));
        assert!(matches!(beta_root.layout, BlobLayout::Packed { .. }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn put_many_writes_large_blobs_direct() {
        let store = test_store();
        let refs = store
            .put_many(vec![vec![b'x'; 129]])
            .await
            .expect("put many");

        assert_eq!(
            store.read_bytes(&refs[0]).await.expect("read blob"),
            vec![b'x'; 129]
        );
        let root = store
            .load_root(&refs[0])
            .await
            .expect("load root")
            .expect("root exists");
        assert!(matches!(root.layout, BlobLayout::Direct { .. }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn put_many_preserves_input_order_and_duplicate_refs() {
        let store = test_store();
        let refs = store
            .put_many(vec![b"same".to_vec(), b"other".to_vec(), b"same".to_vec()])
            .await
            .expect("put many");

        assert_eq!(refs[0], refs[2]);
        assert_ne!(refs[0], refs[1]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn missing_blob_reports_not_found() {
        let store = test_store();
        let missing = BlobRef::from_bytes(b"missing");

        assert!(!store.has_blob(&missing).await.expect("has blob"));
        assert!(matches!(
            store
                .read_bytes(&missing)
                .await
                .expect_err("missing read fails"),
            BlobStoreError::NotFound { .. }
        ));
    }
}
