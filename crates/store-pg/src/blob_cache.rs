//! In-memory CAS blob cache, keyed by `(universe_id, blob_ref)`.
//!
//! Content-addressed blobs are immutable, so a positive-only cache never goes
//! stale — there is no invalidation surface, only a memory budget. Entries are
//! byte-weighted and evicted LRU-ish by moka's TinyLFU policy.
//!
//! Tenancy: the key includes the universe id, mirroring the `cas_blobs`
//! primary key `(universe_id, digest)`. A universe looking up another
//! universe's blob misses even when the identical bytes are resident —
//! knowing a hash is not authorization, membership is. Identical bytes used
//! by two universes are cached twice, matching the store's deliberate
//! no-cross-tenant-dedup stance.
//!
//! Only bytes whose hash has been verified may be inserted: reads verify
//! against the blob ref before returning, and writes derive the ref from the
//! bytes, so both wiring points in `PgStore` uphold this by construction.

use std::sync::Arc;

use engine::BlobRef;
use uuid::Uuid;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct BlobCacheKey {
    universe_id: Uuid,
    blob_ref: String,
}

pub struct BlobCache {
    cache: moka::sync::Cache<BlobCacheKey, Arc<[u8]>>,
    max_entry_bytes: usize,
}

impl BlobCache {
    /// `max_bytes` is the total budget; entries larger than
    /// `max_entry_bytes` bypass the cache entirely so one large media blob
    /// cannot flush the working set.
    pub fn new(max_bytes: u64, max_entry_bytes: usize) -> Self {
        let cache = moka::sync::Cache::builder()
            .max_capacity(max_bytes)
            .weigher(|_key: &BlobCacheKey, value: &Arc<[u8]>| {
                u32::try_from(value.len()).unwrap_or(u32::MAX)
            })
            .build();
        Self {
            cache,
            max_entry_bytes,
        }
    }

    pub fn get(&self, universe_id: Uuid, blob_ref: &BlobRef) -> Option<Arc<[u8]>> {
        self.cache.get(&BlobCacheKey {
            universe_id,
            blob_ref: blob_ref.as_str().to_owned(),
        })
    }

    pub fn insert(&self, universe_id: Uuid, blob_ref: &BlobRef, bytes: &[u8]) {
        if bytes.len() > self.max_entry_bytes {
            return;
        }
        self.cache.insert(
            BlobCacheKey {
                universe_id,
                blob_ref: blob_ref.as_str().to_owned(),
            },
            Arc::from(bytes),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hits_return_the_stored_bytes() {
        let cache = BlobCache::new(1024 * 1024, 1024);
        let universe_id = Uuid::from_u128(1);
        let bytes = b"hello cas".as_slice();
        let blob_ref = BlobRef::from_bytes(bytes);
        assert!(cache.get(universe_id, &blob_ref).is_none());
        cache.insert(universe_id, &blob_ref, bytes);
        assert_eq!(
            cache.get(universe_id, &blob_ref).as_deref(),
            Some(bytes),
            "hit must return the inserted bytes"
        );
    }

    #[test]
    fn universes_never_see_each_others_cached_blobs() {
        // Mirrors the `(universe_id, digest)` store key: knowing a hash is
        // not authorization. Universe B misses on bytes cached for A.
        let cache = BlobCache::new(1024 * 1024, 1024);
        let universe_a = Uuid::from_u128(1);
        let universe_b = Uuid::from_u128(2);
        let bytes = b"shared secret bytes".as_slice();
        let blob_ref = BlobRef::from_bytes(bytes);
        cache.insert(universe_a, &blob_ref, bytes);
        assert!(cache.get(universe_a, &blob_ref).is_some());
        assert!(
            cache.get(universe_b, &blob_ref).is_none(),
            "universe B must not read universe A's cached blob"
        );
    }

    #[test]
    fn oversized_entries_bypass_the_cache() {
        let cache = BlobCache::new(1024 * 1024, 8);
        let universe_id = Uuid::from_u128(1);
        let bytes = b"larger than the entry cap".as_slice();
        let blob_ref = BlobRef::from_bytes(bytes);
        cache.insert(universe_id, &blob_ref, bytes);
        assert!(cache.get(universe_id, &blob_ref).is_none());
    }
}
