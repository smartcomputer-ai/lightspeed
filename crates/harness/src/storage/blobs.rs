//! Content-addressed blob storage contract.

use crate::BlobRef;
use async_trait::async_trait;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BlobStoreError {
    #[error("blob not found: {blob_ref}")]
    NotFound { blob_ref: BlobRef },

    #[error("blob store failure: {message}")]
    Store { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobInfo {
    pub blob_ref: BlobRef,
    pub byte_len: u64,
}

/// Contents of the well-known constant blobs the deterministic core
/// references by hash without being able to write them itself (for example
/// [`crate::UNAVAILABLE_TOOL_RESULT_CONTENT`]).
pub const ENGINE_BLOB_CONTENTS: [&str; 13] = [
    crate::UNAVAILABLE_TOOL_RESULT_CONTENT,
    crate::TOOL_RUNTIME_BOUNDARY_FAILURE_CONTENT,
    crate::LLM_RUNTIME_BOUNDARY_FAILURE_CONTENT,
    crate::CANCELLED_TOOL_RESULT_CONTENT,
    crate::CODE_TOOL_INTERRUPTED_CONTENT,
    crate::UNUSABLE_TOOL_CALLS_FAILURE_CONTENT,
    crate::RUN_INTERNAL_FAILURE_CONTENT,
    crate::AWAIT_REJECTED_CONTENT,
    crate::AWAIT_BATCH_CONFLICT_CONTENT,
    crate::PROMISE_DEADLINE_EXCEEDED_CONTENT,
    crate::RECEIVER_CANCELLED_CONTENT,
    crate::EMPTY_SUCCESS_CONTENT,
    crate::FAILED_WITHOUT_DETAIL_CONTENT,
];

/// Refs of [`ENGINE_BLOB_CONTENTS`]. A long-running process may reference any
/// of them at any moment, so blob collection pins them: they are never
/// candidates for a sweep regardless of holders or age.
pub fn harness_blob_refs() -> Vec<BlobRef> {
    ENGINE_BLOB_CONTENTS
        .iter()
        .map(|content| BlobRef::from_bytes(content.as_bytes()))
        .collect()
}

/// Stores the well-known constant blobs of [`ENGINE_BLOB_CONTENTS`].
///
/// Runtimes that fulfill core actions must call this before driving sessions;
/// content-addressed puts make repeated calls idempotent.
pub async fn ensure_harness_blobs(blobs: &dyn BlobStore) -> Result<(), BlobStoreError> {
    for content in ENGINE_BLOB_CONTENTS {
        let blob_ref = blobs.put_bytes(content.as_bytes().to_vec()).await?;
        debug_assert_eq!(blob_ref, BlobRef::from_bytes(content.as_bytes()));
    }
    debug_assert_eq!(
        harness_blob_refs(),
        vec![
            crate::unavailable_tool_result_ref(),
            crate::tool_runtime_boundary_failure_ref(),
            crate::llm_runtime_boundary_failure_ref(),
            crate::cancelled_tool_result_ref(),
            crate::code_tool_interrupted_ref(),
            crate::unusable_tool_calls_failure_ref(),
            crate::run_internal_failure_ref(),
            crate::await_rejected_ref(),
            crate::await_batch_conflict_ref(),
            crate::promise_deadline_exceeded_ref(),
            crate::receiver_cancelled_ref(),
            crate::empty_success_ref(),
            crate::failed_without_detail_ref(),
        ]
    );
    Ok(())
}

/// Every string in an opaque JSON document shaped exactly like a canonical
/// `sha256:<64 lowercase hex>` ref, at any depth.
///
/// Only for JSON with no type to walk, such as tool output, where a hash may be
/// unrelated to content and an absent one stays plain data. Session logs use the
/// typed walk in [`super::blob_roots`] instead. Refs inside longer strings
/// (previews, prose) are deliberately not refs.
pub fn collect_blob_refs(value: &serde_json::Value) -> BTreeSet<BlobRef> {
    let mut refs = BTreeSet::new();
    collect_blob_refs_into(value, &mut refs);
    refs
}

fn collect_blob_refs_into(value: &serde_json::Value, refs: &mut BTreeSet<BlobRef>) {
    match value {
        serde_json::Value::String(text) => {
            if let Ok(blob_ref) = BlobRef::parse(text.as_str()) {
                refs.insert(blob_ref);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_blob_refs_into(value, refs);
            }
        }
        serde_json::Value::Object(object) => {
            for value in object.values() {
                collect_blob_refs_into(value, refs);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobEdge {
    pub parent: BlobRef,
    pub child: BlobRef,
    pub edge_kind: String,
}

impl BlobEdge {
    pub fn new(parent: BlobRef, child: BlobRef, edge_kind: impl Into<String>) -> Self {
        Self {
            parent,
            child,
            edge_kind: edge_kind.into(),
        }
    }

    pub fn contains(parent: BlobRef, child: BlobRef) -> Self {
        Self::new(parent, child, "contains")
    }
}

/// Pull-based raw byte source. Implementations honor the requested bound and return
/// an empty chunk at EOF; transport framing never contributes to CAS identity.
#[async_trait]
pub trait BlobSource: Send {
    async fn read_chunk(&mut self, max_bytes: usize) -> Result<Vec<u8>, BlobStoreError>;
}

/// Largest range one [`BlobStore::read_blob_range`] call may request; stores
/// refuse larger ranges, so callers moving bigger chunks read several ranges.
pub const MAX_BLOB_RANGE_BYTES: usize = 1024 * 1024;

#[async_trait]
pub trait BlobStore: Send + Sync {
    async fn put_bytes(&self, bytes: Vec<u8>) -> Result<BlobRef, BlobStoreError>;

    /// Bounded range access for transfer, at most [`MAX_BLOB_RANGE_BYTES`] per call. Ranges
    /// are verified by the receiving whole-file hasher. The fallback deliberately rejects
    /// large blobs instead of buffering them.
    async fn read_blob_range(
        &self,
        blob_ref: &BlobRef,
        offset: u64,
        max_bytes: usize,
    ) -> Result<Vec<u8>, BlobStoreError> {
        if max_bytes > MAX_BLOB_RANGE_BYTES
            || self.stat_blob(blob_ref).await?.byte_len > 8 * 1024 * 1024
        {
            return Err(BlobStoreError::Store {
                message: "streaming blob reads unsupported by this store".into(),
            });
        }
        let bytes = self.read_bytes(blob_ref).await?;
        let start = offset.min(bytes.len() as u64) as usize;
        Ok(bytes[start..start.saturating_add(max_bytes).min(bytes.len())].to_vec())
    }

    /// Publish only after length and raw SHA-256 verification. Persistent stores stream
    /// to a temporary file or multipart object; they never buffer the whole payload.
    async fn put_stream(
        &self,
        expected: &BlobRef,
        size: u64,
        source: &mut dyn BlobSource,
    ) -> Result<BlobRef, BlobStoreError> {
        if size > 8 * 1024 * 1024 {
            return Err(BlobStoreError::Store {
                message: "streaming blob writes unsupported by this store".into(),
            });
        }
        let mut bytes = Vec::new();
        loop {
            let chunk = source.read_chunk(256 * 1024).await?;
            if chunk.is_empty() {
                break;
            }
            if chunk.len() > 256 * 1024 || bytes.len() as u64 + chunk.len() as u64 > size {
                return Err(BlobStoreError::Store {
                    message: "blob stream exceeded declared size/chunk bound".into(),
                });
            }
            bytes.extend(chunk);
        }
        if bytes.len() as u64 != size || BlobRef::from_bytes(&bytes) != *expected {
            return Err(BlobStoreError::Store {
                message: "blob stream length/digest mismatch".into(),
            });
        }
        self.put_bytes(bytes).await
    }

    /// Renew admission grace for reused content before publishing a new reference.
    async fn retain_blob(&self, blob_ref: &BlobRef) -> Result<(), BlobStoreError> {
        self.stat_blob(blob_ref).await.map(|_| ())
    }

    /// Stores a batch of blobs and returns one ref per input blob, preserving
    /// input order.
    ///
    /// Implementations may optimize batch layout, for example by writing small
    /// blobs into immutable packs. This operation is not required to be atomic:
    /// if it returns an error, some earlier writes may already be durable.
    async fn put_many(&self, blobs: Vec<Vec<u8>>) -> Result<Vec<BlobRef>, BlobStoreError> {
        let mut blob_refs = Vec::with_capacity(blobs.len());
        for bytes in blobs {
            blob_refs.push(self.put_bytes(bytes).await?);
        }
        Ok(blob_refs)
    }

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

/// Parent-to-child reachability edges between blobs whose bytes embed refs
/// to other blobs (a VFS manifest and its files, a tool output and its
/// attached assets).
///
/// Edges are recorded by the code that already understands why a blob embeds
/// another ref. They are not part of canonical content hashing, and blob
/// stores never infer edges from opaque bytes: a child with an incoming edge
/// from a live parent stays live until the parent is collected. Session-level
/// roots are not recorded here; the session store derives them from the
/// entries it appends.
#[async_trait]
pub trait BlobGraphStore: Send + Sync {
    async fn record_blob_edges(&self, edges: Vec<BlobEdge>) -> Result<(), BlobStoreError>;
}

/// Record one `contains` edge from `parent` to every distinct child ref a
/// writer just embedded in the parent's bytes. A missing graph store (local
/// runners, tests without a catalog) records nothing; self-references and
/// duplicates are dropped.
pub async fn record_contains_edges(
    blob_graph: Option<&dyn BlobGraphStore>,
    parent: &BlobRef,
    children: impl IntoIterator<Item = BlobRef>,
) -> Result<(), BlobStoreError> {
    let Some(blob_graph) = blob_graph else {
        return Ok(());
    };
    let children: BTreeSet<BlobRef> = children
        .into_iter()
        .filter(|child| child != parent)
        .collect();
    if children.is_empty() {
        return Ok(());
    }
    blob_graph
        .record_blob_edges(
            children
                .into_iter()
                .map(|child| BlobEdge::contains(parent.clone(), child))
                .collect(),
        )
        .await
}

#[async_trait]
impl<T> BlobStore for Arc<T>
where
    T: BlobStore + ?Sized,
{
    async fn put_bytes(&self, bytes: Vec<u8>) -> Result<BlobRef, BlobStoreError> {
        self.as_ref().put_bytes(bytes).await
    }

    async fn read_blob_range(
        &self,
        blob_ref: &BlobRef,
        offset: u64,
        max_bytes: usize,
    ) -> Result<Vec<u8>, BlobStoreError> {
        self.as_ref()
            .read_blob_range(blob_ref, offset, max_bytes)
            .await
    }
    async fn put_stream(
        &self,
        expected: &BlobRef,
        size: u64,
        source: &mut dyn BlobSource,
    ) -> Result<BlobRef, BlobStoreError> {
        self.as_ref().put_stream(expected, size, source).await
    }
    async fn retain_blob(&self, blob_ref: &BlobRef) -> Result<(), BlobStoreError> {
        self.as_ref().retain_blob(blob_ref).await
    }

    async fn put_many(&self, blobs: Vec<Vec<u8>>) -> Result<Vec<BlobRef>, BlobStoreError> {
        self.as_ref().put_many(blobs).await
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

#[async_trait]
impl<T> BlobGraphStore for Arc<T>
where
    T: BlobGraphStore + ?Sized,
{
    async fn record_blob_edges(&self, edges: Vec<BlobEdge>) -> Result<(), BlobStoreError> {
        self.as_ref().record_blob_edges(edges).await
    }
}

/// In-memory blob store for local runners and tests.
///
/// Besides the [`BlobStore`] contract it mirrors the catalog behaviour a
/// collecting backend needs: every put stamps a touch time from an injectable
/// clock, blobs can be listed by last touch and deleted, and recorded
/// [`BlobEdge`]s are kept for inspection. Liveness itself is not modelled
/// here; callers that test collection decide which refs are live.
#[derive(Clone)]
pub struct InMemoryBlobStore {
    inner: Arc<RwLock<InMemoryBlobStoreInner>>,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl Default for InMemoryBlobStore {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct InMemoryBlobStoreInner {
    bytes_by_ref: BTreeMap<BlobRef, Vec<u8>>,
    info_by_ref: BTreeMap<BlobRef, BlobInfo>,
    touched_at_ms: BTreeMap<BlobRef, u64>,
    edges: Vec<BlobEdge>,
}

impl InMemoryBlobStore {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(unix_now_ms))
    }

    /// A store whose touch timestamps come from `clock` instead of the system
    /// time, so tests can age blobs deterministically.
    pub fn with_clock(clock: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(InMemoryBlobStoreInner::default())),
            clock,
        }
    }

    pub async fn insert_text(&self, text: impl Into<String>) -> BlobRef {
        self.put_bytes(text.into().into_bytes())
            .await
            .expect("in-memory blob write should not fail")
    }

    /// Unix milliseconds of the most recent put of `blob_ref`.
    pub fn touched_at_ms(&self, blob_ref: &BlobRef) -> Option<u64> {
        self.inner
            .read()
            .expect("blob store lock poisoned")
            .touched_at_ms
            .get(blob_ref)
            .copied()
    }

    /// Refs whose most recent put is strictly older than `cutoff_ms`, oldest
    /// first: the age-eligible sweep candidates before liveness is applied.
    pub fn blobs_touched_before(&self, cutoff_ms: u64) -> Vec<BlobInfo> {
        let inner = self.inner.read().expect("blob store lock poisoned");
        let mut eligible = inner
            .touched_at_ms
            .iter()
            .filter(|(_, touched_at_ms)| **touched_at_ms < cutoff_ms)
            .filter_map(|(blob_ref, touched_at_ms)| {
                inner
                    .info_by_ref
                    .get(blob_ref)
                    .map(|info| (*touched_at_ms, info.clone()))
            })
            .collect::<Vec<_>>();
        eligible.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then(left.1.blob_ref.cmp(&right.1.blob_ref))
        });
        eligible.into_iter().map(|(_, info)| info).collect()
    }

    /// Removes the blobs and every edge that names one of them as parent;
    /// returns how many blobs existed. Edges pointing at a deleted child are
    /// left in place, mirroring the catalog's `RESTRICT` on children: a
    /// caller that deletes a child with an incoming edge has a bug.
    pub fn delete_blobs(&self, blob_refs: &[BlobRef]) -> usize {
        let mut inner = self.inner.write().expect("blob store lock poisoned");
        let mut deleted = 0;
        for blob_ref in blob_refs {
            if inner.bytes_by_ref.remove(blob_ref).is_some() {
                deleted += 1;
            }
            inner.info_by_ref.remove(blob_ref);
            inner.touched_at_ms.remove(blob_ref);
            inner.edges.retain(|edge| &edge.parent != blob_ref);
        }
        deleted
    }

    /// Every edge recorded through [`BlobGraphStore`], in recording order.
    pub fn edges(&self) -> Vec<BlobEdge> {
        self.inner
            .read()
            .expect("blob store lock poisoned")
            .edges
            .clone()
    }

    /// Refs of every stored blob.
    pub fn blob_refs(&self) -> Vec<BlobRef> {
        self.inner
            .read()
            .expect("blob store lock poisoned")
            .bytes_by_ref
            .keys()
            .cloned()
            .collect()
    }
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[async_trait]
impl BlobStore for InMemoryBlobStore {
    async fn put_bytes(&self, bytes: Vec<u8>) -> Result<BlobRef, BlobStoreError> {
        let blob_ref = BlobRef::from_bytes(&bytes);
        let info = BlobInfo {
            blob_ref: blob_ref.clone(),
            byte_len: bytes.len() as u64,
        };
        let now_ms = (self.clock)();
        let mut inner = self.inner.write().expect("blob store lock poisoned");
        inner.bytes_by_ref.entry(blob_ref.clone()).or_insert(bytes);
        inner.info_by_ref.entry(blob_ref.clone()).or_insert(info);
        // Touch-or-insert: existing content keeps its bytes and moves its
        // touch time forward, exactly like the catalog-backed store.
        let touched = inner
            .touched_at_ms
            .entry(blob_ref.clone())
            .or_insert(now_ms);
        *touched = (*touched).max(now_ms);
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

    async fn retain_blob(&self, blob_ref: &BlobRef) -> Result<(), BlobStoreError> {
        let mut inner = self.inner.write().expect("blob store lock poisoned");
        if !inner.bytes_by_ref.contains_key(blob_ref) {
            return Err(BlobStoreError::NotFound {
                blob_ref: blob_ref.clone(),
            });
        }
        let touched = inner.touched_at_ms.entry(blob_ref.clone()).or_default();
        *touched = (*touched).max((self.clock)());
        Ok(())
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

#[async_trait]
impl BlobGraphStore for InMemoryBlobStore {
    async fn record_blob_edges(&self, edges: Vec<BlobEdge>) -> Result<(), BlobStoreError> {
        let mut inner = self.inner.write().expect("blob store lock poisoned");
        for edge in edges {
            if edge.edge_kind.is_empty() {
                return Err(BlobStoreError::Store {
                    message: "blob edge kind must not be empty".into(),
                });
            }
            for blob_ref in [&edge.parent, &edge.child] {
                if !inner.bytes_by_ref.contains_key(blob_ref) {
                    return Err(BlobStoreError::NotFound {
                        blob_ref: blob_ref.clone(),
                    });
                }
            }
            if !inner.edges.contains(&edge) {
                inner.edges.push(edge);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[tokio::test(flavor = "current_thread")]
    async fn in_memory_blob_store_dedupes_and_reads_text() {
        let store = InMemoryBlobStore::new();
        let first = store
            .put_bytes(b"hello".to_vec())
            .await
            .expect("write blob");
        let second = store
            .put_bytes(b"hello".to_vec())
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
    async fn in_memory_blob_store_touches_existing_content_and_deletes() {
        let now = Arc::new(std::sync::atomic::AtomicU64::new(100));
        let clock_now = now.clone();
        let store =
            InMemoryBlobStore::with_clock(Arc::new(move || clock_now.load(Ordering::SeqCst)));
        let old = store.put_bytes(b"old".to_vec()).await.expect("write");
        now.store(200, Ordering::SeqCst);
        let fresh = store.put_bytes(b"fresh".to_vec()).await.expect("write");
        assert_eq!(store.touched_at_ms(&old), Some(100));
        assert_eq!(
            store
                .blobs_touched_before(150)
                .into_iter()
                .map(|info| info.blob_ref)
                .collect::<Vec<_>>(),
            vec![old.clone()]
        );

        // A put of existing content moves the touch forward without
        // rewriting bytes.
        now.store(300, Ordering::SeqCst);
        store.put_bytes(b"old".to_vec()).await.expect("re-put");
        assert_eq!(store.touched_at_ms(&old), Some(300));
        assert_eq!(
            store
                .blobs_touched_before(250)
                .into_iter()
                .map(|info| info.blob_ref)
                .collect::<Vec<_>>(),
            vec![fresh.clone()],
            "only the blob last put before the cutoff is eligible"
        );

        store
            .record_blob_edges(vec![BlobEdge::contains(old.clone(), fresh.clone())])
            .await
            .expect("record edge");
        assert_eq!(store.edges().len(), 1);
        assert_eq!(store.delete_blobs(std::slice::from_ref(&old)), 1);
        assert!(store.edges().is_empty(), "a deleted parent drops its edges");
        assert!(matches!(
            store.read_bytes(&old).await,
            Err(BlobStoreError::NotFound { .. })
        ));
        assert_eq!(store.delete_blobs(&[old]), 0);
        assert!(store.has_blob(&fresh).await.expect("has"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn in_memory_blob_graph_rejects_edges_to_missing_blobs() {
        let store = InMemoryBlobStore::new();
        let parent = store.put_bytes(b"parent".to_vec()).await.expect("write");
        let missing = BlobRef::from_bytes(b"missing");
        assert!(matches!(
            store
                .record_blob_edges(vec![BlobEdge::contains(parent, missing.clone())])
                .await,
            Err(BlobStoreError::NotFound { blob_ref }) if blob_ref == missing
        ));
    }

    #[test]
    fn collect_blob_refs_finds_exact_ref_strings_at_any_depth() {
        let a = BlobRef::from_bytes(b"a");
        let b = BlobRef::from_bytes(b"b");
        let value = serde_json::json!({
            "content_ref": a.as_str(),
            "nested": [{ "deeper": { "child": b.as_str() } }, a.as_str()],
            "preview": format!("see {a} for details"),
            "upper": a.as_str().to_uppercase(),
            "count": 3,
            "flag": true,
            "none": null
        });
        let refs = collect_blob_refs(&value);
        assert_eq!(refs, BTreeSet::from([a, b]));
    }

    #[test]
    fn context_content_and_provenance_are_both_typed_refs() {
        let payload = BlobRef::from_bytes(b"transcript");
        let origin = BlobRef::from_bytes(b"source audio");
        let input = crate::ContextEntryInput {
            kind: crate::ContextEntryKind::Message {
                role: crate::ContextMessageRole::User,
            },
            content: crate::ContentRef::text(payload.clone()),
            preview: None,
            origin: None,
            provenance_ref: Some(origin.clone()),
            token_estimate: None,
        };
        let value = serde_json::to_value(&input).expect("serialize input");
        assert_eq!(
            super::super::typed_blob_refs(&input).unwrap(),
            BTreeSet::from([payload, origin])
        );
        assert_eq!(
            serde_json::from_value::<crate::ContextEntryInput>(value).unwrap(),
            input
        );
    }

    #[test]
    fn harness_blob_refs_match_the_core_constants() {
        assert_eq!(
            harness_blob_refs(),
            vec![
                crate::unavailable_tool_result_ref(),
                crate::tool_runtime_boundary_failure_ref(),
                crate::llm_runtime_boundary_failure_ref(),
                crate::cancelled_tool_result_ref(),
                crate::code_tool_interrupted_ref(),
                crate::unusable_tool_calls_failure_ref(),
                crate::run_internal_failure_ref(),
                crate::await_rejected_ref(),
                crate::await_batch_conflict_ref(),
                crate::promise_deadline_exceeded_ref(),
                crate::receiver_cancelled_ref(),
                crate::empty_success_ref(),
                crate::failed_without_detail_ref(),
            ]
        );
    }
}
