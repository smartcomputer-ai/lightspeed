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
example, the contents of `./output` land directly in `/workspace/results`.
Transfers preserve binary files and empty directories, along with executable
flags on supported Unix filesystems.

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

Transfers compare file-content digests and send only missing bytes. Repeating a
copy can reuse content from the destination or Lightspeed storage, while still
using disk space for verification and staging. Allow space for the existing
tree and its replacement.

Large files move in chunks. The daemon advertises its size, inventory, time,
and concurrency limits when a client connects. The
[transfer types](../../../crates/environment-protocol/src/data/transfer.rs)
define the available limits.

## What a capture selects

A capture copies the whole selected tree, including hidden entries and paths
that `.gitignore` excludes. Version-control metadata directories (`.git`,
`.hg`, `.svn`, and `.jj`), the daemon's own state directory, its
registration key file, and on Linux the `/proc` and `/sys` filesystems are
never captured. A symlink whose target stays inside the selected tree is captured as
the file or directory it points to. A link that leaves the selection, a
symlink cycle, or a special file such as a FIFO or device fails the capture.
Names that are not UTF-8 are skipped and counted in the result.

## Environment data-plane API

Clients implementing their own transfers can use `transfer/*` to exchange
inventories, move chunks, and publish or release a result. See
[Files and transfers](../reference/environment-protocol.md#files-and-transfers)
for the request sequence, limits, and retry rules.

## Generic filesystem scans

For discovery rather than copying, `fs/scan` reads several small trees in one
call. A client selects roots and patterns and can request metadata, inline
content, or SHA-256 digests. This is the lower-level operation used to discover
files such as prompts and skills. A scan observes the environment without
counting as use, so discovery does not keep an idle machine awake.

Check each root's result for missing or skipped paths. Use transfers for
large inventories and content. See the
[scan types](../../../crates/environment-protocol/src/data/scan.rs) for
details on responses and conditional reads.

## Filesystem boundary and consistency

Every transfer path follows the daemon's one confinement rule: it must lie
inside the daemon's filesystem root, a symlink is followed only while its
target stays inside the root, and the daemon's state directory, its
registration key file, and on Linux the `/proc` and `/sys` filesystems are
never addressed. A capture or replacement whose tree contains the state
directory or the key file is refused. On Linux and macOS, replacing a
directory requires filesystem support for atomic replacement. On Windows,
use a new destination when materializing a directory tree. See the
[daemon transfer code](../../../crates/environment-daemon/src/filesystem/transfer/mod.rs)
for filesystem-specific rules.

Capture observes a live filesystem. It verifies metadata and content while
reading and fails on detected changes, but it is not an operating-system
snapshot. Pause writers when a result needs consistency across several files.
The daemon's process permissions still determine what it can access; transfer
checks do not isolate commands running as the same OS user from its state.
