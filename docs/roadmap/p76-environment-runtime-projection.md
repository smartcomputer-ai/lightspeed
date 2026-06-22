# P76: Environment Runtime Projection

**Status**
- Proposed 2026-06-17.
- Implemented 2026-06-17.
- Builds on P75 and `docs/spec/04-environments.md`.
- Breaking changes are allowed. Lightspeed has not shipped a stable
  environment/tool projection boundary.

**Progress**
- Added `ContextEntryKind::{VfsCatalog, EnvironmentCatalog, EnvironmentActive}`
  with stable context keys, validation, planning, projection, and compaction
  exclusions.
- Added `tools::environment::projection` DTOs and CAS-backed publication
  helpers for `VfsCatalog`, `EnvironmentCatalogSnapshot`, and
  `EnvironmentActive`.
- Added provider-neutral rendering for the new projection entries in both LLM
  adapters.
- Published VFS/no-active-environment projection from gateway idle-session
  refresh, VFS mount mutation paths, Temporal worker pre-run refresh, and the
  in-process test runner.
- Improved missing/unsupported process-capability errors so they distinguish
  file tools on `fs:session` from process tools requiring an active `env` target.
- Regenerated API contract artifacts and TypeScript generated types.
- Verified with:
  `cargo test -p engine -p api -p api-projection -p tools -p llm-runtime -p temporal-workflow -p temporal-server -p test-support --tests`
- Verified TypeScript client with:
  `npm run typecheck && npm run test && npm run build` in `interop/ts-client`.

## Goal

Make the current runtime describe the environment model to the agent before
adding provider lifecycle or sandbox provisioning.

After this milestone, a session can publish standing context that says:

- file tools operate through the session filesystem (`fs:session`);
- VFS routes are virtual filesystem routes and have no shell;
- environment actions require an active execution environment (`env:<id>`);
- no active environment is a first-class, model-visible state;
- an active environment, when present, is represented separately from the VFS.

This is a runtime projection milestone. It should not make the deterministic
engine own environment lifecycle state yet.

## Target Model

Add three CAS-backed context entry kinds:

```text
ContextEntryKind::VfsCatalog
ContextEntryKind::EnvironmentCatalog
ContextEntryKind::EnvironmentActive
```

`VfsCatalog` is the standing description of the session VFS routes. It replaces
the earlier spec name `VfsView`; the word "catalog" matches the durable,
CAS-backed snapshot shape and makes it clear the entry is structured runtime
metadata, not display-only prose.

`EnvironmentCatalog` is the menu of available environments. It is distinct from
VFS because VFS is not selectable and has no action namespace.

`EnvironmentActive` is the expanded active-environment description. It is only
present when an environment is active and is the place for "same files as shell"
facts via `FsRoute.same_state_as_active_env`.

## Snapshot Schemas

Add runtime-owned JSON DTOs, likely under `tools::environment::projection`:

```text
VfsCatalog
  schema_version
  revision
  routes[]                       VFS-only fs routes

EnvironmentCatalogSnapshot
  schema_version
  revision
  active_env_id
  environments[]

EnvironmentRecord
  env_id
  kind
  capabilities
  exec_target
  cwd
  status

EnvironmentActive
  schema_version
  revision
  env_id
  fs_routes[]

FsRoute
  path
  access
  source
  same_state_as_active_env
```

Keep these entries thin. Transport config, credentials, leases, provider specs,
and host protocol connection data remain runtime/deployment facts and must not
enter context or the session log.

## Implementation

### G1: Engine Context Kinds

- Add `VfsCatalog`, `EnvironmentCatalog`, and `EnvironmentActive` to
  `ContextEntryKind`.
- Add stable context keys for the three standing entries.
- Validate that those keys can only carry the matching entry kind.
- Plan the standing entries consistently with skills and instructions.
- Exclude these standing environment metadata entries from compaction.

### G2: API Projection

- Add the three variants to `api::ContextEntryKindView`.
- Update `api-projection` mapping and session item projection.
- Regenerate committed API contract artifacts after wire types change.

### G3: Projection Model And Publication Helpers

- Add the snapshot DTOs and schema-version constants.
- Add `*_context_input` helpers.
- Add publication helpers that write JSON snapshots to CAS and skip
  republication when the active context already points at the same blob.
- Build `VfsCatalog` from `VfsMountRecord` without requiring an active
  environment.

### G4: LLM Rendering

- Render `VfsCatalog` as standing filesystem context.
- Render `EnvironmentCatalog` as the available environment menu.
- Render `EnvironmentActive` as the active action environment and same-files
  route facts.
- Keep the text provider-neutral, mirroring the skill catalog renderer pattern.

### G5: Runtime Publication

- Publish a VFS-only `VfsCatalog` from gateway/worker code when VFS mounts are
  present or changed.
- Publish an empty/no-active `EnvironmentCatalog` initially.
- Do not implement provider lifecycle, `SessionEnvironmentManager`, or computer
  use in this milestone.

### G6: Instructive Failures

- Ensure environment tools fail clearly when no active environment is available.
- The model-visible failure should distinguish file tools from process tools:
  file tools may still work through `fs:session`; process tools require
  `env:<id>`.

## Non-Goals

- No sandbox provider integration.
- No computer-use tool implementation.
- No core `EnvironmentState`.
- No environment activation API beyond using the existing `env` default target
  source of truth.
- No VFS/environment filesystem fusion beyond publishing the route facts the
  current runtime can already know.

## Done When

- The new context kinds are accepted, projected, and rendered by both LLM
  adapters.
- A VFS-only session can publish context that tells the agent there are file
  routes but no shell.
- `run_process`/stdin calls with no active environment fail with the
  environment-specific guidance.
- API schemas and generated TypeScript client are current.
- Focused engine/API/tools/LLM/runtime tests pass.
