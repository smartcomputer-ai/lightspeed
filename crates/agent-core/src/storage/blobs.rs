//! Content-addressed blob storage contract.

use crate::BlobRef;
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BlobStoreError {
    #[error("blob not found: {blob_ref}")]
    NotFound { blob_ref: BlobRef },

    #[error("blob store failure: {message}")]
    Store { message: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlobWrite {
    pub bytes: Vec<u8>,
    pub child_refs: Vec<BlobRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobInfo {
    pub blob_ref: BlobRef,
    pub byte_len: u64,
    pub child_refs: Vec<BlobRef>,
}

#[async_trait]
pub trait BlobStore: Send + Sync {
    async fn put_bytes(&self, write: BlobWrite) -> Result<BlobRef, BlobStoreError>;

    async fn read_bytes(&self, blob_ref: &BlobRef) -> Result<Vec<u8>, BlobStoreError>;

    async fn has_blob(&self, blob_ref: &BlobRef) -> Result<bool, BlobStoreError>;

    async fn stat_blob(&self, blob_ref: &BlobRef) -> Result<BlobInfo, BlobStoreError>;

    async fn read_text(&self, blob_ref: &BlobRef) -> Result<String, BlobStoreError> {
        let bytes = self.read_bytes(blob_ref).await?;
        String::from_utf8(bytes).map_err(|error| BlobStoreError::Store {
            message: format!("blob '{blob_ref}' is not valid UTF-8: {error}"),
        })
    }
}

#[derive(Clone, Default)]
pub struct InMemoryBlobStore {
    inner: Arc<RwLock<InMemoryBlobStoreInner>>,
}

#[derive(Default)]
struct InMemoryBlobStoreInner {
    bytes_by_ref: BTreeMap<BlobRef, Vec<u8>>,
    info_by_ref: BTreeMap<BlobRef, BlobInfo>,
}

impl InMemoryBlobStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn insert_text(&self, text: impl Into<String>) -> BlobRef {
        self.put_bytes(BlobWrite {
            bytes: text.into().into_bytes(),
            child_refs: Vec::new(),
        })
        .await
        .expect("in-memory blob write should not fail")
    }
}

#[async_trait]
impl BlobStore for InMemoryBlobStore {
    async fn put_bytes(&self, write: BlobWrite) -> Result<BlobRef, BlobStoreError> {
        let blob_ref = BlobRef::from_bytes(&write.bytes);
        let info = BlobInfo {
            blob_ref: blob_ref.clone(),
            byte_len: write.bytes.len() as u64,
            child_refs: write.child_refs,
        };
        let mut inner = self.inner.write().expect("blob store lock poisoned");
        inner
            .bytes_by_ref
            .entry(blob_ref.clone())
            .or_insert(write.bytes);
        inner.info_by_ref.entry(blob_ref.clone()).or_insert(info);
        Ok(blob_ref)
    }

    async fn read_bytes(&self, blob_ref: &BlobRef) -> Result<Vec<u8>, BlobStoreError> {
        let bytes = self
            .inner
            .read()
            .expect("blob store lock poisoned")
            .bytes_by_ref
            .get(blob_ref)
            .cloned()
            .ok_or_else(|| BlobStoreError::NotFound {
                blob_ref: blob_ref.clone(),
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
        Ok(self
            .inner
            .read()
            .expect("blob store lock poisoned")
            .bytes_by_ref
            .contains_key(blob_ref))
    }

    async fn stat_blob(&self, blob_ref: &BlobRef) -> Result<BlobInfo, BlobStoreError> {
        self.inner
            .read()
            .expect("blob store lock poisoned")
            .info_by_ref
            .get(blob_ref)
            .cloned()
            .ok_or_else(|| BlobStoreError::NotFound {
                blob_ref: blob_ref.clone(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn in_memory_blob_store_dedupes_and_reads_text() {
        let store = InMemoryBlobStore::new();
        let first = store
            .put_bytes(BlobWrite {
                bytes: b"hello".to_vec(),
                child_refs: Vec::new(),
            })
            .await
            .expect("write blob");
        let second = store
            .put_bytes(BlobWrite {
                bytes: b"hello".to_vec(),
                child_refs: Vec::new(),
            })
            .await
            .expect("write blob");

        assert_eq!(first, second);
        assert_eq!(store.read_text(&first).await.expect("read blob"), "hello");
        assert_eq!(
            store.stat_blob(&first).await.expect("stat blob").byte_len,
            5
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn in_memory_blob_store_records_explicit_child_refs() {
        let store = InMemoryBlobStore::new();
        let child = BlobRef::from_bytes(b"child");
        let parent = store
            .put_bytes(BlobWrite {
                bytes: b"parent".to_vec(),
                child_refs: vec![child.clone()],
            })
            .await
            .expect("write blob");

        assert_eq!(
            store
                .stat_blob(&parent)
                .await
                .expect("stat blob")
                .child_refs,
            vec![child]
        );
    }
}
