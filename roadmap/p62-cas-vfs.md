# P62: CAS-Backed Virtual Filesystem

**Status**
- Accepted direction
- Partially implemented: G1-G2.5, first-cut G3, and first-cut G4 in
  `crates/vfs`, `crates/tools`, `crates/store-fs`, and `crates/store-pg`
- Deferred until real VM host targets exist: G5 and the host-target portion of
  G6.
- Deferred until a concrete product write/sync surface exists: G3 workspace
  quotas and mount policy limits.
- G7 now has first-cut CLI-local snapshot/materialize plus session VFS
  workspace/mount APIs and hosted worker VFS tool execution. Higher-level
  bidirectional sync remains future work.

## Goal

Add a CAS-backed virtual filesystem layer that can represent immutable file
tree snapshots and writable working trees independently of any local worker
filesystem, VM, or sandbox.

The first driver is skills: Forge should be able to snapshot skill directories
into CAS, expose those snapshots to the model as a filesystem, and materialize
them into a host target when scripts or conventional process tools need real
paths. The same VFS layer should also support writable filesystem tools for
virtual workspaces and generated artifacts.

The VFS should be generic enough to also support:

- uploaded user files,
- generated artifacts,
- tool output bundles,
- repository or workspace snapshots,
- session-scoped writable workspaces,
- read-only mounted resources,
- future copy-on-write overlays.

## Context

Forge's current runtime stores large context, tool documents, provider-native
items, and run inputs in CAS. That is sufficient for single blobs, but not for
directory-shaped resources. Skills, templates, examples, and bundled assets are
tree-shaped. A skill may contain:

```text
skill-name/
  SKILL.md
  references/
  scripts/
  assets/
```

If Forge only stores `SKILL.md` as one blob, references and scripts lose their
relative path identity. If Forge leaves the directory only in a VM or worker
filesystem, replay and hosted durability depend on mutable external state.

The missing abstraction is a stable, content-addressed filesystem tree.

## Non-Goals

- Do not build a full POSIX filesystem in the first cut.
- Do not put VFS scanning, materialization, or path I/O in `engine`.
- Do not require every VFS snapshot to be materialized into a Unix directory.
- Do not make process execution work against CAS by pretending a process can
  see virtual paths.
- Do not implement git semantics, permissions, hard links, device files, or
  file locking.
- Do not expose a public user-facing file manager API until the agent product
  needs it.

## Design Position

Build VFS as a runtime/storage concern, not as a reducer concern.

`engine` may store and replay `BlobRef`s that point at VFS manifests, and it
may record context items whose payload is a VFS snapshot ref. It should not
parse manifests, list directories, enforce mount policy, or perform I/O.

Target dependency shape:

```text
engine
  owns BlobRef and deterministic context/tool records

vfs
  owns path normalization, manifest schemas, CAS tree read/write helpers

tools
  may provide a FileSystem adapter over vfs
  may provide materialization helpers for host targets

worker/gateway
  own discovery, upload, snapshot, materialization, policy, and API wiring
```

The exact crate name is open. `crates/vfs` is a good first name if it contains
pure VFS data structures plus CAS helpers. If implementation becomes tightly
bound to storage backends, `crates/store-vfs` is acceptable. Avoid putting the
core VFS model inside `tools`; skills and uploads should not need to depend on
host tool code.

## Core Concepts

### VFS Path

Use normalized POSIX-style paths inside VFS manifests.

Rules:

- `/` is the root.
- Separators are `/`.
- Empty components, `.`, `..`, and NUL bytes are invalid.
- Paths are case-sensitive.
- A manifest stores paths relative to the snapshot root, but APIs may expose
  absolute VFS paths for ergonomics.

This should be a distinct type such as `VfsPath`, even if it mirrors
`tools::host::fs::FsPath`.

### Snapshot

A VFS snapshot is an immutable tree rooted at one manifest blob. It is the
stable storage unit for replay, sharing, and commit history.

First-cut manifest shape:

```rust
pub struct VfsSnapshotManifest {
    pub schema_version: String, // "forge.vfs.snapshot.v1"
    pub root: VfsDirectory,
    pub totals: VfsTotals,
}

pub struct VfsDirectory {
    pub entries: BTreeMap<String, VfsEntry>,
}

pub enum VfsEntry {
    File(VfsFile),
    Directory(VfsDirectory),
}

pub struct VfsFile {
    pub blob_ref: BlobRef,
    pub size_bytes: u64,
    pub media_type: Option<String>,
    pub executable: bool,
}

```

The manifest itself is stored in CAS. Its `BlobRef` is the snapshot ref.

The v1 manifest can be a single recursive JSON document. That is simpler and
good enough for skills and small bundles. If repository snapshots become large,
add chunked directory objects later without changing higher-level semantics.

### Writable Workspace

A writable VFS workspace is a mutable view over an optional base snapshot plus
a copy-on-write overlay.

The tool-facing filesystem should support ordinary file operations:

- read file,
- write file,
- edit file,
- apply patch,
- create directory,
- list directory,
- get metadata,
- remove,
- copy,
- grep,
- glob.

Process execution is not a VFS operation. If a process needs files, materialize
the workspace or a committed snapshot into a host target first.

Workspace model:

```rust
pub struct VfsWorkspace {
    pub workspace_id: VfsWorkspaceId,
    pub base_snapshot_ref: Option<BlobRef>,
    pub overlay_ref: BlobRef,
}

pub enum VfsOverlayEntry {
    UpsertFile(VfsFile),
    UpsertDirectory,
    Remove,
    CopyFrom { source_path: VfsPath },
}
```

The exact overlay encoding can be optimized later. The important behavior is:

- reads resolve overlay first, then base snapshot;
- writes affect only the workspace overlay;
- committing the workspace writes a new immutable snapshot manifest;
- the base snapshot remains unchanged;
- every committed state has a `BlobRef` that can be recorded in logs/context.

The first implementation may store a fully materialized tree after each write
if that is simpler. The public behavior should still be copy-on-write snapshots
and explicit commits.

### Commit

Committing a writable workspace produces a new immutable snapshot:

```rust
pub struct CommitVfsWorkspaceRequest {
    pub workspace_id: VfsWorkspaceId,
}

pub struct CommitVfsWorkspaceResult {
    pub snapshot_ref: BlobRef,
    pub files: u64,
    pub bytes: u64,
}
```

Runtime code decides when to commit. For agent tools, reasonable first-cut
policies are:

- commit after every mutating tool call for durability and simple replay, or
- commit at run boundaries while keeping overlay state in runtime storage.

Prefer commit-after-mutation initially unless benchmarks show it is too
expensive. It makes tool effects easy to audit and recover.

### Stable Versus Descriptive Metadata

Do not include volatile fields such as wall-clock timestamps, host inode
numbers, or absolute host paths in the content-addressed manifest unless they
are explicitly part of the snapshot identity.

Use separate catalog records for descriptive metadata:

```rust
pub struct VfsSnapshotRecord {
    pub snapshot_ref: BlobRef,
    pub source: VfsSnapshotSource,
    pub display_name: Option<String>,
    pub created_at_ms: i64,
}

pub struct VfsSnapshotSource {
    pub kind: String,
    pub subject: Option<String>,
    pub metadata_ref: Option<BlobRef>,
}
```

`VfsSnapshotRecord` can live in Pg or another runtime store. The manifest
should remain stable for the same tree contents.

`VfsSnapshotSource` is descriptive provenance only. `kind` values such as
`skill`, `upload`, `host_directory`, or `workspace_commit` are runtime/product
conventions, not VFS schema variants. Put provider-specific details in a
metadata blob referenced by `metadata_ref`.

### Root Ref Catalog

CAS snapshots are immutable. A `snapshot_ref` is the manifest `BlobRef`; it
does not know whether it is latest or mounted anywhere. Mutable root refs live
in a catalog outside CAS.

First-cut catalog records:

```rust
pub struct VfsWorkspaceRecord {
    pub workspace_id: VfsWorkspaceId,
    pub base_snapshot_ref: Option<BlobRef>,
    pub head_snapshot_ref: BlobRef,
    pub revision: u64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

pub struct VfsMountRecord {
    pub session_id: SessionId,
    pub mount_path: VfsPath,
    pub source: VfsMountSource,
    pub access: VfsMountAccess,
}

pub enum VfsMountSource {
    Snapshot { snapshot_ref: BlobRef },
    Workspace { workspace_id: VfsWorkspaceId },
}
```

Use compare-and-set semantics when an expected revision is supplied while
advancing a workspace head:

```rust
pub struct CompareAndSetVfsWorkspaceHead {
    pub workspace_id: VfsWorkspaceId,
    pub expected_revision: Option<u64>,
    pub new_head_snapshot_ref: BlobRef,
    pub updated_at_ms: i64,
}
```

This prevents concurrent mutating tools from losing updates when they know the
base revision; callers can omit `expected_revision` for an intentional
last-writer-wins update. Temporal workflow state may cache these records, but
the durable authority should be a runtime catalog store. `store-fs` can
implement this as JSON files for local/dev, and `store-pg` can implement it as
hosted tables.

### Mount

A mount maps a VFS snapshot or writable workspace into a session-visible
namespace.

Example mount table:

```text
/skills/openai-docs        -> blob:...
/uploads/user-attachments  -> blob:...
/artifacts/run-7           -> blob:...
```

Mounts are runtime state, not engine state, unless a mounted tree or workspace
needs to be part of model context or replay. When a mount is passed into the
model, record the mount description and snapshot/workspace ref in CAS/context.

Mounts can be read-only or writable:

```text
/skills/openai-docs        read-only snapshot
/workspace                 writable workspace
/artifacts/run-7           read-only snapshot
```

Mutating filesystem tools must fail clearly on read-only mounts.

### Materialization

Materialization copies a VFS snapshot to a real filesystem visible to a host
target. It is required when a process must open files by path:

```text
CAS snapshot
  -> host target "vm-123"
  -> /tmp/forge/vfs/<snapshot-digest>/
```

Materialization must be target-scoped. A path on one VM is not meaningful for
another VM.

First-cut materialization behavior:

- idempotently create a directory for the snapshot digest,
- write files with safe permissions,
- create directories before files,
- optionally set executable mode where supported,
- return the target id and materialized root path.

Do not let a model infer that `/skills/foo/...` exists inside a VM unless that
snapshot has been materialized into that VM and the materialized path has been
reported.

## Symlink Policy

Skills and repo snapshots may contain symlinks. Symlinks are also a common
escape vector and complicate mount isolation, replay, and materialization.
Skip them in v1.

First-cut policy:

- Snapshot/import skips symlink entries and records warnings.
- VFS manifests do not contain symlink nodes.
- VFS read/list operations do not resolve symlinks.
- Writable VFS tools do not create symlinks.
- Materialization has no symlinks to recreate.

Later, if a real workflow needs symlinks, add support only for safe relative
symlinks that resolve within the same mounted root. Never allow cross-root or
cross-mount symlink traversal.

## Filesystem Adapter

Expose an adapter that can satisfy the existing host filesystem trait:

```rust
pub struct CasVfsFileSystem {
    pub blobs: Arc<dyn BlobStore>,
    pub mount_table: VfsMountTable,
    pub workspace_store: Arc<dyn VfsWorkspaceStore>,
}
```

It should support:

- `read_file`
- `read_file_text`
- `write_file`
- `create_directory`
- `get_metadata`
- `read_directory`
- `remove`
- `copy`

It should reject writes only when:

- the target path is under a read-only mount,
- the path is outside any mounted root,
- the request violates path, quota, or policy limits.

The adapter can live in `tools` if it implements `tools::host::fs::FileSystem`.
The manifest parser and path model should live in the VFS crate.

Existing filesystem tools should then work over VFS:

- `read_file`
- `write_file`
- `edit_file`
- `apply_patch`
- `grep`
- `glob`
- `list_dir`

If Forge adds explicit metadata, directory-create, remove, or copy tools, those
should use the same VFS filesystem adapter rather than a separate path.

`run_process` and `write_process_stdin` remain host/process tools, not VFS
tools.

## Snapshot Creation

Snapshot creation is a runtime activity:

```rust
pub struct CreateVfsSnapshotRequest {
    pub source: VfsSnapshotInput,
    pub limits: VfsSnapshotLimits,
}

pub enum VfsSnapshotInput {
    InlineFiles(Vec<InlineFile>),
    HostDirectory {
        target: ToolExecutionTarget,
        root_path: String,
        include: Vec<String>,
        exclude: Vec<String>,
    },
    ExistingSnapshot {
        snapshot_ref: BlobRef,
    },
}
```

Limits:

- maximum files,
- maximum total bytes,
- maximum single file bytes,
- maximum directory depth,
- optional allowed extensions/media types,
- maximum skipped symlink warnings.

When snapshotting from a host target, all reads happen through the host
filesystem abstraction, not through the worker's local filesystem.

## CLI-Local Snapshot And Materialization

Local files where the CLI runs need a different path from host-target files.
The gateway usually cannot read or write the CLI user's filesystem. Even when
the gateway happens to run on the same machine during development, the product
contract should not depend on that.

### Snapshot Local CLI Files

For local CLI snapshotting, the CLI is the filesystem reader and the gateway is
the CAS authority:

```text
cli walks local path
  -> applies ignore rules, limits, and symlink-skip policy
  -> uploads file bytes to gateway/CAS
  -> sends VFS manifest commit request with blob refs
  -> gateway validates refs, limits, and manifest shape
  -> gateway stores manifest and returns snapshot_ref
```

For small snapshots, a single `vfs/snapshot/create` request may carry inline
files. For larger trees, use a staged upload:

```text
blob/has_many
blob/put_many
vfs/snapshot/commit
```

The CLI should normalize paths before sending them, but the gateway must still
validate every path and enforce limits. Client-side validation is for fast
feedback only.

### Materialize To Local CLI Files

Materializing a snapshot to the CLI user's local filesystem is also
client-side:

```text
cli asks gateway for snapshot manifest
  -> cli downloads needed blobs
  -> cli writes files under a user-selected local destination
```

The gateway should not attempt to write to the CLI's local filesystem. This
implies a read/download API in addition to host-target materialization:

```text
vfs/snapshot/read
blob/get or vfs/blob/get
```

Local CLI materialization must still enforce safe destination rules:

- require an explicit destination,
- refuse to write outside that destination,
- avoid overwriting unless the user requested it,
- skip symlinks because v1 snapshots do not contain symlink nodes,
- preserve executable bits only where the local platform supports them.

### Host Target Snapshot/Materialization

Host targets are the later but important path. Defer this work until Forge has
real VM host targets and a stable host filesystem protocol; do not implement it
by treating the gateway or worker's local filesystem as a stand-in for a VM.

```text
cli -> gateway: snapshot host directory
gateway/worker -> host target: read/list files
gateway/worker -> CAS: write blobs + manifest

cli -> gateway: materialize snapshot/workspace to host target
gateway/worker -> host target: write files under controlled destination
```

So the boundary rule is:

```text
CLI local filesystem: CLI reads/writes bytes, gateway stores/serves CAS.
Host target filesystem: gateway/worker reads/writes through host abstraction.
```

## Host Target Materialization API

Deferred until real VM host targets exist.

Materialization is also a runtime activity:

```rust
pub struct MaterializeVfsSnapshotRequest {
    pub snapshot_ref: BlobRef,
    pub target: ToolExecutionTarget,
    pub destination_hint: Option<String>,
    pub policy: VfsMaterializationPolicy,
}

pub struct MaterializeVfsSnapshotResult {
    pub snapshot_ref: BlobRef,
    pub target: ToolExecutionTarget,
    pub root_path: String,
    pub files_written: u64,
    pub bytes_written: u64,
    pub warnings_ref: Option<BlobRef>,
}
```

Destination paths are chosen by the runtime or host, not by the model. A hint
can be accepted only after policy checks.

Writable workspaces can be materialized too:

```rust
pub struct MaterializeVfsWorkspaceRequest {
    pub workspace_id: VfsWorkspaceId,
    pub target: ToolExecutionTarget,
    pub destination_hint: Option<String>,
    pub policy: VfsMaterializationPolicy,
}
```

The materialized directory is a point-in-time copy. Process-side mutations do
not automatically update the VFS workspace unless an explicit import/sync step
is later added.

## Engine Interaction

The deterministic engine should see VFS snapshots only as ordinary refs:

- context item payload refs,
- tool argument/result refs,
- optional session config refs,
- optional runtime context items.

Do not add VFS path operations to `CoreAgentCommand`.

If the model needs to browse a VFS snapshot, use tools:

- `vfs.read_file`
- `vfs.list_dir`
- `vfs.grep`
- `vfs.glob`
- `vfs.write_file`
- `vfs.edit_file`
- `vfs.apply_patch`
- `vfs.create_directory`
- `vfs.get_metadata`
- `vfs.remove`
- `vfs.copy`

Those tools can be implemented by runtime/tool packages over `CasVfsFileSystem`.
They should be targetless unless they materialize, import, or read from a host
target.

VFS tool results for mutating operations should include the new committed
snapshot ref or workspace revision ref so the runtime can project and recover
the latest tree.

## Skill Use Case

P63 builds on this VFS layer.

Typical skill flow:

```text
discover skill directory on host target or product bundle
  -> snapshot skill directory into VFS
  -> store snapshot_ref in SkillMetadata
  -> expose /skills/<skill-id>/SKILL.md to model as a VFS path
  -> activation reads SKILL.md from snapshot_ref
  -> scripts require materialization into the selected host target
```

This keeps the activated skill pinned even if the VM's installed skill changes
later.

## Security Rules

- All VFS paths must normalize before use.
- All host snapshotting must stay inside an explicitly allowed root.
- Host snapshot/import must skip symlinks and report warnings.
- Snapshot creation must enforce size and file-count limits before writing
  unbounded data into CAS.
- Writable workspaces must enforce quotas on total bytes, file count, and
  pending overlay size.
- Mutating tools must be blocked on read-only mounts.
- Materialization must write only under a runtime-controlled destination.
- Materialization must never follow unsafe symlinks.
- Skills and other user-provided VFS trees must be treated as untrusted input.
- File media type is advisory only; do not trust it for execution decisions.

## Implementation Slices

### G1: VFS Manifest Model

- Done: add a VFS crate with path normalization and manifest types.
- Done: add encode/decode helpers for `VfsSnapshotManifest`.
- Done: add round-trip tests and path validation tests.

### G2: Snapshot Writer

- Done: implement inline-file snapshot creation over `BlobStore`.
- Done: compute file blob refs and write the manifest to CAS.
- Done: add limits for file count, total bytes, single file bytes, and depth.

### G2.5: Root Ref Catalog Contracts

- Done: define snapshot metadata records.
- Done: define workspace head records with revisioned compare-and-set requests.
- Done: define mount records for session-visible snapshot/workspace roots.
- Done: define catalog/workspace/mount store traits.
- Done: implement `store-fs` JSON-backed catalog records.
- Done: implement `store-pg` catalog tables for hosted use.

### G3: CAS Filesystem Adapter

- Done: implement lookup, read, stat, and list operations over a snapshot ref.
- Done: add a read-only `tools::host::fs::FileSystem` adapter over a snapshot.
- Done: add a writable `VfsWorkspaceFileSystem` over a `VfsWorkspaceStore`
  head. The first implementation rewrites a full manifest and advances the
  workspace head after every mutating filesystem operation.
- Done: support write, create directory, remove, and copy through the writable
  adapter.
- Done: verify existing `read_file`, `write_file`, `edit_file`,
  `apply_patch`, `grep`, `glob`, and `list_dir` tools against a VFS workspace.
- Done: add a mount-table filesystem adapter that resolves mixed read-only
  snapshots and writable workspaces under one filesystem namespace, including
  synthetic parent directories such as `/skills`.
- Done: route cross-mount file and recursive directory copy into writable
  workspace destinations.
- Deferred until a concrete product write/sync surface exists: enforce
  workspace quotas and mount policy limits. This is still required before any
  user-facing sync can accidentally stream unbounded data into CAS, but the
  exact policy should be shaped by that surface.

### G4: Workspace Commit

- Done: commit writable workspace state into immutable snapshot manifests.
- Done: advance workspace heads with compare-and-set revision checks.
- Done: make revision conflicts fail clearly instead of losing updates.
- Done: add tests that the base snapshot is unchanged after writes.
- Done: add tests that a reloaded workspace filesystem reads the committed
  head snapshot.
- Done: first version exposes new snapshot refs and workspace revisions as
  structured, non-model-visible tool effects on mutating VFS workspace tool
  results.
- Remaining: replace full-tree rewrite with a real overlay only if benchmarks
  or product workflows need it.

Implementation note: expose commit refs as structured, non-model-visible tool
result metadata. Prefer a generic tool-effect contract over embedding VFS
knowledge in `engine`; for VFS commits, carry the workspace id, new snapshot
manifest ref, and new revision inline. Do not write a separate effect blob just
to link this metadata; use refs only when the effect naturally points at an
existing CAS object or the metadata is too large for the event/result shape.

### G5: Host Directory Snapshot (Deferred)

Deferred until real VM host targets exist. This should use the eventual VM or
remote host filesystem protocol rather than the gateway or worker's local
filesystem.

- Snapshot a directory through a `HostToolContext` or remote host filesystem.
- Enforce scoped roots and skip symlinks with warnings.
- Add tests with in-memory and scoped local filesystems.

### G6: Host Target Materialization (Deferred)

Deferred until real VM host targets exist. CLI-local materialization belongs in
G7 because the CLI, not the gateway or worker, has access to the user's
filesystem.

- Materialize a snapshot or workspace into a host target.
- Make materialization idempotent by snapshot or committed workspace digest.
- Return root path and warnings.
- Add tests with an in-memory/local host implementation first; remote-host
  materialization can follow when host protocol coverage is ready.

### G7: CLI-Local API And Projection Hooks

Do this before host-target snapshot/materialization. The goal is for the CLI on
the user's computer to be the local filesystem reader/writer while the
gateway/CAS remains the authority for blobs and manifests.

- Done: first-cut JSON-RPC gateway helpers for `blob/put`, `blob/put_many`,
  `blob/get`, `blob/has_many`, `vfs/snapshot/commit`, and
  `vfs/snapshot/read`, including manifest shape validation, referenced-blob
  existence and size checks, and a larger configurable gateway request body
  limit for local uploads.
- Done: reusable CLI snapshot upload flow plus `forge vfs snapshot <dir>`.
  The CLI scans a local directory, skips symlinks, computes content digests,
  checks existing CAS refs with `blob/has_many`, uploads missing unique blobs
  with batched `blob/put_many`, and commits a VFS manifest by ref.
- Done: reusable CLI materialization flow plus
  `forge vfs materialize <snapshot-ref> <dest>`. The CLI reads the snapshot
  manifest, downloads blobs through `blob/get`, skips local files whose digest
  already matches, writes only below the selected destination, refuses
  destination symlink traversal, and applies executable bits conservatively.
- Done: add API/gateway helpers for `vfs/workspace/create`,
  `vfs/workspace/read`, `vfs/workspace/update`, `vfs/workspace/delete`,
  `vfs/mount/put`, `vfs/mount/delete`, and `vfs/mount/list`.
- Done: project session VFS mounts through `SessionView` so clients can see
  mounted paths, sources, access modes, workspace heads, and revisions.
- Done: add explicit CLI commands for workspace create/read/update/delete and
  session mount put/delete/list.
- Done: add `forge chat --mount <dir>`, which snapshots a local directory,
  creates a writable workspace, mounts it at `/workspace` by default, and starts
  the chat against that VFS cwd.
- Done: hosted worker tool execution can load session VFS mounts and run the
  existing DirectFs host tools over `MountedVfsFileSystem`; `FORGE_TOOLS=fake`
  remains available for dev/test fallback.
- Extend CLI-local sync so snapshot/upload and materialize/download can be
  composed into higher-level bidirectional flows.
- Add public API only when a product surface needs direct VFS access.
- Project VFS-backed context items with useful previews.

## Verification

Required tests:

- manifest encode/decode is stable for sorted entries,
- invalid paths are rejected,
- read/list/stat work for nested trees,
- write/edit/apply-patch/remove/copy work through the VFS filesystem adapter,
- mutating operations fail under read-only mounts,
- mutating operations produce recoverable snapshot/workspace revision refs,
- committing a workspace does not mutate its base snapshot,
- snapshot writer deduplicates identical file content through existing CAS
  semantics,
- size/depth/file-count limits fail clearly,
- CLI-local snapshotting uploads blobs and commits a validated manifest,
- CLI-local materialization writes only under the selected destination,
- CLI-local sync preserves unchanged files by digest where practical,
- CLI-local materialization does not write outside its destination,
- CLI-local materialization handles executable bits conservatively.

Deferred host-target tests:

- host snapshotting cannot escape the configured root,
- host snapshotting skips symlinks and records warnings,
- host-target materialization does not write outside its destination,
- host-target materialization handles executable bits conservatively.

Deferred quota/policy tests:

- writable workspace quotas fail clearly once a product write/sync surface
  defines the relevant limits.

## Open Questions

- Should the first manifest be a single recursive JSON document or a directory
  object DAG? Recommendation: single recursive document for P62 v1.
- Should VFS live in `crates/vfs` or `crates/store-vfs`? Recommendation:
  `crates/vfs` for model/helpers; storage backends can remain behind
  `BlobStore`.
- Should mutable overlays be part of P62? Recommendation: no. Add overlays only
  after a concrete workflow needs editable virtual workspaces.
- Should public clients see VFS paths? Recommendation: only through projected
  context and explicit future APIs, not as a required part of `run/start`.
