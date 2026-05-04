# P43: CAS and Blob References

**Status** - Priority 0-1 complete; Priority 2 deferred

Implemented so far:

- G1-G5 are implemented in `forge-agent`.
- `BlobRef` is the public model reference and serializes as a plain
  `sha256:<64hex>` string.
- `storage::BlobStore` is the byte substrate with in-memory test support.
- Artifact refs/storage aliases were removed directly without compatibility
  shims.

## Goal

Replace the current generic artifact model with a simpler content-addressed
blob model:

```text
large bytes -> BlobStore -> BlobRef("sha256:<64hex>")
semantic records -> carry BlobRef plus their own preview/media/display metadata
```

The key design decision is that a ref should identify bytes, not describe how
those bytes are used. Prompt bodies, assistant messages, tool arguments, tool
outputs, patches, compaction summaries, and raw provider payloads can all point
at blobs, but the semantic role belongs to the owning model record.

This phase intentionally starts with interfaces and in-memory test support. It
does not implement the production filesystem/object-store CAS backend yet.

## Design Position

### BlobRef

`BlobRef` is the model-facing reference type.

Target shape:

```rust
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BlobRef(String);
```

Serialized form is just a string:

```json
"sha256:0123..."
```

Rules:

- The canonical format is `sha256:<64 lowercase hex chars>`.
- `BlobRef` contains no byte length, media type, preview, URI, or arbitrary
  metadata.
- Rust should use a transparent newtype rather than raw `String` so parsing,
  validation, and type safety stay centralized.
- Store-local paths, object keys, packed ranges, cache paths, and signed URLs
  are backend details and must not leak into model records.

### BlobStore

`BlobStore` is the byte substrate.

Target interface:

```rust
#[async_trait]
pub trait BlobStore: Send + Sync {
    async fn put_bytes(&self, write: BlobWrite) -> Result<BlobRef, BlobStoreError>;
    async fn read_bytes(&self, blob_ref: &BlobRef) -> Result<Vec<u8>, BlobStoreError>;
    async fn has_blob(&self, blob_ref: &BlobRef) -> Result<bool, BlobStoreError>;
    async fn stat_blob(&self, blob_ref: &BlobRef) -> Result<BlobInfo, BlobStoreError>;
}
```

Supporting types:

```rust
pub struct BlobWrite {
    pub bytes: Vec<u8>,
    pub child_refs: Vec<BlobRef>,
}

pub struct BlobInfo {
    pub blob_ref: BlobRef,
    pub byte_len: u64,
    pub child_refs: Vec<BlobRef>,
}
```

Notes:

- `put_bytes` computes the hash from the exact bytes and returns the canonical
  `BlobRef`.
- If a caller supplies child refs, they describe explicit reachability edges
  for GC/provenance. Blob stores must not discover edges by scanning arbitrary
  JSON/text/bytes.
- `read_text` can be a convenience helper on top of `read_bytes`, not a core
  storage primitive.
- The initial in-memory implementation may store `bytes_by_ref` and
  `info_by_ref`.

### Semantic Metadata

No generic `ArtifactRef` or `ArtifactStore` should remain in the core model.

Metadata moves to the records that need it:

```rust
TranscriptItem {
    content_ref: Option<BlobRef>,
    preview: Option<String>,
    media_type: Option<String>,
}

ToolInvocationReceipt {
    output_ref: Option<BlobRef>,
    model_visible_output_ref: Option<BlobRef>,
}
```

Other semantic records may carry their own role-specific fields, for example
`raw_response_ref`, `reasoning_summary_ref`, `source_refs`, or `replacement_refs`.
Those fields should point at `BlobRef` values without embedding generic
artifact metadata.

## Scope

### In scope

- Add `BlobRef` as the replacement for `ArtifactRef`.
- Add `storage::BlobStore`, `BlobWrite`, `BlobInfo`, `BlobStoreError`, and
  `InMemoryBlobStore`.
- Replace model fields that currently use `ArtifactRef` with `BlobRef`.
- Replace `storage::ArtifactStore` usage with `storage::BlobStore`.
- Keep previews/media types on transcript/projection/tool/context records, not
  on refs.
- Preserve deterministic tests for argument-ref loading and tool output refs.
- Add format validation and hash computation tests.

### Out of scope

- Production filesystem CAS.
- Object-store CAS.
- Packed blob layout.
- Signed URLs, external download links, or direct object-store keys.
- Full GC implementation.
- Refactoring transcript/projection query stores beyond the type migration.
- Host filesystem/process tools.

## Target Module Shape

Planned `crates/forge-agent/src/` changes:

- `model/common/blobs.rs`
  - replace `ArtifactRef` with transparent-string `BlobRef`
  - add `BlobRef::parse`, `BlobRef::new_unchecked_for_tests`, and
    `BlobRef::as_str`
- `storage/blobs.rs`
  - `BlobStore`, `BlobWrite`, `BlobInfo`, `BlobStoreError`,
    `InMemoryBlobStore`
- `storage/mod.rs`
  - export blob storage contracts
  - stop exporting artifact storage contracts
- `tools/handler.rs`
  - inject `Arc<dyn BlobStore>` into `ToolInvocationContext`
- `tools/dispatcher.rs`
  - load argument refs through `BlobStore`
  - construct synthetic error outputs through `BlobStore` or deterministic test
    helpers
- model modules
  - replace `ArtifactRef` imports and fields with `BlobRef`

The file `storage/artifacts.rs` should be removed or replaced by
`storage/blobs.rs`.

## AOS Implementation Reference

Useful implementation reference:

- `/Users/lukas/dev/aos/crates/aos-node/src/infra/blobstore/fs_cas.rs`
- `/Users/lukas/dev/aos/crates/aos-node/src/infra/blobstore/cas.rs`
- `/Users/lukas/dev/aos/crates/aos-node/src/infra/blobstore/mod.rs`

Pieces to borrow later:

- `FsCas` local layout: `<cas-root>/<first-two-digest-hex>/<remaining-hex>`.
- Atomic local writes: temp file in shard directory, fsync, rename.
- Verify content hash on every read.
- `HostedCas`: local cache plus remote CAS, write to both, read local first,
  hydrate local cache from remote.
- Object-store root records that map one logical hash to either direct object
  storage or a packed object range.
- Small-blob packing as a backend optimization invisible above `BlobStore`.

Pieces not to copy directly into `forge-agent` core:

- AOS world/universe-specific checkpoint metadata.
- AOS blob put/get workflow effects.
- Synchronous wrappers around async object-store operations.
- Host/process/workspace-specific tool code.

Forge should keep CAS as runner/storage infrastructure. The deterministic core
should mostly see `BlobRef` values in events, receipts, snapshots, and semantic
records.

## Refactor Strategy

This code is still in early build and is not a compatibility surface. The
change should be aggressive and direct:

- Do not keep public compatibility aliases.
- Do not support both artifact and blob terminology at the same time.
- Do not add intermediate migration shims.
- Rename and refactor the model/storage/tool APIs in one coherent slice.

Steps:

1. Replace `ArtifactRef` with `BlobRef`.
2. Replace `storage::ArtifactStore` with `storage::BlobStore`.
3. Move previews/media types/metadata from refs onto semantic records where
   needed.
4. Delete artifact types/files/re-exports in the same implementation slice.
5. Fix tests and call sites against the final blob API only.

## Priority 0: Model and Interface

### [x] G1. Define BlobRef

- Add transparent-string `BlobRef`.
- Validate canonical `sha256:<64hex>` format.
- Provide hash computation helper from bytes.
- Keep serialized form as a JSON string.

Acceptance:

- JSON round trip emits/accepts a plain string.
- Invalid hash strings fail loudly.
- Unit tests cover valid/invalid refs and byte hashing.

Implementation:

- Added transparent `BlobRef`, `BlobRef::parse`, `BlobRef::from_bytes`,
  `BlobRef::as_str`, and `BlobRef::new_unchecked_for_tests`.
- Added SHA-256 hash computation and canonical format validation.

### [x] G2. Define BlobStore

- Add `storage::BlobStore` and in-memory implementation.
- Support put/read/has/stat.
- Record explicit child refs supplied at write time.

Acceptance:

- Writing identical bytes returns the same `BlobRef`.
- Reading verifies identity by hash in implementations that materialize bytes.
- `BlobInfo` includes byte length and explicit child refs.

Implementation:

- Added `storage::BlobStore`, `BlobWrite`, `BlobInfo`, `BlobStoreError`, and
  `InMemoryBlobStore`.
- In-memory storage dedupes by blob hash and records explicit child refs.

### [x] G3. Wire tools to BlobStore

- Update `ToolInvocationContext` to expose blob storage.
- Update dispatcher argument-ref loading.
- Update testing tool helpers to write/read blob refs.

Acceptance:

- Existing tool dispatcher tests pass after replacing artifact terminology.
- Handler authors no longer import artifact storage types.

Implementation:

- Updated `ToolInvocationContext` to expose `blobs: Arc<dyn BlobStore>`.
- Updated dispatcher argument-ref loading to use `BlobStore`.
- Updated testing handlers to write model-visible output through `BlobStore`.

## Priority 1: Model Migration

### [x] G4. Replace ArtifactRef in model records

- Migrate state, context, transcript, effect, batch, and projection records to
  `BlobRef`.
- Rename generic `artifact_refs` fields where they are not semantically useful.
- Keep record-local previews/media metadata where needed.

Acceptance:

- Public model no longer exposes `ArtifactRef`.
- Existing tests pass with updated blob refs.

Implementation:

- Replaced model, loop, tool, storage, and testing call sites with `BlobRef`.
- Renamed generic compaction/artifact fields and variants to blob terminology.
- Removed preview/media/metadata from refs; previews stay on projection and
  transcript records.

### [x] G5. Remove artifact storage aliases

- Delete `storage/artifacts.rs`.
- Remove artifact re-exports from `storage/mod.rs`.
- Update spec/roadmap wording once code is migrated.

Acceptance:

- `rg "ArtifactRef|ArtifactStore|ArtifactWrite|artifact store"` returns no
  public SDK usage in `crates/forge-agent/src`.
- `cargo test -p forge-agent` passes.

Implementation:

- Replaced `storage/artifacts.rs` with `storage/blobs.rs`.
- Removed artifact storage re-exports from `storage/mod.rs`.

## Priority 2: Future Backend Notes

### [ ] G6. Filesystem CAS backend

- Implement local sharded filesystem CAS after the interface migration.
- Follow AOS `FsCas` write/verify behavior.
- Keep backend paths private to the implementation.

### [ ] G7. Hosted/object-store CAS backend

- Add local-cache-plus-remote CAS later.
- Keep direct/packed object layouts invisible above `BlobStore`.
- Verify hash on hydrate/read.

### [ ] G8. Reachability and GC

- Define roots from journal events, snapshots, agent versions, transcript
  projections, and explicit blob child refs.
- Do not scan opaque blob bytes for refs.
- Add GC only after production storage exists.
