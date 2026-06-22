# P79: Session Environment API

**Status**
- Proposed 2026-06-17.
- Implemented 2026-06-17.
- Builds on P75-P78 and `docs/spec/04-environments.md`.
- Breaking changes remain allowed. Lightspeed has not shipped a stable
  environment API.

**Progress**
- Added API DTOs and JSON-RPC methods for:
  - `session/environments/list`
  - `session/environments/read`
  - `session/environments/activate`
  - `session/environments/deactivate`
- Gateway environment list/read now projects registered runtime environments
  from `SessionEnvironmentManager`.
- Activation resolves an `envId` to its `EnvironmentRecord.exec_target` and
  lowers to `CoreAgentCommand::SetDefaultToolTarget { namespace: "env", ... }`.
- Deactivation lowers to `CoreAgentCommand::ClearDefaultToolTarget { namespace:
  "env" }`.
- Projection refresh now uses the gateway-owned `SessionEnvironmentManager`, so
  API activation and model-visible context share the same registry.
- Regenerated committed API contract artifacts under `interop/contract/`.
- Verified with:
  `cargo test -p api -p temporal-server --tests`
- Also checked adjacent environment/runtime crates with:
  `cargo test -p tools -p llm-runtime -p api-projection -p test-support --tests`

## Goal

Expose the minimal session environment control surface needed by
`docs/spec/04-environments.md` without adding provider lifecycle.

After this milestone, clients can discover available environments, see which one
is active, activate one ready environment, and deactivate the current environment.
The deterministic source of truth remains the core tool default target for the
`env` namespace.

## API

`session/environments/list`

Returns all runtime-registered environments for the session and the active
environment id, if any.

`session/environments/read`

Returns one environment by `envId`.

`session/environments/activate`

Validates that the session is open and idle, the environment exists and is
`ready`, and the environment has an `env` execution target. The gateway submits a
default-target command for that target, waits for routing state to match, then
refreshes environment projection.

`session/environments/deactivate`

Validates that the session is open and idle, clears the `env` default target,
waits for routing state to match, then refreshes environment projection.

## Non-Goals

- No create/attach API.
- No close/detach API.
- No provider-specific sandbox request shape.
- No core `EnvironmentState`.
- No computer-use lifecycle.
- No VFS/environment workspace fusion decision.

## Done When

- `api` exposes list/read/activate/deactivate DTOs and method manifest entries.
- Gateway implements all four methods.
- Activation and deactivation use existing core tool-target commands.
- Environment projection refresh uses the same registered environments that the
  environment API lists.
- Invalid environment ids return typed API errors.
- API contract artifacts are current.
