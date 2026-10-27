# Transfer between VFS and an environment

Use `vfs_materialize` to copy a VFS file or directory onto the active machine,
and `vfs_capture` to save machine output into a VFS workspace. This is the
bridge between an agent's persistent files and the filesystem its commands
use. The copies remain independent after the transfer.

For example, copy a review skill to the machine before running it, then retain
the resulting output in the workspace:

```text
vfs_materialize(
  source_vfs_path = "/workspace/skills/review",
  destination_environment_path = "./skills/review"
)

vfs_capture(
  source_environment_path = "./output",
  destination_vfs_path = "/workspace/results"
)
```

Each destination is exact: the source's basename is not appended. In this
example, the contents of `./output` become `/workspace/results`, not
`/workspace/results/output`. Binary files, empty directories, and executable
flags are preserved on supported Linux and macOS filesystems.

## Prepare the attachments

The session needs an attached workspace or snapshot and an active environment.
The two operations require different access:

| Operation | VFS access | Active environment access |
| --- | --- | --- |
| Materialize | Read or edit | Edit files or higher |
| Capture | Edit on a workspace | Read files or higher |

Snapshot attachments are read-only, so they can supply a materialize source
but cannot receive captured output. Select a path inside one attachment;
a synthetic VFS directory spanning several attachments cannot be transferred
as one tree. Neither operation requires permission to run commands.

The transfer tools appear when the session's attachments permit them. A call
still checks the active environment's access, readiness, and transfer support.
See [Using environments](using-environments.md#choose-the-access-level) for
how attachments and selection work.

## Replace a destination and retain the result

The default `on_existing` policy is `replace`. Replacing a directory removes
entries found only in that destination and publishes the complete replacement
at once. Sibling paths are untouched. Use `on_existing = "error"` when an
existing destination should cause the operation to fail instead.

Missing destination parents are created where permitted. A symlinked
ancestor is followed while it stays inside the daemon's filesystem root; a
destination that is itself a symlink, a special file, or a non-directory
ancestor is rejected. A failed materialize can leave newly created parent
directories, but it does not publish a partial replacement. There is no
automatic merge or ongoing synchronization.

Capture first saves an immutable VFS snapshot with the selected content at
`/selection`, then attempts to update the workspace. If another writer changed
that workspace during transfer, the result reports `published: false` and
retains the snapshot reference. Inspect that result before treating a capture
as published; the saved snapshot lets you recover the output without
overwriting the other writer's changes.

## Content reuse and large files

Transfers compare file-content digests and send only missing bytes. A repeated
copy or rename can therefore reuse content already present at the destination
or in Lightspeed storage. Files still need local verification and sometimes
copying, so reused bytes do not mean the operation has no disk cost.

Large files move in bounded chunks rather than one large tool argument or
response. The daemon's default limits are 100,000 entries, depth 64, one TiB
per file and selected tree, and 24 hours per operation. There are also inventory
size and concurrency limits. Allow enough machine disk for the existing tree,
missing uploads, and the staged replacement. Overlapping materialization
destinations are rejected.

The tool result reports references and counts rather than file payloads. Each
daemon advertises its exact transfer limits when a client connects; the
[transfer types](../../../crates/environment-protocol/src/data/transfer.rs)
define them, and the [transfer implementation](../../../crates/tools/src/transfer.rs)
describes content reuse and workspace publication.

## What a capture selects

A capture copies the whole selected tree, including hidden entries and paths
that `.gitignore` excludes. Version-control metadata directories (`.git`,
`.hg`, `.svn`, and `.jj`) and the daemon's own state directory are never
captured. A symlink whose target stays inside the selected tree is captured as
the file or directory it points to. A link that leaves the selection, a
symlink cycle, or a special file such as a FIFO or device fails the capture.
Names that are not UTF-8 are skipped and counted in the result.

## Environment data-plane API

Clients implementing their own transfers use the `transfer/*` methods of the
[environment protocol](../reference/environment-protocol.md#files-and-transfers).
Every daemon serves them as part of its filesystem family. The daemon
interface handles machine files; the Lightspeed tools above connect it to VFS
and content-addressed storage.

| Methods | Purpose |
| --- | --- |
| `transfer/begin`, `transfer/advance` | Start an operation and advance scanning or staging. |
| `transfer/readInventory`, `transfer/append`, `transfer/listMissing` | Exchange inventory pages and list content the daemon still needs. |
| `transfer/read`, `transfer/write` | Transfer bounded content chunks by digest. |
| `transfer/commit` | Release a capture or publish the staged replacement. |
| `transfer/getStatus`, `transfer/abort` | Inspect progress or a receipt, or abandon incomplete work. |

Keep the same operation ID and input when retrying. The first failure moves an
operation to `failed` with its error, every later call reports that error, and
nothing is committed; start a new operation to try again. A finished
operation's receipt is kept for 24 hours after it finished, across daemon
restarts, so a repeated commit does not overwrite subsequent local edits. An
operation that was in progress when the daemon restarted reports as failed and
its staging is discarded.

Captures, materializations, and single-file writes use separate selections of
one operation type. The
[transfer module](../../../crates/environment-protocol/src/data/transfer.rs)
defines the phases, pagination, retry, and chunk semantics.

## Generic filesystem scans

For discovery rather than copying, `fs/scan` reads several small trees in one
call. A client selects roots and patterns and can request metadata, inline
content, or SHA-256 digests. This is the lower-level operation used to discover
files such as prompts and skills. A scan observes the environment without
counting as use, so discovery does not keep an idle machine awake.

Each root is reported on its own: complete, missing, or failed with its error.
Entries the scan cannot include, such as an oversized file or a symlink
leaving the filesystem root, are skipped and listed with a reason without
failing the root. A complete root can return a fingerprint for conditional
`ifNoneMatch` requests. A metadata-only fingerprint covers metadata, not file
bytes. Scans have small response and time limits; use transfers for large
inventories and content. The
[scan types](../../../crates/environment-protocol/src/data/scan.rs)
define those request fields and responses.

## Filesystem boundary and consistency

Every transfer path follows the daemon's one confinement rule: it must lie
inside the daemon's filesystem root, a symlink is followed only while its
target stays inside the root, and the daemon's state directory is never
addressed. Replacement uses atomic filesystem operations on Linux and macOS;
unsupported filesystems fail instead of exposing a partly replaced tree.
Windows execution support is not implemented. Platform-specific details live
beside the
[daemon transfer code](../../../crates/environment-daemon/src/filesystem/transfer/mod.rs).

Capture observes a live filesystem. It verifies metadata and content while
reading and fails on detected changes, but it is not an operating-system
snapshot. Pause writers when a result needs consistency across several files.
The daemon's process permissions still determine what it can access; transfer
checks do not isolate commands running as the same OS user from its state.
