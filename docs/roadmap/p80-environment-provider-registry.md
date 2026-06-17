# P80: Environment Provider Registry

**Status**
- Proposed 2026-06-17.
- G1-G3 implemented 2026-06-17.
- Builds on P75-P79 and `docs/spec/04-environments.md`.
- Breaking changes remain allowed. Lightspeed has not shipped a stable
  environment API.

## Goal

Introduce the runtime registry that lets sandbox runners and bridge runners
become environment providers for Lightspeed sessions.

After this milestone, an external runner can register itself, heartbeat while it
is reachable, advertise a `host-protocol` controller, and be used by the gateway
to create or attach session environments. The deterministic engine still records
only semantic tool target identity (`env:<id>`), not provider lifecycle.

## Design Decision

Use three explicit protocols/boundaries:

1. **Public Lightspeed session API**
   - Used by CLI, UI, and users.
   - Owns session intent: list, create, attach, activate, deactivate, close.
   - Enforces auth, policy, quotas, and session ownership.

2. **Lightspeed environment-provider registry protocol**
   - Internal runtime API for sandbox/bridge runners.
   - Owns provider registration, heartbeat, leases, and discovery.
   - Does not mutate session active targets directly.

3. **`host-protocol` controller/data plane**
   - Already mostly exists in `crates/host-protocol`.
   - Any registered provider must speak it directly, or be wrapped by an adapter
     that speaks it.
   - Owns target lifecycle and operations:
     - `controller/initialize`
     - `controller/listTargets`
     - `controller/createTarget`
     - `controller/attachTarget`
     - `controller/getTarget`
     - `controller/closeTarget`

Do not let runners register arbitrary session environments through the public
session API. Runners register provider capacity. The gateway decides when that
capacity is bound to a session environment.

## Process Boundary

Keep the registry in `temporal-server` first.

This is a separate API boundary, not necessarily a separate service boundary.
Splitting into a standalone provider-registry service is a deployment decision
for later. The first implementation should be in-process with the gateway and
worker, backed by `store-pg`.

## Concepts

### Environment Provider

An environment provider is a reachable runner or control-plane endpoint that can
provide one or more host targets.

Examples:

- a local bridge process running on a developer machine;
- a sandbox runner backed by a VM/container provider;
- a cloud service that provisions short-lived coding environments;
- a future computer-use bridge, if it exposes the required controller/data-plane
  capabilities.

Every provider must advertise a `host-protocol` controller connection.

### Host Target

A host target is the provider-side object managed by `host-protocol`.

Host targets are not session environments by themselves. A target becomes a
session environment only when Lightspeed creates a session binding to it.

### Session Environment Binding

A binding maps a session-visible `env_id` to a provider target and the data-plane
connection used by tools.

This is runtime state. It feeds:

- `SessionEnvironmentManager`;
- `EnvironmentCatalog` projection;
- `EnvironmentActive` projection;
- `ToolTargets` composition for workers.

It is not core `EnvironmentState`.

## Records

### EnvironmentProviderRecord

```text
provider_id
provider_kind             sandbox | bridge | custom
display_name
status                    registering | online | stale | offline | disabled
controller_connection     HostControllerConnectionSpec
capabilities              EnvironmentProviderCapabilities
implementation            name/version/protocol version
last_seen_ms
lease_expires_ms
metadata
created_at_ms
updated_at_ms
```

`controller_connection` is how Lightspeed reaches the provider's
`host-protocol` controller. It may be a URL, a reverse tunnel channel id, a Unix
socket descriptor in local mode, or another runtime-supported connection kind.

### EnvironmentTargetRecord

```text
provider_id
target_id                 host-protocol HostTargetId
status                    creating | starting | ready | stopped | closing | closed | failed | unknown
scope                     host-protocol HostScope
capabilities              host-protocol HostCapabilities
default_cwd
metadata
observed_at_ms
```

This mirrors the provider's controller view. It can be updated by heartbeat
payloads or by polling `controller/listTargets`.

### SessionEnvironmentBindingRecord

```text
session_id
env_id
provider_id
target_id
exec_target               ToolExecutionTarget(namespace="env", id=env_id)
kind                      sandbox | attached_host
status                    attaching | ready | degraded | detached
capabilities              EnvironmentCapabilities
connection                HostConnectionSpec
cwd
fs_routes
created_at_ms
updated_at_ms
```

`connection` is the data-plane connection returned by
`controller/createTarget` or `controller/attachTarget`. Worker-side
`SessionTools` uses this to build `EnvironmentToolContext`.

## Provider Registry API

These endpoints are internal to the hosted runtime. They should require a
provider credential, deployment token, or local trust channel.

### `environmentProviders/register`

Registers or refreshes a provider instance.

Input:

```text
provider_id
provider_kind
display_name
controller_connection
implementation
capabilities
lease_ttl_ms
metadata
```

Behavior:

- validates the provider credential;
- calls `controller/initialize` on the advertised controller;
- verifies protocol version and required capabilities;
- stores or updates `EnvironmentProviderRecord`;
- marks provider `online`;
- returns the accepted lease expiry.

### `environmentProviders/heartbeat`

Refreshes provider liveness.

Input:

```text
provider_id
observed_targets?          optional HostTargetSummary[]
lease_ttl_ms?
```

Behavior:

- extends the provider lease;
- updates `last_seen_ms` and `lease_expires_ms`;
- optionally upserts mirrored target summaries;
- marks provider `online`.

If no heartbeat arrives before `lease_expires_ms`, runtime queries should treat
the provider as `stale`, then `offline` after a configurable grace period.

### `environmentProviders/unregister`

Gracefully disables a provider instance.

Behavior:

- marks provider `offline` or `disabled`;
- does not mutate deterministic core state;
- bound session environments become `degraded` or `detached` depending on target
  state and policy.

## Public Session API Additions

P79 already added list/read/activate/deactivate.

P80 adds lifecycle methods that bind provider targets to sessions:

### `session/environments/create`

Creates a provider target and binds it as a session environment.

Input:

```text
session_id
env_id?
provider_id
request                  opaque provider request or host-protocol create request
activate?                default false
```

Gateway behavior:

1. validate session is open and idle if `activate=true`;
2. read `EnvironmentProviderRecord`;
3. call provider `controller/createTarget`;
4. store `SessionEnvironmentBindingRecord`;
5. refresh environment projection;
6. if `activate=true`, lower to `SetDefaultToolTarget(namespace="env")`.

### `session/environments/attach`

Attaches an existing provider target to the session.

Input:

```text
session_id
env_id?
provider_id
target_id or provider attach spec
activate?                default false
```

Gateway behavior is the same as create, but calls `controller/attachTarget`.

### `session/environments/close`

Closes or detaches a bound environment.

Input:

```text
session_id
env_id
force?
close_target?            default depends on provider/session ownership
```

Gateway behavior:

1. validate session is open and idle if the environment is active;
2. if active, clear the `env` default target;
3. optionally call `controller/closeTarget`;
4. mark binding detached;
5. refresh environment projection.

## Status and Heartbeat Semantics

Provider heartbeat affects runtime availability only.

It must not directly mutate deterministic core state. If a provider disappears:

- provider status becomes `stale` or `offline`;
- bound session environments project as `degraded` or `detached`;
- tool execution against that `env:<id>` fails with a typed runtime error;
- the active `env` default target may remain in core until a user or policy
  deactivates it.

This keeps replay deterministic and avoids environment liveness events becoming
part of core branching accidentally.

## Bridge Runner Flow

```text
bridge starts on developer machine
bridge opens reverse connection or exposes local controller
bridge registers provider_id="bridge:lukas-macbook"
bridge heartbeats with available target summaries

CLI lists providers
CLI attaches target to session
gateway calls controller/attachTarget
gateway stores session environment binding
gateway optionally activates env:<id>
worker builds EnvironmentToolContext from binding connection
```

Bridge providers usually expose attached targets that already exist. They may
support `createTarget`, but it is not required.

## Sandbox Runner Flow

```text
sandbox runner starts in deployment
runner registers provider_id="sandbox:pool-a"
runner heartbeats capacity and existing targets

CLI or policy creates environment for session
gateway calls controller/createTarget
provider provisions VM/container
provider returns HostConnectionSpec
gateway stores binding and projects environment
worker executes process/computer/file-backed environment tools through data plane
```

Sandbox providers usually support `createTarget` and `closeTarget`. They may not
expose pre-existing attachable targets.

## Store Traits

Add an environment registry crate, likely `crates/environment-registry`, with
traits similar to `mcp-registry`:

```text
EnvironmentProviderStore
  register_provider
  read_provider
  list_providers
  update_provider_heartbeat
  update_provider_status
  delete_provider

EnvironmentTargetStore
  upsert_target
  read_target
  list_targets
  update_target_status

SessionEnvironmentBindingStore
  create_binding
  read_binding
  list_bindings_for_session
  update_binding_status
  delete_binding
```

Implement these for `store-pg`.

## Runtime Integration

`SessionEnvironmentManager` should stop depending on builder-injected
environments in hosted production paths.

Instead:

- gateway list/read/create/attach/close loads session bindings from the registry;
- gateway projection refresh builds `EnvironmentRecord`s from bindings;
- worker `SessionTools` loads bindings for the session and materializes
  `EnvironmentToolContext`s from `HostConnectionSpec`;
- test-support can keep an in-memory registry/provider implementation.

The current builder-injected environment path remains useful for tests until the
registry-backed path is complete.

## Security

Provider registration is privileged.

At minimum:

- providers authenticate with deployment-scoped credentials;
- provider ids are stable and validated;
- session attach/create checks user authorization;
- a provider cannot choose arbitrary `session_id` bindings;
- opaque provider specs are stored only where needed and should not contain
  long-lived secrets directly;
- data-plane credentials in `HostConnectionSpec` are treated as secrets if they
  carry bearer tokens or tunnel keys.

## Non-Goals

- No separate provider-registry service process.
- No core `EnvironmentState`.
- No workspace VFS/environment fusion decision.
- No computer-use tool implementation.
- No specific sandbox vendor integration.
- No billing or quota enforcement beyond placeholder policy checks.

## Implementation Plan

### G1: Registry Crate

Add `crates/environment-registry` with provider, target, binding DTOs, statuses,
validation, errors, and in-memory stores.

Implemented in `crates/environment-registry`.

### G2: Postgres Store

Add `store-pg` tables and trait implementations for providers, targets, and
session environment bindings.

Implemented through `store-pg` migration `006_environment_registry.sql` and
the `PgStore` environment registry trait implementations.

### G3: Internal Provider API

Add gateway endpoints or an internal HTTP/JSON-RPC surface for provider
registration, heartbeat, and unregister.

Implemented as JSON-RPC methods:

- `environmentProviders/register`
- `environmentProviders/heartbeat`
- `environmentProviders/unregister`

This first API pass validates and persists provider registry state. It does not
perform `controller/initialize`; controller transport integration starts in G4.

### G4: Host-Protocol Controller Client Integration

Use `host-client`/`host-protocol` to initialize controllers and call
`listTargets`, `createTarget`, `attachTarget`, `getTarget`, and `closeTarget`.

### G5: Public Session Lifecycle API

Add `session/environments/create`, `session/environments/attach`, and
`session/environments/close` on top of the registry and controller client.

### G6: Registry-Backed SessionEnvironmentManager

Build gateway projection and worker `ToolTargets` from session binding records.
Keep core activation as `SetDefaultToolTarget` / `ClearDefaultToolTarget`.

### G7: Fake Provider Integration Test

Add a fake host-protocol controller that registers, heartbeats, creates or
attaches a target, and supports a process tool call through the resulting
session environment.

## Done When

- Providers can register and heartbeat.
- Provider liveness is visible in runtime state without mutating core state.
- Gateway can create or attach a session environment through a host-protocol
  controller.
- Gateway can close or detach a session environment.
- `SessionEnvironmentManager` can project and execute registry-backed
  environments.
- Worker tool execution uses `HostConnectionSpec` from the binding.
- Tests cover bridge-style attach and sandbox-style create against a fake
  host-protocol provider.
