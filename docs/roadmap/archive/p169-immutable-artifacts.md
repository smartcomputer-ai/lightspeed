# P169 — Immutable artifacts without workspace mounts

Status: implementation started, 2026-09-09.

Replace the agent-facing VFS with immutable artifacts and explicit environment
transfers. Keep CAS and the existing file/tree snapshot representation; remove
mutable workspace heads, session filesystem mounts, and VFS editing tools.
This follows the [VFS expansion freeze](p166-vfs-environment-transfer.md#vfs-expansion-freeze).

## Behavior

- An artifact identifies immutable uploaded or captured content and descriptive
  metadata. A directory artifact retains names, empty directories, file blobs,
  sizes, and executable flags.
- Sessions explicitly receive access to artifacts. Captured outputs belong to
  the capturing session and can be attached to other sessions deliberately.
- Artifact reads and listings identify an artifact and a path within it. They
  do not compose an absolute session filesystem namespace.
- Materialize copies a selected artifact file or subtree to an exact environment
  destination. Capture creates a new artifact from a selected environment path.
  Neither operation establishes a mount, advances a workspace, or registers
  automatic propagation.
- Artifact records and recorded session content retain their CAS references.
  Access checks use ownership and explicit attachments, never possession of a
  content hash alone.
- Persistent mandatory instructions use the existing instruction mechanism.
  Skill discovery must be independent of VFS workspace links.

## Implementation progress

- [x] Freeze new VFS workspace/mount integrations in the related roadmaps.
- [ ] Separate immutable content and artifact records from workspace stores.
- [ ] Add artifact attachments, scoped read/list tools, and transfer adapters.
- [ ] Replace VFS prompt/skill sourcing and runtime projections.
- [ ] Remove workspace/mount APIs, runtime adapters, and client editors.
- [ ] Update storage schema and define the existing-data transition.
- [ ] Regenerate API/workflow contracts and all consumers.
- [ ] Update examples, product documentation, and demos.
- [ ] Verify authorization, immutable capture/retry behavior, retention,
      environment transfer, replay, and component/build checks.
