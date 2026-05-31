# P72: Timers, Schedules, Cron, And External Triggers

**Status**
- Proposed
- Builds on the current Forge session/run API, the Temporal-backed workflow
  runtime, P62 CAS/VFS, P70 fleet concepts, and P71 prompt management.
- Inspired by OpenClaw's cron and heartbeat systems, but intended for Forge's
  deterministic engine and hosted product architecture.

## Goal

Add a first-class trigger system for Forge agents.

The first trigger types should cover:

- one-shot timers,
- recurring interval schedules,
- cron-expression schedules,
- manually fired trigger runs,
- future external event triggers such as webhooks, inboxes, file/VFS watches,
  product events, calendar events, and connector notifications.

The trigger system should let an agent or operator say:

- "Every hour, check this page and tell me if the item is available."
- "Every morning, summarize overnight activity."
- "At 17:00 tomorrow, remind me and prepare a draft."
- "When this external event arrives, run this agent task."

The key architectural requirement is that long-running recurring work must not
depend on an ever-growing main conversation context. Schedules should persist
as small durable trigger definitions. Each firing should reconstruct a scoped
agent run from the trigger payload, current time, routing policy, prompt
configuration, and any explicitly selected context.

## Background

Forge is already oriented around durable sessions and asynchronous hosted runs:

- `README.md` describes the runtime model: clients start/open sessions through
  `api`, runtimes admit input as CoreAgent commands, the core emits append/LLM
  or tool actions, and runtime/workflow substrates fulfill actions through
  stores, adapters, or workflow activities.
- `crates/api/src/lib.rs` exposes `session/start`, `session/update`,
  `session/read`, `session/events/read`, `session/close`, `run/start`, and
  `run/cancel`.
- `crates/gateway/src/service.rs` owns API-to-command conversion for
  `run/start`: inline input is written to CAS or an existing blob ref is
  validated, then a `CoreAgentCommand::RequestRun` is encoded as a workflow
  admission and signaled to Temporal.
- `crates/workflow/src/workflow.rs` runs `AgentSessionWorkflow`, keeps a tiny
  workflow-local pending admission queue, replays committed Forge session
  events into `CoreAgentState`, and drives the core to quiescence.
- `crates/engine` remains deterministic and must not perform wall-clock reads,
  provider calls, host filesystem access, network I/O, or scheduler work.

This gives Forge the low-level machinery needed to execute scheduled work once
someone submits a run. What is missing is the product/runtime layer that owns:

- durable trigger definitions,
- schedule calculation,
- trigger fire reservation and retry,
- mapping trigger firings to Forge sessions/runs,
- delivery behavior,
- trigger observability,
- update/reload semantics,
- external trigger sources beyond time.

## OpenClaw Reference Study

OpenClaw has two scheduling-related systems and one adjacent command-routing
system:

1. Explicit cron jobs.
2. Heartbeat / `HEARTBEAT.md` task directives.
3. Native slash-command routing, which can invoke cron tooling but is not the
   scheduler itself.

The important lesson from OpenClaw is that scheduled work is not kept alive by
the main chat transcript. OpenClaw persists small job definitions and creates a
scoped run when a timer fires.

### Explicit Cron Types

OpenClaw's cron model lives in:

- `/Users/lukas/dev/tmp/openclaw/src/cron/types.ts`

The key types are:

- `CronSchedule`
  - `{ kind: "at"; at: string }`
  - `{ kind: "every"; everyMs: number; anchorMs?: number }`
  - `{ kind: "cron"; expr: string; tz?: string; staggerMs?: number }`
- `CronSessionTarget`
  - `"main"`
  - `"isolated"`
  - `"current"`
  - `session:<id>`
- `CronWakeMode`
  - `"next-heartbeat"`
  - `"now"`
- `CronDeliveryMode`
  - `"none"`
  - `"announce"`
  - `"webhook"`
- `CronPayload`
  - `systemEvent`
  - `agentTurn`

`CronJobState` tracks durable scheduling and execution state such as
`nextRunAtMs`, `runningAtMs`, `lastRunAtMs`, status, duration, error text, and
consecutive errors.

Validation is explicit in:

- `/Users/lukas/dev/tmp/openclaw/src/cron/service/jobs.ts`

OpenClaw enforces:

- `sessionTarget: "main"` requires `payload.kind: "systemEvent"`.
- `sessionTarget: "isolated"`, `"current"`, or `session:<id>` require
  `payload.kind: "agentTurn"`.
- `sessionTarget: "main"` is only valid for the default agent.
- non-default agents should use isolated `agentTurn` jobs.
- delivery and failure-routing modes are restricted based on target shape.

This separation is valuable. "Main" means "wake the assistant with a small
event/reminder", not "keep doing arbitrary background work inside the user's
ordinary chat context".

### Cron Store And Loading

OpenClaw stores cron definitions separately from ordinary transcripts:

- `/Users/lukas/dev/tmp/openclaw/src/cron/store.ts`
- `/Users/lukas/dev/tmp/openclaw/src/cron/service/store.ts`

The default file store is a `cron/jobs.json` path under the OpenClaw config
directory, with SQLite support also present. The service loads the store,
normalizes jobs, hydrates state, quarantines invalid persisted records, and
persists a sanitized result.

The store load path has a fast in-memory path during ordinary operations and a
force-reload path used by the timer loop before reserving due jobs. This lets
external job updates be seen without restarting the whole agent.

### Cron Timer Loop

The scheduler loop lives in:

- `/Users/lukas/dev/tmp/openclaw/src/cron/service/timer.ts`

The loop shape is:

```text
start service
  -> load cron store
  -> mark previously running jobs interrupted
  -> run missed-job handling
  -> recompute next wake
  -> arm bounded setTimeout

timer fires
  -> force-reload store
  -> collect due jobs
  -> mark due jobs runningAtMs
  -> persist reservation
  -> execute jobs with bounded concurrency
  -> force-reload store
  -> apply outcomes
  -> recompute next runs
  -> persist
  -> sweep expired run sessions
  -> arm next timer
```

The timer deliberately clamps delays to a short maximum, currently one minute,
so clock jumps, missed wakeups, or stale timers are corrected by periodic
rechecking. It also avoids hot loops by imposing a small minimum refire gap.

Manual `cron.run` uses the same execution machinery, queued through a cron
command lane rather than blocking the API call.

### Startup And Missed Runs

OpenClaw startup behavior lives in:

- `/Users/lukas/dev/tmp/openclaw/src/cron/service/ops.ts`

On start, the cron service:

- loads persisted jobs,
- marks jobs with stale `runningAtMs` as interrupted/failed,
- applies missed-run handling,
- recomputes maintenance state,
- emits interrupted-run events,
- arms the timer.

This is another key lesson: scheduler durability is in the job store and
execution state, not in a long-lived in-memory timeout alone.

### Main-Session Cron Path

The `main` target path is handled by:

- `/Users/lukas/dev/tmp/openclaw/src/cron/service/timer.ts`
- `/Users/lukas/dev/tmp/openclaw/src/cron/service/task-runs.ts`
- `/Users/lukas/dev/tmp/openclaw/src/infra/system-events.ts`
- `/Users/lukas/dev/tmp/openclaw/src/infra/heartbeat-runner.ts`
- `/Users/lukas/dev/tmp/openclaw/src/infra/heartbeat-events-filter.ts`

For `sessionTarget: "main"`:

1. The cron payload is a small `systemEvent` text.
2. OpenClaw creates a cron run session key shaped like:

   ```text
   agent:<agentId>:cron:<jobId>:run:<runId>
   ```

3. It enqueues the system event with a cron context key.
4. Depending on `wakeMode`, it either requests the next heartbeat or tries to
   run heartbeat immediately.
5. Heartbeat selects the cron event, builds a cron-specific prompt body, and
   dispatches a normal assistant turn/delivery.

The system-event queue itself is in-memory and intentionally small. This means
cron definitions and job state are durable, but an already-enqueued event can be
lost on process crash if it was marked successful before heartbeat consumed it.
Forge should avoid this gap by representing trigger firings durably, even when
delivery is delayed.

### Isolated Cron Agent-Turn Path

The isolated/background work path lives in:

- `/Users/lukas/dev/tmp/openclaw/src/gateway/server-cron.ts`
- `/Users/lukas/dev/tmp/openclaw/src/cron/isolated-agent/run.ts`
- `/Users/lukas/dev/tmp/openclaw/src/cron/isolated-agent/session.ts`
- `/Users/lukas/dev/tmp/openclaw/src/cron/isolated-agent/run-executor.ts`

For `payload.kind: "agentTurn"`:

1. OpenClaw resolves an agent runtime config snapshot.
2. It resolves a cron session key, commonly:

   ```text
   agent:<agentId>:cron:<jobId>
   ```

3. For `sessionTarget: "isolated"`, it forces a fresh session id per run.
4. It constructs a run session key, often including `:run:<uuid>`.
5. It prepares bootstrap/workspace files and skills.
6. It builds a command body roughly shaped like:

   ```text
   [cron:<jobId> <jobName>] <job message>
   Current time: ...
   <delivery instruction>
   ```

7. It executes the agent through the CLI or embedded runtime with
   `trigger: "cron"` and `bootstrapContextRunKind: "cron"`.
8. It finalizes status, usage, delivery, run logs, and task-run records.
9. It disposes in-memory run context for isolated jobs.

This path is the stronger model for long-running recurring work. Each firing
gets a scoped run. The task definition stays durable. Context is intentionally
limited and reconstructed each time.

### Prompt Behavior

OpenClaw does not inject cron content in one uniform way:

- Main `systemEvent` cron jobs are transformed into heartbeat prompt body text.
  They are not provider-level system prompts.
- Ordinary queued system events can be drained as prefixed body text such as
  `System: [timestamp] ...`.
- Isolated `agentTurn` cron jobs build a fresh user/command body containing the
  cron marker, job message, current time, and delivery instructions.
- Cron and subagent session keys resolve to minimal system-prompt mode in:
  `/Users/lukas/dev/tmp/openclaw/src/agents/embedded-agent-runner/run/attempt.prompt-helpers.ts`
- System prompt modes and prompt sections live in:
  `/Users/lukas/dev/tmp/openclaw/src/agents/system-prompt.ts`

This matters for Forge. Timer payloads should be ordinary run input or explicit
runtime instructions, not hidden mutations of a session's historical chat
messages. Provider-level instructions should still come from Forge's compiled
session config / P71 prompt management path.

### Heartbeat And `HEARTBEAT.md`

Heartbeat is distinct from explicit cron jobs:

- `/Users/lukas/dev/tmp/openclaw/src/infra/heartbeat-runner.ts`
- `/Users/lukas/dev/tmp/openclaw/src/infra/heartbeat-wake.ts`
- `/Users/lukas/dev/tmp/openclaw/src/auto-reply/heartbeat.ts`

`HEARTBEAT.md` can contain prose directives and a `tasks:` block. Parsed tasks
have names, intervals, and prompts. Due checks compare the interval against
last-run state stored on the session entry, not in the cron job store.

Heartbeat can also wake for reasons other than explicit tasks, such as queued
cron system events. It has busy checks for active replies, active cron jobs,
and command lanes. It can retry when the agent is busy.

Useful lessons:

- periodic maintenance/wake behavior can be separate from explicit user-created
  cron jobs;
- lightweight heartbeat tasks are ergonomic for simple recurring nudges;
- trigger state still needs a clear durability boundary;
- prompt directives loaded from editable files can change without process
  restart because the runner re-reads them at heartbeat preflight.

### Slash Commands

Native slash commands are adjacent but separate:

- `/Users/lukas/dev/tmp/openclaw/src/channels/native-command-session-targets.ts`
- `/Users/lukas/dev/tmp/openclaw/src/auto-reply/reply/commands-steer.test.ts`

They route command handling into per-user command sessions and carry a target
session key. They can be used as a UI/transport surface to create or manage
cron jobs, but they are not where schedules persist.

## Problem Statement

Forge currently has no product-level trigger store or scheduler.

The current system can execute a run once a client submits it through
`run/start`, but it cannot yet answer:

- Which scheduled tasks exist for this agent?
- Which session or agent profile owns a schedule?
- When will the next firing occur?
- Is a job currently running?
- Was a firing missed while the runtime was down?
- Should missed firings catch up, coalesce, skip, or fail?
- Does the scheduled task run in the main session, a side session, or a fresh
  ephemeral session?
- Does a prompt edit affect already-created triggers?
- Does a trigger update affect an already-running firing?
- How does a webhook or VFS watch reuse the same routing model as a timer?
- How does the user inspect, pause, run-now, or delete recurring work?

Forge needs this as a runtime/API feature. The deterministic engine should not
become a scheduler.

## Non-Goals

- Do not put wall-clock timers, cron parsing, file watches, webhooks, or
  schedule calculation inside `engine`.
- Do not make the main conversation transcript the source of truth for
  recurring work.
- Do not require every scheduled task to share the user's ordinary session
  context.
- Do not mutate historical context items when a trigger definition changes.
- Do not hide trigger firings only in in-memory queues.
- Do not require Temporal for the type model; local runtimes should be able to
  implement a simpler scheduler. Temporal should own production durability.
- Do not implement a full connector/event-ingestion system in the first timer
  milestone.
- Do not require OpenClaw's exact `main` / `isolated` names if Forge lands on
  clearer terminology.

## Design Position

Triggers are a hosted runtime and API concern that compile down to ordinary
Forge session/run admissions.

Target shape:

```text
trigger definition
  -> trigger store
  -> Temporal Schedule / local scheduler / external event source
  -> durable trigger firing record
  -> session routing policy
  -> CAS input materialization
  -> CoreAgentCommand::RequestRun admission
  -> existing Forge session workflow
```

The engine should see the same thing it sees today: a run request with an input
blob and run configuration. It may later record trigger provenance on run
metadata, but it should not own timing.

For hosted timer sources, Forge should prefer Temporal Schedules as the durable
clock. Forge should still keep its own trigger definitions and firing history
as product state. Temporal Schedules should wake Forge with stable ids; Forge
should resolve prompts, routing, permissions, delivery, and run admission.

## Core Concepts

### Trigger Definition

A trigger definition is the durable intent:

```rust
struct TriggerDefinition {
    trigger_id: TriggerId,
    agent_id: Option<AgentId>,
    owner: TriggerOwner,
    schedule_or_source: TriggerSource,
    routing: TriggerRouting,
    payload: TriggerPayload,
    delivery: TriggerDelivery,
    policy: TriggerPolicy,
    status: TriggerStatus,
    created_at_ms: i64,
    updated_at_ms: i64,
    revision: u64,
}
```

Possible source variants:

```rust
enum TriggerSource {
    At { at_ms: i64 },
    Every { every_ms: i64, anchor_ms: Option<i64> },
    Cron { expr: String, timezone: Option<String>, stagger_ms: Option<i64> },
    Webhook { endpoint_id: String },
    VfsWatch { workspace_id: String, path_glob: String, event_kinds: Vec<String> },
    External { provider: String, source_id: String, filter_ref: Option<BlobRef> },
    Manual,
}
```

The source model should be extensible from the start. Timers are just the first
source family.

### Trigger State

Trigger state should be separate from the definition but persisted with the
same transactional guarantees:

```rust
struct TriggerRuntimeState {
    next_fire_at_ms: Option<i64>,
    running_firing_id: Option<TriggerFiringId>,
    last_fire_at_ms: Option<i64>,
    last_success_at_ms: Option<i64>,
    last_failure_at_ms: Option<i64>,
    consecutive_failures: u32,
    last_error_ref: Option<BlobRef>,
}
```

The scheduler should persist reservations before running work:

```text
due trigger found
  -> create firing record
  -> set running_firing_id / lease
  -> commit
  -> submit run
  -> observe completion
  -> update firing and trigger state
```

This avoids OpenClaw's in-memory system-event loss mode.

### Trigger Firing

A firing is a durable execution attempt:

```rust
struct TriggerFiring {
    firing_id: TriggerFiringId,
    trigger_id: TriggerId,
    trigger_revision: u64,
    scheduled_for_ms: Option<i64>,
    fired_at_ms: i64,
    lease_expires_at_ms: Option<i64>,
    status: TriggerFiringStatus,
    route: ResolvedTriggerRoute,
    input_ref: Option<BlobRef>,
    session_id: Option<SessionId>,
    submission_id: Option<SubmissionId>,
    run_id: Option<RunId>,
    output_ref: Option<BlobRef>,
    error_ref: Option<BlobRef>,
    completed_at_ms: Option<i64>,
}
```

This is the durable object users and operators inspect when asking "what
happened last night?" It should be visible without replaying arbitrary chat
history.

### Routing

Forge should support at least four routing modes:

```rust
enum TriggerRouting {
    MainSession { session_id: SessionId },
    FreshEphemeralSession { profile_ref: Option<AgentProfileRef> },
    StableTriggerSession { key: String, profile_ref: Option<AgentProfileRef> },
    ExplicitSession { session_id: SessionId },
}
```

Interpretation:

- `MainSession` is for reminders and assistant-visible events tied to a user's
  ordinary session. It should still create a normal run admission, not mutate
  the transcript outside the event log.
- `FreshEphemeralSession` is the default for independent recurring work. Each
  firing gets its own session/run scope and can be retained or pruned by policy.
- `StableTriggerSession` gives a recurring task durable task-local memory
  without polluting the user's main session.
- `ExplicitSession` intentionally binds to a named session and should be used
  only when the operator wants that context coupling.

OpenClaw's `"isolated"` maps mostly to `FreshEphemeralSession`.
OpenClaw's `"current"` and `session:<id>` map to `ExplicitSession` or
`StableTriggerSession` depending product semantics. OpenClaw's `"main"` maps to
`MainSession`, but Forge should avoid a non-durable in-memory event queue.

### Payload

Initial payload variants:

```rust
enum TriggerPayload {
    RunInput {
        text: String,
        run_config: Option<RunStartConfig>,
        context_policy: TriggerContextPolicy,
    },
    PromptRef {
        input_ref: BlobRef,
        run_config: Option<RunStartConfig>,
        context_policy: TriggerContextPolicy,
    },
    Event {
        text: String,
        severity: TriggerEventSeverity,
    },
}
```

For v1, all payloads can materialize to an ordinary `run/start` input blob. The
distinction is product/API-level: an event/reminder is meant to be relayed or
acknowledged; a run input is meant to perform a task.

### Prompt Assembly

Trigger prompt assembly should be explicit and observable.

For timer-backed runs, the effective run input should include:

- trigger id/name,
- firing id,
- scheduled time,
- actual fire time,
- current time and timezone,
- user/operator payload,
- delivery instructions,
- external event payload summary when applicable,
- selected context snippets if the trigger requested them.

Example materialized input:

```text
[trigger:trg_123 Daily inventory check]
Scheduled for: 2026-06-01T08:00:00Z
Fired at: 2026-06-01T08:00:03Z

Check whether the item at <url> is available. If it is available, notify me.

Delivery: announce to session session_abc only if availability changed.
```

Provider-level instructions should still come from `SessionConfig` and the P71
prompt management layer. Trigger payloads should not be secretly appended to
the session's compiled system instructions.

### Delivery

Delivery should be separate from execution:

```rust
enum TriggerDelivery {
    None,
    SessionAnnouncement { session_id: SessionId },
    Webhook { endpoint_id: String },
    Inbox { user_id: String },
}
```

The scheduled run may produce an output, but delivery policy decides whether
that output is:

- appended to a visible session,
- sent to an external webhook,
- stored silently for inspection,
- converted into a notification,
- escalated only on failure or state change.

### Missed-Run Policy

Every recurring trigger needs an explicit missed-run policy:

```rust
enum MissedRunPolicy {
    Skip,
    RunOnce,
    CatchUp { max_runs: u32 },
}
```

Recommended defaults:

- one-shot `At`: keep due until fired successfully or explicitly cancelled;
- `Every`: run once after downtime, then schedule from completion or original
  anchor depending `anchor_ms`;
- `Cron`: run once for the latest missed slot unless configured otherwise.

The first implementation should avoid unbounded catch-up.

### Concurrency Policy

Triggers need per-trigger and global concurrency controls:

```rust
enum OverlapPolicy {
    SkipIfRunning,
    QueueOne,
    AllowParallel { max_parallel: u32 },
}
```

Default should be `SkipIfRunning` or `QueueOne` for recurring jobs. A trigger
that checks a website every minute should not build a backlog for days.

## API Surface

Add trigger methods to `api` after the type model stabilizes:

```text
trigger/create
trigger/update
trigger/delete
trigger/pause
trigger/resume
trigger/run
trigger/list
trigger/read
trigger/firings/read
```

`trigger/run` should create a manual firing using the same route and payload.

Capabilities should advertise trigger support from `initialize`, similar to how
the API already advertises notifications and event-log support.

## Storage

Add a trigger store outside `engine`.

The store should support:

- create/update/delete definitions with revision checks,
- list by owner/agent/session,
- query due triggers before a timestamp,
- reserve due firings transactionally,
- complete/fail firings idempotently,
- read recent firings,
- recover stale leases,
- compute and persist next fire times for local mode, or cache/reflect next
  fire times from Temporal Schedule descriptions in hosted mode.

For local development, a filesystem or SQLite store is acceptable. For hosted
Forge, use Postgres through `store-pg`.

The store should not be the Forge session log. It is a sibling durable product
store, like a catalog of schedules and their firing history.

## Runtime Architecture

### Local Scheduler

A local scheduler can use a bounded Tokio timer loop:

```text
loop:
  now = clock.now()
  due = trigger_store.reserve_due(now, limit)
  for firing in due:
    spawn/queue execute_firing(firing)
  next = trigger_store.next_fire_at()
  sleep(min(next - now, max_recheck_delay))
```

This is useful for tests and local demos, but it has weaker crash semantics.

### Hosted Temporal Schedules

Hosted Forge should use Temporal Schedules as the preferred durable timer
substrate for recurring timer sources.

Temporal Schedules are not only deploy-time/static cron declarations. They are
independent Temporal resources with ids and service APIs for create, describe,
update, pause, unpause, trigger, backfill, list, and delete. They are separate
from older Temporal Cron Jobs, which are attached to workflow execution start
options. Temporal recommends Schedules over Cron Jobs because Schedules provide
more configuration and can be updated or paused while active.

References:

- Temporal Schedule docs: `https://docs.temporal.io/schedule`
- TypeScript Schedule update docs:
  `https://docs.temporal.io/develop/typescript/workflows/schedules#update-a-schedule`
- Temporal API exposes `CreateSchedule`, `DescribeSchedule`,
  `UpdateSchedule`, `PatchSchedule`, `DeleteSchedule`, and `ListSchedules`.
- The current local Rust dependency has schedule wrappers in
  `/Users/lukas/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/temporalio-client-0.4.0/src/schedules.rs`.

Recommended hosted shape:

```text
trigger/create
  -> write Forge TriggerDefinition in Postgres
  -> create Temporal Schedule with id forge-trigger:<trigger_id>
  -> Temporal Schedule action starts TriggerFireWorkflow(trigger_id)

Temporal Schedule fires
  -> TriggerFireWorkflow(trigger_id)
  -> reserve durable Forge TriggerFiring in Postgres
  -> resolve trigger revision, route, prompt/input, delivery policy
  -> materialize input to CAS
  -> submit CoreAgentCommand::RequestRun to AgentSessionWorkflow
  -> observe completion
  -> complete/fail TriggerFiring
  -> perform delivery
```

The Temporal Schedule should contain stable routing data only:

- schedule id,
- workflow type `TriggerFireWorkflow`,
- task queue,
- trigger id,
- minimal search attributes/memo for operations.

It should not contain full prompt text, VFS prompt material, user memory, large
payloads, or delivery logic. Those belong in Forge's trigger store, CAS, VFS,
and prompt management layer.

Trigger updates should mirror to Temporal:

```text
trigger/update
  -> update Forge TriggerDefinition revision
  -> update Temporal Schedule spec/action if timer settings changed

trigger/pause
  -> mark Forge trigger paused
  -> pause Temporal Schedule with note

trigger/resume
  -> mark Forge trigger active
  -> unpause Temporal Schedule

trigger/delete
  -> tombstone Forge trigger
  -> delete or pause Temporal Schedule
```

All Temporal Schedule API calls are external service calls. If a running agent
workflow decides a schedule should change, the workflow should not call the
Temporal Schedule API directly. It should either:

- call an activity such as `apply_trigger_update`, and that activity updates
  Forge Postgres plus the Temporal Schedule; or
- emit/propose a trigger update that the gateway or trigger manager applies.

This keeps workflow replay deterministic and gives Forge one idempotent place
to enforce permissions, revision checks, and audit logging.

One-shot future work should be represented in Forge as a trigger/firing for
product visibility. The Temporal substrate can use either:

- workflow `startDelay` for a single future `TriggerFireWorkflow`, or
- a Temporal Schedule with a bounded action count when schedule operations and
  UI consistency matter more than minimal Temporal object count.

For local mode, the bounded Tokio scheduler remains useful. For hosted mode,
timer-source triggers should default to Temporal Schedules unless the Rust SDK
or deployment environment lacks the required schedule API.

### Trigger Manager Fallback

A scanner workflow remains a fallback, not the preferred hosted timer path:

```text
TriggerManagerWorkflow
  -> durable timer / periodic scan
  -> reserve due trigger firing in Postgres activity
  -> submit admission to target AgentSessionWorkflow
  -> observe or poll completion
  -> complete/fail firing in Postgres activity
```

Use this fallback only when:

- Temporal Schedule support is unavailable in the chosen SDK/runtime,
- a trigger source is not time-based,
- the product needs bulk reconciliation,
- Temporal Schedule objects drift from Forge trigger definitions and need
  repair.

Forge should still keep reconciliation logic even when using Temporal
Schedules, because Forge's trigger store is the product source of truth.

## Session Interaction

Trigger execution should use existing session/run mechanics:

1. Resolve route to a session id and session config.
2. Start/open the session if policy allows.
3. Materialize trigger input to CAS.
4. Build `CoreAgentCommand::RequestRun` with a `SubmissionId` derived from the
   firing id.
5. Signal `AgentSessionWorkflow`.
6. Record session id/submission id on the firing.
7. Observe run id and completion through session events/projection.

This avoids a second agent execution path.

Forge may later add run provenance:

```rust
RunProvenance::Trigger {
    trigger_id,
    firing_id,
    trigger_revision,
}
```

That provenance should be reducer-visible only if needed for projection,
auditing, cancellation, or policy. The scheduler can already track the same
mapping in the trigger store.

## Prompt And Context Policy

Triggers need a context policy because the wrong default can pollute or starve
the run:

```rust
enum TriggerContextPolicy {
    None,
    MainSessionRecent { item_limit: u32 },
    StableTriggerSession,
    ExplicitRefs { refs: Vec<BlobRef> },
    VfsSnapshot { workspace_id: String, snapshot_ref: BlobRef, paths: Vec<String> },
}
```

Defaults:

- independent recurring tasks: `None` or `StableTriggerSession`;
- reminders: `MainSessionRecent` with a small limit;
- file/watch triggers: VFS snapshot refs and changed paths;
- external event triggers: event payload blob plus small rendered summary.

This connects directly to P71: prompt files and agent profile instructions can
be resolved into session config, while trigger inputs remain per-firing run
inputs.

## Update And Reload Semantics

Trigger updates should be revisioned and immediately affect future firings.

Rules:

- Updating a trigger definition does not mutate already-created firing records.
- A currently running firing continues with the route/payload snapshot recorded
  on that firing.
- Future firings use the latest enabled trigger revision.
- Pause prevents new firings but does not cancel an already-running firing
  unless explicitly requested.
- Delete should tombstone by default so historical firings remain inspectable.
- Manual run should record the trigger revision it used.
- Prompt file edits affect future session/run prompt assembly according to P71
  reload rules, not by mutating the trigger definition itself.

For VFS-watch triggers, the event should include the workspace revision or
snapshot ref that caused the firing. This prevents "what changed?" ambiguity if
the workspace changes again before the agent runs.

## Observability

Expose:

- trigger list with status and next fire time,
- trigger read with definition, revision, state, and recent firings,
- firing read/list with route, input ref, session id, run id, status, output,
  error, duration, scheduled time, and actual fire time,
- scheduler health,
- stale leases,
- skipped overlap events,
- missed-run decisions,
- delivery attempts.

The operator should be able to answer:

- "Is this job enabled?"
- "When will it run next?"
- "What prompt/input did it use last time?"
- "Which session did it run in?"
- "Did it notify anyone?"
- "Why did it not run?"

## Safety And Trust

Scheduled agents can quietly create cost and side effects. The first version
should include:

- explicit owner/agent/session scoping,
- permissions for creating/updating triggers,
- rate limits and max concurrency,
- default disabled external webhooks unless configured,
- safe handling of untrusted external payloads,
- clear distinction between system instructions and external trigger input,
- audit trail for agent-created triggers,
- optional human confirmation for high-risk recurring jobs.

For external inputs, trigger payload text should be treated as data unless the
trigger is explicitly trusted. This mirrors the P71 trust boundary for VFS
prompt material.

## Roadmap

### Phase 1: Type Model And Local Store

- Add trigger API types behind an experimental capability.
- Add a local trigger store with definitions, state, and firings.
- Support `At`, `Every`, and `Cron` source variants.
- Support manual `trigger/run`.
- Implement next-fire calculation and missed-run policy.
- Add focused tests for schedule math, revision checks, reservation, stale
  lease recovery, and update semantics.

### Phase 2: Session Routing And Run Admission

- Implement routing to an existing session and fresh ephemeral session.
- Materialize trigger input to CAS.
- Submit `CoreAgentCommand::RequestRun` through the same gateway/workflow path
  as `run/start`.
- Record firing-to-session/submission/run mapping.
- Expose firings through API.
- Add projection or metadata support if needed for trigger provenance.

### Phase 3: Hosted Temporal Runtime

- Add `TriggerFireWorkflow` as the workflow started by Temporal Schedules.
- Add Temporal Schedule create/update/pause/resume/delete mirroring from Forge
  trigger definitions.
- Keep Forge Postgres trigger definitions and firing records as the product
  source of truth.
- Implement durable firing reservation and completion through activities.
- Handle crash recovery and stale leases.
- Add concurrency and overlap policy.
- Add reconciliation for Temporal Schedule objects that drift from Forge
  trigger definitions.
- Add live ignored tests against local Temporal/Postgres.

### Phase 4: Delivery

- Add session announcement delivery.
- Add webhook delivery.
- Add failure notification policy.
- Record delivery attempts on firing records.

### Phase 5: External Sources

- Add webhook-trigger source.
- Add VFS-watch-trigger source using workspace revision/snapshot refs.
- Add connector event triggers as product needs become concrete.
- Unify all source types through the same durable firing path.

### Phase 6: Agent-Managed Triggers

- Add a tool/API affordance that lets agents propose or create triggers.
- Require explicit policy for whether the agent can create, update, pause, or
  delete triggers autonomously.
- Make trigger creation visible in the session event stream or notifications.

## Open Questions

- Which one-shot cases should use Temporal workflow `startDelay` versus a
  single-action Temporal Schedule?
- Should trigger deletion delete the Temporal Schedule immediately, pause it
  first, or keep it for a short audit/reconciliation window?
- Should `MainSession` trigger firings create visible run items, silent system
  events, or both?
- How aggressively should ephemeral trigger sessions be retained or pruned?
- Should stable trigger sessions have their own compaction policy separate from
  user sessions?
- How should trigger ownership interact with P70 fleet agents?
- Should trigger definitions be editable VFS files, API records, or both?
- What is the minimum safe permission model for agent-created recurring jobs?
- Should failed trigger firings retry at the scheduler layer, the workflow
  layer, or both?

## Acceptance Criteria

- Trigger definitions are durable and inspectable outside session transcripts.
- Timer firings do not rely on main conversation history to remember the task.
- Each firing records the trigger revision, input blob, route, session/run
  mapping, status, and error/output refs.
- Future trigger updates do not mutate already-running or historical firings.
- The deterministic engine remains free of timers, cron parsing, file watches,
  webhooks, and wall-clock scheduling.
- Existing `run/start` and `AgentSessionWorkflow` remain the execution path for
  scheduled work.
- Hosted timer sources use Temporal Schedules by default, with a documented
  scanner workflow fallback.
- Temporal Schedule ids map cleanly to Forge trigger ids and can be reconciled.
- External trigger sources can reuse the same firing pipeline after timers land.
