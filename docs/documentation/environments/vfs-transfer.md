# Transfer between VFS and an environment

VFS workspaces and machine filesystems remain separate. Use `vfs_materialize`
to copy a VFS file or directory into the selected environment, and `vfs_capture`
to save an environment file or directory into a writable VFS workspace. Binary
files, scripts, empty directories, and executable flags are supported on Linux
and macOS.

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

Each destination is exact: the source's basename is not appended. The default
is **replace**. Replacing a directory removes destination-only entries within
that selected directory, preserves siblings, and publishes the complete new
tree at once. Set `on_existing` to `error` to reject an existing destination.
Missing destination parents are created where permitted. Existing non-directory
ancestors and environment symlinks are rejected. Environment parents are created
during preparation and may remain if a transfer fails; VFS parents are published
with the captured content in one workspace commit. There is no merging or
automatic sync.

The session needs both environment access and VFS tools. Materialize requires
read access to its VFS source; capture requires an editable workspace link.
Snapshot links are read-only. A selected VFS path must belong to one linked
workspace or snapshot, rather than a synthetic directory spanning several
links. Ordinary VFS operations do not require a selected environment.

Profiles and session settings use the same grants. Under **Virtual File System**,
**Read only** file tools plus **Environments** enable materialize; **Edit files**
plus **Environments** enable both directions. A VFS configured only to source
prompts or skills enables neither transfer tool. Read-only VFS access allows
materialization to write the environment while preserving the VFS source.
Environment selection tools are optional: a profile or API can select the machine.
The tool catalog stays stable while environments change; calls check current
readiness and daemon support when they execute.

Capture saves an immutable snapshot first, with the selected node at
`/selection`. It then publishes to the workspace using the revision read before
transfer. If the workspace changed, the result reports `published: false` and
retains the captured snapshot reference for recovery. It does not merge or
replace concurrent workspace edits.
The recorded tool result retains that snapshot and its file blobs even when
workspace publication fails.

Environment permissions also apply: materialize requires environment **Edit files**;
capture requires environment **Read only** or **Edit files**. VFS read access
is required for materialize, and a writable VFS link plus VFS editing tools for
capture. Command execution is not needed for either transfer.

## Content reuse and large files

A transfer is one logical operation across many bounded exchanges. Inventories
contain relative paths, kinds, file sizes, executable flags and SHA-256 digests
of raw bytes. Path names and executable flags do not change a file's digest.

Materialize compares those digests with files in the existing destination and
transfers missing content from CAS. Capture compares its inventory with CAS and
uploads missing content. Repeated content transfers once per operation; renames,
mode changes and deletions can require no file bytes over the connection.
An identical complete tree is verified and left in place. Otherwise, reused
files are verified and copied into private staging. They are never
hardlinked to a mutable destination. Local hashing and copying still cost I/O.

Unpublished CAS content uses the configured admission grace, renewed when
content is admitted and before capture publication. Keep that grace longer
than a transfer; the default is seven days against a 24-hour transfer ceiling.

File content moves in 256 KiB chunks. Scanning and staging advance in steps
that hash or copy at most 4 MiB. Directory enumeration is bounded by the
inventory quotas, and wire inventories use 128-entry pages. Complete capture
inventories are spooled for receipt recovery. CAS range reads and streamed
writes avoid buffering whole files.
Filesystem CAS uses a temporary file; object-backed PostgreSQL CAS uses a
verified multipart upload and then publishes its catalog row. Multipart memory
is bounded independently of file size, with one part in flight and a maximum
of approximately 128 MiB for the largest supported file.

Default daemon operation ceilings are 100,000 entries, depth 64, 32 MiB of
serialized inventory, one TiB per file and selected tree, and 24 hours. These
are separate from chunk/page limits. At most eight operations can be active;
overlapping materialization destinations are rejected. Staging disk must hold
missing uploads and the staged replacement, in addition to any existing target.
A 20 GiB tree is not sent as one JSON message or loaded as a 20 GiB buffer.

Long transfers use the runtime's bulk execution budget and ordinary activity
heartbeats/cancellation. Tool results contain references and counts, never
file payloads. Existing text-reading tools and their formatting are unchanged.

## Environment data-plane API

`EnvironmentDataClient::transfer` sends `fs/transfer`. Negotiate
`filesystemTransfer` before using it. A read-only daemon accepts capture and
rejects materialize. The typed DTOs live in
`environment-protocol::data::transfer_session` and `inventory`; they contain
no VFS, database or CAS-store identities.

| Action | Purpose |
| --- | --- |
| `begin` | Bind an operation ID to capture source or materialize destination, replacement policy and quotas. |
| `advance` | Advance bounded inventory scanning or staging; inspect the returned phase. |
| `inventory` | Read a page of a complete inventory. |
| `append` | Submit a parent-before-child materialize inventory page; `last` seals it. |
| `missing` | Read missing content digests. Fetch offset zero again after uploads, because the list shrinks. |
| `read` / `write` | Transfer one bounded raw-byte chunk at an explicit file offset. |
| `commit` | Verify capture observations, or atomically publish the complete staged destination. |
| `status` | Inspect progress or recover a completion receipt. |
| `abort` | Abandon incomplete work and reclaim staging. A completed operation stays complete. |

Reuse the same operation ID for retries and never assign it to different input.
Identical inventory pages and already-written chunks can be retried. Complete
uploads are reused. Completed replacement retries return their receipt and do
not overwrite later local edits.

The daemon journals operation identities and completion receipts in its state
directory. Receipts survive restart until operation expiry. An operation
interrupted by a restart fails closed: inspect its destination and abort that
operation before deliberately starting a new one. There is no claim of
transparent resumption of partially staged files after a daemon crash. Retired
trees are deleted asynchronously, and an expiry sweep reclaims abandoned work.

The older `fs/materialize` and `fs/capture` methods remain small-copy
compatibility adapters over this same implementation. Their original 8 MiB,
1,024-entry and 30-second ceilings still apply. Current daemons own retirement
cleanup and return no `retiredDirectory`.

## Generic filesystem scans

`EnvironmentDataClient::scan` sends `fs/scan` when `filesystemScan` is negotiated.
It accepts roots, include patterns, `readContent`, optional
`digestAlgorithm: "sha256"`, `followSymlinks`, quotas, and `ifNoneMatch`. This is a generic
filesystem operation: filenames such as `SKILL.md` are caller-supplied patterns.
Entries retain root-relative paths and include `canonicalPath`. Symlink following
is opt-in and remains confined to the endpoint access scope; loops, dangling
links, and inaccessible targets produce incomplete observations. A missing root
is a complete empty observation. Quotas apply across all roots. The data-plane
handshake also reports the execution user's `homeDirectory` for caller-side root
resolution, without assigning any skill semantics to the endpoint.

Metadata-only scans do not read file payloads. Content reads or requested
SHA-256 digests inspect the complete matching files; unrelated files are not
hashed. Small responses are capped at 4 MiB, 10,000 returned entries and 256 KiB
of inline content per file, with a cooperative scan deadline of at most 30
seconds. Use transfer sessions for large inventories/content.

A complete observation has a query- and scope-specific fingerprint.
`ifNoneMatch` suppresses unchanged results; it does not promise to eliminate
local traversal or hashing. Incomplete or inaccessible observations include
diagnostics, have no fingerprint, and never report unchanged. A metadata-only
fingerprint proves only the requested metadata, not byte equality.

## Filesystem boundary and consistency

The OS backend opens user selections relative to pinned directories and rejects
symlinks, special files and non-UTF-8 names. Configured root aliases are resolved
when establishing the administrator-owned anchor. Mounted directories inside
that namespace are allowed, including macOS's data volume. Staging is created
with mode 0700 in the destination's parent. Configured cwd aliases are also
resolved before handling user-relative selections.

Linux publication uses `renameat2` with `RENAME_NOREPLACE` or `RENAME_EXCHANGE`;
macOS uses `renameatx_np` with `RENAME_EXCL` or `RENAME_SWAP`. Unsupported
filesystems fail rather than falling back to a remove-then-rename gap. A backend
boundary isolates those APIs and native metadata from the shared state machine;
a future Windows implementation can use directory handles and reparse-point
checks without changing the wire protocol. Windows execution support is not
implemented yet.

Capture is a verified live observation, not an OS snapshot. Metadata is checked
around reads and before completion, and streamed content must match its expected
whole-file digest. Detected changes fail the operation. External writers can
still change files after an observation; stabilize the source when stronger
cross-file consistency is required. Processes with the daemon's own authority
are not isolated from its private state by these helpers.
