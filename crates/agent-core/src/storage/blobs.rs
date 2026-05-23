//! Content-addressed blob storage contract.

use crate::BlobRef;
use async_trait::async_trait;
use std::collections::{BTreeMap, HashMap, VecDeque};
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

#[async_trait]
impl<T> BlobStore for Arc<T>
where
    T: BlobStore + ?Sized,
{
    async fn put_bytes(&self, write: BlobWrite) -> Result<BlobRef, BlobStoreError> {
        self.as_ref().put_bytes(write).await
    }

    async fn read_bytes(&self, blob_ref: &BlobRef) -> Result<Vec<u8>, BlobStoreError> {
        self.as_ref().read_bytes(blob_ref).await
    }

    async fn has_blob(&self, blob_ref: &BlobRef) -> Result<bool, BlobStoreError> {
        self.as_ref().has_blob(blob_ref).await
    }

    async fn stat_blob(&self, blob_ref: &BlobRef) -> Result<BlobInfo, BlobStoreError> {
        self.as_ref().stat_blob(blob_ref).await
    }
}

/// Hard limits for an in-process blob cache.
///
/// `max_bytes` counts cached payload bytes, not map/key overhead. `max_entries`
/// bounds both metadata-only and payload-backed entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlobCacheLimits {
    pub max_bytes: u64,
    pub max_entries: usize,
}

impl BlobCacheLimits {
    pub const fn new(max_bytes: u64, max_entries: usize) -> Self {
        Self {
            max_bytes,
            max_entries,
        }
    }
}

/// Point-in-time cache occupancy and configured limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlobCacheStats {
    pub current_bytes: u64,
    pub entries: usize,
    pub max_bytes: u64,
    pub max_entries: usize,
}

/// Bounded in-memory cache for immutable content-addressed blobs.
///
/// Entries are evicted least-recently-used first. Blob bytes and blob metadata
/// can be cached independently, so `stat_blob` does not force payload bytes
/// into memory.
#[derive(Clone)]
pub struct InMemoryBlobCache {
    inner: Arc<RwLock<InMemoryBlobCacheInner>>,
}

struct InMemoryBlobCacheInner {
    limits: BlobCacheLimits,
    current_bytes: u64,
    entries: HashMap<BlobRef, CachedBlob>,
    lru: VecDeque<BlobRef>,
}

#[derive(Clone, Default)]
struct CachedBlob {
    bytes: Option<Vec<u8>>,
    info: Option<BlobInfo>,
}

impl CachedBlob {
    fn cached_byte_len(&self) -> u64 {
        self.bytes.as_ref().map_or(0, |bytes| bytes.len() as u64)
    }
}

impl InMemoryBlobCache {
    pub fn new(max_bytes: u64, max_entries: usize) -> Self {
        Self::with_limits(BlobCacheLimits::new(max_bytes, max_entries))
    }

    pub fn with_limits(limits: BlobCacheLimits) -> Self {
        Self {
            inner: Arc::new(RwLock::new(InMemoryBlobCacheInner {
                limits,
                current_bytes: 0,
                entries: HashMap::new(),
                lru: VecDeque::new(),
            })),
        }
    }

    pub fn stats(&self) -> BlobCacheStats {
        let inner = self.inner.read().expect("blob cache lock poisoned");
        BlobCacheStats {
            current_bytes: inner.current_bytes,
            entries: inner.entries.len(),
            max_bytes: inner.limits.max_bytes,
            max_entries: inner.limits.max_entries,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.stats().entries == 0
    }

    pub fn contains(&self, blob_ref: &BlobRef) -> bool {
        self.inner
            .read()
            .expect("blob cache lock poisoned")
            .entries
            .contains_key(blob_ref)
    }

    pub fn can_store_bytes(&self, byte_len: u64) -> bool {
        let inner = self.inner.read().expect("blob cache lock poisoned");
        inner.limits.max_entries > 0 && byte_len <= inner.limits.max_bytes
    }

    pub fn get_bytes(&self, blob_ref: &BlobRef) -> Option<Vec<u8>> {
        let mut inner = self.inner.write().expect("blob cache lock poisoned");
        let bytes = inner
            .entries
            .get(blob_ref)
            .and_then(|entry| entry.bytes.clone())?;
        inner.touch(blob_ref);
        Some(bytes)
    }

    pub fn get_info(&self, blob_ref: &BlobRef) -> Option<BlobInfo> {
        let mut inner = self.inner.write().expect("blob cache lock poisoned");
        let info = inner
            .entries
            .get(blob_ref)
            .and_then(|entry| entry.info.clone())?;
        inner.touch(blob_ref);
        Some(info)
    }

    pub fn insert_bytes(&self, blob_ref: BlobRef, bytes: Vec<u8>) {
        self.insert(blob_ref, Some(bytes), None);
    }

    pub fn insert_info(&self, info: BlobInfo) {
        self.insert(info.blob_ref.clone(), None, Some(info));
    }

    pub fn insert_blob(&self, info: BlobInfo, bytes: Vec<u8>) {
        self.insert(info.blob_ref.clone(), Some(bytes), Some(info));
    }

    pub fn clear(&self) {
        let mut inner = self.inner.write().expect("blob cache lock poisoned");
        inner.current_bytes = 0;
        inner.entries.clear();
        inner.lru.clear();
    }

    fn insert(&self, blob_ref: BlobRef, bytes: Option<Vec<u8>>, info: Option<BlobInfo>) {
        let mut inner = self.inner.write().expect("blob cache lock poisoned");
        if inner.limits.max_entries == 0 {
            return;
        }

        let bytes = bytes.filter(|bytes| bytes.len() as u64 <= inner.limits.max_bytes);
        if bytes.is_none() && info.is_none() && !inner.entries.contains_key(&blob_ref) {
            return;
        }

        let mut entry = inner.entries.remove(&blob_ref).unwrap_or_default();
        inner.current_bytes = inner.current_bytes.saturating_sub(entry.cached_byte_len());
        if let Some(bytes) = bytes {
            entry.bytes = Some(bytes);
        }
        if let Some(info) = info {
            entry.info = Some(info);
        }
        if entry.bytes.is_none() && entry.info.is_none() {
            inner.remove_from_lru(&blob_ref);
            return;
        }

        inner.current_bytes = inner.current_bytes.saturating_add(entry.cached_byte_len());
        inner.remove_from_lru(&blob_ref);
        inner.lru.push_back(blob_ref.clone());
        inner.entries.insert(blob_ref, entry);
        inner.evict_to_limits();
    }
}

impl InMemoryBlobCacheInner {
    fn touch(&mut self, blob_ref: &BlobRef) {
        self.remove_from_lru(blob_ref);
        self.lru.push_back(blob_ref.clone());
    }

    fn remove_from_lru(&mut self, blob_ref: &BlobRef) {
        self.lru.retain(|candidate| candidate != blob_ref);
    }

    fn evict_to_limits(&mut self) {
        while self.entries.len() > self.limits.max_entries
            || self.current_bytes > self.limits.max_bytes
        {
            let Some(evicted) = self.lru.pop_front() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&evicted) {
                self.current_bytes = self.current_bytes.saturating_sub(entry.cached_byte_len());
            }
        }
    }
}

/// Write-through/read-through cache decorator for any [`BlobStore`].
#[derive(Clone)]
pub struct CachedBlobStore<S> {
    inner: S,
    cache: InMemoryBlobCache,
}

impl<S> CachedBlobStore<S> {
    pub fn new(inner: S, cache: InMemoryBlobCache) -> Self {
        Self { inner, cache }
    }

    pub fn with_limits(inner: S, max_bytes: u64, max_entries: usize) -> Self {
        Self::new(inner, InMemoryBlobCache::new(max_bytes, max_entries))
    }

    pub fn inner(&self) -> &S {
        &self.inner
    }

    pub fn cache(&self) -> &InMemoryBlobCache {
        &self.cache
    }

    pub fn into_inner(self) -> S {
        self.inner
    }
}

#[async_trait]
impl<S> BlobStore for CachedBlobStore<S>
where
    S: BlobStore,
{
    async fn put_bytes(&self, write: BlobWrite) -> Result<BlobRef, BlobStoreError> {
        let expected_ref = BlobRef::from_bytes(&write.bytes);
        let cache_bytes = self
            .cache
            .can_store_bytes(write.bytes.len() as u64)
            .then(|| write.bytes.clone());
        let blob_ref = self.inner.put_bytes(write).await?;
        if blob_ref != expected_ref {
            return Err(BlobStoreError::Store {
                message: format!(
                    "blob store returned non-canonical ref: expected {expected_ref}, got {blob_ref}"
                ),
            });
        }

        if let Some(bytes) = cache_bytes {
            self.cache.insert_bytes(blob_ref.clone(), bytes);
        }
        if let Ok(info) = self.inner.stat_blob(&blob_ref).await {
            self.cache.insert_info(info);
        }

        Ok(blob_ref)
    }

    async fn read_bytes(&self, blob_ref: &BlobRef) -> Result<Vec<u8>, BlobStoreError> {
        if let Some(bytes) = self.cache.get_bytes(blob_ref) {
            return Ok(bytes);
        }

        let bytes = self.inner.read_bytes(blob_ref).await?;
        let actual = BlobRef::from_bytes(&bytes);
        if &actual != blob_ref {
            return Err(BlobStoreError::Store {
                message: format!("blob hash mismatch: expected {blob_ref}, got {actual}"),
            });
        }
        self.cache.insert_bytes(blob_ref.clone(), bytes.clone());
        Ok(bytes)
    }

    async fn has_blob(&self, blob_ref: &BlobRef) -> Result<bool, BlobStoreError> {
        if self.cache.contains(blob_ref) {
            return Ok(true);
        }
        self.inner.has_blob(blob_ref).await
    }

    async fn stat_blob(&self, blob_ref: &BlobRef) -> Result<BlobInfo, BlobStoreError> {
        if let Some(info) = self.cache.get_info(blob_ref) {
            return Ok(info);
        }

        let info = self.inner.stat_blob(blob_ref).await?;
        self.cache.insert_info(info.clone());
        Ok(info)
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
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    #[test]
    fn in_memory_blob_cache_evicts_least_recently_used_entry() {
        let cache = InMemoryBlobCache::new(6, 2);
        let first = BlobRef::from_bytes(b"one");
        let second = BlobRef::from_bytes(b"two");
        let third = BlobRef::from_bytes(b"tre");

        cache.insert_bytes(first.clone(), b"one".to_vec());
        cache.insert_bytes(second.clone(), b"two".to_vec());
        assert_eq!(cache.get_bytes(&first), Some(b"one".to_vec()));
        cache.insert_bytes(third.clone(), b"tre".to_vec());

        assert_eq!(cache.get_bytes(&first), Some(b"one".to_vec()));
        assert_eq!(cache.get_bytes(&second), None);
        assert_eq!(cache.get_bytes(&third), Some(b"tre".to_vec()));
        assert_eq!(cache.stats().current_bytes, 6);
        assert_eq!(cache.stats().entries, 2);
    }

    #[test]
    fn in_memory_blob_cache_skips_oversized_bytes_but_keeps_metadata() {
        let cache = InMemoryBlobCache::new(2, 8);
        let blob_ref = BlobRef::from_bytes(b"large");
        let info = BlobInfo {
            blob_ref: blob_ref.clone(),
            byte_len: 5,
            child_refs: Vec::new(),
        };

        cache.insert_blob(info.clone(), b"large".to_vec());

        assert_eq!(cache.get_bytes(&blob_ref), None);
        assert_eq!(cache.get_info(&blob_ref), Some(info));
        assert_eq!(cache.stats().current_bytes, 0);
        assert_eq!(cache.stats().entries, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cached_blob_store_reads_through_then_hits_cache() {
        let inner = CountingBlobStore::new();
        let blob_ref = inner
            .put_bytes(BlobWrite {
                bytes: b"hello".to_vec(),
                child_refs: Vec::new(),
            })
            .await
            .expect("write blob");
        inner.reset_counts();
        let store = CachedBlobStore::with_limits(inner.clone(), 1024, 8);

        assert_eq!(
            store.read_bytes(&blob_ref).await.expect("read first"),
            b"hello".to_vec()
        );
        assert_eq!(
            store.read_bytes(&blob_ref).await.expect("read second"),
            b"hello".to_vec()
        );
        assert_eq!(inner.read_count(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cached_blob_store_stats_through_then_hits_cache() {
        let inner = CountingBlobStore::new();
        let child = BlobRef::from_bytes(b"child");
        let blob_ref = inner
            .put_bytes(BlobWrite {
                bytes: b"hello".to_vec(),
                child_refs: vec![child.clone()],
            })
            .await
            .expect("write blob");
        inner.reset_counts();
        let store = CachedBlobStore::with_limits(inner.clone(), 1024, 8);

        assert_eq!(
            store
                .stat_blob(&blob_ref)
                .await
                .expect("stat first")
                .child_refs,
            vec![child.clone()]
        );
        assert_eq!(
            store
                .stat_blob(&blob_ref)
                .await
                .expect("stat second")
                .child_refs,
            vec![child]
        );
        assert_eq!(inner.stat_count(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cached_blob_store_put_uses_backing_store_metadata() {
        let inner = InMemoryBlobStore::new();
        let first_child = BlobRef::from_bytes(b"first-child");
        let second_child = BlobRef::from_bytes(b"second-child");
        let store = CachedBlobStore::with_limits(inner, 1024, 8);

        let blob_ref = store
            .put_bytes(BlobWrite {
                bytes: b"same".to_vec(),
                child_refs: vec![first_child.clone()],
            })
            .await
            .expect("write first");
        store
            .put_bytes(BlobWrite {
                bytes: b"same".to_vec(),
                child_refs: vec![second_child],
            })
            .await
            .expect("write duplicate");

        assert_eq!(
            store
                .stat_blob(&blob_ref)
                .await
                .expect("stat cached blob")
                .child_refs,
            vec![first_child]
        );
    }

    #[derive(Clone, Default)]
    struct CountingBlobStore {
        inner: InMemoryBlobStore,
        counts: Arc<CountingBlobStoreCounts>,
    }

    #[derive(Default)]
    struct CountingBlobStoreCounts {
        reads: AtomicUsize,
        stats: AtomicUsize,
    }

    impl CountingBlobStore {
        fn new() -> Self {
            Self::default()
        }

        fn reset_counts(&self) {
            self.counts.reads.store(0, Ordering::SeqCst);
            self.counts.stats.store(0, Ordering::SeqCst);
        }

        fn read_count(&self) -> usize {
            self.counts.reads.load(Ordering::SeqCst)
        }

        fn stat_count(&self) -> usize {
            self.counts.stats.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl BlobStore for CountingBlobStore {
        async fn put_bytes(&self, write: BlobWrite) -> Result<BlobRef, BlobStoreError> {
            self.inner.put_bytes(write).await
        }

        async fn read_bytes(&self, blob_ref: &BlobRef) -> Result<Vec<u8>, BlobStoreError> {
            self.counts.reads.fetch_add(1, Ordering::SeqCst);
            self.inner.read_bytes(blob_ref).await
        }

        async fn has_blob(&self, blob_ref: &BlobRef) -> Result<bool, BlobStoreError> {
            self.inner.has_blob(blob_ref).await
        }

        async fn stat_blob(&self, blob_ref: &BlobRef) -> Result<BlobInfo, BlobStoreError> {
            self.counts.stats.fetch_add(1, Ordering::SeqCst);
            self.inner.stat_blob(blob_ref).await
        }
    }
}
