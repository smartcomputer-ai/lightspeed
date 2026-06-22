# P78: Active Environment Runtime Wiring

**Status**
- Proposed 2026-06-17.
- Implemented 2026-06-17.
- Builds on P75, P76, P77, and `docs/spec/04-environments.md`.
- Breaking changes remain allowed. Lightspeed has not shipped a stable
  environment API.

**Progress**
- Added a runtime environment registry to `SessionEnvironmentManager`.
- Added `RuntimeEnvironment`, pairing the projected `EnvironmentRecord`, live
  `EnvironmentToolContext`, and active filesystem route facts.
- Taught hosted `SessionTools` to build one inline runtime containing
  `fs:session` when VFS mounts exist plus registered `env:<id>` contexts.
- Kept `default_targets["env"]` as the activation source of truth for projection:
  if the default env target matches a registered environment, the manager
  publishes `EnvironmentActive`.
- Enabled hosted process tool bindings only when a runtime environment is
  registered.
- Added a fake-process environment test proving a single runtime can read VFS
  files through `fs:session` and execute a command through `env:test`.
- Verified with:
  `cargo test -p temporal-server -p tools -p llm-runtime --tests`

## Goal

Prove the active-environment runtime path end to end without adding provider
lifecycle or public environment APIs.

After this milestone, the runtime can hold a concrete live environment context,
project it as an available/active environment, and resolve process tools against
its `env:<id>` target while file tools continue to use the session filesystem.

## Implementation

### G1: Runtime Environment Registry

`SessionEnvironmentManager` now owns a map of `RuntimeEnvironment`s. Each entry
contains:

- the model-visible `EnvironmentRecord`;
- the live `EnvironmentToolContext`;
- filesystem route facts for `EnvironmentActive`.

This keeps projection metadata and invocation context in one owner.

### G2: Tool Target Composition

Hosted `SessionTools` builds `ToolTargets` from:

- `fs:session` when VFS mounts exist;
- every registered `env:<id>` context.

The no-mount fast failure now only applies to filesystem-targeted calls. An
environment-targeted process call can run even if the session has no VFS mounts.

### G3: Active Projection

`SessionEnvironmentManager` still derives active selection from
`default_targets["env"]`. If that target matches a registered environment, the
projection publishes:

- a non-empty `EnvironmentCatalog`;
- `EnvironmentActive` with that environment id and its route facts.

No second active-environment state is introduced.

### G4: Process Tool Binding

Hosted runtime catalogs include process tools when at least one runtime
environment is registered. VFS-only sessions keep the existing file/web surface.

### G5: Provider-Free Test Environment

The first concrete environment is test-only: a fake `ProcessExecutor` wired into
`SessionTools`.

The test proves:

- `read_file` resolves through `fs:session` against a mounted VFS workspace;
- `exec_command` resolves through `env:test`;
- the environment process cwd is applied;
- both calls succeed in one hosted runtime.

## Non-Goals

- No sandbox provider integration.
- No public session environment API.
- No lifecycle persistence.
- No local process executor.
- No computer-use implementation.
- No VFS/environment workspace fusion decision.

## Done When

- `SessionEnvironmentManager` owns projected records and live contexts together.
- Hosted `SessionTools` can resolve both `fs:session` and `env:<id>`.
- Active environment projection is non-empty when the default env target points
  at a registered environment.
- A fake process environment test covers file-tool plus process-tool routing.
- Focused tools, LLM-rendering, and temporal-server tests pass.
