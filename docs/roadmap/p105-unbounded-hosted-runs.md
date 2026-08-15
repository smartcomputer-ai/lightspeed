# P105: Unbounded Hosted Runs And Active-Run History Rollover

**Status**
- Proposed 2026-07-29 after the `ls-dev` PR-creation incident described below.
- Supersedes P59's idle-only continue-as-new boundary and closes P59 G4's
  deferred step-limit failure case.
- Completed 2026-07-30 for the production correctness cut.
- Hosted unbounded drive, safe active-run rollover, legacy cutover, structured
  process logging, and stale-projection detection are implemented.
- Admission-failure continuation sizing, an operational runbook, custom metric
  exporters/alert rules, long soak validation, and broader live fault injection
  are explicitly deferred and are not implemented by P105.

## Decision

Hosted agent runs have no implicit orchestration-step limit.

An agent may work for hours and take thousands of model, tool, compaction, and
event-append transitions. Those transitions are progress, not evidence of a
runaway workflow. Product limits such as a caller-selected deadline, spend
budget, turn limit, tool-round limit, or cancellation remain separate and
explicit. The hosted runtime must not invent a hidden lifetime limit.

Temporal workflow history is bounded independently. The session workflow
continues as new only when Temporal recommends it or the configured workflow
history threshold is reached. A long active Lightspeed run may therefore span
multiple Temporal workflow executions while retaining one session id, one run
id, and one durable session transcript.

In particular:

- do **not** continue as new automatically every 128 agent-drive steps;
- do **not** fix the incident by replacing 128 with a larger arbitrary number;
- do **not** let a drive checkpoint or safety policy fail the session workflow;
- do **not** wait for an hours-long run to become idle before managing Temporal
  history;
- do continue as new at a safe durable boundary when workflow history actually
  needs rollover.

The Lightspeed session log in PostgreSQL remains the authoritative recovery
source. Temporal history is execution history and may be rolled over.

## Implemented Cut And Continuation-State Audit

This cut uses a drain-before-rollover design instead of copying transport
queues into continuation input:

- `CoreAgentState`, the session head, and run-submission identities reload from
  the PostgreSQL session log/bootstrap activity.
- pending admissions, tool-batch resumes, emissions, source resolutions, and
  Promise cancellations make the active drive yield to the outer workflow
  loop; rollover waits until those queues have been admitted or flushed;
- parked awaits and Promise-source polls reconstruct from durable run/promise
  state and current workflow time;
- a rehydrated active or queued run is itself workflow work, so the new
  execution resumes the CoreAgent drive without waiting for another signal;
- confirmed workflow starts and issued execution cancellations may be
  rechecked after rollover because their execution identities are stable and
  the activities treat already-started/already-terminal targets idempotently;
- workflow-start retry backoff and cancellation-watchdog clocks temporarily
  gate rollover so their attempt/deadline semantics are not reset;
- admission failures are query/correlation facts not present in the session
  log, so they are carried in a dedicated version-1 continuation object.

No transport queue is carried, so a new queue-payload size policy is not part
of this cut. A workflow execution must reach at least one post-bootstrap safe
checkpoint (a durable append or a completed Promise-source poll whose next
schedule is installed) before it can roll over. This prevents a deliberately
low test threshold, or a server suggestion immediately after bootstrap, from
creating a zero-progress continue-as-new loop while still bounding the history
of a long-lived pending Promise poll.

The safe active-run check occurs before starting a new activity and after an
append activity has returned, its entries have reduced into both the drive and
workflow state, and its head has been installed. It is never evaluated between
an LLM/tool/compaction result and the append that commits that result.

The new active-run branch is guarded by Temporal patch marker
`p105_active_run_rollover_v1`. Replaying an execution whose recorded history
predates P105 therefore retains the old idle-only branch, while newly started
executions record the marker and use active rollover. The unbounded drive is
safe for old executions immediately: it is behaviorally identical through the
old recorded steps and only differs when new progress would previously have
hit the ceiling. An old active execution will adopt active rollover after it
first reaches the existing idle continue-as-new boundary. Operators should
restart/reconcile any pre-P105 execution already unusually close to Temporal's
hard history limit rather than wait for that boundary.

## Incident: `ls-dev` PR Creation

This issue was first observed in the `ls-dev` coding environment on
2026-07-29.

```text
session:     session_2f272f873d87494cb0610305c12cfd8f
environment: local
run:         run_5
repository:  former private deployment repository
result:      pull request 10
```

The agent successfully completed the requested external side effect: it pushed
the branch and created pull request 10. It then requested a final verification
batch containing:

```text
gh pr view 10 --json number,title,url,state,headRefName,baseRefName,mergeable
git status --short --branch
```

The durable session log contains `toolBatchStarted` and `toolCallStarted` for
both calls, but no corresponding completion events. Reading the session then
failed with:

```text
Agent drive step limit reached: max_steps=128
```

The operating-system processes were no longer running when inspected, the
repository was clean and synchronized, and the PR already existed. Waiting
could not make progress. The workflow had failed after partial durable run
progress, leaving projections that could still look active.

The run had reached 26 model turns and 22 tool batches. The limit did not mean
128 shell commands, 128 turns, 128 tokens, or 128 seconds. `CoreAgentDrive`
counts internal planning actions, event appends, generations, compactions, and
tool invocations; one ordinary model/tool round consumes several steps.

## Current Failure Mechanism

The current hosted path combines unrelated concerns:

1. `GatewayAgentApiBuilder` defaults `max_steps_per_input` to `128`.
2. That value is copied into `AgentSessionArgs` when the Temporal workflow is
   started.
3. `drive_until_idle` resets the counter and drives one admission until the
   CoreAgent becomes idle or closed.
4. `CoreAgentDrive::next_action` emits `StepLimitReached` when the counter is
   exhausted.
5. The session workflow converts that outcome into an `anyhow` failure.
6. Temporal fails the whole session workflow even though many prior run events
   and external effects may already be durable.

P59 explicitly deferred correct resume semantics for this partial-progress
case. Long coding runs now make the deferred case normal rather than
exceptional.

## Required Runtime Semantics

### 1. Hosted drive is unbounded

Remove `max_steps_per_input` from the hosted gateway and
`AgentSessionWorkflow` input. The Temporal runtime calls an unbounded CoreAgent
drive operation.

The deterministic engine may retain an explicit step limit for tests, evals,
and intentionally bounded in-process runners. That facility must be opt-in and
must not leak into hosted session defaults.

`max_turns` and `max_tool_rounds`, where explicitly configured as run policy,
are separate reducer-visible product limits. P105 does not silently change
their semantics or use them as infrastructure history controls.

### 2. Continue-as-new is history-driven

Continue-as-new is requested only when either condition is true:

```text
ctx.continue_as_new_suggested()
OR
ctx.history_length() >= continue_as_new_history_threshold
```

The existing default history threshold is 10,000 events. P105 should retain it
unless live testing shows a different history-size target is warranted.

Agent-drive step count is not a continue-as-new trigger. A run below the
history threshold does not roll over merely because it passed 128, 1,000, or
10,000 logical drive steps.

### 3. Active runs may cross a rollover

The P59 first cut checks continue-as-new only after the workflow becomes idle.
That is insufficient for an individual run lasting hours.

The session workflow must also check the history policy at safe durable
boundaries inside `drive_until_idle`. A safe boundary has all of these
properties:

- no LLM, tool, compaction, storage, or preprocessing activity is in flight;
- the most recent activity result has been converted into CoreAgent events;
- those events have been successfully appended to the PostgreSQL session log;
- the workflow's reduced state and head reflect that committed append;
- every workflow-local fact not reconstructible from the session log is either
  carried to the next execution or deliberately drained before rollover.

Do not continue as new between an activity completing and its result being
appended. That would lose the result and could repeat an external side effect.

After rollover, initialization reloads CoreAgent state and the session head
from PostgreSQL. Planning resumes the same active run from its next durable
transition.

### 4. Preserve transient workflow state

The current idle-only implementation gates continue-as-new while transient
transport queues are non-empty. An active run may receive signals while an LLM
or tool activity is executing, so merely retaining that gate could block
rollover for the entire long run.

Before implementing active-run rollover, audit every field on
`AgentSessionWorkflow` and classify it as one of:

1. reconstructed from the PostgreSQL session log;
2. reconstructed deterministically from current workflow time and log state;
3. transient transport state that must be carried in continuation input;
4. state that must be flushed or admitted before rollover.

At minimum the audit must cover:

- pending admissions;
- pending tool-batch resumes;
- pending emissions;
- pending source resolutions;
- pending Promise cancellations;
- workflow-start confirmation and retry state;
- cancellation watchdog state;
- Promise-source polling state;
- submission correlation and admission-failure state.

The continuation payload must not carry `CoreAgentState` or the transcript;
those belong to PostgreSQL. If transient queues are carried, use a dedicated,
versioned continuation-state object rather than turning immutable creation
arguments into an unstructured snapshot.

The implementation must define a payload-size policy. It may drain or durably
admit oversized queues before rollover, but it must never silently drop them.

### 5. Preserve Temporal termination typing

The Rust SDK represents continue-as-new as a special
`WorkflowTermination::ContinueAsNew`, not an ordinary failure. A request from a
nested drive helper must reach the workflow entry point without being wrapped
in `anyhow`, recorded as `last_error`, or converted into workflow failure.

Use a typed successful drive outcome such as:

```text
DriveOutcome::Idle
DriveOutcome::ContinueAsNew
```

The top-level workflow entry point issues `ctx.continue_as_new(...)` and
propagates the SDK termination directly.

### 6. Existing product cancellation remains responsive

Unlimited does not mean uncontrollable.

- An operator or caller can cancel a run or force-close a session.
- Provider and tool failures remain ordinary durable run/tool facts.
- Explicit run budgets and deadlines may terminate a run when configured.
- Worker restart and Temporal replay continue from durable state.

P105 must not introduce a hidden wall-clock timeout as a replacement for the
step limit.

## In-Flight Activity Rule

Temporal history does not grow continuously while one activity runs for a long
time. Therefore no rollover is needed while an LLM or tool activity itself is
in flight. Wait for it to finish, durably append its result, and then evaluate
the history policy.

This is also the external-side-effect safety rule. Continue-as-new must never
be used to abandon an activity whose completion is ambiguous.

Tool calls should continue to use stable batch, call, and invocation identities
across replay. P105 does not claim that every external tool is idempotent; it
prevents history rollover from creating a new ambiguity window.

## Recovery And Cutover

Deploying P105 prevents new step-limit failures but does not automatically
repair workflows that already failed.

For the incident session:

- treat pull request 10 as successfully created;
- inspect the durable log before retrying the incomplete verification batch;
- force-close/reconcile the failed session or continue work in a new session;
- do not blindly replay an incomplete side-effecting tool batch unless its
  invocation is idempotent or the external result has been checked.

Existing open workflow executions were started with
`max_steps_per_input = 128`. The implementation plan must explicitly choose one
cutover strategy:

1. terminate/reconcile and restart existing sessions from their PostgreSQL
   logs with new workflow input; or
2. temporarily decode the legacy field but ignore it in the hosted drive,
   allowing the next history-driven rollover to emit the new input shape.

The second option is operationally smoother, but any compatibility code must
be narrowly scoped and removed after the deployment's existing sessions have
rolled over. New sessions must never receive the field.

## Observability

P105 emits structured process log records through `tracing`; it does **not**
append diagnostic events to the durable session log. Workflow-side records are
suppressed during Temporal replay and include the Temporal run id so downstream
log processing can deduplicate the remaining at-least-once edge cases.

Implemented records:

- `session_continue_as_new` at successful rollover, including reason
  (`server_suggested` or `history_threshold`), history length and threshold,
  universe/session/workflow ids, Temporal run id, active Lightspeed run id,
  session head, and admission-failure count;
- `session_rollover_delayed` once per workflow execution when history rollover
  is due but the first safe checkpoint or transient workflow state still gates
  continuation, including blocker counts;
- `session_workflow_failed`, including a stable error class and the same
  workflow/session/run context;
- `stale_active_projection` from the deployment reaper when PostgreSQL still
  projects a nonterminal run but the Temporal workflow is terminal or missing;
- `session_workflow_status_unavailable` when Temporal could not be queried, so
  transport failures are not misreported as stale projections.

The continuation payload currently contains only accumulated admission
failures. Encoded-size measurement and a cap are deferred: rejected admissions
are not expected to be the source of unbounded growth, and any caller retry
loop should be fixed at its admission boundary. Custom counters, histograms,
exporters, dashboards, and deployment alert rules are also deferred; the
stable structured records are available for log-derived metrics when needed.

Normal long runs should show periodic history-driven rollovers, not workflow
failures and not fixed-step churn.

## Implementation Slices

### Slice 1: Remove the hosted step ceiling

- [x] Add an unbounded CoreAgent drive entry point or make the limit optional.
- [x] Remove the gateway's default `Some(128)`.
- [x] Remove the field from new `AgentSessionArgs` payloads.
- Keep explicit limits only in bounded test/eval/in-process substrates.
- [x] Add a regression test that exceeds 128 drive transitions without failing or
  continuing as new when the history policy is not due.

### Slice 2: Active-run history rollover

- [x] Evaluate the existing history policy after durable append boundaries inside
  the drive loop.
- [x] Return a typed `ContinueAsNew` outcome to the workflow entry point.
- [x] Reload from PostgreSQL and continue the same active run.
- [x] Prove run id, turn/tool state, and session head continuity across rollover.

### Slice 3: Continuation-state audit and transport preservation

- [x] Classify every workflow-state field.
- [x] Add a versioned continuation payload for state that cannot be reconstructed.
- [x] Restore it after bootstrap without duplicating already-durable commands or
  emissions.
- [x] Avoid carrying transport queues, so no queue payload-size policy is needed.

### Slice 4: Recovery and operations

- [x] Decode but ignore legacy input and omit it from every new/continued payload.
- [x] Guard the new active-run command branch with a Temporal replay patch marker.
- [x] Add structured process logs for rollover, delayed rollover, and workflow
  failure.
- [x] Detect and log stale active projections from the deployment reaper.
- [ ] **Deferred, not implemented:** failed-session reconciliation runbook. The
  deployment is still greenfield, so migration/recovery ceremony is not part
  of this cut.
- [ ] **Deferred, not implemented:** custom metrics and deployment alert rules.
  They may be derived from the structured records once the monitoring stack is
  selected.

## Required Tests

### Deterministic/unit coverage

- [x] Hosted workflow construction has no step limit.
- [x] Passing 128, 1,000, and several thousand drive transitions is ordinary
  progress.
- [x] Step count alone never requests continue-as-new.
- [x] Temporal suggestion requests rollover at a safe boundary.
- [x] History threshold requests rollover at a safe boundary.
- [x] Below-threshold history does not request rollover.
- [x] No rollover is requested with an activity result not yet durably appended.
- [x] Continuation-state encode/decode preserves every non-log-derived field.

### Live Temporal coverage

- [x] Passed against the local Temporal/PostgreSQL stack on 2026-07-30: one
  fake run exceeds 128 drive transitions and completes in one Temporal
  execution when history remains below the test threshold.
- [x] Passed against the local Temporal/PostgreSQL stack on 2026-07-30: one
  fake active run crosses a low history threshold, continues as new, retains
  its Lightspeed session/run ids, and exposes multiple Temporal run ids.
- [x] Passed against the local Temporal/PostgreSQL stack on 2026-07-30: a
  workflow-tool start and its parked holder await survive continue-as-new,
  reconstruct pending start work, and resolve the original run.
- [ ] **Deferred, not implemented:** signal delivery during an LLM activity and
  at the continue-as-new command boundary.
- [ ] **Deferred, not implemented:** fault injection proving a tool result is
  appended before rollover without reinvocation.
- [ ] **Deferred, not implemented:** Promise-source poll and cancellation
  rollover cases beyond existing deterministic coverage.
- [ ] **Deferred, not implemented:** worker restart/replay during a long run.
- [ ] **Deferred, not implemented:** live stale-projection reconciliation after
  terminal workflow failure. P105 detects and reports the condition; it does
  not mutate the session log to repair it.

### Incident regression — deferred, not implemented

An exact end-to-end reproduction of the external `run_5` sequence is not part
of the completed cut. If implemented later, it should:

1. perform more than 128 internal drive transitions;
2. complete a simulated external PR-creation side effect;
3. start a final verification tool batch;
4. complete and record that batch;
5. finish the run without `StepLimitReached`, workflow failure, or a stale
   active projection.

## Explicitly Rejected Fixes

- Raise 128 to 1,024, 10,000, or another guessed value.
- Continue as new every N agent steps.
- Disable continue-as-new and allow unbounded Temporal history.
- Continue as new while an activity or uncommitted activity result exists.
- Drop queued signals or emissions to reach a quiescent checkpoint.
- Treat continue-as-new as an ordinary error and rely on workflow retry.
- Make clients periodically start replacement runs or sessions.
- Declare the PR incident successful and leave the poisoned-session behavior in
  place.

## Done When

- [x] Hosted runs have no default or hidden drive-step ceiling.
- [ ] **Deferred validation:** a single live run executes for hours and
  thousands of transitions. Deterministic coverage exercises thousands of
  transitions, and the hosted live regression exceeds the former 128-step
  ceiling.
- [x] Continue-as-new is driven only by Temporal history need.
- [x] Active runs safely cross continue-as-new boundaries.
- [x] No in-flight activity result, signal, emission, or cancellation is lost by rollover.
- [x] Continue-as-new is never surfaced as a workflow failure.
- [x] Existing `max_steps_per_input = 128` sessions have a defined cutover.
- [ ] **Deferred, not implemented:** an exact live reproduction of the `ls-dev`
  PR-creation incident. The implemented synthetic live regression covers its
  long model/tool-round shape without repeating the external PR side effect.
- [x] Operators can distinguish healthy rollover from actual workflow failure
  using structured process logs, without adding diagnostic session events.
