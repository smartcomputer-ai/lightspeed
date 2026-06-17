# P77: Session Environment Manager

**Status**
- Proposed 2026-06-17.
- Implemented 2026-06-17.
- Builds on P75, P76, and `docs/spec/04-environments.md`.
- Breaking changes remain allowed. Lightspeed has not shipped a stable
  environment API or runtime projection boundary.

**Progress**
- Added a shared environment projection planner in
  `tools::environment::projection`.
- Added `temporal_server::environment::SessionEnvironmentManager` as the hosted
  owner for session environment projection refreshes.
- Replaced duplicated projection command construction in the gateway, Temporal
  worker skill-refresh activity, and in-process test runner.
- Kept the hosted environment registry empty by default. P77 establishes the
  ownership boundary; it does not add a sandbox provider or local process
  executor.
- Verified with:
  `cargo test -p tools -p temporal-server -p test-support --tests`
- After the final cleanup, verified the manager compile path with:
  `cargo test -p temporal-server environment::tests::manager_projects_active_environment_from_default_env_target --lib`

## Goal

Centralize the runtime-owned environment projection path before adding provider
lifecycle, activation APIs, or environment-backed filesystem routes.

After P77, code that needs to refresh environment context asks one owner for the
current projection. It should not independently decide how to publish
`VfsCatalog`, `EnvironmentCatalog`, and `EnvironmentActive`.

## Problem

P76 made the agent-facing projection real, but the refresh logic was copied in
three places:

- gateway idle-session refresh before prompt/skill/VFS operations;
- Temporal worker skill-catalog refresh before a run;
- `test-support` in-process run refresh.

Each copy built the VFS catalog, published an empty environment catalog, and
cleared stale active-environment context. That was acceptable for P76, but it is
the wrong foundation for provider work: once environments can attach,
activate, expose filesystem routes, or change status, duplicated refresh logic
would drift immediately.

## Decision

Use two layers:

- `tools::environment::projection` owns pure snapshot planning and CAS-backed
  command preparation.
- `temporal-server::environment::SessionEnvironmentManager` owns hosted runtime
  inputs: VFS mounts now, environment records and active selection next.

The deterministic engine still only sees ordinary context upsert/remove
commands. It does not learn provider lifecycle or environment registry state.

## Implementation

### G1: Shared Projection Planner

Add:

```text
EnvironmentProjectionInput
EnvironmentProjectionRefresh
prepare_environment_projection_refresh
environment_catalog_from_records
environment_active_snapshot
```

The planner:

- builds `VfsCatalog` from VFS mounts;
- builds `EnvironmentCatalogSnapshot` from environment records and active id;
- builds or clears `EnvironmentActive`;
- skips unchanged context entries by comparing existing context refs;
- returns the exact `CoreAgentCommand`s needed for the reducer state.

### G2: Hosted Manager Boundary

Add `SessionEnvironmentManager` in `temporal-server`.

The manager currently owns:

- blob storage for projection snapshots;
- VFS mount store access;
- an environment-record list, empty by default;
- optional active-environment route facts.

It can infer the active environment from the current `env` default target when
the configured environment records contain a matching target. This keeps the
existing P51 deterministic routing rule intact: activation still lowers to a
default target, and projection reports what that target means.

### G3: Replace Duplicate Call Sites

Route the existing refresh paths through the shared planner/manager:

- gateway `refresh_environment_projection_for_idle_session`;
- worker `refresh_skill_catalog`;
- test-support `SessionRunner` pre-run refresh.

The gateway remains responsible for submitting commands and waiting for context
entries to apply. The worker remains responsible for composing environment
projection refresh with skill-catalog refresh. Test-support calls the pure
planner directly to avoid a dependency cycle on `temporal-server`.

## Non-Goals

- No session environment API.
- No provider lifecycle or sandbox provisioning.
- No local process executor implementation.
- No computer-use implementation.
- No environment-backed filesystem route fusion.
- No core `EnvironmentState`.

## Done When

- Projection command construction exists in one shared planner.
- Hosted runtime refreshes go through `SessionEnvironmentManager`.
- The in-process runner uses the same pure planner as hosted code.
- Existing P76 behavior is preserved for VFS-only sessions: publish VFS catalog,
  publish empty environment catalog, and clear active environment.
- Focused tools, temporal-server, and test-support tests pass.
