# P101: Durable Work — A Goal Loop Over Managed Sessions

**Status**
- Proposed 2026-07-21.
- Revised 2026-07-23 after narrowing Work from a one-run lifecycle wrapper to
  the durable objective-achievement loop.
- Renumbered from P100 when its generic workflow-tool port substrate was split
  into the new P100 proposal.
- Revised 2026-07-24 with the P100 revision: run-terminal notifications ride
  P100's generic emission envelope and fixed `deliver_emission` signal, and
  Work consumes `work_report` emissions by **pull** at run reconciliation —
  Work needs no push delivery pump and keeps no signal-buffering state.
- Greenfield: wire and internal workflow protocol changes may be breaking. Do
  not retain promise-specific compatibility aliases when extracting an already
  generic run-terminal transport.
- Builds on **P100 (Workflow Emissions And Tool Ports)**, **P92 (Unified
  Suspension)**, and the Fleet `agent_request`/run-promise machinery.

## Decision

Add a first-class `AgentWorkWorkflow` whose authoritative state lives in
Temporal.

A Work is a durable commitment to achieve an objective. It owns the policy
that keeps asking an agent session to make progress until the objective is
explicitly completed, blocked, failed, or cancelled.

The first implementation fixes the cardinality to:

```text
one Work
  -> one active attempt
  -> one managed Session
  -> many sequential Runs (execution cycles)
  -> one explicit Work outcome
```

The managed session is the executor. It owns model context, LLM/tool execution,
compaction, runs, and the transcript. A successful run means only that the
agent yielded at a safe execution boundary. It does **not** mean the Work is
complete.

The Work workflow owns the objective, semantic Work status, current execution
cycle, queued caller input, continuation policy, and final result. It starts
another run when the previous run ended without an explicit Work disposition.

The agent reports a disposition through one Work-owned workflow tool port:

```text
work_report(outcome = complete | blocked, ...)
```

P100 admits that port with the managed session and gives it the fixed
`AgentWorkWorkflow` controller. A valid call becomes a typed, log-backed
workflow-port emission in the managed session's log.

Work does not receive report deliveries mid-run at all. When the existing
run-terminal emission arrives, Work reconciles the exact completed run —
reading that run's durable port emissions through P100's pull operation —
and decides what to do next. The run-terminal boundary guarantees every
prior emission for that run is durable, so the pull is complete by
construction.

P101 therefore adds no Work-specific message bus, subscription system, signal,
tool transport, or caller-selected workflow address.

## Product Invariant

The distinction that makes Work valuable is:

```text
Run Completed  = the agent stopped this execution cycle
Work Completed = the agent explicitly reported that the objective was achieved
```

Without this distinction, Work is only a convenience wrapper around
`session/start` plus `session/runs/start` and does not warrant a new workflow.

With it, the runtime owns a useful policy that an API caller would otherwise
have to keep alive and implement correctly:

```text
run terminates
  -> inspect durable disposition and progress facts
  -> complete, block, fail, cancel, or continue
```

The caller may disconnect after `work/start`. Worker restarts, workflow replay,
session continue-as-new, and ordinary run completion do not abandon the
objective.

## Goal

Make this end-to-end interaction a supported product primitive:

```text
caller
  -> start Work(profile, objective)
  <- stable Work id immediately

AgentWorkWorkflow
  -> create/reopen one managed session
  -> start cycle 1 with objective + Work instructions
  <- existing run-terminal emission
  -> pull and reconcile the run's log-backed port reports

  if work_report(complete)
    -> publish completed Work result

  if work_report(blocked)
    -> park until caller input or cancellation

  if no disposition
    -> start the next continuation run in the same session

caller
  -> read, supply input, or cancel Work by id
  -> inspect detailed execution through the linked session
```

This is the first useful Work slice. Triggers, verification, budgets,
multi-attempt recovery, and playbooks may build on it later but are not needed
to validate the goal loop.

## Vocabulary

P101 does not introduce a separate public `Goal` resource.

- **Work** is the product object: objective, lifecycle, execution, and result.
- **Objective** is the caller-provided outcome the Work must achieve.
- **Goal loop** is the internal control policy that schedules execution cycles
  until Work reaches a semantic outcome.
- **Attempt** is one executor assignment. P101 has exactly one attempt.
- **Execution cycle** is one run in the managed session.
- **Disposition** is the agent's durable report that Work is complete or
  blocked.

If future product pressure requires editable goal stacks, subgoals, or goals
shared by multiple Work objects, Goal may become its own object then. P101
should not create both abstractions before that need exists.

## Why Work Is A Separate Workflow

The two workflows own different lifecycles:

| `AgentWorkWorkflow` owns | `AgentSessionWorkflow` owns |
|---|---|
| objective | model/provider context |
| semantic Work status | run queue and run status |
| whether another cycle is needed | execution of one cycle |
| current attempt/session reference | LLM and tool drive loop |
| queued caller input | mailbox and run admission |
| explicit disposition/result | assistant/tool output production |
| continuation/no-spin policy | context compaction and rehydration |
| later: executor replacement | promises, jobs, and suspended turns |

Not every session is Work-managed. Existing `session/*`, chat, bridge, and
Fleet behavior continue to use standalone sessions.

Putting the goal loop directly into `AgentSessionWorkflow` would make every
interactive session carry business-task semantics, conflate chat idleness with
objective completion, and make a later fresh-session attempt or executor
replacement difficult.

The separate Work workflow becomes justified because it remains active across
normal session run boundaries. It is not justified merely because it offers a
shorter API for starting one run.

## Communication Architecture: Reuse, Do Not Multiply

Lightspeed already has several communication surfaces with intentionally
different jobs:

| Existing mechanism | Responsibility |
|---|---|
| `message_send` and bridge tools | outbound delivery to WhatsApp, Telegram, and other external channels through the messaging outbox |
| `agent_send` | fire-and-forget content delivery into another session's mailbox |
| `agent_request` + Promise | ask another session to run and create an awaitable result |
| run-terminal emission (`RunTerminalNotifyIntent`) | log-backed completion edge, delivered on P100's emission spine, that wakes a holder workflow when one specific run terminates |
| P100 workflow-bound tool port | schema-validated semantic fact emitted by a session for one fixed admitted receiver workflow |

P101 must compose these primitives rather than create a Work-specific
transport.

### Work to Session

Work starts execution cycles through the same lower-level run-request path used
by `agent_request`:

```text
request_session_run(
  target_session,
  input,
  deterministic_submission_id,
  RunTerminalNotifyIntent {
    holder_workflow_id: work_workflow_id,
    token: cycle_token,
  },
) -> run_id
```

Factor this operation beneath the model-facing Fleet service so Fleet and Work
share retry, admission, profile/runtime, and notification behavior. Work should
not call the `agent_request` tool or manufacture a parent-session Promise; it
is a workflow consumer of the same lower-level substrate.

### Session to Work

The managed session uses the existing run-terminal notification edge, which
P100 slice 1 generalizes into the emission envelope:

```rust
EmissionBody::RunTerminal {
    token: String,
    run_id: RunId,
    status: RunStatus,
    output_ref: Option<BlobRef>,
    failure_message_ref: Option<BlobRef>,
}
```

Both sessions holding run-backed Promises and Work workflows receive the same
fixed `deliver_emission` signal:

- `AgentSessionWorkflow` maps the opaque token to its local `PromiseId` and
  admits the existing `ResolvePromise` command.
- `AgentWorkWorkflow` maps the opaque token to its current execution cycle.

This is the existing transport under its generic name, not another
transport. There is still one log-backed notify-intent, one fixed signal,
and no subscription table.

### Agent to Work

The agent does **not** choose a workflow, signal name, Work id, session id, or
cycle token. It calls the ordinary model-visible `work_report` function.

The managed session's P100 binding fixes:

```text
tool name:        work_report
semantic type:    lightspeed.work.report.v1
schema revision:  1
receiver:         this session's AgentWorkWorkflow
```

P100 validates the arguments and atomically records the tool result and the
`WorkflowPort::Emitted` fact in the session log. Nothing is pushed to Work
mid-run: Work is a boundary-subscribed receiver in P100's terms, and pull
consumption is complete by construction once the run-terminal emission
arrives.

At that boundary, Work reads the exact completed run through the
run-reconciliation activity (the Temporal workflow does not read CAS or the
session log directly) and reconciles:

- the run's `lightspeed.work.report.v1` port emissions, read through P100's
  `read_port_emissions` operation;
- the terminal assistant output ref;
- whether any non-control tool was invoked;
- any malformed or conflicting reports.

Because reports are consumed only by pull, Work keeps no invocation buffer
and no dedupe set for port signals; reconciliation is idempotent because the
activity result is recorded once per cycle and repeated reconciliation of an
already-reconciled cycle is a no-op. P101 adds only the Work payload and
handler; declaration, schema validation, log events, and the pull read
belong to P100. If Work ever needs mid-run reports (it should not), that is
P100's deferred push slice, not a Work-specific transport.

### Caller to Work

`work/input` is a product command to the Work workflow, not a general messaging
protocol. Work queues the input in its own state and delivers it to the managed
session through the existing run admission/mailbox vocabulary at the next safe
cycle boundary.

P101 does not add mid-run Work steering. Input arriving during an active cycle
is processed before an automatic continuation after that cycle terminates.

## Scope

P101 includes:

1. `AgentWorkWorkflow`, registered by the hosted worker.
2. One Work objective and one managed-session attempt.
3. Multiple sequential execution cycles in that session.
4. Named-profile execution with P100's `work_report` port declared by the
   managed-session setup path.
5. A Work-owned payload schema and pull-consuming handler for that generic
   port.
6. Reuse of `RunTerminalNotifyIntent` and P100's fixed `deliver_emission`
   signal for run-terminal delivery to a non-session receiver.
7. A shared internal run-request operation used by Fleet and Work.
8. Automatic continuation when a run terminates without a disposition.
9. Blocked Work that resumes when caller input arrives.
10. User input priority over automatic continuation.
11. A deterministic no-spin guard and hard cycle safety limit.
12. Work start, read, input, and cancel API methods.
13. CLI and TypeScript client support.
14. Unit, gateway, and live Temporal coverage for retry, replay, fast runs,
    duplicate signals, worker restart, continue-as-new, input races, and
    cancellation.

## Explicit Non-Goals

P101 does not add:

- another mailbox, message bus, subscription table, Work-specific tool
  transport, or Work-specific signal;
- generic workflow-tool port infrastructure beyond P100;
- schedules, webhooks, email, or other trigger adapters;
- reusable Responsibility or Playbook documents;
- a separate public Goal resource;
- multiple attempts, fresh-session retry, or agent reassignment;
- multiple managed sessions per Work;
- verification or acceptance contracts;
- spend/token budgets, deadlines, approval gates, or escalation policies;
- Work trees or child Work;
- a Work event stream separate from Temporal history;
- `work/list` or a permanent Work database;
- full session-event replication into Work workflow state;
- persistent progress subscriptions;
- arbitrary result/artifact schemas;
- historical replay, eval, or mission-control UI.

## Current Machinery To Reuse

Fleet already performs the core request-and-notify operation.

For `agent_spawn` and `agent_request`, it:

1. derives deterministic session/run/submission/promise identities;
2. starts or enqueues a run;
3. attaches a `RunTerminalNotifyIntent` naming the holder workflow and opaque
   token;
4. records a run-backed Promise in the holder session;
5. resolves that Promise when the target run signals terminal.

The existing intent is:

```rust
pub struct RunTerminalNotifyIntent {
    pub holder_workflow_id: String,
    pub token: String,
}
```

Important existing properties:

- the completion edge is admitted atomically with the run, so there is no
  subscribe-after-completion race;
- the intent is log-backed on the observed session and survives its
  continue-as-new;
- delivery is queued before the session workflow may continue-as-new;
- delivery may be repeated, and the opaque token makes the receiver
  idempotent;
- output and failure bodies are already CAS references.

P101 should extract the reusable run-request and terminal-notification
substrate from its Fleet/promise-specific naming where necessary. It should not
replace the behavior or run a second system beside it.

P100 supplies the other reusable substrate:

- the generic emission envelope and fixed `deliver_emission` signal carrying
  run-terminal notifications to session and non-session receivers alike;
- an optional trusted lifecycle-controller reference on the managed session;
- function-tool ports with a fixed admitted receiver per binding;
- typed `WorkflowPort::Emitted` session events with deterministic invocation
  identity;
- the boundary-complete `read_port_emissions` pull operation.

P101 consumes that substrate and must not fork or specialize it.

## Work's Workflow Tool Port

During managed-session setup, `AgentWorkWorkflow` declares one P100 port:

```text
tool name:         work_report
semantic type:     lightspeed.work.report.v1
schema revision:   1
receiver:          the creating AgentWorkWorkflow
mode:              notify
consumption:       pull at run reconciliation
```

The tool is present because of Work's controller-bound port declaration, not a
public Work feature flag and not an independently installable Work tool
bundle. A standalone session has no Work controller and therefore cannot
grant itself a meaningful `work_report` port.

This binding does not make Work the receiver for every port in the session.
The same managed session may independently bind messaging or later service
tools to their own workflow receivers.

Illustrative tool input:

```rust
pub struct WorkReportArgs {
    pub outcome: WorkDispositionKind,
    pub summary: Option<String>,
    pub requested_input: Option<String>,
}

pub enum WorkDispositionKind {
    Complete,
    Blocked,
}
```

Validation:

- `complete` may include a concise summary and must not request input;
- `blocked` should include a reason in `summary` and may describe the input
  needed;
- arguments and large text follow P100's CAS-backed invocation path;
- the runtime creates invocation metadata; the model cannot select a Work id,
  workflow id, session id, run id, or cycle token.

The P100 invocation envelope supplies session/run/turn/batch/call joins and a
deterministic invocation id. Work validates domain rules only:

- the invocation belongs to its managed session and a known cycle;
- semantic type, schema revision, and binding fingerprint match the admitted
  port;
- `complete` does not request input;
- duplicate invocation ids are ignored;
- more than one conflicting disposition in one run moves Work to
  `Blocked { reason: invalid_disposition }`;
- duplicate identical reports are idempotent.

A successful port tool result means the report was durably recorded, not that
Work has already accepted or acted on the disposition.

## Workflow Identity

Add a validated `WorkId` and one canonical workflow-id composer. Work and
session workflow ids must remain distinct even if a caller chooses the same
logical id.

Illustrative identities:

```text
Work workflow:      {universe}/work-{work_id}
managed session:    work-{work_id}-attempt-1
cycle submission:   work-{work_id}-cycle-{cycle_no}
terminal token:     attempt-1-cycle-{cycle_no}
```

The exact encoding must obey Temporal and `SessionId` limits and prevent
collisions. Use a stable digest suffix rather than independent truncation.

Retrying `work/start` with the same Work id and admitted profile/objective
returns the existing Work. Reusing an id with different arguments is a
conflict. Store a stable request fingerprint over normalized profile and input
in Work state.

Every cycle has deterministic submission and notification identities.
Retrying session/run setup therefore reopens the same session and reuses the
same run instead of creating another cycle.

## Temporal-Owned Work State

The exact module placement may follow existing `temporal-workflow`
conventions. The initial semantic shape should be approximately:

```rust
pub struct AgentWorkArgs {
    pub universe_id: Uuid,
    pub work_id: WorkId,
    pub profile_id: String,
    pub objective: Vec<ContextEntryInput>,
}

pub struct AgentWorkWorkflow {
    work_id: Option<WorkId>,
    initialized: bool,
    request_fingerprint: Option<String>,
    profile_id: Option<String>,
    objective: Vec<ContextEntryInput>,
    status: WorkStatus,
    attempt: Option<WorkAttempt>,
    pending_terminal: Option<RunTerminalNotification>,
    pending_input: Vec<ContextEntryInput>,
    result: Option<WorkResult>,
    cancel_requested: bool,
    last_error: Option<String>,
}

pub struct WorkAttempt {
    pub attempt_no: u32,
    pub session_id: SessionId,
    pub cycle_no: u32,
    pub active_cycle: Option<WorkCycle>,
    pub automatic_continuations: u32,
    pub last_automatic_cycle_made_progress: Option<bool>,
}

pub struct WorkCycle {
    pub cycle_no: u32,
    pub token: String,
    pub submission_id: SubmissionId,
    pub run_id: Option<RunId>,
    pub origin: WorkCycleOrigin,
}

pub enum WorkCycleOrigin {
    Initial,
    AutomaticContinuation,
    CallerInput,
}

pub struct WorkReportInvocation {
    pub invocation_id: WorkflowToolInvocationId,
    pub run_id: RunId,
    pub binding_fingerprint: String,
    pub report: WorkReportArgs,
}
```

`WorkReportInvocation` is a bounded, decoded activity result produced by run
reconciliation. `RunTerminalNotification` names the buffered
`EmissionBody::RunTerminal` payload. The workflow's signal handler stores
only that bounded terminal body; port reports never arrive by signal and the
workflow never performs CAS or session-log I/O.

The first status vocabulary is semantic rather than a mirror of run status:

```rust
pub enum WorkStatus {
    Starting,
    Working,
    Blocked,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}
```

`Blocked` includes explicit agent blockage, no-progress suppression, and the
hard cycle guard. Its view must include a machine-readable reason and optional
summary/requested-input refs.

`Completed` means the agent emitted a valid `complete` disposition during a
successfully terminal cycle. P101 does not claim independent verification.

## Work Main Loop

The workflow state machine is:

```text
initialize
  -> establish attempt/session identity
  -> create or reopen managed session from profile + P100 work_report port
  -> start initial cycle with objective
  -> Working

Working with active cycle
  -> wait for the run-terminal emission, caller input, or cancellation
  -> caller input is queued; it does not create a parallel run

run_terminal
  -> reconcile the exact completed run, pulling its port emissions

  cancelled/failed run
    -> map to terminal Work outcome in P101

  valid complete disposition
    -> Completed

  valid blocked disposition + queued caller input
    -> start caller-input cycle

  valid blocked disposition without queued input
    -> Blocked

  no disposition + queued caller input
    -> start caller-input cycle

  no disposition + continuation allowed
    -> start automatic continuation cycle

  no disposition + no-progress/limit guard
    -> Blocked

Blocked
  -> wait for caller input or cancellation
  -> caller input starts a new cycle in the same session

cancel_requested
  -> Cancelling
  -> cancel the active run through the existing cancellation path
  -> wait for its ordinary terminal notification
  -> Cancelled unless completion won the race
```

Only one cycle may be active at a time. The Work workflow writes the
`WorkCycle` record before scheduling the retryable start/enqueue operation so a
very fast run can safely notify Work before the operation returns.

Work never transitions from a report alone: reports are visible to Work only
through reconciliation at the terminal boundary, which reads the run's
durable port emissions by pull. A duplicate terminal emission for an
already-reconciled cycle is a no-op.

## Initial And Continuation Inputs

The initial cycle receives:

- the objective;
- the fact that this is managed Work;
- the semantic distinction between yielding and completing;
- instructions to use `work_report(complete)` only when the objective is
  actually achieved;
- instructions to use `work_report(blocked)` only when progress requires
  caller/external intervention;
- the hard rule that simply ending a response does not complete Work.

An automatic continuation should be concise and stable:

```text
Continue working toward the Work objective. Review the session state and
evidence, take the next useful actions, and do not merely restate a plan.
Use work_report only when the objective is complete or genuinely blocked.
```

Do not repeat the full objective text on every cycle if it already remains in
effective session context. Carry a stable objective reference or context key so
compaction preserves the Work contract.

Caller input cycles identify the input as new information for the same Work and
retain the same objective.

## No-Spin And Safety Guard

Automatic continuation needs a narrow safety boundary even before budgets are
introduced.

For P101:

1. The initial cycle may end without a disposition and receive an automatic
   continuation.
2. A caller-input cycle may likewise receive an automatic continuation.
3. If an **automatic continuation** ends without a disposition and invokes no
   non-control tool, do not schedule another automatic continuation. Move Work
   to `Blocked { reason: no_progress }`.
4. Apply a deployment-level hard maximum number of cycles. Reaching it moves
   Work to `Blocked { reason: cycle_limit }`; it does not falsely mark the
   objective failed or complete.
5. Caller input may resume either blocked state, subject to a new bounded cycle
   window.

The exact first progress predicate is deliberately observable and simple:

```text
made_progress = at least one non-work-control tool invocation
```

This may later include durable context/artifact changes. P101 must not add an
LLM judge merely to decide whether to continue.

## Run Reconciliation

On terminal notification, Work calls one internal read operation:

```rust
read_work_cycle_result(session_id, run_id) -> WorkCycleResult
```

It reads the existing session log/projection and returns only the bounded facts
the Work state machine needs:

```rust
pub struct WorkCycleResult {
    pub status: RunStatus,
    pub output_ref: Option<BlobRef>,
    pub failure_message_ref: Option<BlobRef>,
    pub reports: Vec<WorkReportInvocation>,
    pub non_control_tool_calls: u32,
    pub invalid_disposition: Option<String>,
}
```

The activity reads the run's port emissions through P100's
`read_port_emissions` operation and validates that each report comes from the
admitted `work_report` binding and belongs to the requested run. The
workflow derives at most one `WorkDisposition` from the returned reports,
deduplicated by `invocation_id`.

The activity performs I/O; the workflow records its returned facts in Temporal
history. The deterministic Work state machine branches only on that result.

Do not copy the full transcript, tool arguments, or session event stream into
Work workflow state.

## API Surface

Add four universe-scoped JSON-RPC methods.

### `work/start`

Accepts:

- optional caller-selected Work id;
- named profile id;
- objective using existing input item shapes.

It starts or idempotently reopens `AgentWorkWorkflow` and returns once accepted.
It does not wait for completion.

### `work/read`

Returns a compact `WorkView`:

```rust
pub struct WorkView {
    pub work_id: WorkId,
    pub status: WorkStatus,
    pub profile_id: String,
    pub session_id: Option<SessionId>,
    pub active_run_id: Option<RunId>,
    pub cycle_no: u32,
    pub blocked_reason: Option<WorkBlockedReason>,
    pub result: Option<WorkResultView>,
    pub last_error: Option<String>,
}
```

Detailed transcript, run, tool, and context progress remains available through
the linked `session/read` and `session/events/read` surfaces.

### `work/input`

Accepts caller input for active or blocked Work.

- Input during an active cycle is queued in Work.
- Input for blocked Work starts a caller-input cycle.
- Input for terminal Work is rejected.
- A stable caller submission id makes retries idempotent.

### `work/cancel`

Records cancellation intent and drives an active run through the existing
session cancellation path. Cancellation is not reported complete merely
because the signal was admitted.

No `work/list`, `work/events`, `work/update`, or `work/delete` method is added.

The CLI adds:

```text
lightspeed work start --profile <id> [--id <work-id>] <objective>
lightspeed work read <work-id>
lightspeed work input <work-id> <input>
lightspeed work cancel <work-id>
lightspeed work run --profile <id> [--id <work-id>] --wait <objective>
```

`work run --wait` is client-side convenience and does not change the
asynchronous server boundary.

After wire changes, regenerate `interop/contract/` and verify the TypeScript
client and Configurator MCP. Decide explicitly which Work mutations, if any,
the Configurator facade advertises.

## Point Reads, Visibility, And Storage

P101 treats Temporal as authoritative for Work lifecycle.

Advantages:

- signals, operation results, cancellation intent, and transitions are durable
  and replayed;
- Work can wait without a process or polling loop;
- no dual-write protocol or new persistence trait is required;
- another Temporal workflow can start/await Work directly;
- active Work survives worker restarts and continue-as-new.

Payload discipline:

- large objective/input/report bodies are CAS-backed;
- Work state stores references, not generated documents;
- run-terminal notifications carry references only;
- session event/context detail is never copied wholesale into Work history.

Point reads by known Work id use a workflow query. A thin rebuildable projection
becomes justified when implementing:

- `work/list` or arbitrary filtering;
- completed Work retention beyond Temporal history retention;
- fleet-wide analytics or mission control;
- efficient joins across Work, sessions, profiles, and artifacts.

P101 does not add that projection.

## Managed Session Lifecycle

The managed session is an independently addressable
`AgentSessionWorkflow`, not an in-process loop and not hidden inside Work
workflow state.

P101 prefers an independently started workflow over a Temporal child workflow
because it:

- reuses the existing profile-aware session start path;
- keeps session inspection on the current API;
- matches Fleet's top-level-session model;
- avoids coupling Work correctness to Temporal parent-close behavior;
- leaves a clean seam for later executor replacement.

The session starts with `close_on_terminal = false`; otherwise its first run
would close it before the goal loop could continue.

After Work reaches a terminal outcome and its result is recorded, Work requests
ordinary session close as cleanup. Session history remains inspectable. Cleanup
failure is recorded operationally but does not erase a valid Work result.

## Failure And Recovery Semantics

### Start operation partially succeeds

Work/session/cycle/submission identities are deterministic. Retry reopens the
same session and reuses the same admitted run.

### Run terminates before start operation returns

The active cycle and token already exist in Work state. The signal handler
buffers the notification. The operation result also returns a current run
snapshot for reconciliation.

### Duplicate or stale terminal notification

A duplicate notification for the current reconciled cycle is a no-op. A token
or run id not belonging to the current cycle is ignored and recorded for
diagnostics.

### Work disposition is committed before terminal delivery

This is the normal ordering in the session log. Work reads and interprets the
report only after the matching terminal boundary is known, and the boundary
guarantees the emission is readable then.

### Duplicate or stale port emission

Reconciliation deduplicates reports by invocation id. A report from another
session, unknown binding, terminal cycle, or unrelated run does not mutate
Work and is retained only as a bounded diagnostic.

### Tool activity retries

The tool call id and normal session tool-result admission remain idempotency
anchors. P100 emits one deterministic workflow-port invocation; repeated
reconciliation reads return the same emission and derive the same
disposition.

### Session continues as new

The notify-intent lives on the stored run record and is rebuilt. Port
emissions are ordinary session-log facts and need no carried delivery state;
only the run-terminal emission's transient flush queue gates session
continue-as-new, per P92 §6.

### Work continues as new

The workflow id remains stable. Objective, attempt, active cycle, pending
terminal notification, queued input, and result state are carried into the
next run.

### Caller input races terminal completion

Input is stored in Work first. At reconciliation:

- a valid complete disposition wins and terminal Work rejects the unused input;
- otherwise the queued input takes priority over automatic continuation;
- a blocked disposition plus queued input starts a caller-input cycle without
  parking.

### Cancellation races completion

A valid complete disposition in a successfully completed cycle may win if it
was durable before cancellation made the run terminal. Otherwise the actual
cancelled run drives Work to `Cancelled`.

### Conflicting dispositions

Conflicting complete/blocked reports in one cycle never pick an arbitrary
winner. Work moves to `Blocked { reason: invalid_disposition }` and exposes the
diagnostic.

### Work workflow is externally terminated

The managed session remains independently addressable. The existing reaper can
eventually be extended to identify Work-owned orphan sessions. P101 records the
relationship and deterministic identity needed for that repair but does not add
a new fleet-wide sweep.

## Crate And Module Shape

Expected changes:

```text
crates/api/
  Work DTOs, views, and method constants

crates/temporal-workflow/
  AgentWorkWorkflow
  Work state/query/input/cancel DTOs
  WorkReportV1 typed handler over the P100 envelope
  generic RunTerminalNotification
  work-cycle activity DTOs

crates/temporal-server/
  workflow registration
  Work gateway service
  shared session-run request runtime beneath Fleet and Work
  managed-session setup
  work-cycle reconciliation activity

crates/cli/
  work start/read/input/cancel/run commands

interop/contract/ and interop/ts-client/
  regenerated contract and client
```

P100 owns all engine/tools changes for workflow ports. Do not add Work
semantics to `CoreAgentState`; it records generic port invocations and run
state, while `AgentWorkWorkflow` interprets `WorkReportV1`.

## Implementation Plan

### Slice 1: Consume the shared run-completion substrate

P100 slice 1 owns the emission envelope, the fixed `deliver_emission`
signal, and the deletion of the promise-specific vocabulary. P101 consumes
it:

- [ ] Keep `RunTerminalNotifyIntent` log-backed on run admission.
- [ ] Register the fixed `deliver_emission` handler on `AgentWorkWorkflow`
      and map `RunTerminal` tokens to execution cycles.
- [ ] Factor the retry-safe lower-level session run-request operation from
      Fleet so Work can supply a non-session holder workflow id/token.
- [ ] Preserve all Fleet Promise, duplicate delivery, cancellation, and
      continue-as-new tests.

### Slice 2: Bind Work to the P100 port substrate

- [ ] Start the managed session with Work as its P100 controller.
- [ ] Declare the `work_report` function schema with semantic type
      `lightspeed.work.report.v1`.
- [ ] Decode and validate the typed payload through the Work-owned
      reconciliation activity over P100's `read_port_emissions`, not in the
      session core or deterministic workflow code.
- [ ] Test report-before-terminal ordering, duplicate terminal emissions,
      repeated reconciliation, malformed, stale, and conflicting reports.

### Slice 3: Work workflow and goal loop

- [ ] Add `WorkId`, deterministic workflow/session/cycle identity helpers,
      statuses, args, query, signals, and result DTOs.
- [ ] Register `AgentWorkWorkflow`.
- [ ] Create/reopen one profile-based managed session with
      `close_on_terminal = false` and the P100 `work_report` port.
- [ ] Implement initial, automatic-continuation, and caller-input cycles.
- [ ] Implement terminal buffering and exact-run reconciliation.
- [ ] Implement complete, blocked, failed, cancelled, no-progress, and
      cycle-limit transitions.
- [ ] Carry all small semantic state through Work continue-as-new.

### Slice 4: API, CLI, and client

- [ ] Add `work/start`, `work/read`, `work/input`, and `work/cancel`.
- [ ] Return linked session/run ids for detailed inspection.
- [ ] Add CLI commands and `work run --wait`.
- [ ] Regenerate JSON Schema, method manifest, OpenRPC, API reference, and
      TypeScript client.
- [ ] Review Configurator MCP exposure explicitly.

### Slice 5: Validation and dogfood

- [ ] Script a run that terminates without a disposition and prove Work starts
      a second run automatically.
- [ ] Complete on a later cycle and prove Work, not the earlier run, owns the
      semantic result.
- [ ] Block, submit caller input, and prove execution resumes in the same
      session with retained context.
- [ ] Exercise an automatic continuation with no non-control tool call and
      prove Work parks instead of spinning.
- [ ] Restart the worker during an active cycle.
- [ ] Force session and Work continue-as-new independently.
- [ ] Exercise a run that terminates before the request operation returns.
- [ ] Retry `work/start` and `work/input` with matching and conflicting
      idempotency arguments.
- [ ] Cancel queued, active, and blocked Work.
- [ ] Confirm Fleet promises, `agent_send`, external messaging bridges, and
      standalone sessions remain unchanged.
- [ ] Dogfood one real repository objective that requires at least two
      execution cycles.

Live Temporal tests must source `local/env.sh` and run serially with
`--test-threads=1`.

## Acceptance Criteria

P101 is complete when:

1. A caller can start Work with a named profile and objective and immediately
   receive a stable Work id.
2. Work durably owns one managed session and schedules more than one run when
   needed.
3. A successfully completed run without a disposition does not complete Work.
4. A valid `work_report(complete)` P100 port invocation observed at the
   matching terminal boundary completes Work.
5. A valid blocked report parks Work, and `work/input` resumes it in the same
   session with prior context.
6. User input arriving during a cycle is processed before an automatic
   continuation.
7. An unproductive automatic continuation and the hard cycle guard stop
   self-spinning without falsely claiming completion.
8. Work reuses the existing run-request, notify-intent, session admission,
   cancellation, and P100 workflow-port substrates; it adds no Work message
   bus, subscription table, tool transport, or direct report signal.
9. Retry, duplicate signal, worker restart, and continue-as-new do not create
   duplicate cycles or lose Work state.
10. Detailed execution remains inspectable through the linked session while
    Work exposes a compact semantic lifecycle.
11. No Work database, trigger plane, verification framework, approval system,
    or multi-attempt scheduler is required.
12. A real repository task requiring multiple execution cycles completes
    end-to-end through the CLI or TypeScript client.

## Follow-On Boundary

P101 leaves intentional seams:

- a new attempt may create a fresh session after infrastructure/model failure;
- verification may turn `complete` into `proposed_complete` before acceptance;
- a trigger may create Work without changing the goal loop;
- a Responsibility/Playbook may template Work profile, objective, and policy;
- a projection store may add listing, retention, and mission-control queries;
- Work may later supervise multiple sessions or delegate attempts;
- counterfactual replay may evaluate Work trajectories and continuation policy;
- stable repeated execution may be hardened into deterministic steps.

None of those belongs in the first implementation until the single-session,
multi-cycle goal loop is running and dogfooded.
