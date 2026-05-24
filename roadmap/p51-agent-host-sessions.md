# P51: Tool Execution Targets

**Status**
- In progress

**Progress**
- Added `ToolExecutionTarget`, `ToolTargetRequirement`, and `ToolRoutingState`
  to `agent-core`.
- Added `execution_target` to `ToolInvocationIntent`.
- Added `SessionCommand::SetDefaultToolTarget`,
  `SessionCommand::ClearDefaultToolTarget`, `ToolConfigEvent::DefaultTargetSet`,
  and `ToolConfigEvent::DefaultTargetCleared`.
- Core admission/replay validates target namespace/id shape and routing state
  consistency.
- Core tool policy now resolves default targets from durable routing state and
  copies the resolved target into each created tool invocation intent.
- Host tool specs now declare `Required { namespace: "host" }`.
- Local runtime setup accepts configured default tool targets; CLI/eval local
  host composition opts into `host:local`.
- Host tool execution now resolves `host` targets through `HostToolTargets` and
  rejects missing, non-host, or unknown targets clearly.

## Goal

Add the smallest durable model needed for host-targeted tool execution.

Forge should support agents that run tools against a default target such as the
local host, a session sandbox, or a remote VM/container. Longer term, one agent
session should be able to use several targets at the same time, but the first
cut should keep the common one-target case simple.

This should not turn `agent-core` into a host lifecycle manager. Host sessions,
sandboxes, VMs, leases, credentials, and live status remain runtime/tool-package
concerns.

## First-Cut Scope

Implement only:

- a generic `ToolExecutionTarget`
- target requirements on durable tool specs
- durable default target routing state
- copying the resolved target onto each created tool effect
- validation/tests around target selection and replay behavior

Do not preserve backwards compatibility for current host-tool internals if the
new target model makes a cleaner cut.

## Design Position

`agent-core` should know only the semantic target of a tool call, not how that
target is reached.

The target is the external resource identity whose change would change the
meaning, permissions, data, network origin, filesystem state, or process state
of a tool call. It is not a Temporal queue, worker type, activity name, URL, or
credential.

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolExecutionTarget {
    pub namespace: String,
    pub id: String,
}
```

Examples:

```text
host:local
host:sandbox_123
host:vm_abc
connector:gmail_primary
browser:b_7
```

Host availability is external truth. The session log records what target was
selected and used; the host provider answers whether that target still exists
or is healthy now.

## Tool Invocation Targeting

Add an optional target to tool effects:

```rust
pub struct ToolInvocationIntent {
    pub effect_id: EffectId,
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub batch_id: ToolBatchId,
    pub call_id: ToolCallId,
    pub tool_name: ToolName,
    pub arguments_ref: BlobRef,
    pub execution_target: Option<ToolExecutionTarget>,
}
```

The field is optional because not all tools are host-related:

- host tools such as `exec_command`, `write_stdin`, `read_file`, and
  `apply_patch` should carry `Some(host:...)`
- runtime-side web search should usually carry `None`
- runtime-side HTTP should usually carry `None`
- sandbox-originated HTTP should carry `Some(host:...)`
- connector tools may carry `Some(connector:...)` if account/workspace identity
  changes the meaning of the call

Rule: record a target only when changing the target changes what the call
means.

## Target Requirements

Keep target requirements as durable tool metadata:

```rust
pub enum ToolTargetRequirement {
    None,
    Optional { namespace: String },
    Required { namespace: String },
}
```

This belongs on `ToolSpec`, not provider-visible function JSON. The runtime
sidecar `ToolBinding` / `ToolCatalog` can mirror the same requirement, but core
policy needs a durable copy in order to resolve targets deterministically before
creating `ToolInvocationIntent`.

Expected first-cut bindings:

- host filesystem/process tools: `Required { namespace: "host" }`
- runtime-only tools: `None`
- tools that may run either in runtime or inside a sandbox:
  `Optional { namespace: "host" }`

The tool executor validates package-specific requirements. Core should only
validate generic structural invariants.

## Default Target Routing

Most sessions should run against one default target. Add generic routing state
to `agent-core`:

```rust
pub struct ToolRoutingState {
    pub default_targets: BTreeMap<String, ToolExecutionTarget>,
}
```

Example:

```text
default_targets["host"] = host:sandbox_123
```

Add a command/event to change defaults:

```rust
SessionCommand::SetDefaultToolTarget {
    target: ToolExecutionTarget,
}

SessionCommand::ClearDefaultToolTarget {
    namespace: String,
}
```

Semantics:

- `SetDefaultToolTarget` sets or replaces `default_targets[target.namespace]`
- `ClearDefaultToolTarget` clears the default for `namespace`

When policy creates a `ToolInvocationIntent`, it resolves the target from the
tool binding's requirement:

- `None`: `execution_target = None`
- `Required { namespace }`: copy `default_targets[namespace]` into the intent,
  or fail/mark the tool unavailable if no default exists
- `Optional { namespace }`: copy the default when present, otherwise leave
  `execution_target = None`

The copy is the replay-safety boundary. Changing the default later must not
change already-created effects.

## Per-Call Target Selection

The first cut should allow future per-call target selection without requiring
it for the common path.

Two acceptable shapes:

1. Targeted tool arguments include a `target_id` or `host_id`.
2. A future model/context mechanism lets the agent select a target for one tool
   call before the effect is created.

Either way, once selected, the resolved target must be copied into
`ToolInvocationIntent.execution_target`. Dispatch must never depend solely on a
mutable runtime "current host" value.

For the first implementation, default target routing is enough. Explicit
per-call target tools can come later.

## Runtime Responsibilities

`agent-local` / `agent-tools` resolve targets to live capabilities.

For host targets, the runtime maps:

```text
ToolExecutionTarget { namespace: "host", id: "sandbox_123" }
```

to a concrete `HostToolContext` containing:

- `Arc<dyn FileSystem>`
- optional `Arc<dyn ProcessExecutor>`
- `BlobStore`
- limits
- cwd

The first implementation can use a simple configured host target such as
`host:local` or `host:sandbox_123`. A full multi-host manager is not required
for P51.

## Later Aspirations

P51 should leave room for:

- one session using several host targets simultaneously
- explicit multi-host tools
- host-control tools such as `host_list`, `host_status`, `host_select`,
  `host_provision`, and `host_close`
- remote exec-server adapters
- virtual filesystem targets backed by S3, Postgres, or CAS
- connector, browser, and MCP target namespaces

These are not part of the first cut.

## Implementation Order

### [x] G1. Add `ToolExecutionTarget`

- Add the model type in `agent-core`.
- Add `execution_target: Option<ToolExecutionTarget>` to
  `ToolInvocationIntent`.
- Update serde tests, validation, effect fingerprints, and any snapshot-like
  expectations.

### [x] G2. Add Routing State and Command

- Add `ToolRoutingState` to `SessionState`.
- Add `SessionCommand::SetDefaultToolTarget`.
- Add `SessionCommand::ClearDefaultToolTarget`.
- Add a reducer event for setting/clearing a default target.
- Validate namespace/id shape.

### [x] G3. Add `ToolTargetRequirement`

- Add target requirement metadata to `ToolSpec`.
- Mark host filesystem/process tools as `Required { namespace: "host" }`.
- Mark non-host tools as `None`.

### [x] G4. Resolve Targets When Creating Tool Effects

- When a tool call becomes a `ToolInvocationIntent`, resolve its target from
  routing state and the binding requirement.
- Copy the resolved target into the intent.
- If a required target is missing, produce a clear unavailable/failure path.

### [x] G5. Make Host Tool Execution Target-Aware

- Update host tool execution to require a valid `host` target.
- Resolve the target through a simple configured map/provider.
- Remove old assumptions that one `HostToolContext` is implicitly global.

### [x] G6. Add Replay and Routing Tests

- A host tool effect records the default host target at creation time.
- Changing the default later does not change an existing effect.
- A missing required host target fails clearly.
- Non-host tools can execute without a target.
- Optional target tools work with and without a default.

Coverage is in place for default target replay, required missing target errors,
non-host/no-target tools, optional target resolution, executor target
validation, and multi-target host context dispatch.

## Out Of Scope

- Rich host lifecycle state in `agent-core`.
- Host provisioning services.
- Remote exec protocol implementation.
- Host-control tools.
- Multi-host UI/API design.
- Storing credentials, private URLs, or live VM/container handles in the
  session log.
- Backwards compatibility with the current single-context host executor shape.

## Done When

- Host-dependent tool effects durably identify the target they run against.
- The default host target can be changed mid-session through a replayed command.
- Already-created effects cannot be silently rerouted by later default changes.
- Non-host tools do not need host ids.
- `agent-core` remains generic and deterministic.
