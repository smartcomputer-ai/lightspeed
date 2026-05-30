# P62: CAS-Backed Virtual Filesystem

**Status**
- Accepted direction
- Not implemented

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
    Symlink(VfsSymlink),
}

pub struct VfsFile {
    pub blob_ref: BlobRef,
    pub size_bytes: u64,
    pub media_type: Option<String>,
    pub executable: bool,
}

pub struct VfsSymlink {
    pub target: String,
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
```

`VfsSnapshotRecord` can live in Pg or another runtime store. The manifest
should remain stable for the same tree contents.

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
- refuse symlinks unless symlink policy explicitly allows them,
- return the target id and materialized root path.

Do not let a model infer that `/skills/foo/...` exists inside a VM unless that
snapshot has been materialized into that VM and the materialized path has been
reported.

## Symlink Policy

Skills and repo snapshots may contain symlinks. Symlinks are also a common
escape vector. Use a conservative first cut:

- Snapshotting from a host target may record symlinks only when the link target
  is relative and stays inside the snapshotted root after lexical resolution.
- Absolute symlinks and `..` escapes are rejected or copied as inert metadata.
- Materialization may skip symlinks by default and return a warning.
- A later trusted-target mode can preserve safe symlinks when the host
  implementation supports `O_NOFOLLOW`-style checks.

For skills specifically, rejecting unsafe symlinks is preferable to silently
following them.

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
- symlink policy.

When snapshotting from a host target, all reads happen through the host
filesystem abstraction, not through the worker's local filesystem.

## Materialization API

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

- Add a VFS crate with path normalization and manifest types.
- Add encode/decode helpers for `VfsSnapshotManifest`.
- Add round-trip tests and path validation tests.

### G2: Snapshot Writer

- Implement inline-file snapshot creation over `BlobStore`.
- Compute file blob refs and write the manifest to CAS.
- Add limits for file count, total bytes, and depth.

### G3: CAS Filesystem Adapter

- Implement lookup, read, stat, and list operations over a snapshot ref.
- Implement write, create directory, remove, and copy against a writable
  workspace overlay.
- Add `tools` adapter implementing `FileSystem` if needed by existing grep/glob
  and edit/apply-patch helpers.
- Add tests for nested directories, missing paths, invalid paths, UTF-8 reads,
  writes, removes, copies, and read-only mount failures.

### G4: Workspace Commit

- Commit writable workspace overlays into immutable snapshot manifests.
- Return new snapshot refs after mutating operations or at explicit commit
  boundaries.
- Add tests that the base snapshot is unchanged after writes.
- Add tests that committed snapshots are readable after runtime restart.

### G5: Host Directory Snapshot

- Snapshot a directory through a `HostToolContext` or remote host filesystem.
- Enforce scoped roots and symlink policy.
- Add tests with in-memory and scoped local filesystems.

### G6: Materialization

- Materialize a snapshot or workspace into a host target.
- Make materialization idempotent by snapshot or committed workspace digest.
- Return root path and warnings.
- Add tests with an in-memory/local host implementation first; remote-host
  materialization can follow when host protocol coverage is ready.

### G7: API And Projection Hooks

- Add internal gateway/worker service helpers for snapshot, workspace, commit,
  and materialization.
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
- writable workspace quotas fail clearly,
- host snapshotting cannot escape the configured root,
- materialization does not write outside its destination,
- materialization handles executable bits conservatively,
- materialization either rejects or safely handles symlinks.

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
