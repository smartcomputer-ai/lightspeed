# P82: Fleet Subagent Control Plane

**Status**
- Proposed 2026-06-23.
- Builds on the current `api` session/run surface, the Temporal-backed
  `AgentSessionWorkflow`, environment/provider work from P79-P81, and
  `docs/spec/05-subagents-reference-study.md`.
- This is the first concrete Fleet/subagent implementation plan.

## Goal

Let an agent safely create, configure, task, inspect, and cancel other agents.

The first implementation should support the happy path:

1. A parent agent calls a small Fleet tool such as `agent_spawn`.
2. Lightspeed validates policy and reserves a child identity.
3. Lightspeed creates the child as an ordinary session backed by its own
   `AgentSessionWorkflow`.
4. Lightspeed applies the child run configuration.
5. Lightspeed starts the child run with the requested task.
6. The parent receives a durable child handle it can inspect or task later.

The model should not receive the raw `session/*`, `run/*`, environment, MCP,
auth, VFS, and blob APIs as separate tools. It should receive a small Fleet
control surface that compiles intent into those lower-level API operations.

## Design Decision

Use a **Fleet control plane** in front of the existing session/run API.

The model-visible surface is small and semantic:

```text
agent_capabilities
agent_list
agent_read
agent_configure
agent_spawn
agent_task
agent_cancel
```

The internal implementation may call the existing `AgentApiService`, stores, and
Temporal client directly. It should not bounce through HTTP JSON-RPC when running
in the same process.

Default subagents are **top-level Lightspeed sessions**, not Temporal Child
Workflows. Temporal remains an implementation substrate behind the API/runtime
boundary. A child agent gets its own workflow id, session log, config revision,
run ids, and inspectable status.

## Runtime Shape

```text
parent model tool call
  -> CoreAgent emits tool invocation
  -> Temporal tool activity runs outside deterministic engine
  -> Fleet service validates the request
  -> Fleet service writes/updates Fleet metadata
  -> Fleet service calls internal AgentApiService / Temporal gateway logic
  -> child AgentSessionWorkflow starts as an ordinary session
  -> child run is admitted through run/start semantics
  -> parent tool result returns child handle
```

Starting another top-level workflow is a side effect. It belongs in an activity
or runtime service, not in `engine` and not in pure workflow reducer code.

The first hosted implementation should live in `temporal-server`, with pure
record types and store traits factored into a small registry crate if that keeps
`api`, `store-pg`, and tests clean.

## Identity Model

For P82, one Fleet agent node maps to one Lightspeed session.

```text
agent_id == session_id
```

Keep the Fleet vocabulary separate even while the ids are equal:

- `agent_id`: Fleet-visible logical agent node.
- `session_id`: event-sourced session/workflow identity.
- `run_id`: one admitted execution inside a session.
- `agent_type`: product/runtime type, for example `lightspeed.core`.
- `role`: prompt/tool/profile preset within an agent type.

This avoids a premature separate `agent_id` namespace while keeping room to split
agent nodes from sessions later.

## Fleet Records

P82 needs only small metadata records outside the session log.

### Agent Node

```text
agent_id
session_id
agent_type
role
display_name
status                  creating | ready | running | idle | closed | failed
created_by_session_id
created_by_run_id
created_at_ms
updated_at_ms
metadata
```

The session remains the source of truth for transcript, durable events, config
revision, and run state. Fleet status can be a compact projection/cache.

### Agent Link

```text
link_id
parent_agent_id
child_agent_id
relationship            spawned_by
spawn_id
created_at_ms
metadata
```

The link is product graph state. It should survive parent workflow
`Continue-As-New` and should not depend only on Temporal parent/child history.

### Spawn Record

```text
spawn_id
idempotency_key
parent_session_id
parent_run_id
parent_tool_call_id
child_session_id
state                   pending | starting | running | completed | failed | canceled
task_name
agent_type
role
config_summary
created_at_ms
updated_at_ms
error
```

`idempotency_key` should be derived from the parent session/run/tool-call
identity so activity retries cannot create duplicate children.

## Tool Contracts

### `agent_spawn`

Creates a child agent and optionally starts its first run.

Input shape:

```text
task_name
input
agent_type?
role?
context_policy?
config_overrides?
lifecycle?
```

Defaults:

- `agent_type`: current default core agent type.
- `role`: default role for that type.
- `context_policy`: explicit task plus compact parent summary.
- `lifecycle.run_immediately`: true.

Output:

```text
spawn_id
child_agent_id
child_session_id
child_run_id?
status
```

### `agent_configure`

Applies a declarative configuration patch to `self` or an authorized child.

Input shape:

```text
target                  self | agent_id
mode                    apply | propose
expected_revision?
patch
```

Patch fields should be semantic, not raw API method calls:

```text
model
instructions_profile
tool_profile
environment_policy
mcp_links
run_defaults
metadata
```

The Fleet service compiles the patch into existing session config, tool config,
environment, and MCP operations.

Self-configuration changes should apply only at a safe boundary. If a parent run
is active, changes normally apply to the next turn or next run.

### `agent_task`

Sends a task to an existing agent and starts a run when allowed.

Input shape:

```text
target_agent_id
input
run_config_overrides?
idempotency_key?
```

This is the normal way for one agent to ask another agent to do more work.

### `agent_read` / `agent_list`

Read Fleet-level status and compact session/run projections.

These tools should not dump full transcripts by default. The caller must request
specific fields such as status, config summary, active run, children, or recent
events.

### `agent_cancel`

Cancels an active run or closes a child agent, depending on scope and policy.

Input shape:

```text
target_agent_id
scope                   active_run | queued_runs | session
reason?
```

## Configuration Model

Child configuration is compiled from three layers:

1. `agent_type` and `role` defaults.
2. Inherited parent runtime facts that policy allows.
3. Explicit `config_overrides` from the spawn/configure request.

Prefer named profiles over ad hoc low-level lists:

- `tool_profile`: for tool allowlists and permissions.
- `instructions_profile`: for system/developer prompt selection.
- `environment_policy`: for reusing, cloning, or requesting an environment.
- `mcp_links`: for allowed MCP server links.
- `run_defaults`: for model, effort, service tier, and timeout defaults.

The compiled effective config should be stored or summarized on the spawn record
for audit. Secrets and resolved credentials must not be stored there.

## Context Forking

The child should not inherit the full parent transcript by default.

Supported policies:

```text
none
summary
last_n_turns
explicit_items
```

The default should be `summary`: the parent task, a compact parent state summary,
selected file/blob references, and explicit identity metadata.

Full transcript fork can be added later, but it should be opt-in because it
increases context size, privacy exposure, and child confusion.

## Policy

Every Fleet operation is checked against the parent agent's capabilities.

Initial capability names:

```text
can_spawn_agent
can_read_agent
can_task_agent
can_cancel_agent
can_configure_self
can_configure_child
```

Policy evaluates:

- caller session and run;
- target agent relation to caller;
- requested agent type and role;
- requested tool/environment/MCP/auth capabilities;
- whether the operation is self-targeted or targets another agent.

Risky changes should return a proposal instead of applying directly when policy
requires human or operator approval.

## Reentrancy

Do not expose a generic "call the session API" tool to the model.

When an agent configures itself or spawns another agent, the tool call becomes a
Fleet intent handled by the runtime. The runtime admits any resulting session
commands through normal session/run boundaries.

Self changes should not mutate the running model step in place. They take effect
at the next safe boundary.

## Temporal Behavior

Default child creation:

```text
start_workflow(AgentSessionWorkflow, workflow_id = child_session_id)
signal_submit_admission(child_session_id, RequestRun)
```

Use an internal service or activity with a Temporal client. Do not make the
parent workflow's deterministic loop own external workflow creation directly.

Temporal Child Workflows are deferred. They are appropriate for bounded,
attached helper work where parent close/cancel behavior should govern the child.
Fleet subagents are durable product resources, so they should be top-level
session workflows by default.

## Implementation Steps

### G1. Fleet Types And Store

- Add Fleet DTOs for agent nodes, links, spawn records, capabilities, and
  configuration patches.
- Add store traits and a Postgres implementation.
- Enforce `agent_id == session_id` for P82.

### G2. Fleet Service

- Add a runtime service that validates policy, reserves ids, writes spawn
  records, starts child sessions, and admits child runs.
- Make spawn/task/cancel idempotent by request key.
- Use internal service calls, not HTTP loopback, when in the same process.

### G3. Model-Visible Tools

- Add the small Fleet tool package:
  `agent_capabilities`, `agent_list`, `agent_read`, `agent_configure`,
  `agent_spawn`, `agent_task`, `agent_cancel`.
- Register these tools only for sessions whose policy allows Fleet control.
- Keep schemas tight and deny unknown fields.

### G4. Child Session Configuration

- Compile child config from agent type, role, inherited facts, and overrides.
- Apply session/tool/environment/MCP configuration through existing runtime
  paths.
- Record a non-secret config summary on the spawn record.

### G5. Projection And Inspection

- Project Fleet agent/link/spawn state into `agent_read` and `agent_list`.
- Include compact child run status.
- Do not require full transcript reads for normal parent status checks.

### G6. Tests

- Unit-test validation, idempotency, capability checks, and config compilation.
- Add an in-process runner test where a parent spawns a child and receives a
  child handle.
- Add an ignored Temporal/Postgres live test proving a parent tool call starts a
  separate child `AgentSessionWorkflow` and child run.

## Deferred

- Completion and important-update notifications back to the parent.
- Rich `wait_agent` semantics.
- Multi-parent or peer graph policies beyond spawned child links.
- Temporal Child Workflow execution mode.
- Raw session API tools for privileged debugging.

## Acceptance Criteria

- A parent agent can spawn a child with a task and receive a durable child
  handle.
- Retrying the same spawn tool call does not create a duplicate child.
- The child is visible through normal session/read behavior and through
  Fleet-level `agent_read`.
- The parent can task or cancel the child when policy allows it.
- The model-visible tool surface stays small and does not expose the full
  session API.
- No Fleet side effects are performed inside `engine`.
