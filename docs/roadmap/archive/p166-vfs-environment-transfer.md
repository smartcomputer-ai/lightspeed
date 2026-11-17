# P166 — VFS–Environment Transfer: Materialize and Capture

Status: generic scans, incremental transfer sessions, streaming CAS, and VFS model tools implemented, 2026-09-09. Existing workspaces remain supported. Automatic workspace materialization and its UI are parked; VFS retirement and the artifact replacement are also parked.

## VFS expansion freeze

Keep the existing workspace model, session links, and explicit capture and
materialize tools. Park VFS retirement and the artifact replacement until a
concrete continuation is chosen. No workspace migration or replacement resource
model is part of the current work.

Automatic workspace propagation, environment-owned workspace mappings, and
their profile/UI surfaces remain frozen. Do not merge VFS and environment skill
catalogs or use mappings to select a preferred copy or provide a fallback.
[Environment skill discovery](p168-environment-skill-catalogs.md) proceeds with
its own catalog, independently of the existing VFS skill catalog.

The delivered generic scans and explicit transfer primitives remain useful and
supported. This decision takes precedence over the proposal below; its
unimplemented automatic workspace materialization remains parked design context.

## Original scope

Provide explicit transfer between Lightspeed's VFS and an
execution environment. The same operations should handle source trees, scripts,
documents, images, audio, and generated outputs. Register reusable workspace
materializations on the environment so its copies follow workspace changes
automatically. Skills are one consumer, not part of the transfer protocol or
storage model.

## Terms and boundaries

Use **VFS–environment transfer** for the feature and these verbs for its directions:

- **Materialize:** write a VFS file or directory snapshot into an environment.
- **Capture:** read an environment file or directory into VFS storage.

These names describe the useful asymmetry: materialization starts from VFS
content; capture creates VFS content from an environment filesystem. CAS is
the underlying byte storage, not a separate artifact-management feature. Avoid
upload/download, whose direction depends on which participant is speaking.
Registered workspace materializations provide automatic one-way propagation;
they do not provide bidirectional synchronization or merge competing edits.

VFS and environment paths remain distinct domains. Transfer has explicit
source and destination identities. It does not mount a workspace into an
environment or route ordinary file reads across domains. One-off transfers
remain copies. Explicitly registered workspace mappings authorize subsequent
automatic copies to their configured destinations; VFS links alone do not.

Related work:

- [Filesystem domains](archive/p113-explicit-vfs-and-environment-tool-domains.md)
  establishes the separation this feature preserves.
- [Skill simplification](p167-simplify-skill-activation.md) makes skill use an
  ordinary file read.
- [Environment skill catalogs](p168-environment-skill-catalogs.md) discovers
  directories regardless of how they arrived in an environment.
- [Shared content](p160-shared-context-content-and-projections.md) supplies the
  existing content descriptors and provenance used by model adapters.
- [Profile provisioning](archive/p125-profile-provisioned-environments.md)
  establishes the pattern of copying initial settings onto an environment.

## Storage model

Reuse CAS for file bytes and VFS snapshots for directory trees. VFS already
records a file's blob reference, size, media type, and executable flag. A
workspace supplies an editable, readable organization of those snapshots;
CAS and workspaces are complementary layers, not competing storage choices.

A source is either a VFS file or a subtree of a VFS directory snapshot.
When a caller supplies a live workspace path, resolve its head once before
starting materialization and record the resolved snapshot. Retries of that
operation use the same bytes. A later transfer can resolve a newer head.

Capture returns a VFS file descriptor or a VFS directory snapshot reference.
The result can be linked into a workspace, attached to context, or used as a
later materialization source. Capturing bytes need not create a workspace.

Record the usual blob containment edges and retain the result through its
durable owner, such as a recorded tool result, workspace, or environment
materialization record. Merely returning a hash must not leave a completed
capture unrooted for CAS collection.

## Operations

The following signatures describe behavior, not final public DTOs or tool names:

```text
materialize(vfs_source, environment_id, destination_path, on_existing)
  -> source_ref, environment_id, destination_path, transferred_totals

capture(environment_id, source_path)
  -> file_or_snapshot_ref, environment_id, source_path, captured_totals
```

Capture environment identity when the operation is scheduled, as ordinary
environment tool calls already do. Changing the session's selected environment
must not redirect an operation or its retry to another machine.

Expose the operations through ordinary runtime tools and public API adapters.
Use bounded environment I/O and CAS references so file bytes do not pass
through model arguments or expand the deterministic workflow history. Longer
transfers can use existing workflow-backed execution facilities; this proposal
does not require a new job or orchestration system.

### Model tools and capability placement

Give the model a pair of explicit transfer tools. Use `capture` consistently
for collecting environment files into VFS; do not add a second synonymous
`collect` tool. Proposed model-facing names and arguments are:

```text
vfs_materialize(source_vfs_path, destination_environment_path,
                on_existing = replace)

vfs_capture(source_environment_path, destination_vfs_path,
            on_existing = replace)
```

The first copies a VFS file or directory into the selected environment. The
second captures an environment file or directory and publishes it at the
selected VFS workspace path. Internally, capture produces immutable stored
content before the revision-checked workspace update; the model does not need
to orchestrate those two storage steps. The low-level API can still return a
standalone capture reference without publishing it into a workspace.

Place these in the VFS tool family and derive availability from the intersection
of existing VFS and environment grants. No new transfer feature toggle is
needed for the initial version:

| Tool | Session grants | Per-call access |
| --- | --- | --- |
| `vfs_materialize` | VFS tools `readOnly` or `edit`, plus environments | Read the VFS source and write the environment destination. |
| `vfs_capture` | VFS tools `edit`, plus environments | Read the environment source and write the VFS workspace destination. |

Environment access alone does not grant VFS access; VFS access alone does not
grant environment I/O. Check linked-route permissions and live environment
capabilities during execution. A snapshot link remains read-only, and a VFS
edit tool grant cannot make it a capture destination. Keep tool definitions
stable when the selected environment changes; ordinary selection/readiness
errors handle a call with no usable environment.

Use the session's existing VFS paths to identify the model's sources and
destinations. Resolve them through its authorized workspace/snapshot links.
Do not expose an arbitrary universe workspace ID or raw blob reference as a
way for a model call to bypass those links. Each selected VFS transfer path
must resolve within one linked workspace or snapshot; copying a synthetic
directory that spans independent links is outside the initial scope.

Unlike ordinary VFS readers/editors, these two operations explicitly depend
on an environment. Classify them as such in dispatch/readiness and tool-batch
validation even though their model-facing names start with `vfs_`. Capture
the selected environment identity when scheduling, prohibit selection changes
in the same dependent batch, and prevent overlapping transfer destinations
from being published concurrently. Ordinary VFS operations remain independent
of environment availability.

Session grants govern these ad hoc model actions. Environment-owned mappings
continue to apply under their own configured source access without requiring
an attached session or enabling its VFS tools. Calling a transfer tool does
not register a mapping or grant the model permission to edit environment setup.

### Partial transfers and replacement scope

Support a single file, a selected directory subtree, or the whole workspace
from the first version. Both directions take an exact source path and an exact
destination path. A directory source's children become the destination's
children; do not silently append the source directory's basename.

| Example | Scope |
| --- | --- |
| Materialize VFS `/library/skills/review` to environment `/opt/skills/review` | Copy only that skill directory and its contents. |
| Materialize VFS `/project/config.json` to environment `/work/config.json` | Copy one file. |
| Capture environment `/work/results/run-42` to VFS `/project/results/run-42` | Replace only that destination subtree; preserve other runs and project files. |
| Materialize a linked workspace root to environment `/work/project` | Copy the whole linked workspace. |

`replace` applies at the selected destination boundary in either direction.
Replacing `/project/results/run-42` in VFS removes destination-only entries
inside `run-42`, while leaving `/project/src`, `/project/results/run-41`, and
other siblings intact. The same rule applies to environment destinations.
Create missing parent directories where permitted; a conflicting non-directory
ancestor is an error. `error` instead rejects an existing selected target.

Publishing a partial capture creates a new workspace head that references the
new subtree and retains all unaffected entries from the observed head. It does
not transfer the entire workspace's file bytes. Reuse existing CAS references
for untouched files and update the head with the normal revision check. A
concurrent commit conflicts, including one outside the replaced subtree;
automatic rebasing or merging is not part of this feature. Keep the captured
content available if workspace publication fails so it can be inspected or
published again without unnecessarily recapturing mutable source files.

The operation is complete or failed for the selected file/tree, not for the
entire workspace. Do not publish half a selected tree on transfer failure.
No include/exclude globs, arbitrary file selection sets, or multi-destination
transactions are needed initially; callers can select a smaller directory or
make separate transfers.

### Materialize

- Preserve relative paths, bytes, and executable flags for the entire selected
  tree. Materializing a directory is useful without interpreting its contents.
- Support `on_existing = error | replace` from the first version. `error`
  requires an absent destination; `replace` replaces the selected target in
  full. Default to `replace` for both one-off transfers and registered
  materializations. Keep the distinction explicit in the API; the normal UI
  uses replacement without an overwrite-policy selector. An API caller that
  chooses `error` receives a conflict when its destination already exists.
- Replacing a directory removes destination-only entries inside that directory.
  It does not overlay source files onto an existing tree or merge file contents.
  Siblings outside the destination are unaffected. The caller selects the whole
  replacement boundary, rather than a set of per-file conflict policies.
- Stage a tree outside the final destination and publish it after all bytes
  have arrived and been checked. A failed transfer must not expose a partial
  final tree as successfully materialized content. Keep an existing target
  intact during staging. Define recoverable replacement at the endpoint;
  do not assume that ordinary rename atomically replaces a nonempty directory
  on every supported filesystem.
- Use an operation identity and completion receipt to recognize successful
  retries. Do not overwrite subsequent local edits merely because an earlier
  successful transfer is retried.
- Return the actual destination path. Consumers use that path for later reads
  and execution; they do not assume that a VFS path is also an environment path.

### Capture

- Read bytes without requiring UTF-8. Preserve supported file metadata and
  relative paths in the resulting VFS content.
- A capture is a bounded observation of a live filesystem, not an atomic
  operating-system snapshot. Detect observable changes during transfer and
  report an incomplete capture rather than claiming a complete stable tree.
  Applications needing a consistent dataset must first make the source stable.
- Publish a successful directory result only after all selected entries have
  been captured. Missing or unsupported entries and exceeded limits are explicit
  failures, not silently omitted files in a supposedly complete snapshot.
- A capture may remain an independent snapshot. Applying it to a live workspace
  is a separate, revision-checked VFS update using the existing conflict rules.
- Record a successful capture result for retry/replay. A new capture request
  reads the environment again and may produce different VFS content.

## Environment materializations

**Parked:** this section and its subsections describe a possible future design,
not current implementation work or a prerequisite for environment skills.

The persistent configuration belongs to the **environment**, not the session.
An environment is a shared, independently lived filesystem: several sessions,
bots, or jobs can use it, and its files outlive any individual attachment.
Session-owned mappings would let different sessions prescribe conflicting
contents for the same directory and make attachment unexpectedly mutate a
shared machine.

Add an environment-owned collection of named materializations. The primary
source is a workspace plus a path within that workspace. Registering it makes
that VFS subtree the source of truth for the destination: saves by humans,
agents, or API clients automatically schedule an updated copy. An immutable
snapshot is an alternative for callers that need a fixed source. Use stable
workspace identity, not a path in one session's VFS link table.

Illustrative configuration, not final public DTOs:

```json
{
  "materializations": [
    {
      "id": "team-skills",
      "source": {
        "type": "workspace",
        "workspaceId": "team-skills-workspace",
        "path": "/"
      },
      "destinationPath": "/opt/lightspeed/skills/team",
      "onExisting": "replace"
    }
  ]
}
```

The snapshot source variant supplies a snapshot reference and source path
instead of a workspace ID. A setting refers to a source; it does not copy the
source bytes into configuration. Retain explicitly configured snapshot refs
and the resolved snapshot needed by a pending or last applied operation through
environment-owned storage roots.

Validate source access and destination paths when configuring and applying
materializations. Source workspaces/snapshots belong to the environment's
universe. Managing these mappings requires environment configuration authority
and access to the source; selecting an environment alone does not grant the
right to change its mappings. Materialized files are then accessible according
to that environment's ordinary filesystem permissions, independently of any
session's VFS links.

Reject overlapping destination paths within the configured collection,
including ancestor/descendant targets. Do not make application order resolve
competing mappings. Serialize materialization operations that touch the same
environment target, and bind each operation to the configuration revision it
was requested for. A late completion must not mark newer settings as applied.

### Automatic application

Saving a mapping or its source workspace is sufficient. There is no required
manual Apply step and no automatic/manual mode selector in the initial UI.
Use replacement by default and show the workspace, source path, destination,
and a small Updating/Current/Error status. Pending work for an offline
environment is shown as waiting for availability. Retain explicit Retry or
Reapply operations for recovery and API use, not as part of the normal edit flow.

| Action | Effect |
| --- | --- |
| Create an environment with materializations | Apply the initial set once the environment filesystem is ready, before releasing dependent workloads. |
| Add or edit mappings on an existing environment | Save configuration and automatically schedule materialization. |
| Edit the source workspace | Automatically schedule its latest committed contents for every environment mapping of the changed subtree. |
| Materialize a snapshot mapping | Use that fixed snapshot; it has no workspace head to follow. |
| Retry the same transfer operation | Reuse its resolved source and receipt; do not resolve a newer head or repeat a completed replacement. |
| Start a session or use the environment | Wait for relevant pending workspace updates before dispatching environment work. |
| Reconnect or wake the environment | Catch up to current source contents; skip already applied, unchanged content. |
| Remove a mapping or change its destination | Leave the old destination on disk; deletion is a separate filesystem action. |

An explicit Reapply is a new operation even when the source snapshot has not
changed: with `replace`, it can restore the target after local edits. Retrying
an already completed operation only returns its recorded result. Normal
workspace updates automatically create new transfer work and require neither.

Track desired configuration/source revision, last applied revision and source
snapshot, destination, and pending/applied/failed result per mapping. Current
means that the latest desired source contents were applied successfully, not
that the environment directory has been continuously checked for local edits.
The initial version does not monitor or repair environment-side drift.

Initial preparation, ongoing updates, and failures belong to environment
lifecycle/runtime coordination shared across sessions. Saving VFS files does
not power on idle environments just to propagate changes. Keep the latest
pending source and catch up through normal readiness handling on next use.
Registered/external environments follow the same behavior when reachable.

### Scheduling and consistency

Observe committed workspace-head changes at the shared store/runtime boundary
so UI saves, agent file tools, direct API updates, and captures all take the
same path. Persist the desired work or recover it by reconciling workspace heads
against applied revisions. An in-memory notification alone is insufficient;
missed notifications and worker restarts must not leave a mapping stale forever.
Keep storage/domain code free of environment network I/O; runtime coordination
consumes the change and runs the low-level transfer.

Coalesce rapid edits and skip a replacement when the selected subtree's bytes,
paths, and executable metadata are unchanged. A workspace edit outside that
subtree does not need to replace its environment copy. Resolve one committed
snapshot per transfer attempt. If newer source contents arrive during staging,
retain newer desired work and converge to it; do not falsely mark the mapping
current when only an older revision completed. Serialize publication and fence
completion by configuration/source revision so an older retry cannot replace
a newer published target. Cancel obsolete work on mapping removal or a change
of destination before publication.

Automatic propagation is asynchronous, but Lightspeed-controlled operations
need a predictable edit-then-use path. Before dispatching environment file,
process, or job work, observe the configured source revisions and wait until
those revisions, or newer ones for the same configuration, are applied. This
includes an agent's VFS write followed by its next environment command. Use a
fixed observed revision for each wait so a continuously edited workspace does
not create an unbounded moving goal. Propagation failures and timeouts are
reported instead of silently running against known stale files.

This gate belongs in environment use/readiness adapters, not in a session-owned
materialization list or a skill activation mechanism. The transfer's own envd
I/O bypasses the consumer gate so waiting for preparation cannot deadlock its
execution. Existing long-running processes and clients accessing the machine
outside Lightspeed are not paused by this gate.

Do not infer a filesystem reset from a new connection or an ordinary reboot.
If a later environment-rebuild feature replaces storage, that operation can
request fresh initialization explicitly. It must not reuse receipts for the
old filesystem. Automatic rebuild detection is not required here.

Automatic replacement affects every user of the environment. The source
workspace is authoritative for the mapped directory. Local changes, including
destination-only files, can disappear on the next source update; put outputs
and scratch work outside mapped input directories. The UI should explain once
beside the mapping that workspace changes replace the environment copy. Do not
introduce merge prompts or a conflict-resolution workflow for local drift.

Replacement does not isolate existing processes or promise that multiple file
reads by an already running job all observe one source revision. Workloads
requiring a fixed tree should use a snapshot source or a separate one-off copy.

### Profiles and reusable setup

Expose the collection directly on environment creation/settings. A profile
that provisions a new environment may supply its initial materializations,
copied onto the created environment just like initial environment credential
bindings. The environment owns the settings and applied state from then on.

Reapplying the profile must not replace an existing environment's mapping
configuration. Its registered workspace sources continue propagating changes
independently of profile application. Profile modes that select an existing
environment or inherit a parent's environment only select it; they do not
apply session-specific workspace overlays. Keep Lightspeed workspace references
in the Lightspeed environment configuration, not in provider-owned machine
templates or Incus-specific configuration.

One-off session/tool transfers remain useful and use the same primitives.
They do not register or mutate persistent environment mappings. This preserves
task-local transfer without making the session the authority for machine setup.

### Capture back to a workspace

Keep capture explicit. A mapping provides convenient defaults for a Capture
action: its environment destination becomes the capture source, and its
workspace source/path becomes the suggested destination. Capture first creates
a snapshot; publishing it back requires the caller's workspace write access
and an expected workspace revision. Serialize a capture with replacement of
the same target while its files are read. A concurrent workspace edit produces
a conflict, not an automatic merge.

Do not write back on session end, environment stop, or disconnect. Snapshot
sources remain immutable; their captures produce a new snapshot or are saved
to a selected workspace. Capture does not reverse or change a mapping by itself.
Publishing captured contents to a mapped workspace is an ordinary workspace
commit and automatically propagates to its mapped environments. Materialization
does not itself write VFS, so this creates no automatic round-trip loop.

## Reuse and incremental transfer

Replacement describes the final selected tree; it does not require sending
every file's bytes on every operation. Include reuse at whole-file granularity
in the initial transfer design. Build a complete inventory, reuse content the
receiver already has, transfer missing content, and publish the complete result.
Deletion and executable-flag changes can change the tree without transferring
any new file bytes. Deduplicating storage after uploading everything would not
provide this transport saving.

Share bounded traversal, path handling, metadata, and optional content hashing
with the [filesystem scan helper](p168-environment-skill-catalogs.md#efficient-envd-helpers).
A transfer inventory describes every selected file and directory, including
empty directories: relative path, entry kind, and, for files, size, executable
flag, and a digest of the complete file bytes. Use SHA-256 over the exact raw
bytes, identical to the existing CAS file-blob digest. Do not include paths,
executable flags, framing, or text normalization in that hash. A protocol
digest represented as `sha256:<lowercase hex>` directly names the corresponding
CAS blob; no rehashing or content-identity translation is needed. Envd does not
need VFS manifests, workspace identities, or storage
access. A scan's aggregate fingerprint identifies its query result and is
separate from a file's content digest or a persisted VFS snapshot reference.

### Materialization reuse

The resolved VFS snapshot already supplies the desired tree and file content
identities; no environment scan is needed to discover the source. The endpoint
uses those identities to find reusable destination files, verifies their bytes,
and reports the missing content. Send only those missing blobs. A persistent
endpoint content cache is optional; verified files in the existing destination
are sufficient to provide reuse for ordinary updates.

The last applied source manifest can identify reuse candidates, but cannot
prove their current bytes: a process may have edited the environment copy.
Verify reusable bytes while constructing the staged result. Use independent
copies or filesystem-supported copy-on-write clones; hard links to writable
destination files would let concurrent edits corrupt staging. Apply executable
metadata separately and omit destination-only entries from the new tree. The
existing replacement and completion guarantees still apply. This can require
local reads and copies of unchanged files even when no network bytes are needed.

Skipping a registered update whose selected VFS subtree has not changed remains
the cheapest path, with the existing rule that ordinary preparation does not
repair local drift. A new transfer or explicit Reapply must verify the content
it reuses; a completed operation retry still returns its receipt.

### Capture reuse

Envd inventories and hashes the selected source locally. The runtime checks
which content is already available in the authorized CAS scope and requests
only missing bytes, deduplicating repeated content within the transfer too.
Build the new snapshot using both reused and newly stored blobs. Retain reused
content through publication so collection cannot invalidate a successful
presence check. A renamed file can reuse its bytes at a new path, and removed
files disappear from the new selected tree. Workspace publication retains its
existing revision and replacement rules even when every blob was reused.

An inventory observation does not pin mutable environment files. Capture must
bind subsequent reads to the advertised digests and verify the received bytes,
or retain verified immutable staging content for the operation. Detected changes
make the selected capture incomplete; do not silently substitute new bytes for
the inventory's content identities. Reuse does not strengthen capture into an
atomic operating-system snapshot.

### Protocol and cost boundary

Use scans for observation and reuse planning. Transfer operations additionally
own missing-content negotiation, bounded byte streaming, verification, staging,
and completion. Share inventory entry types and implementation where useful;
large transfer inventories need not fit in one small `fs/scan` response. A
partial or metadata-only scan cannot establish that the full selected tree's
content is unchanged or authorize deletion of unobserved entries.

Initially, discovering reusable environment content may require reading and
hashing all selected files locally. The savings are in network transfer and
duplicate storage, not a promise of zero filesystem I/O. Hashing limits must
count inspected bytes even when the response returns only digests. A correct
future change-tracking cache can reduce that work. Whole-file reuse does not
require block-level deltas, chunk deduplication, filesystem watches, or a
bidirectional synchronization protocol; a changed large file transfers in full.
Report observed, reused, and transferred totals separately so savings can be
verified without claiming that a reused file was never read.

### Large trees and one logical transfer

Treat 2 GB and 20 GB selected trees as normal supported workloads, including
individual files larger than an RPC payload. A logical materialize/capture
operation spans bounded protocol exchanges; it must not require holding or
sending the entire tree, or the largest file, in one request or response.
Whole-file reuse and large-transfer support are requirements of the transfer
foundation, not optional optimizations after integrating VFS and mappings.

The transfer sequence is:

1. Establish an operation identity and its fixed source/destination scope.
   Materialization resolves a VFS snapshot; capture observes a selected
   environment tree without claiming an atomic filesystem snapshot.
2. Enumerate paths, kinds, sizes, executable flags, and file digests using the
   shared scan/inventory machinery. Read large inventories in bounded batches
   or streams, with completion and mutation diagnostics for the full inventory.
3. Compare content identities against verified environment reuse candidates
   or available CAS blobs, then negotiate only missing file content.
4. Transfer missing content in bounded chunks with bounded concurrency and
   backpressure. Large files are streamed and hashed incrementally; their
   identity remains SHA-256 of the complete raw bytes. Network chunking does
   not require a new chunk-addressed CAS format or block-level delta algorithm.
5. Verify the complete intended result, then publish the selected target or
   capture reference. Record completion so a lost response can be recovered
   without repeating a successful replacement. Clean up or expire abandoned
   staging and retain reusable content through completion.

The model or UI starts one operation and receives progress/completion through
the existing runtime facilities. It does not enumerate files or drive byte
chunks through model context. Retrying interrupted transfer work should reuse
verified completed blobs; a retry may restart an incomplete file in the first
version. A scan fingerprint alone does not retain bytes or make separately
observed inventory pages consistent. Bind transfer reads to expected digests,
and fail or restart a capture observation when detected mutations invalidate it.

Bound message size and memory independently of configurable operation quotas
for total bytes, entries, staging disk, and lifetime. Do not extend the current
8 MiB inline endpoint by simply raising its constant or splitting the selected
tree into independently published replacements. Stage the full intended result
and remove destination-only entries only as part of its successful publication.
On filesystems without copy-on-write cloning, staging can require local copying
and space for both the old and new trees even when few network bytes change.

Streaming must extend through the runtime and CAS/object-store adapters. The
current `BlobStore::put_bytes(Vec<u8>)` and `read_bytes() -> Vec<u8>` paths buffer
a complete blob; using them unchanged would still buffer a large individual
file even if environment transport is chunked. Provide a bounded storage path
and spool large inventories where needed, with all I/O outside the engine.

## Environment support

Put efficient transfer helpers in envd and describe their capability and wire
contract in `environment-protocol`. The existing filesystem methods provide
byte reads and writes, but writes do not carry executable flags, and directory
publication needs an explicit completion operation. Extend those capabilities
or add a bounded tree-transfer helper rather than encoding transfers as
model-generated shell commands.

Use chunking or streaming with limits on file count, bytes, and operation time.
The runtime bridges the environment connection and storage. The protocol should
describe files, metadata, transfer identity, and completion without requiring
an endpoint to access Lightspeed's database or implement its VFS store.

Define symlink behavior before implementing tree round trips. VFS currently
stores files and directories, not symlink entries. The first implementation
should capture a resolved source directory as ordinary files, including when
the selected root itself is a symlink. Internal links may be expanded when
their targets stay inside that resolved tree. Cycles and links outside the
selected tree must be reported; silently dropping them would corrupt a
snapshot. General preservation of symlinks is a separate extension.

Path handling must keep materialized entries within the destination. Support
ordinary files and directories first; report unsupported filesystem objects
and missing metadata capabilities explicitly. These are transfer semantics,
independent of whether a directory happens to contain a skill.

## Relationship to model content

Keep byte transfer separate from rendering or interpreting bytes for a model:

```text
environment file --capture--> VFS file backed by CAS
VFS file bytes --content handling--> provider-supported model input
```

An image already in CAS can reach a provider without an environment or a VFS
workspace. A document conversion may materialize input into an environment,
run a converter, and capture its outputs. The conversion and provider lowering
belong above this layer. OCR, document rendering, transcription, and provider
media capability negotiation are not implemented by VFS–environment transfer.

### Tool output representation and performance

Whole-file transfer reuse does not require changing ordinary tool-result
storage. Current file tools return structured read metadata and line-numbered
model text; process tools return structured output and a presentation containing
status, handles, and other execution information. The runtime persists both
the structured result and the bounded model-facing projection. These formats
serve model behavior, API inspection, and durable replay. A formatted result
containing file text is not the same CAS object as the raw file bytes.

Preserve those model-facing formats and exact historical observations. The
model does not depend on the storage layout behind the rendered result, but
moving formatting from result creation to request assembly would add projection
work and potentially extra blob reads. Such a change must preserve historical
rendering even after files or renderer implementations change.

Do not require an additional raw file or stdout/stderr blob for every ordinary
tool call as part of this proposal. Adding it alongside both existing payloads
can increase hashing, storage operations, retained bytes, and containment-edge
work; deduplication only avoids storing byte-identical objects again. It does
not itself reduce model input tokens or provider request size. Partial reads
and streamed command output must retain their existing bounds, without reading
extra file bytes or aggregating an entire process output merely to obtain a
whole-file hash.

Raw content already held in VFS or by a capture can be referenced without
creating another byte payload, with the usual retention rules. Broader reuse
for complete environment reads is a separate optimization to evaluate against
the current result layout, rather than simply adding a third representation.
Measure tool and prompt-assembly latency, CAS operations, retained bytes, and
actual later-transfer savings before adopting it. Generic shell output remains
an observed stream; do not infer source files by recognizing commands such as
`cat`. Transfer correctness and efficiency must work independently of whether
the model previously read or printed the source.

## Skill use as an example

A skill saved in a library workspace can be read directly through VFS. A one-off
transfer can materialize the selected skill directory when its scripts are
needed. Preserve the whole directory so relative links to scripts, references,
and assets work together. Environment skill discovery reads the resulting
directory through its configured roots; transfer does not infer or register
skill roots. The two catalogs keep their entries independently, including after
an explicit copy. Automatic propagation and catalog merging remain parked.

A skill installed directly into an environment can be captured into a library
for reuse elsewhere. Discovery and execution do not require that capture to
happen first. Copying a skill does not install its language dependencies,
system packages, external tools, or credentials.

## Implementation and verification

1. Define file/tree inputs, results, limits, collision behavior, and the envd
   capability contract. Reuse existing CAS/VFS representations where possible.
2. Implement bounded capture and materialization, including executable metadata,
   file inventories/digests, missing-content negotiation, whole-file reuse,
   staging, operation receipts, and cleanup after failure.
3. **Parked:** add environment-owned materialization settings/results, automatic workspace
   change propagation, explicit Capture, and creation-time profile defaults.
   Reuse the transfer primitives and existing environment runtime coordination.
4. Connect runtime tools and API operations, workspace source resolution, and
   result retention. Derive the transfer tool pair from VFS/environment grants
   and validate both filesystem domains during execution. Keep all I/O outside
   the engine. **Parked UI portion:** expose workspace-first
   configuration with replacement as the UI default and update status in
   environment details; keep `error | replace` distinct in the API.
5. Verify binary and text round trips, relative paths, executable scripts,
   symlink handling, target conflicts and full replacement, staging/recovery,
   retries, environment switches, capture changes, workspace conflicts, and CAS
   reachability. Verify replacement removes destination-only entries and leaves
   siblings alone for both partial workspace capture and environment
   materialization, and snapshot links reject capture publication.
   Verify repeated transfers reuse existing content, a one-file edit transfers
   only missing file bytes, and deletions, renames, empty directories, and
   executable-only changes produce the correct tree. Cover environment edits
   invalidating reuse candidates, mutation between inventory and byte reads,
   missing/collected blobs, incomplete inventories, and staging isolation.
6. **Parked:** verify all workspace commit paths trigger updates, rapid edits coalesce,
   unrelated subtree edits are skipped, missed notifications/restarts converge,
   and offline environments catch up on use without being woken by every save.
   Verify VFS write followed by environment execution waits for the new content.
7. **Parked:** verify shared-session initialization, overlapping mapping rejection,
   configuration/source revision races, stale retry fencing, removal during a
   transfer, source access, capture coordination, and profile creation defaults.
   Reattachment, profile reapply, and reboot must not replay a completed
   replacement when source contents and configuration are unchanged.
8. Verify tool availability for VFS read/edit with and without environment
   grants, both domains' path access, immutable destinations, environment
   selection races, and partial-capture publication conflicts. A model must
   be able to transfer files through the pair without moving their bytes
   through its own context or constructing storage operations manually.
9. Regenerate API and workflow contracts when their wire surfaces change.
   Update the workspace/environment guides and README when the feature ships.

### Delivered transfer foundation

The implementation now uses `fs/scan` and `fs/transfer`, typed protocol/client
DTOs, bounded inventories, whole-file SHA-256 identities and missing-content
negotiation. File chunks are 256 KiB; scan/staging advances hash or copy at most
4 MiB. Tree/file quotas are independent of these transport bounds. Filesystem
and PostgreSQL/object CAS support bounded range reads and streamed verified
writes; reused CAS refs renew admission grace. Full capture inventories are
spooled for receipt recovery, and completed operations release payload buffers
and open descriptors.

`vfs_materialize` and `vfs_capture` are available through the existing VFS and
environment grants on every provider surface. Their logical IDs are
`vfs.materialize` and `vfs.capture`. Explicit built-in resource requirements
load both VFS links and the selected environment and govern readiness and
selection-related batch rules. Tool family names do not determine runtime
resource dependencies. Capture saves a snapshot before revision-checked
workspace publication and returns its reference if publication conflicts.
The stored tool result records a containment edge to retain the capture even
when workspace publication fails.
Profiles and sessions share the existing VFS/environment feature grants; no
transfer-specific setting is added. Their shared editor explains the derived
materialize/capture availability beside VFS file tools. Transfer orchestration
receives both contexts directly, outside either filesystem domain's ownership.
Transfer tools use the generic bulk execution budget, not an interactive RPC
budget. Ordinary file-reading tools and their result storage remain unchanged.

VFS tool composition now assembles ordinary file tools and eligible transfers
in one method; the shared registrar consumes that complete family. Built-in
operations use `Materialize` and `Capture`, with their VFS domain supplied
separately. Tool identities and names are handled alongside other VFS tools;
public names, compatibility aliases, and profile/session grants are unchanged.

Missing destination parents are created in both directions. The OS backend
creates environment parents through pinned directory descriptors without
following symlinks; these parents may remain after failed transfers. Capture
prepares missing VFS parents in its private manifest and publishes them with
the captured selection in one revision-checked workspace commit. Non-directory
ancestors and read-only destinations remain errors.

Linux and macOS share the transfer state machine and descriptor-relative
traversal. Only publication and native metadata access differ inside the OS
backend. Staging is private at creation; replacement does not require reading
old content; file/directory swaps are atomic. The old inline methods are retained
as small-copy adapters. There is no `/proc` or `openat2` dependency. Windows has
no implementation yet, but the protocol and shared state machine contain no
Unix descriptor or permission-bit types.

Operation IDs bind retries. The daemon persists completion receipts, rejects
re-execution of interrupted operations after restart, cleans retired trees in
the background and expires abandoned operations. Partial file uploads can be
retried within a live daemon; transparent restart/resume of in-flight file
staging is deliberately not claimed. There is no block-level delta protocol.

The current behavior and exact limits are documented in
[the transfer guide](../documentation/environments/vfs-transfer.md). Configured
root aliases and mounted directories are supported; user-selected symlinks and
special files are rejected. Symlink expansion remains deferred. Capture remains
a checked live observation rather than an atomic OS snapshot.

Tests exercise the serialized RPC client/daemon/VFS/CAS path, files above the old
inline limit, empty directories, rename/mode/delete reuse, same-ID page/chunk
retries, lost completion responses, restart receipts, changed capture sources,
read-only routes, staging permissions, write-only replacement targets, malformed
inputs, overlapping destinations, and workspace publication conflicts. A sparse
20 GiB file verifies bounded scan advancement without a whole-file allocation;
this is not a full 20 GiB throughput benchmark.

Hosted dispatch tests additionally exercise standalone transfer calls through
the per-call and batch paths on every provider surface, including workspace
publication, result retention, completed retries, missing resources and read-only
links. Profile/session grant tests cover absent, sourcing-only, read-only and
editing VFS configurations, and environment readiness/batch tests include both
transfer tools.

The live transfer suite runs named profiles through a real Temporal worker,
PostgreSQL/MinIO storage, and a registered daemon behind the environment
gateway. It exercises all three provider tool surfaces, the VFS/environment
grant matrix, multi-page inventories, executable files, replacement, content
reuse, and capture of 10 MiB binary files into object storage. Conditional
scans use the same registered route. A separate PostgreSQL live test covers
inline and multipart stream ingestion, bounded range reads, deduplicated
admission, and rejection of corrupt streams.
Mixed small/large file regressions verify that every scan and staging read
fits the remaining step allowance, including a partially consumed read buffer.

Atomic publication uses Rustix's flagged rename API across glibc Linux, musl
Linux, and macOS, avoiding a direct dependency on libc's `renameat2` symbol,
which musl does not expose. Regression coverage checks file and directory
publication, overwrite refusal, and retention of the exchanged target in
staging for cleanup.

Descriptor-relative opens, metadata checks, creation, removal, and streaming
directory enumeration also use Rustix, with owned descriptors and no manual
`DIR` lifetime or errno handling. Enumeration retains independent offsets and
bounded cleanup, including non-UTF-8 names and symlinks in retired trees. Process
and process-group signaling use typed Rustix APIs; group IDs with special OS
semantics are rejected. Direct libc usage is limited to macOS metadata-only
open flags and native process inspection.

Progress:

- [x] Generic filesystem scan and bounded transfer session contracts.
- [x] Linux/macOS environment helpers and typed client operations.
- [x] Whole-file reuse and missing-content transfer in both directions.
- [x] Streaming filesystem/object CAS and bounded large-file processing.
- [x] Completion receipts, interruption handling, cleanup and expiry.
- [x] VFS model tools, file/subtree selection and revision-checked publication.
- [ ] Environment materializations, automatic updates, and edit-then-use ordering.
- [ ] Profile provisioning defaults and environment settings UI.
- [ ] Dedicated public management-API transfer methods and transfer UI.
- [ ] Full multi-GB throughput and disk-pressure benchmarking.
