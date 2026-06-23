# Subagents Reference Study

Design notes from reading how Codex and Claude Code let a main agent trigger
subagents and configure the child run.

Status: reference study for Fleet/sub-agent design. This is not an
implementation plan by itself, but it records concrete mechanisms worth copying
or avoiding.

Codebases studied:

- Codex: `/Users/lukas/dev/tmp/codex`
- Claude Code: `/Users/lukas/dev/tmp/claude-code`

## Summary

Codex and Claude Code use two different models.

Codex treats a spawned agent as a real child thread in the same root agent tree.
The child has durable thread identity, parent/child metadata, persisted spawn
edges, inherited runtime config, optional forked context, and inter-agent
message routing. This is closest to Lightspeed's Fleet direction.

Claude Code treats a normal subagent as an in-process nested `query()` loop. The
child gets its own model, prompt, tools, permission mode, hooks, MCP clients,
transcript sidechain, and task/progress wrapper, but it is not a separate
durable session in the same way. This is useful as a run-configuration envelope,
but less useful as a durable Fleet architecture.

The design lesson for Lightspeed is: copy Codex's durable graph shape and
Claude Code's rich child-run configuration, while keeping side effects outside
the deterministic engine.

## Vocabulary

Use these terms consistently when translating the references to Lightspeed:

- `parent`: the session/run whose model requested a subagent.
- `child`: the spawned subagent run.
- `agent type`: public product/runtime type, for example a future
  `lightspeed.claw`.
- `role` or `persona`: a prompt/tool preset within an agent type.
- `agent node`: Fleet-level logical identity, if/when Fleet exists.
- `session`: durable event-stream identity.
- `run`: one admitted unit of execution within a session.
- `spawn edge`: durable relation from parent to child.
- `fork policy`: which parent context is made visible to the child.

Do not collapse `agent type`, `role`, `session`, and `run` into one field. Both
reference systems suffer some naming ambiguity here.

## Codex

### High-Level Shape

Codex subagents are real child threads managed by a session-scoped
`AgentControl`.

Relevant files:

- `/Users/lukas/dev/tmp/codex/codex-rs/core/src/tools/handlers/multi_agents_v2/spawn.rs`
- `/Users/lukas/dev/tmp/codex/codex-rs/core/src/tools/handlers/multi_agents/spawn.rs`
- `/Users/lukas/dev/tmp/codex/codex-rs/core/src/tools/handlers/multi_agents_common.rs`
- `/Users/lukas/dev/tmp/codex/codex-rs/core/src/agent/control.rs`
- `/Users/lukas/dev/tmp/codex/codex-rs/core/src/agent/control/spawn.rs`
- `/Users/lukas/dev/tmp/codex/codex-rs/protocol/src/protocol.rs`

The main path is:

```text
model tool call: spawn_agent
  -> multi_agents_v2::spawn::handle_spawn_agent
  -> build child Config from the live parent turn
  -> create SessionSource::SubAgent(ThreadSpawn { ... })
  -> AgentControl::spawn_agent_with_metadata
  -> AgentControl::spawn_agent_internal
  -> ThreadManagerState::{spawn_new_thread_with_source | fork_thread_with_source}
  -> send initial input to the child thread
  -> notify clients and persist parent/child edge
```

### Tool Surface

Codex has a legacy namespace shape and a newer v2 shape.

Legacy v1 exposes tools under a namespace:

```text
multi_agent_v1.spawn_agent
multi_agent_v1.send_input
multi_agent_v1.wait_agent
multi_agent_v1.resume_agent
multi_agent_v1.close_agent
```

Multi-agent v2 exposes plain tools:

```text
spawn_agent
send_message
followup_task
wait_agent
interrupt_agent
list_agents
```

The v2 split is important:

- `send_message` queues a message without triggering a new turn.
- `followup_task` sends a task and triggers a turn when the target can run.
- `wait_agent` waits for mailbox activity or timeout, and intentionally returns
  only a summary, not the full final content.
- `interrupt_agent` interrupts a target agent's current turn.
- `list_agents` lists live agents in the root tree by canonical task path.

This is a better model than one generic "message" operation because it separates
notification, tasking, waiting, and interruption.

### Spawn Arguments

The v2 `spawn_agent` schema requires:

```text
message: string
task_name: string
```

Optional fields:

```text
agent_type: string
model: string
reasoning_effort: string
service_tier: string
fork_turns: "none" | "all" | positive integer string
```

The handler denies unknown fields and rejects the legacy `fork_context` flag in
v2.

`task_name` is joined onto the parent's canonical agent path. If the current
agent is `/root/planner` and it spawns `task_name = "reader"`, the child is
`/root/planner/reader`.

`fork_turns` controls context propagation:

- `none`: start the child with only the initial task.
- `all`: fork full parent history.
- `N`: fork the most recent N turns.

For `fork_turns = all`, Codex rejects `agent_type`, `model`, and
`reasoning_effort` overrides. Full-history forks inherit parent identity and
model settings to keep the fork coherent and cache-compatible.

### Child Session Source

Codex tags child sessions with:

```rust
SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
    parent_thread_id,
    depth,
    agent_path,
    agent_nickname,
    agent_role,
})
```

This metadata is durable enough to:

- identify the parent thread,
- derive the child path,
- route relative names,
- distinguish thread-spawn subagents from review/compact/internal subagents,
- emit subagent analytics,
- restore parent/child links when resuming.

The important design point is that "this is a subagent" is not only UI state.
It is part of the thread/session source.

### Child Config Construction

Codex does not blindly clone stale parent config. It builds a spawn config from
the live turn:

```text
base config from turn.config
  + live model slug
  + live model provider info
  + live reasoning effort/default
  + live reasoning summary setting
  + live developer instructions
  + base instructions
  + live approval policy
  + approvals reviewer
  + live cwd
  + live permission profile
  + service tier selection
  + selected environments
  + optional role-specific config
  + optional model/reasoning override
```

This is a concrete thing to copy. Child runs must inherit the parent's active
runtime state, not just the originally persisted session config. Otherwise the
child can run with the wrong model, cwd, permission policy, sandbox, or
environment bindings.

Codex applies model overrides by resolving against the available model list and
validating requested reasoning effort against the selected model. It also
validates requested service tier against model capabilities.

### AgentControl

`AgentControl` is the control-plane handle shared by a root thread and all its
descendants. It owns:

- the session id shared by the root tree,
- a weak handle to `ThreadManagerState`,
- an in-memory `AgentRegistry`,
- spawn reservations and concurrency accounting,
- v2 residency state,
- rollout budget state.

On spawn it:

1. Computes the effective multi-agent version.
2. Checks execution capacity.
3. Reserves a spawn slot.
4. Reserves the agent path and nickname.
5. Creates or forks the child thread.
6. Commits the reservation with the new thread id.
7. Emits subagent session-start analytics.
8. Notifies clients that a thread was created.
9. Persists the parent/child spawn edge.
10. Sends the initial task to the child.

The reservation step is important. It avoids races where two child agents claim
the same path/nickname or exceed the concurrency cap.

### Context Forking

For forked children, Codex flushes the parent's rollout before reading stored
history. This prevents a fork from missing recent parent items that are queued
for async persistence.

It then filters the copied rollout:

- keeps system/developer/user messages,
- keeps final assistant answers,
- keeps compaction/session metadata,
- drops tool calls, tool outputs, reasoning items, web/image calls, and
  inter-agent communication,
- for truncated forks, drops reference context items that are only valid for
  full-history forks,
- strips old multi-agent usage hints,
- injects the child-specific multi-agent usage hint.

This is the right default for event-sourced systems: fork a sanitized semantic
history, not raw provider traffic with incomplete tool protocol state.

### Messaging and Completion

In v2, text initial tasks and follow-up tasks are represented as
`InterAgentCommunication` with:

```text
author agent path
recipient agent path
message content
trigger_turn bool
```

When a v2 child completes or aborts a turn, the child's session forwards a
standard completion envelope to the direct parent. Parent notification is not
just a UI callback; it is modeled as inter-agent communication.

For legacy/non-v2 paths Codex also has a detached completion watcher that
subscribes to the child status and injects a parent notification when the child
reaches a final status.

### Codex Lessons

Copy:

- real child sessions/threads,
- durable parent/child spawn edges,
- canonical task paths,
- explicit `send_message` vs `followup_task`,
- explicit context fork policy,
- config built from live turn state,
- concurrency reservations,
- completion notifications as structured inter-agent messages,
- model/reasoning/service-tier validation before spawn.

Be careful with:

- overloading `agent_type` for what is really a role/persona,
- returning raw thread ids to the model in public product APIs,
- hiding too much metadata if operators need debugging visibility,
- full-history forks that accidentally preserve incomplete provider tool state.

## Claude Code

### High-Level Shape

Claude Code's normal subagent is an in-process nested `query()` loop. It is not
a separate durable session. The system wraps that nested loop in task state,
transcript sidechains, progress events, and optional background execution.

Relevant files:

- `/Users/lukas/dev/tmp/claude-code/src/tools/AgentTool/AgentTool.tsx`
- `/Users/lukas/dev/tmp/claude-code/src/tools/AgentTool/runAgent.ts`
- `/Users/lukas/dev/tmp/claude-code/src/tools/AgentTool/loadAgentsDir.ts`
- `/Users/lukas/dev/tmp/claude-code/src/tools/AgentTool/agentToolUtils.ts`
- `/Users/lukas/dev/tmp/claude-code/src/utils/forkedAgent.ts`
- `/Users/lukas/dev/tmp/claude-code/src/tasks/LocalAgentTask/LocalAgentTask.tsx`

The main path is:

```text
model tool call: Agent
  -> AgentTool.call
  -> resolve selected AgentDefinition
  -> decide sync vs async/background
  -> build child messages/system prompt/tool pool/model/permissions
  -> optional worktree or remote isolation
  -> register LocalAgentTask for async or foreground task for sync
  -> runAgent
  -> createSubagentContext
  -> query({ messages, systemPrompt, userContext, systemContext, child ToolUseContext })
  -> record sidechain transcript and emit progress
```

### Agent Tool Arguments

The base `Agent` tool schema accepts:

```text
description: short task description
prompt: task prompt
subagent_type: optional specialized agent type
model: optional sonnet | opus | haiku override
run_in_background: optional bool
```

Additional gated fields include:

```text
name: addressable spawned teammate name
team_name: target team
mode: permission mode for spawned teammate
isolation: worktree | remote
cwd: absolute cwd override
```

Claude Code has a separate "teammate" path: if `team_name` and `name` are set,
`AgentTool` calls `spawnTeammate` rather than the normal in-process subagent
path. That path can involve split panes or tmux-like multi-agent behavior. The
ordinary `Agent` subagent path is otherwise a nested query.

### Agent Definitions

Subagent types are loaded from built-ins, user/project files, policy settings,
flag settings, and plugins. A definition can include:

```text
agentType
whenToUse
tools
disallowedTools
model
effort
permissionMode
mcpServers
hooks
maxTurns
skills
initialPrompt
memory
background
isolation
requiredMcpServers
color
omitClaudeMd
```

Built-ins illustrate the pattern:

- `general-purpose`: all tools, inherits model by default, broad research and
  implementation prompt.
- `Explore`: read-only search agent, blocks edit/write tools, often uses a
  cheaper model, omits `CLAUDE.md` context.
- `Plan`: read-only architect/planning agent, blocks edit/write tools, inherits
  model.

This is a useful distinction for Lightspeed: most "agent types" in Claude Code
are actually role/persona/tool-profile presets over the same runtime.

### Spawn Decision

`AgentTool.call` does the following before launching a child:

1. Resolves current app state and permission mode.
2. Rejects unavailable team/subagent combinations.
3. If `team_name + name` are set, uses teammate spawning.
4. Chooses a fork path or a named agent definition.
5. Checks permission deny rules for the selected agent type.
6. Checks required MCP servers are connected and have tools.
7. Resolves the child model.
8. Decides isolation mode from the explicit argument or the agent definition.
9. Builds prompt messages:
   - fork path: clone parent context with fork-specific messages,
   - normal path: a simple user message containing the child prompt.
10. Builds or defers the system prompt.
11. Builds the worker tool pool from the worker permission context.
12. Creates a stable child agent id.
13. Optionally creates a git worktree and runs child tools from that cwd.
14. Decides sync vs async.

### Sync vs Async

Claude Code runs a child asynchronously when any of these are true:

- `run_in_background = true`,
- the selected agent definition has `background = true`,
- coordinator mode is active,
- the fork-subagent experiment forces async,
- assistant/proactive modes force async,
- background tasks are not disabled.

Async path:

```text
registerAsyncAgent
  -> create LocalAgentTask
  -> create output file symlink to transcript
  -> create child abort controller
  -> register task in AppState
  -> start runAsyncAgentLifecycle in detached async closure
  -> return { status: "async_launched", agentId, outputFile, ... }
```

Background agents are intentionally unlinked from the parent turn's cancellation
in the main REPL path. Pressing ESC cancels the main thread, not necessarily the
background child. They are killed explicitly through task controls.

Sync path:

```text
register foreground task
run runAgent inline
stream subagent tool/progress messages through Agent tool progress
return final AgentToolResult
```

A sync child can be backgrounded while running. Claude Code then stops the
foreground iterator, starts a background continuation, and returns the same
`async_launched` shape to the parent.

### runAgent Configuration

`runAgent` is where most useful child-run configuration happens.

It resolves:

- child `agentId`,
- effective model,
- transcript subdirectory,
- forked or fresh message history,
- child read-file cache,
- user context and system context,
- permission mode,
- effort level,
- child tool pool,
- child system prompt,
- abort controller,
- SubagentStart hook context,
- frontmatter hooks,
- preloaded skills,
- agent-specific MCP servers,
- child `ToolUseContext`,
- sidechain transcript and metadata.

Then it calls:

```ts
query({
  messages: initialMessages,
  systemPrompt: agentSystemPrompt,
  userContext: resolvedUserContext,
  systemContext: resolvedSystemContext,
  canUseTool,
  toolUseContext: agentToolUseContext,
  querySource,
  maxTurns,
})
```

It records each assistant/user/progress message into a sidechain transcript and
yields recordable messages back to the caller for progress/finalization.

Cleanup kills child shell tasks, releases MCP clients created for the agent,
clears hooks, clears prompt-cache tracking, clears cloned file-state caches, and
removes per-agent todo state.

### Model Resolution

Claude Code's subagent model logic is useful:

1. Environment override `CLAUDE_CODE_SUBAGENT_MODEL` wins.
2. Tool-specified model wins over agent definition.
3. Agent definition model wins over default.
4. Default is `inherit`.
5. If a bare alias like `opus`, `sonnet`, or `haiku` matches the parent's tier,
   the child inherits the exact parent model string rather than resolving to a
   provider default.
6. Bedrock region prefixes are inherited from the parent unless the child
   explicitly specifies a full regional model id.

Lightspeed should do the equivalent for provider-specific deployment details:
do not let an alias override accidentally change region, tier, or deployment
policy.

### Tool and Permission Resolution

Claude Code starts from an assembled worker tool pool, then applies agent-level
filters:

- MCP tools are generally allowed.
- globally disallowed agent tools are removed.
- custom agents have additional disallowed tools.
- async agents are restricted to an async-safe allowlist.
- explicit `disallowedTools` are removed.
- explicit `tools` restrict to a named subset unless `*` is used.
- `Agent(...)` can be used on the main thread to constrain which subagent types
  the model may select.

Async children usually cannot show permission prompts, so their child
`getAppState` sets `shouldAvoidPermissionPrompts`. Bubble/interactive modes can
opt back into prompt display.

### Child Context Isolation

`createSubagentContext` is the most reusable Claude Code pattern.

By default it isolates mutable state:

- clones read-file state,
- creates a child abort controller linked to the parent,
- wraps `getAppState`,
- makes most mutation callbacks no-ops,
- creates fresh memory/skill trigger sets,
- creates local denial tracking,
- removes UI callbacks,
- creates a new query tracking chain with incremented depth.

It explicitly opts back into sharing where needed:

- `shareSetAppState` for interactive subagents,
- `shareSetResponseLength` for metrics,
- root `setAppStateForTasks` so background shell/task state is still tracked,
- cloned content-replacement state for prompt-cache-compatible forks.

This is the right default stance: isolate child mutable state first, then share
only the small pieces required for product behavior.

### Claude Code Lessons

Copy:

- explicit agent definitions as role/tool/model presets,
- per-child tool and permission resolution,
- child MCP server additions,
- child hooks and skill preloading,
- worktree/remote isolation options,
- robust sync-to-background transition,
- sidechain transcripts for child progress,
- context isolation with opt-in sharing,
- model inheritance that preserves provider-specific deployment details.

Be careful with:

- in-process child loops that are hard to make durable,
- background tasks that outlive their parent unless lifecycle is explicit,
- hiding durable identity behind local task ids,
- letting agent definitions become a marketplace before product pressure exists,
- allowing child tools/permissions to inherit parent state accidentally.

## Comparison

| Topic | Codex | Claude Code | Lightspeed implication |
| --- | --- | --- | --- |
| Child identity | Real child thread | In-process `agentId` plus task/transcript | Use durable child sessions/runs |
| Parent link | `ThreadSpawn` source and persisted edge | Parent context/task metadata | Persist a spawn edge outside child local state |
| Trigger | `spawn_agent` tool | `Agent` tool | Expose a model-visible fleet/subagent tool |
| Context fork | `none/all/N`, sanitized rollout | fork context or prompt-only messages | Make fork policy explicit and replayable |
| Config inheritance | Live turn config copied into child config | child `ToolUseContext` assembled dynamically | Derive child run config from live parent run state |
| Tools | generally same tool universe, role/config can alter | definition-level tools/disallowed tools/async allowlist | Use named tool profiles plus per-role restrictions |
| Model override | model/reasoning/service tier validated | alias/inherit/provider-region aware | Validate overrides against provider capabilities |
| Completion | inter-agent communication to parent | task notification / Agent result | Use structured mailbox/completion events |
| Waiting | mailbox summary, not full content | task output/progress file | Separate wakeup from content delivery |
| Durability | strong | local sidechain, weaker | Prefer Codex for hosted product semantics |

## Reproducing a Subagent System

A minimal but serious subagent system needs these pieces.

### 1. Spawn Tool

Model-visible input:

```text
task_name: string
message: string or structured input
role_id: optional string
model: optional override
reasoning_effort: optional override
service_tier: optional override
context: { mode: "none" | "all" | "last_n", turns?: number }
tool_profile: optional string
permission_profile: optional string
environment: optional inherit | isolate | target id
background: optional bool
```

Return:

```text
child_agent_id or child_session_id
task_name / canonical path
status
```

For a public product API, prefer returning a product handle and canonical task
name over raw reducer/thread internals.

### 2. Admission Boundary

Validate before creating the child:

- task name syntax and uniqueness under parent,
- spawn depth and concurrency,
- caller is allowed to spawn,
- selected role/type exists,
- model/reasoning/service-tier override is supported,
- tool/permission profile exists,
- environment inheritance or isolation is possible,
- requested context policy is valid.

### 3. Child Config

Build child config from live parent state:

```text
parent base instructions
parent current developer instructions
parent selected model/provider/deployment
parent reasoning settings
parent approval and permission mode
parent active cwd/environment binding
parent tool registry revision
selected role/persona prompt
selected tool profile
selected MCP/connector grants
selected context fork
```

Do not use only the session-start config. The parent may have changed model,
tool profile, environment, cwd, or permissions during the live turn.

### 4. Durable Graph

Persist a spawn edge:

```text
parent_agent_id / parent_session_id
child_agent_id / child_session_id
relation = "spawned"
task_name / path
role_id
created_at_ms
status
metadata
```

The child session log should not need to replay the whole Fleet registry to know
its own state, but the parent and operators need a durable graph for routing,
listing, waiting, and cleanup.

### 5. Context Fork

Build child context from event-sourced parent state:

- `none`: only the child task.
- `last_n`: recent semantic turns.
- `all`: full semantic history.

Sanitize the fork:

- keep durable user/developer/system facts,
- keep final assistant answers if useful,
- keep compaction summaries,
- drop incomplete tool calls,
- drop raw tool outputs unless explicitly selected,
- drop provider/runtime noise,
- replace parent usage hints with child usage hints,
- include explicit parent/child identity metadata.

The forked context should be reproducible from the session log and CAS, not from
live process memory.

### 6. Runtime Launch

For Lightspeed, the deterministic engine should not directly spawn sessions or
perform side effects. The likely split is:

```text
CoreAgent plans/records a subagent-spawn intent
  -> runtime adapter validates and creates child session/run
  -> runtime records spawn accepted/failed event for parent
  -> child workflow/session receives initial input
```

In Temporal, child work can be represented as a new session workflow or a child
workflow, but the public model should still look like a child agent/session with
a durable spawn edge.

### 7. Messaging

Expose separate operations:

```text
send_message(target, message)      # deliver, do not start turn
followup_task(target, message)     # deliver and start/resume work
wait_agent(timeout)                # wait for mailbox/status activity
interrupt_agent(target)            # interrupt active turn
list_agents(path_prefix?)          # inspect live children
```

Do not make `wait_agent` the only way to receive content. Completion should be a
mailbox/event delivery to the parent. Waiting is just a synchronization aid.

### 8. Status and Cleanup

Track at least:

```text
pending_init
running
waiting
completed
errored
interrupted
shutdown
not_found
```

Cleanup should be explicit:

- cancel or interrupt child runs,
- release concurrency slots,
- close or archive child sessions,
- detach or dispose isolated environments/worktrees,
- preserve logs and CAS blobs according to retention policy.

## Lightspeed Design Position

For the first Lightspeed subagent implementation, prefer:

- child subagents as ordinary Lightspeed sessions/runs,
- a durable parent-child edge in Fleet/session metadata,
- one backing session per child agent node,
- context fork policy as an explicit spawn field,
- child config compiled from parent live run state plus role/tool/environment
  overrides,
- mailbox-style inter-agent events,
- model-visible tools implemented by runtime adapters, not by nondeterministic
  engine code.

Do not start with:

- a public marketplace of agent types,
- arbitrary plugin loading,
- a full graph policy engine,
- generic SDK-style dynamic interfaces,
- in-process-only subagent loops that cannot survive workflow replay.

Codex gives the better durable control plane. Claude Code gives the better
per-child execution envelope. A good Lightspeed design should combine those two
pieces rather than copying either system wholesale.

