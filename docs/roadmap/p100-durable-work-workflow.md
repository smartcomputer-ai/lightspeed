# P100: Durable Work — Temporal-Owned State And Managed Session Attempts

**Status**
- Proposed 2026-07-21.
- Greenfield: wire and internal workflow protocol changes may be breaking. Keep
  the first implementation narrow rather than carrying compatibility aliases
  for promise-specific notification names.
- Builds on **P92 (Unified Suspension)** and the log-backed
  `RunTerminalNotifyIntent` transport it introduced.

## Decision

Add a first-class `AgentWorkWorkflow` whose authoritative state lives in
Temporal. A Work workflow owns one externally submitted piece of work and
supervises an independently addressable agent session/run as its execution
attempt.

The first implementation deliberately fixes the cardinality to:

```text
one Work -> one managed Session -> one initial Run
```

Work does not enter the CoreAgent reducer and does not extend
`AgentSessionWorkflow` with business-task state. The session remains the
agent execution/context primitive; Work is the durable outer lifecycle that
starts it, observes it, and returns its result.

Most Work state lives only in `AgentWorkWorkflow`. P100 does **not** require
an authoritative Work store. Point reads use a workflow query, and large
inputs/results remain blob-backed. A thin query projection may be added later
for listing, analytics, availability independent of a live worker, or history
beyond Temporal retention, but it is not part of the first cut.

Session-to-Work completion uses the existing run-terminal push shape. P100
generalizes the promise-named receiver DTO/signal into one narrow workflow
protocol shared by sessions and Work:

```text
"the run identified by this opaque correlation token is terminal"
```

It does not add a general workflow event bus or copy the session event stream
into Work history.

## Goal

Make this end-to-end interaction a supported product primitive:

```text
caller
  -> start durable Work with a named profile and input
  <- work id immediately

AgentWorkWorkflow
  -> create/reopen a managed session
  -> start/reuse one run with a terminal notify-intent
  -> wait without polling while the agent works
  <- terminal run notification with output/failure refs
  -> publish a terminal Work result

caller
  -> read or cancel Work by id
  -> inspect detailed progress through the linked session
```

The result should be useful immediately from the CLI, TypeScript client, and
another Temporal workflow without requiring callers to compose
`session/start`, `session/runs/start`, event following, and terminal-output
extraction themselves.

## Why Work Is A Separate Workflow

The two workflows own different lifecycles.

| `AgentWorkWorkflow` owns | `AgentSessionWorkflow` owns |
|---|---|
| submitted work input | model/provider context |
| outer Work status | run queue and run status |
| managed attempt identity | LLM and tool drive loop |
| session/run references | context compaction and rehydration |
| cancellation intent | safe run cancellation transitions |
| terminal result refs | assistant/tool output production |
| later: attempts and caller interaction | promises, jobs, and exact suspended turn |

`AgentSessionWorkflow` already owns CoreAgent state, admissions, LLM/tool
activities, promise transport, cancellation watchdogs, and continue-as-new.
Putting Work into that workflow would make every interactive chat session
carry a business-task lifecycle and would make later fresh-session attempts
or executor replacement difficult.

Not every session is Work-managed. Existing `session/*` APIs and chat clients
continue to create standalone sessions. `work/start` creates a Work workflow,
which in turn creates its managed session.

The domain relation is `Work -> Attempt -> Session/Run`. It must be recorded
explicitly in Work state rather than inferred only from Temporal parent/child
metadata. The managed session may be implemented as a Temporal child workflow
or as an independently started workflow; P100 prefers the latter because it
reuses the existing profile-aware session start path, keeps the session
directly addressable through the current API, and matches fleet behavior.

## Scope

P100 includes:

1. A Temporal `AgentWorkWorkflow` registered by the hosted worker.
2. One managed attempt containing one session and one initial run.
3. Named-profile execution; the existing profile-aware session setup path is
   reused rather than reimplemented in the workflow.
4. Retry-safe, deterministic Work/session/submission/notification identities.
5. Run-terminal push notification from the managed session to Work.
6. Work start, read, and cancel API methods.
7. A small Work view containing status, attempt identifiers, and terminal
   output/failure refs (with gateway-side text hydration where appropriate).
8. CLI start/read/cancel plus a start-and-wait convenience command.
9. Generated contract and TypeScript client support.
10. Unit, gateway, and live Temporal coverage, including retry, fast-run,
    worker restart, continue-as-new, duplicate signal, and cancellation cases.

## Explicit Non-Goals

P100 does not add:

- schedules, webhooks, email, or other trigger adapters;
- reusable Responsibility/Playbook documents;
- immutable agent deployment or release aliases;
- multiple attempts, fresh-session retry, or agent reassignment;
- follow-up caller input or a `waiting_for_input` state;
- approvals, verification, budgets, deadlines, or escalation;
- Work trees or child Work;
- a Work event stream separate from Temporal history;
- a Work list endpoint or permanent Work database;
- full session-event replication into the Work workflow;
- generalized workflow-to-workflow publish/subscribe;
- result/artifact schemas beyond the existing run output and blob refs;
- historical replay, eval, or mission-control UI.

These may be built on Work later, but none is needed to validate durable work
submission and completion.

## Current Machinery To Reuse

P92 already records an observed-side terminal notification edge on the run:

```rust
pub struct RunTerminalNotifyIntent {
    pub holder_workflow_id: String,
    pub token: String,
}
```

`RunRequestCommand.notify_on_terminal` is persisted with the run record. When
a terminal run event is appended, `AgentSessionWorkflow` rebuilds the run,
queues one notification per intent, and signals each holder workflow. The
payload already contains everything Work needs:

```rust
pub struct PromiseResolutionSignal {
    pub token: String,
    pub status: RunStatus,
    pub output_ref: Option<BlobRef>,
    pub failure_message_ref: Option<BlobRef>,
}
```

Important existing properties:

- the notify-intent is admitted atomically with the run, so there is no
  subscribe-after-completion race;
- the intent is log-backed on the observed session and survives its
  continue-as-new;
- delivery is queued before the session workflow can continue-as-new;
- delivery may be repeated, and the receiver token makes it idempotent;
- the output and failure are already CAS references rather than large signal
  payloads.

The fleet runtime currently creates these intents so a child run can resolve a
promise in its parent session. P100 reuses the transport, not the parent
session's promise registry.

## Workflow Identity

Add a validated `WorkId` and one canonical workflow-id composer. Work and
session workflow ids must be distinct even when a caller chooses the same
logical id.

Illustrative shapes:

```text
Work workflow:    {universe}/work-{work_id}
managed session:  work-{work_id}-attempt-1
run submission:   work-initial
notify token:     attempt-1
```

The exact encoding must obey Temporal and `SessionId` length/character limits
and must not admit collisions. If raw Work ids cannot fit safely in derived
session ids, use a stable digest suffix through a shared helper rather than
truncating independently at call sites.

All derived ids are deterministic. Retrying `work/start` with the same Work id
and the same admitted input/profile returns the existing Work. Reusing an id
with different arguments is a conflict. Admission computes a stable request
fingerprint over the normalized profile id and input and stores it in Work
state so the gateway can distinguish an idempotent retry from conflicting id
reuse. An auto-generated Work id is retry-safe only after the caller has
received and retained it; callers that need retry safety across a lost start
response supply their own stable Work id.

The run's `submission_id` is deterministic per attempt, so an activity retry
after session creation or run admission reopens the session and returns the
same run rather than creating another one.

## Workflow Types

The exact crate placement may follow existing `temporal-workflow` conventions,
but the initial semantic types should be approximately:

```rust
pub struct AgentWorkArgs {
    pub universe_id: Uuid,
    pub work_id: WorkId,
    pub profile_id: String,
    pub input: Vec<ContextEntryInput>,
}

pub struct AgentWorkWorkflow {
    work_id: Option<WorkId>,
    initialized: bool,
    request_fingerprint: Option<String>,
    status: WorkStatus,
    profile_id: Option<String>,
    attempt: Option<WorkAttempt>,
    pending_terminal: Option<RunTerminalNotification>,
    result: Option<WorkResult>,
    cancel_requested: bool,
    last_error: Option<String>,
}

pub struct WorkAttempt {
    pub attempt_no: u32,
    pub token: String,
    pub session_id: SessionId,
    pub run_id: Option<RunId>,
}

pub struct WorkResult {
    pub status: WorkTerminalStatus,
    pub output_ref: Option<BlobRef>,
    pub failure_message_ref: Option<BlobRef>,
}

pub enum WorkTerminalStatus {
    Completed,
    Failed,
    Cancelled,
}
```

The first Work status vocabulary is intentionally execution-oriented:

```rust
pub enum WorkStatus {
    Starting,
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}
```

`Completed` means the managed run completed successfully according to the
existing run lifecycle. It does not mean a future business acceptance
contract verified the result. Do not introduce `Succeeded` as a stronger
claim in P100.

The workflow exposes:

```text
run(args) -> WorkResult
signal cancel(request)
signal run_terminal(notification)
query status() -> AgentWorkStatus
```

The signal handlers only validate/correlate and enqueue state. Activities and
multi-step transitions stay in the async workflow loop.

## Starting The Managed Attempt

Add one internal activity/runtime operation rather than teaching the Work
workflow how to apply profiles, provision profile environments, or append
session commands itself:

```rust
start_work_attempt(StartWorkAttemptRequest) -> StartWorkAttemptResult
```

The request contains:

- universe and Work ids;
- deterministic attempt number, session id, and submission id;
- named profile id;
- input/context entries;
- receiver workflow id and opaque attempt token.

The implementation reuses the hosted gateway/runtime path to:

1. create or reopen the managed session with the named profile;
2. set `close_on_terminal = true` for this one-shot managed session;
3. start or reuse the initial run;
4. attach a `RunTerminalNotifyIntent` targeting the Work workflow;
5. return the session id, run id, and current projected run status.

The public `session/runs/start` DTO does not expose arbitrary notification
targets. P100 keeps receiver workflow ids on an internal trusted boundary.

The Work workflow establishes `WorkAttempt { token, session_id, ... }` before
scheduling the start activity. A very fast run may signal terminal completion
before the activity result is observed; the synchronous signal handler stores
the matching notification in `pending_terminal`, and the main loop consumes it
after the activity completes.

If the start activity reports that the idempotently reused run is already
terminal, the Work workflow applies the same terminal transition directly.
The push notification remains the normal path; the returned snapshot closes
retry/recovery edge cases without adding polling.

## Generic Run-Terminal Notification Protocol

The current sender is typed and named as if every receiver were an
`AgentSessionWorkflow` promise holder:

```rust
.signal(AgentSessionWorkflow::resolve_promise, signal)
```

P100 generalizes this one layer. Rename or replace the promise-specific
transport DTOs with:

```rust
pub struct RunTerminalNotification {
    pub token: String,
    pub status: RunStatus,
    pub output_ref: Option<BlobRef>,
    pub failure_message_ref: Option<BlobRef>,
}
```

Both `AgentSessionWorkflow` and `AgentWorkWorkflow` expose the same Temporal
signal name, `run_terminal`, with this DTO.

- The session receiver interprets `token` as the local `PromiseId` and queues
  the existing `ResolvePromise` admission.
- The Work receiver interprets `token` as its current attempt token and queues
  the outer Work transition.

The observed session does not know the receiver workflow type. Its only
responsibility is to send the opaque terminal notification to the workflow id
recorded on the run.

The Work receiver applies these rules:

1. A token not belonging to its current attempt is ignored as stale or
   unrelated.
2. A duplicate notification for an already terminal Work is a no-op.
3. A duplicate matching pending notification is a no-op.
4. `RunStatus::Completed` maps to `WorkStatus::Completed`.
5. `RunStatus::Failed` maps to `WorkStatus::Failed`.
6. `RunStatus::Cancelled` maps to `WorkStatus::Cancelled`.
7. Non-terminal run states in this signal are invalid and recorded as a
   workflow error rather than silently accepted.

The existing outbound queue continues to gate session continue-as-new until
delivery is attempted. Work continue-as-new keeps the same workflow id, so
run notifications remain correctly addressed across Work runs.

Do not add standalone subscribe/unsubscribe signals or a subscription table.
The log-backed notify-intent on the run remains the subscription edge.

## Work Main Loop

The first workflow can be a small state machine:

```text
initialize
  -> record deterministic attempt
  -> start_work_attempt activity
  -> Running

Running
  -> wait_condition(cancel_requested || pending_terminal)

pending_terminal
  -> map terminal run state and refs
  -> return WorkResult

cancel_requested
  -> Cancelling
  -> cancel_work_attempt activity
  -> wait for the ordinary terminal run notification
  -> Cancelled / Failed / Completed according to the actual terminal run
```

Cancellation uses the existing session run-cancellation path and watchdog;
Work does not invent a second run cancellation state machine. A completion
that wins the race with cancellation is reported as completed. Cancellation
is an intent until the managed run reaches a terminal state.

The Work workflow should not close or delete the session before consuming its
terminal result. `close_on_terminal` lets the managed session quiesce and
finish after its first run while preserving its stored session log for
inspection.

## Point Reads And Detailed Progress

`AgentWorkWorkflow::status` returns a compact snapshot:

```rust
pub struct AgentWorkStatus {
    pub work_id: WorkId,
    pub status: WorkStatus,
    pub profile_id: String,
    pub session_id: Option<SessionId>,
    pub run_id: Option<RunId>,
    pub output_ref: Option<BlobRef>,
    pub failure_message_ref: Option<BlobRef>,
    pub last_error: Option<String>,
}
```

Detailed progress remains owned by the linked session:

```text
work/read -> coarse lifecycle + session_id/run_id/result
session/read and session/events/read -> context, tool, and run detail
```

Do not signal every session event into Work or reproduce the session
projection there. The Work workflow should remain small and mostly dormant
while the agent executes.

The gateway may hydrate `output_ref` and `failure_message_ref` into optional
text fields in the public view using the existing CAS read path. The workflow
state and signals carry refs only.

## API Surface

Add three universe-scoped JSON-RPC methods:

### `work/start`

Accepts:

- optional caller-selected Work id;
- named profile id;
- one or more existing input item shapes.

Starts or idempotently reopens `AgentWorkWorkflow` and returns once accepted;
it does not wait for completion.

### `work/read`

Queries Work by id and returns `WorkView`, including linked session/run ids
and terminal result when available.

### `work/cancel`

Signals cancellation and returns the current Work view. The caller observes
the terminal result through subsequent reads; cancellation is not claimed
complete merely because the signal was admitted.

No `work/list`, `work/events`, `work/update`, or `work/delete` method is added
in P100.

The CLI adds:

```text
lightspeed work start --profile <id> [--id <work-id>] <input>
lightspeed work read <work-id>
lightspeed work cancel <work-id>
lightspeed work run --profile <id> [--id <work-id>] --wait <input>
```

`work run --wait` is client-side convenience: it starts Work, follows its
coarse status, and prints the hydrated terminal output. It does not change the
asynchronous server boundary.

After changing the wire contract, regenerate `interop/contract/` and verify
the TypeScript client and Configurator MCP. Decide explicitly whether the
Configurator advertises mutating Work methods; do not inherit exposure only
because they appeared in the generated universe manifest.

## Temporal State, Visibility, And Storage

P100 treats the Temporal workflow as authoritative for Work lifecycle.

Advantages in the first cut:

- signals, activity results, cancellation intent, and state transitions are
  already durable and replayed;
- Work can wait without a process or poll loop;
- no dual-write protocol or new persistence trait is required;
- another Temporal workflow can start/await Work directly;
- active Work survives worker restarts and continue-as-new.

Payload discipline remains strict:

- large input bodies are CAS-backed before or during admission;
- workflow state stores `BlobRef`, not generated documents;
- terminal notification carries refs only;
- session event/context detail is never copied into Work history.

The first workflow should still implement a continue-as-new threshold even
though its history is expected to be small. `AgentWorkArgs` for the next run
must carry the complete small semantic state needed to resume, including the
current attempt and any terminal notification not yet consumed.

### Why no Work store yet

P100 supports point operations by a known Work id. Workflow query plus the
linked session log is sufficient for that use case during Temporal retention.

A thin Work projection becomes justified when one of these is implemented:

- `work/list` or arbitrary filtering;
- completed Work retention beyond Temporal history retention;
- reads while the Temporal worker/query path is unavailable;
- fleet-wide analytics, aggregates, or mission control;
- efficient joins across Work, sessions, profiles, and artifacts.

If added, the first projection should remain a rebuildable index, not a second
source of lifecycle truth:

```text
universe_id, work_id, workflow_id, status,
profile_id, session_id, run_id,
created_at_ms, updated_at_ms,
output_ref, failure_message_ref
```

Temporal Search Attributes may cover early operational discovery, but P100
does not make correctness depend on Visibility indexing or its consistency and
retention characteristics.

## Failure And Recovery Semantics

### Start activity partially succeeds

Session id, run submission id, and notification token are deterministic.
Activity retry reopens the same session and reuses the same run. It must not
create a second attempt.

### Run completes before start activity returns

The Work attempt/token already exists. The signal handler buffers the terminal
notification. The activity result also carries the current run state as a
reconciliation snapshot.

### Duplicate terminal signal

Receiver correlation and terminal-state checks make it a no-op.

### Stale terminal signal

Ignore a token that is not the current attempt. This matters when multiple
attempts are added later and costs nothing now.

### Session continues as new

The notify-intent lives on the stored run record and is rebuilt. Pending
outbound delivery gates the session's continue-as-new.

### Work continues as new

The workflow id remains stable. Current attempt/token and any pending
notification are carried into the next run.

### Cancellation races completion

The actual run terminal status wins. Work cancellation is not itself a
terminal result.

### Work is terminated externally

The managed session is independently addressable and is not implicitly
destroyed by a Temporal parent-close policy. Operator termination is an
exceptional path; a later orphan reconciler may cancel managed sessions whose
Work workflow no longer exists. P100 does not add that fleet-wide sweep, but
the session id derivation and Work relationship must make it possible.

## Crate And Module Shape

Expected changes:

```text
crates/api/
  work DTOs, views, method constants, service trait methods

crates/temporal-workflow/
  AgentWorkWorkflow
  AgentWorkArgs / AgentWorkStatus / WorkResult
  generic RunTerminalNotification protocol
  work-attempt activity DTOs

crates/temporal-server/
  workflow registration
  work gateway service
  start/cancel attempt activities or runtime adapter
  profile-aware managed-session setup reuse

crates/cli/
  work start/read/cancel/run commands

interop/contract/ and interop/ts-client/
  regenerated contract and client
```

Do not add Work semantics to `engine::CoreAgentState`. The only engine-level
change expected is the naming/generalization of the existing terminal notify
intent/DTO if that protocol remains housed beside run types.

## Implementation Plan

### Slice 1: Generic terminal notification

- [ ] Replace promise-specific terminal transport names with
      `RunTerminalNotification` and the shared `run_terminal` signal.
- [ ] Keep the notify-intent log-backed on the run.
- [ ] Adapt `AgentSessionWorkflow` to map the generic notification token into
      its existing promise resolution admission.
- [ ] Preserve duplicate delivery, continue-as-new, and fleet promise tests.
- [ ] Add a protocol test proving a non-session workflow can receive the same
      notification shape.

### Slice 2: Work workflow and attempt runtime

- [ ] Add `WorkId`, workflow-id/session-id derivation, statuses, args, query,
      signals, and terminal result DTOs.
- [ ] Register `AgentWorkWorkflow` with the hosted worker.
- [ ] Add retry-safe `start_work_attempt` and `cancel_work_attempt` runtime
      operations reusing named-profile session setup.
- [ ] Start one managed session with `close_on_terminal = true` and one run
      carrying the Work notify-intent.
- [ ] Implement fast-completion buffering, duplicate/stale signal handling,
      cancellation, and continue-as-new state carry.

### Slice 3: API and projection

- [ ] Add `work/start`, `work/read`, and `work/cancel` to `api` and the hosted
      gateway.
- [ ] Hydrate terminal output/failure text at the gateway without placing it
      in workflow state.
- [ ] Return linked session/run ids for detailed inspection.
- [ ] Regenerate JSON Schema, method manifest, OpenRPC, API reference, and the
      TypeScript client.
- [ ] Review the Configurator MCP method filter for the new mutating methods.

### Slice 4: CLI and Temporal-native use

- [ ] Add `lightspeed work start/read/cancel/run --wait`.
- [ ] Add a small TypeScript helper that starts Work and waits through reads
      without hiding the Work id.
- [ ] Add an example Temporal workflow/activity that submits Work and consumes
      its terminal result.
- [ ] Add one dogfood profile and documented real task using a mounted
      Lightspeed checkout.

### Slice 5: Live validation

- [ ] Run the live Temporal suite serially with `--test-threads=1`.
- [ ] Start Work, terminate/restart the worker during the managed run, and
      verify eventual terminal result.
- [ ] Force session continue-as-new before run completion and verify delivery.
- [ ] Force Work continue-as-new while waiting and verify delivery.
- [ ] Exercise a run that finishes before the start activity returns.
- [ ] Retry `work/start` with the same and conflicting arguments.
- [ ] Cancel queued, active, and parked managed runs.
- [ ] Confirm standalone sessions and fleet promise resolution remain
      unchanged.

## Acceptance Criteria

P100 is complete when:

1. A caller can start Work with a named profile and input in one API call and
   immediately receive a stable Work id.
2. The Work workflow durably starts exactly one managed session/run under
   arbitrary activity retries.
3. The session pushes its terminal status and output/failure refs to Work
   without polling and without a subscribe-after-start race.
4. Duplicate and stale notifications do not alter the final result.
5. `work/read` exposes coarse state and links to the detailed session view.
6. `work/cancel` drives the managed run through the existing cancellation
   path and reports the actual terminal outcome.
7. Worker restart and continue-as-new on either workflow do not lose Work or
   its completion notification.
8. Large output remains CAS-backed and is not copied into Temporal state.
9. No Work database, Work list, trigger plane, approval system, or acceptance
   framework is required for the implementation to be useful.
10. The CLI and TypeScript helper can run real repository work end to end.

## Follow-On Boundary

P100 intentionally leaves a clean expansion seam:

- another run in the same session can later implement caller clarification;
- another managed session can later implement a fresh attempt;
- a trigger can later create Work without changing Work execution;
- a Responsibility can later template `work/start` inputs;
- verification can later evaluate `WorkResult` before the outer lifecycle is
  considered accepted;
- a projection store can later add listing and permanent summaries;
- counterfactual eval can later operate over the explicit
  Work/attempt/session relationship.

None of those future concepts belongs in the P100 state machine until the
one-attempt Work path is running and dogfooded.
