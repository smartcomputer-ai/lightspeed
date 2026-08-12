# P108: Universe Environments And Active Session Selection

**Status**

- Environment ownership/selection remains current, but P113 supersedes its
  generic target and fused filesystem routing; see
  [P113](p113-explicit-vfs-and-environment-tool-domains.md).
- P117/P118 supersede its universe-scoped provider registration, heartbeat,
  lease, provider-policy, and connection model. Providers are
  operator-registered reachable controller endpoints; allocation and ingress
  policy are provider-owned.
- Implemented 2026-07-30 as a greenfield replacement for the P76-P80/P96
  session-binding and environment-context model.
- Amended 2026-07-30 so model-driven environment discovery and selection is an
  independent, default-off `selectionTools` sub-grant.
- Amended 2026-07-30 so `environment_read` is always available with the
  environments feature and defaults to the active environment when its id is
  omitted; `selectionTools` gates only list/activate/deactivate.
- Builds on P107's separation of durable session intent from live resource
  resolution.
- Lightspeed is still greenfield. Breaking API, event, storage, and generated
  contract changes are expected; do not add compatibility aliases, data
  migration, dual reads, or a feature-version transition.

## Decision

Environments are universe resources backed by external providers. Sessions do
not attach, configure, or own copies of them. A session records only which
universe environment is currently active for environment-targeted tools.

Keep the concerns separate:

- the universe owns environment identity, lifecycle metadata, credentials, and
  provider policy;
- external providers own live availability and execution;
- session config grants the environment feature and may restrict allowed
  providers; and
- deterministic session state owns the active environment identity only.

Environment availability is live resource state. Active environment selection
is durable session intent.

## Universe Resources

Use one universe-scoped `EnvironmentId` as the public identity. Remove the
session-local `env_id` alias and the distinction between a session environment
and an environment instance.

The operational environment record retains only what the hosted runtime needs
to resolve and manage the external resource, such as:

```text
Environment
  environment_id
  provider_id
  provider_target_id
  ownership / lifecycle policy
  current connection observation
  current capabilities and status observation
  observed_at
```

Provider registration, leases, environment observations, and connection data
may remain in PostgreSQL because gateways, workers, restarts, and durable jobs
need shared coordination. Treat mutable status and capabilities as timestamped
provider observations, not as a durable catalog or source of truth.

Selecting an environment does not give the session ownership of it and closing
a session does not implicitly close a universe environment. Explicit lifecycle
policy may still close a session-created environment when that policy owns it;
that is independent of active selection.

## Provider Extension Seam And Internal Resolver

The existing environment-provider registration API plus `host-protocol` is the
only external environment extension seam. External providers advertise
universe environments and own their live execution. P108 does not introduce an
environment plugin type, plugin manifest, workflow-plugin binding, dynamic
library ABI, or second provider protocol.

Clean up Lightspeed's side behind one internal environment service, referred to
here as `EnvironmentResolver`. This is ordinary Rust architecture, not a
public API or plugin/extension contract. It centralizes:

- universe environment list/read and provider-filter enforcement;
- liveness and unavailable/stale interpretation;
- resolution of an environment into its current host connection,
  capabilities, cwd, and filesystem support;
- universe-scoped environment credential resolution; and
- structured failures shared by gateway APIs, model tools, tool execution, and
  environment-job activities.

Gateway handlers, workers, model tools, filesystem/process runtime assembly,
credential injection, and job activities should consume this service instead
of independently coordinating environment stores and host connections. It may
be a concrete service or an internal trait where test substitution is useful;
do not add a network boundary merely to make it look extensible.

This consolidation leaves future service extraction possible without designing
or committing to it now. Provider implementations remain pluggable through the
existing provider API and `host-protocol`; `EnvironmentResolver` itself is not
pluggable.

## Session Config And Provider Policy

Keep `SessionConfig.features.environments` as the feature grant. Retain its
existing optional provider filter:

```text
features.environments
  version
  providers?    // absent means every universe provider is allowed
  selectionTools // default false; grants model discovery and selection
  jobs
```

The provider filter is the only environment-selection policy in the first
version. When `selectionTools` is true, the model may discover and activate
every universe environment whose provider is allowed by this filter. When it
is false or absent, API clients and profiles may still select an allowed
environment, but the selection tools are not model-visible. Do not add
environment allowlists, tags, capability predicates, or per-environment grants
yet.

## Active Environment State

Add an explicit environment component to deterministic core state:

```text
EnvironmentState
  active_environment_id: Option<EnvironmentId>

CoreAgentCommand
  SetActiveEnvironment { environment_id }
  ClearActiveEnvironment

EnvironmentEvent
  ActiveEnvironmentSet { environment_id }
  ActiveEnvironmentCleared
```

Admission validates the identifier and feature/provider policy at the hosted
boundary. The core records identity only; it does not record provider status,
capabilities, connection data, credentials, or cwd.

An active reference may become dangling because the environment is deleted or
the provider disappears. Keep the selection in session state and resolve it as
unavailable. Environment-targeted tools must fail with a structured error and
must never silently choose another environment. The user or model can select a
replacement or clear the selection.

Do not place active selection in session config. Config is client-owned,
whole-document capability policy; environment selection is mutable run-time
session state and may be changed by the model.

## Model Tools

Install `environment_read` whenever the environments feature is granted. Its
optional `environment_id` selects a known allowed environment; omission reads
the active environment and fails with `no_active_environment` when none is
selected. When the default-off `selectionTools` sub-grant is true, additionally
install:

- `environment_list` returns paginated summaries of the universe environments
  allowed by the session's provider filter, including current observed status
  and the active marker.
- `environment_activate` validates that the environment exists, its provider is
  allowed, and it is currently selectable, then changes active session state.
- `environment_deactivate` clears active session state.

List and read are live runtime queries. Their results enter model context as
ordinary tool results; do not maintain an eager environment catalog in session
context. Provisioning and closing are separate lifecycle operations and are not
folded into these tools.

Model-driven activation returns a trusted tool effect that the engine converts
to `ActiveEnvironmentSet` after successful tool completion. An activation or
deactivation call must not share a planned batch with tools that depend on the
active environment: sibling calls have already resolved against the previous
selection. Reject such a mixed batch and let the model continue in the next
turn.

Do not inject the active environment id or live environment details into model
instructions or context. The model can obtain both on demand by calling
`environment_read` without an id.

## Credentials

Credentials belong to the universe environment, not to a session. Replace
session-scoped credential bindings with:

```text
EnvironmentCredentialBinding
  universe_id
  environment_id
  env_name
  source             // auth grant, provider credential, or direct secret
```

Move credential operations from `session/environments/credentials/*` to
universe `environments/credentials/*` methods. At invocation time the runtime
resolves the target environment and injects that environment's configured
credentials for every Lightspeed-started process or job, including bare
`environments/jobs/create` calls and session tool calls. Selecting an
environment carries no credential ids. Deleting the environment deletes its
credential bindings.

## Tool Routing

Remove the generic durable default-target layer:

- delete `ToolRoutingState.default_targets`;
- delete `SetDefaultToolTarget`, `ClearDefaultToolTarget`,
  `DefaultTargetSet`, and `DefaultTargetCleared`; and
- remove their API event projections.

File tools resolve to one fixed session-filesystem target. At invocation time,
the runtime composes config-derived VFS workspace links with the live active
environment filesystem, when present; workspace links retain precedence and
the environment's cwd/routes are derived from its live record rather than a
session binding. Process tools resolve to
`EnvironmentState.active_environment_id`. Untargeted tools remain untargeted.
Tool policy may represent these sources explicitly rather than by looking up a
namespace in a mutable default-target map.

Retain `ToolExecutionTarget` on planned tool calls and copy the resolved
environment target into each invocation intent/event. This is the replay-safety
boundary: changing active selection later must not redirect an already-planned
call. Job handles continue to identify their originating environment and do not
reroute through the current active selection.

## Remove Session Environment Bindings And Context Projection

Remove:

- `session_environment_bindings` and `SessionEnvironmentBindingStore`;
- session-local aliases, attached/detached state, cwd overrides, and stored
  per-binding filesystem routes;
- `session/environments/attach` and `session/environments/detach`;
- session-scoped environment list/read projections where the universe
  `environments/list` and `environments/read` APIs are sufficient;
- `ContextEntryKind::EnvironmentCatalog` and
  `ContextEntryKind::EnvironmentActive`;
- `EnvironmentCatalogSnapshot`, the current active-environment CAS snapshot,
  their LLM renderers, and their gateway/Temporal refresh fields; and
- runtime toolset churn based on whether an environment happens to be ready.

`environment_read` is derived from the environments feature itself;
list/activate/deactivate are derived from `selectionTools`. Process tools remain
independent and may be used with an environment selected externally. Job tools
are controlled by the separate `jobs` sub-grant. Process and job tools may
remain installed when no environment is active and return clear, structured
selection or availability errors.

The VFS catalog remains. Unlike live environment inventory, workspace links
affect file routing plus prompt, instruction, and skill discovery. Future
environment-local discovery should explicitly snapshot selected content into
CAS rather than restoring an eager catalog of environments.

## API And Runtime Notes

- Keep universe `environments/create`, `environments/read`,
  `environments/list`, and `environments/close` as the control-plane resource
  API.
- Keep the existing session activation/deactivation RPCs as client-facing
  commands, but make them operate directly on explicit environment state and
  accept a universe `environmentId`.
- Share the live environment resolver and session provider-policy helper across
  the universe API, session activation API, and model tools so observation and
  filtering behavior cannot drift. Universe control-plane list/read operations
  themselves are not constrained by one session's provider filter.
- Profiles carry at most one optional `activeEnvironmentId` and Fleet paths
  select that universe environment through the session activation command.
  Profiles do not provision or close environments, and absence leaves an
  existing selection unchanged. Copying session state naturally copies the
  active identity; environment ownership and credentials remain universe state.
- Remove environment projection fields from Temporal activity inputs. Carry the
  active environment id required to resolve planned tools, while resolving live
  provider state and credentials at invocation time.

## Non-Goals

P108 does not add:

- finer-grained environment permissions beyond the provider filter;
- automatic fallback when an environment is unavailable;
- environment lifecycle operations to the four selection/discovery model
  tools;
- live provider observations to deterministic session state; or
- environment-local prompt, instruction, or skill discovery.

## Done When

- [x] Universe environment ids are the only selectable environment identity;
      no session-local aliases or attachment records remain.
- [x] Core replay reconstructs explicit active environment state from dedicated
      environment events.
- [x] `environment_list`, `environment_read`, `environment_activate`, and
      `environment_deactivate` enforce the configured provider filter and use
      live universe resolution; read is always present and defaults to the
      active environment, while the other three require `selectionTools`.
- [x] Environment credentials are universe/environment scoped and used by every
      session selecting that environment.
- [x] Environment catalog and active-environment CAS context projections are
      removed without changing the VFS catalog and VFS-derived discovery path.
- [x] Generic default-target state/events are removed while every planned
      environment call still pins its resolved `ToolExecutionTarget`.
- [x] Missing, deleted, stale, and disallowed environments produce structured
      errors without silently mutating active selection.
- [x] Session environment attach/detach storage and APIs are removed, and API
      contracts plus generated clients are current.
- [x] Unit, integration, PostgreSQL, Temporal, provider, prompt, and skill live
      suites cover the new state and resolution boundaries.
