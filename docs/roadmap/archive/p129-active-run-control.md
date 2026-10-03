# P129: Active-Run Control — Cancel, Steer, Queue

**Status**

- Proposed 2026-08-19. **Phase 1 and Phase 2 implemented and live-validated
  2026-08-19** (Phase 1 slices 1–4, Phase 2 slice 5; slice 6 CLI polish
  folded into Phase 1; see the slice list). Phase 1 summary:
  (slices 1–4 below): engine cancel without grace turn and engine-resolved
  cancellation of open turns/pending calls, workflow admission draining at
  every drive boundary with activity races + `TryCancel`/heartbeat abort,
  `session/runs/steer`, `session/runs/start` returning at `queued`, queued
  runs and entry sources in projections, CLI wiring, five `temporal_live`
  scenarios. Phase 2: platform web UI stop/steer/queue with authoritative
  run state, verified in a real browser against the dev stack.
- Follows the survey of run interactions done the same day; supersedes the
  inert "grace turn v1" note in
  [P92](archive/p92-unified-suspension.md) §3 for client-initiated cancels.
- Greenfield: the session workflow loop, engine run state machine, API, and
  platform UI change in place. No compatibility shims, no patch markers for
  old executions beyond what the Temporal SDK needs for replay of
  in-flight workflows during deployment (see "Rollout").

## Goal

Make the three client-facing ways of interacting with an active run actually
work, end to end:

1. **Cancel** — stop the active run (or dequeue a queued one) promptly,
   aborting the in-flight LLM call or tool batch instead of waiting for it.
2. **Steer** — inject a message into the *current* run; the model sees it at
   the next turn boundary.
3. **Queue** — submit a follow-up message while a run is active; it becomes
   the next run and is visible as queued until it starts.

Phase 1 fixes the engine, the Temporal session workflow, and the API/gateway
so these are correct and observable. Phase 2 makes the platform web UI (and
the CLI) expose all three with proper feedback.

Non-goals: delivering messages into a run parked on `await(mailbox=true)`
(`SubmitMessage` → `MessageBuffered`, fleet-only; removed with fleet by P134
slice 7 on 2026-08-25);
`ForceCancelRun` as a client surface (remains a watchdog/reaper/force-close
lever); streaming partial-output preservation on cancel; per-call tool
cancellation UI.

## Today

### Engine (`crates/harness`) — mostly complete

- `CancelRun` → `CancellationRequested` → `Cancelling`; the planner waits
  for the open turn/tool batch to drain, then `CancellationGraceStarted` →
  `CancellingGrace` → **one more full LLM turn** → `Cancelled`
  (`core/components/run.rs:424-457`, `turn.rs:59`). Queued runs →
  `QueuedCancelled`. Tool batches do not start new invocations while
  cancelling (`tooling.rs:95-104`). Parked awaits wake with
  `WakeReason::Cancelled` (`core/drive.rs:1161-1170`).
- `RequestRunSteering` → `SteeringAccepted` on the active run; steering is
  materialized into context and consumed by the next turn
  (`admit.rs:535-561`, test `drive.rs:3577`). Rejected unless the run is
  `Active` (`active_run_for_command`): not while `Parked`, `Cancelling`, or
  absent.
- `RequestRun` has no active-run check; while a run is active the new run is
  queued and started by `plan_next` when the active run ends
  (`admit.rs:166-249`, `run.rs:458-470`).
- An LLM result with `LlmGenerationStatus::Cancelled` yields
  `TurnOutcome::Cancelled`; on an `Active` run that becomes
  `RunFailure::Cancelled`, on a `Cancelling` run it simply drains
  (`run.rs:504-511`).

### Session workflow (`crates/temporal-workflow`) — the actual defect

- Every client command arrives as one `submit_admissions` signal and is only
  pushed onto `pending_admissions` (`workflows/session/mod.rs:233-244`).
- The **only** consumer of `pending_admissions` is the outer loop
  (`mod.rs:186-199`). `drive_until_idle` (`workflows/session/drive.rs:98-147`)
  awaits `call_llm_generate`, then tool activities, then the next turn, and
  keeps going until the drive is idle — i.e. the run is terminal or parked on
  an await. It never looks at `pending_admissions`, never races the in-flight
  activity against anything, and when the run ends `plan_next` starts the
  next queued run inside the same loop.
- Consequences:
  - `CancelRun` is admitted only after the run finished by itself (no-op), or
    while the *next* queued run is active (no-op for the intended run, and
    the wrong run is left alone). The in-flight LLM call is never aborted.
    The LLM activity has no heartbeat (`config.rs:134`), so Temporal activity
    cancellation could not reach the worker anyway.
  - `RequestRunSteering` would be admitted after the run ended and rejected
    (`MissingActiveRun`).
  - `RequestRun` (queue) is admitted after the active run ended, which is
    "correct" only by accident and makes the gateway wait (below).
- Backstop: the 60 s cancelling watchdog (`watchdog.rs`) force-cancels a run
  stuck in `Cancelling`/`CancellingGrace` — but it only arms once the
  `CancellationRequested` event exists, which is the part that never happens
  in time.

### Gateway / API (`crates/api`, `crates/temporal-runtime`)

- `session/runs/cancel` pre-validates against store state, signals
  `CancelRun`, then polls until the run leaves `Active` or a 90 s
  `operation_timeout` (`gateway/service/mod.rs:2546-2606`,
  `workflow.rs:527-572`). Given the workflow defect it returns either the
  *completed* run (if the run ended within 90 s) or
  `internal: timed out waiting for agent run cancellation`.
- There is **no steering method**. `RunSteeringAccepted` is observable as a
  session event (`api/src/sessions.rs:993-997`) but nothing can submit one.
  The CLI's `/steer` and `/interrupt` are stubs (`cli/src/chat/driver.rs:350-361`).
- `session/runs/start` while a run is active signals `RequestRun` and then
  `wait_for_run_accepted` (`workflow.rs:347-400`) looks only at
  `status.active_run` and `status.completed_runs` — never `queued_runs` — so
  the call blocks until the queued run actually *starts*, or times out.
- No live test exercises cancel of an active run, steering success, or
  queueing through the gateway. The only workflow-level steering test uses
  it as a guaranteed-rejecting command (`runs_live.rs`).

### Platform UI (`platform/web`, `platform/backend`)

- Stop: `SessionsPage.tsx:738-751` → `POST …/runs/:runId/cancel` →
  `gateway.ts:644-657` → `session/runs/cancel`. Wired, but:
  `runActive = activeRun !== null || pending.length > 0`
  (`SessionsPage.tsx:700`) shows Stop during the optimistic-echo window while
  `stop()` bails on `!activeRun` (`:739`); `send()` discards the run id the
  server returns (`:727-731`); the cancel response is thrown away
  (`gateway.ts:651-655`); there is no "stopping" state; `activeRun` is
  projected only from the event tail (`lib/sessions/transcript.ts:106-163`)
  whose catch-up truncates at 20 pages (`tail.ts:67-76`) and is never
  reconciled against `SessionView.runs`; `pending` clears by text match
  (`:685-698`).
- Composer: textarea stays enabled but `submit()` silently drops input while
  `runActive` (`components/session/composer.tsx:27-31`). No steer UI, no
  queue UI, no queued-run display.

## Design

### Semantics (product contract)

| Interaction | Engine | Effect on in-flight work | Terminal state |
|---|---|---|---|
| Cancel active run | `CancelRun` | LLM activity and tool-call activities are **cancelled immediately**; no grace LLM turn | run `cancelled`; turn `cancelled`; pending tool calls recorded as cancelled |
| Cancel queued run | `CancelRun` | none | `QueuedCancelled` → run `cancelled` |
| Steer | `RequestRunSteering` | none — the in-flight turn and its tool batch finish; steering is materialized and delivered at the **next turn boundary** | run continues |
| Queue | `RequestRun` | none | run `queued`; starts when the active run reaches a terminal state |

Decisions embedded above (each is a deliberate choice; alternatives noted):

- **Steering does not interrupt the in-flight turn.** It lands before the next
  LLM call, after the current tool batch (if any) completes. This wastes no
  provider work and is deterministic. An `interrupt: true` variant (abort
  generation, re-plan with steering included) is a possible follow-up, not
  part of this P. Two refinements made during implementation:
  - Steering **materializes at the turn boundary, not mid-turn.** An
    in-flight turn's request is frozen at its planned context/config/toolset
    revisions and the hosted runtime re-derives it from state, so context
    must not move under it; the engine's context planner now skips steering
    materialization while `active_turn_id` is set. Steering admitted during
    a turn is durable (`SteeringAccepted`) and lands right after that turn.
  - **Unconsumed steering extends the run by one turn.** A final-output turn
    no longer completes the run while a steering batch is unconsumed; the
    run takes one more turn whose request carries the steering. Without this
    a steer during a single-turn answer would silently do nothing. A steer
    admitted before a cancel is dropped with the cancelled run (the web UI
    says so).
- **Run control lands mid-turn; context/config/tool mutations do not.**
  While a turn's generation is in flight the workflow admits only cancel,
  force-cancel, steer, run requests, messages, promise resolutions,
  workflow-tool facts, tool-batch resumes, and close; `session/context/*`,
  config, tool, and environment mutations stay queued (in order) until the
  turn completes, for the same frozen-revision reason. Previously they
  waited for the whole run, so this is strictly earlier.
- **Client cancel skips the grace turn.** P92 already stated that
  operator/client cancels must not fan out farewell LLM turns and that grace
  v1 is inert. Once the in-flight generation is actually aborted there is
  nothing for a grace turn to "complete". Recommendation: remove
  `CancellingGrace`, `CancellationGraceStarted`, and
  `cancellation_grace_turn_id` entirely (`Cancelling` → drain → `Cancelled`);
  the watchdog keeps covering `Cancelling`. Alternative if fleet semantics
  want a farewell turn later: keep the state but add `CancelRun { grace: bool }`
  with the API always passing `false`. Either way the API cancel never runs a
  grace turn.
- **Queue is first-class and observable.** `session/runs/start` returns as
  soon as the run is *queued* (status `queued`), not when it starts. Queued
  runs are visible in `SessionView.runs`, cancellable with
  `session/runs/cancel`, and emit `runAccepted` / `runStarted` events as
  today.
- **Cancel returns at `cancelling`**, as documented today; terminal
  `runCancelled` is observed via events. With the workflow fix that gap is
  sub-second to a few seconds, bounded by the activity cancellation.

### Interaction with parked runs (await)

A run is `Parked` only when the model itself chose to wait: an `await` tool
call or a joined workflow-tool batch suspended with an `AwaitSpec`
(`run.rs:177-214`). While parked the drive is idle and the workflow sits in
its outer loop, so admissions are already drained promptly — the P129 §1.2
fix is about *active* runs executing an activity (including long process
tools, which are in-flight activities, not parked).

- **Cancel while parked — unchanged, already correct.** `CancelRun` →
  `Cancelling` → `await_wake` yields `WakeReason::Cancelled`
  (`core/drive.rs:1161-1170`) → `process_satisfied_await` resumes the batch
  with `AwaitOutcome::Cancelled` (`awaits.rs:57-80`) → the await tool result
  is recorded, run-scoped pending promises cascade-cancel (child runs get
  `CancelRun`, environment jobs the cancel activity — P92/P100b), the batch
  completes its bookkeeping while `Cancelling` (`tooling.rs:95-104`) and the
  run reaches `Cancelled`. With §1.1 the grace detour disappears; nothing
  else changes. Covered by
  `resume_of_deferred_batch_while_cancelling_reaches_cancelled`; add a
  live scenario (cancel a run parked on a child-session await).
- **Steer while parked — accept, do not wake (decision).** Today
  `active_run_for_command` rejects steering unless the run is `Active`
  (`admit.rs:901-915`). P129 accepts `RequestRunSteering` for `Active` *and*
  `Parked`: `SteeringAccepted` is appended, the await keeps waiting on
  exactly what the model asked for, and the steering entry is included in
  the first LLM turn after the batch resumes (promise terminal, timeout,
  mailbox, or cancel). This is the same rule as everywhere else in this P —
  steering lands at the next turn boundary and never interrupts work the
  model chose. A user who wants to break the wait cancels the run and queues
  a follow-up, which continues with the same session context.
  `session/runs/steer` therefore accepts parked runs, and the returned
  `RunView` status lets clients say "will be seen when the agent resumes".
- **Not in scope: steering that wakes the await.** Treating steering like a
  mailbox message (`WakeReason::Steering`, await outcome `steered`) would
  change the await tool's contract and overlaps the fleet-only
  `MessageBuffered` path. If ever wanted, make it opt-in on the await spec
  (e.g. `wakeOnSteering`, or let `mailbox: true` also admit steering), never
  the default.
- **Steer while `Cancelling`** stays rejected (`ActiveWork`).

### Phase 1 — engine + workflow + API

#### 1.1 Engine: cancel without grace; steering admissibility

- Remove the grace turn from the cancellation funnel (see decision above):
  `Cancelling` with no open turn and no open batch → `Cancelled`. Delete
  `RunStatus::CancellingGrace`, `RunEvent::CancellationGraceStarted`,
  `ActiveRun::cancellation_grace_turn_id`, the grace branches in
  `turn.rs`/`tooling.rs`/`run.rs`, the codec name, and the
  `api-projection` collapse. Update the engine tests
  (`resume_of_deferred_batch_while_cancelling_reaches_cancelled`,
  `force_cancel_run_reaps_parked_cancelling_run_and_retries_are_noops`,
  `force_close_cancels_active_and_queued_work_and_closes`).
- Cancelled in-flight turn: `LlmGenerationResult { status: Cancelled, … }` on
  a `Cancelling` run must leave the turn `Cancelled` with no context entries
  and let the run drain to `Cancelled` (not `Failed { Cancelled }`). Add an
  engine test for exactly this sequence: `RequestRun` → generate pending →
  `CancelRun` → `resume_generation(Cancelled)` → `Cancelled`.
- Cancelled tool calls: a batch whose remaining calls come back as cancelled
  while the run is `Cancelling` completes its bookkeeping
  (`tooling.rs:95-104` already does this) and the run drains. Test it.
- Steering: allow `RequestRunSteering` while the run is `Active` **or**
  `Parked` per "Interaction with parked runs" above (accepted, does not
  wake the await, materializes on resume); keep rejecting while
  `Cancelling`. Add an engine test: steer a parked run, resume the await,
  assert the next request carries the steering entry. Reject empty input with `InvariantViolation` (already).
  Confirm `next_generation_request` includes unconsumed steering entries and
  marks them `consumed_by_turn_id` (existing test `drive.rs:3577` covers the
  snapshot ordering — extend it to a full request build).
- `CancelRun` for a queued run stays `QueuedCancelled`. `CancelRun` while
  already `Cancelling` stays an idempotent no-op.

#### 1.2 Workflow: admissions are drained at every drive boundary and race in-flight activities

This is the core of the P. The session workflow must treat client admissions
as first-class inputs to the drive loop, not as something to look at between
runs.

- **Unify admission processing.** Extract `admit_pending_admissions(ctx,
  drive)` from `process_admissions` (`admissions.rs:3-38`) so it can run
  against the *live* drive from inside `drive_until_idle` (preprocessing
  activities and the runtime-projection refresh remain ordinary awaited
  activities inside it). The outer loop keeps calling it too.
- **Drain at every action boundary.** In `drive_until_idle`, before computing
  each `next_action`, drain `pending_admissions` through the live drive. A
  `RequestRun` queues, a `RequestRunSteering` appends `SteeringAccepted`, a
  `CancelRun` appends `CancellationRequested`; the next `plan_next` then sees
  the new state (no new turn after cancel; steering included in the next
  request; queued run started after terminal).
- **Race in-flight activities against admissions.** `call_llm_generate`,
  `call_context_compact`, and the per-call tool activities in
  `execute_call_group` are awaited inside a `select!` (or the existing
  `select_all` window for tool calls) together with
  `ctx.wait_condition(|s| !s.pending_admissions.is_empty())`. Per the
  TMPRL1100 constraint, keep the activity futures boxed/pinned and re-polled
  across iterations; no `FuturesUnordered`.
  - On wake with pending admissions: drain them through the live drive
    (append events). If the active run is now `Cancelling` (or closed):
    cancel every in-flight activity future (`CancellableFuture::cancel`) and
    treat each as a cancelled result — `LlmGenerationResult { status:
    Cancelled }` for the LLM call, a terminal cancelled call result for each
    tool call (the `resume_call` path already maps cancelled activities).
    Otherwise (steer, queue, context admissions) go back to awaiting the same
    activity futures.
  - Activity options: `llm_activity_options()` and the tool-call options get
    `cancellation_type = TryCancel` so the workflow does not wait on the
    worker to acknowledge, plus a `heartbeat_timeout` so cancellation is
    delivered to the worker.
- **Worker: honour cancellation.** `llm_generate` and `tool_invoke_call`
  heartbeat on a ticker for the lifetime of the call and race the provider
  request / tool execution against `ctx.cancelled()`; on cancellation abort
  the HTTP request (drop the future) or signal the tool execution (processes
  get the existing grace/kill path) and return
  `ActivityError::cancelled()`. Token/compute is stopped, not just ignored.
- **Queued run after cancel.** When the cancelled run reaches `Cancelled` and
  a queued run exists, `plan_next` starts it in the same drive loop (existing
  behaviour, now correct because the cancel landed on the right run).
- **Watchdog** keeps covering `Cancelling` only; the grace branch goes away
  with 1.1. Consider lowering `CANCELLING_WATCHDOG_MS` (60 s) now that the
  normal path is activity cancellation; keep it as the "missed edge" backstop.
- **Status/observability**: `AgentSessionStatus` already exposes
  `queued_runs` and `pending_admissions`; add a `cancelling` marker on
  `AgentActiveRunSummary` if not already derivable from `status`.

#### 1.3 Gateway + API

- `session/runs/steer` — new method. Params `RunSteerParams { sessionId,
  runId, items: Vec<InputItem> }` (same `InputItem` vocabulary as
  `session/runs/start`; media goes through the same preprocessing);
  result `RunSteerResponse { steeringId, run: RunView }`. Gateway: validate
  the run is the active run in `Active`/`Parked`, signal
  `RequestRunSteering` with a correlation token, wait until the run's
  steering count advances or a correlated admission failure appears (reuse
  the correlation-token failure lookup from context append), then return the
  projected run. `runId` is required so a late steer for a finished run is
  rejected (`rejected: run is not active`) rather than landing on the next
  one. Register in the manifest, add routing test, regenerate contract +
  TypeScript client.
- `session/runs/start` — `wait_for_run_accepted` returns as soon as the
  submission appears in `queued_runs` (status `queued`), not only
  `active_run`/`completed_runs`. Doc text already says "returns once the run
  is queued/accepted".
- `session/runs/cancel` — keep the contract (returns at `cancelling`), but:
  it must not return a `completed` run for a cancel it never delivered; with
  1.2 this no longer happens. Shorten the wait loop's reliance on the 90 s
  timeout is unnecessary once the workflow admits promptly; leave as is.
- `RunView`/`SessionView.runs`: ensure queued runs are projected with their
  source so clients can render "queued: <text>"; ensure steering entries are
  projected into the run's `entries` with a distinguishable source
  (`ContextEntrySource::Steering` → a `steering` role/kind on the view) so
  transcripts can show "steered: …" inline.
- CLI: wire `/interrupt` to `session/runs/cancel` and `/steer` to
  `session/runs/steer`; plain input during an active run becomes a queued
  run (and is shown as queued). This is small and lives with Phase 1 since
  the CLI is the fastest live driver for the new methods.

#### 1.4 Tests (Phase 1)

- Engine unit tests listed in 1.1.
- Workflow unit tests (`workflows/session/tests.rs`): admission drained at an
  action boundary; steering materialized into the next request; cancel
  during LLM await cancels the activity and drains; cancel during a tool
  group cancels the remaining calls.
- Live (`temporal_live`, `--test-threads=1`): with the fake/slow provider,
  (a) cancel an active run mid-generation → `cancelling` within the activity
  heartbeat interval and `cancelled` shortly after, no further LLM calls;
  (b) steer an active run between two tool-calling turns → the second turn's
  request contains the steering entry and the final output reflects it;
  (c) queue a run while one is active → `runs/start` returns `queued`
  promptly, the queued run starts after the first ends, and a queued run can
  be cancelled; (d) cancel the active run while another is queued → only the
  active one is cancelled and the queued one starts.
- Gateway routing test for `session/runs/steer`; contract export; `npm run
  check`.

### Phase 2 — platform UI (and CLI polish)

Goal: a user in the web UI can always see what the session is doing and can
stop it, talk to it, or line up the next thing — with honest feedback.

- **Run state is authoritative, not inferred.** Reconcile `activeRun` /
  queued runs from `SessionView.runs` (fetched on mount and after every
  mutation) in addition to the event tail; the tail only advances. Fix the
  catch-up truncation so the reducer never skips `runStarted`/`runCompleted`
  (page to the head instead of jumping, or seed from `SessionView.runs`).
  Track optimistic sends by the returned run id (`send()` must keep the
  `SessionRunAccepted` result), not by text match.
- **Stop.** Always enabled while a run is active or queued; uses the real run
  id; on click enter a local `stopping` state, call cancel, render the
  returned `RunView` status, surface errors inline, and clear `stopping` on
  `runCancelled`/`runCompleted`. Queued runs get their own × control calling
  the same endpoint.
- **Composer while a run is active.** Never silently drop input. The composer
  stays live:
  - Enter = **Queue as next run** (default);
  - ⌘/Ctrl+Enter = **Steer** ("send now, into the current run"; shown in
    the transcript as a steering message attached to the run);
  - both call platform backend routes → `session/runs/start` /
    `session/runs/steer`.
- **Queued display.** A queued list under the transcript (source text, ×),
  fed by `SessionView.runs` + `runAccepted`; collapses as runs start.
- **Transcript.** Render steering entries distinctly inside the run they
  steered; render `cancelling`/`cancelled` states on the run header.
- **Platform backend.** Add `POST …/runs/:runId/steer` and make
  `…/messages` accept a `mode: "queue"` (or a dedicated `…/runs` route) that
  returns the queued `RunView`; return the cancel response body instead of
  `{ ok: true }`.
- **CLI (polish).** Show queued runs and steering acknowledgements in the
  TUI; keep `/steer`, `/interrupt`, and plain-input-queues from 1.3.

## Rollout

- Workflow changes alter the command sequence of in-flight executions (new
  `select!` wake points, activity cancellation commands). Use a patch marker
  (`p129_active_run_control_v1`, as P105 did) for the drive-loop branch so
  executions already mid-run at deploy time replay deterministically; the
  new path activates on their next workflow task. Remove the marker once no
  pre-P129 histories remain (the dev stack can be reset).
- Engine event removal (`CancellationGraceStarted`, `CancellingGrace`) is a
  log-vocabulary change; dev stores are reset, there is no production log to
  migrate.
- Contract regeneration (`cargo run -p api --bin export-schema`, `npm run
  check`) after the API change.

## Slices

1. **[DONE]** Engine: grace turn removed (`CancellingGrace`,
   `CancellationGraceStarted`, `cancellation_grace_turn_id` deleted);
   steering accepted while `Parked`; new `Turn::Cancelled` event — a
   cancelling run's open turn (started/planned/generation-pending) is
   cancelled by the planner, its pending tool calls get engine-synthesized
   cancelled results (well-known `CANCELLED_TOOL_RESULT_CONTENT` blob), a
   completed tool-call turn whose batch has not started still gets a batch so
   no tool call is left without a result; `next_generation_request` /
   `next_tool_batch_request` never ask the runtime for work on a cancelling
   run. Engine tests for each shape.
2. **[DONE]** Workflow: `admit_admissions` / `drain_pending_admissions`
   shared by the outer loop and `drive_until_idle` (drained before every
   action, re-planned after); `control::race_activity_with_admissions` races
   LLM generation, unit tool batches, per-call tool activities (own
   `first_ready_call` poll, no `FuturesUnordered`), and the environment
   readiness wait/re-dispatch against `wait_condition(pending_admissions)`;
   `still_wanted` predicates decide preemption; preempted activities are
   `cancel()`ed (`TryCancel`) and awaited, results discarded. Standalone
   compaction is neither raced nor drained over. Worker: `llm_generate`,
   `context_compact`, `tool_invoke_call`, `tool_invoke_batch` run through a
   heartbeating, cancellation-aware wrapper; `heartbeat_timeout` 10 s on
   those options (cancel reaches the worker within ~8 s). Watchdog covers
   `Cancelling` only.
3. **[DONE]** Gateway/API: `session/runs/steer` (`RunSteerParams {
   sessionId, runId, items }` → `{ steeringId, run }`, correlated admission
   wait, rejects queued/cancelling/terminal runs); `runs/start` returns at
   `queued`; `runs/cancel` waits for `Cancelling`/terminal (a parked run no
   longer returns `running`); `SessionView.runs` includes queued runs
   (`status: queued`); `ContextEntryView.source` exposes run input / steering
   / assistant / tool / reasoning / runtime provenance; tool-call views take
   `cancelled` from the durable per-call completion; contract + TypeScript
   client regenerated; CLI `/interrupt` → cancel (queued first, then
   active), `/steer` → steer, plain input during a run → queued run.
4. **[DONE]** Live validation (`temporal_live`, fake runtime with scripted
   delays and shared counters): cancel mid-generation (engine cancels in
   <1 s, worker abandons the provider call, no grace turn, session serves the
   next run), cancel during a tool batch (cancelled call result with the
   well-known content, batch view `cancelled`), steering between turns (final
   answer echoes the steering; finished run rejects steering), queueing
   (immediate `queued`, visible in the session view, cancel queued, cancel
   active with one queued → queued runs next), cancel while parked on a child
   await (await resolves `cancelled`, child run cancelled through the promise
   cascade). Existing fake-run, parallel-batch, admission-failure, and
   await-parks scenarios still pass.
5. **[DONE]** Platform UI (`platform/web`, `platform/backend`): the
   transcript reducer tracks `activeRun` (with `cancelling`), `queuedRuns`,
   per-run phases, and a `runRevision`; `reconcileRuns` folds the
   authoritative `SessionView.runs` into the tail forward-only (heals a
   truncated catch-up; the page refetches `session/read` on every run
   lifecycle change); the follow loop always advances its cursor; optimistic
   sends are reconciled by run id (the POST result), steers by steering
   entry on their run; the composer stays live during a run: Enter
   queues the message as the next run, ⌘/Ctrl+Enter steers it into the
   current run (falls back to queue while the run cannot be steered), a
   Stop button with a stopping state,
   and a queued-messages bar with per-item cancel; steering entries render
   with a `steer` tag; an undelivered steer (run ended first) leaves a
   notice instead of vanishing. Server routes: `…/runs/:id/cancel` returns
   the run state, new `…/runs/:id/steer`. Verified in headless Chromium
   against the dev stack: steer during a long answer → tagged entry + extra
   turn; queue → bar → cancel-queued; stop → `run cancelled` in <1 s and the
   queued run starts.
6. CLI polish was folded into slice 3 (`/interrupt`, `/steer`, plain input
   queues, steering/cancel status lines). `docs/design.md` has no run-control
   section yet; the README feature bullet and AGENTS.md rule cover it.

Known follow-ups from Phase 1:

- Provider-call abandonment latency is bounded by the heartbeat throttle
  (~0.8 × 10 s); the engine-side cancel is immediate regardless.
- The runtime-projection refresh (skill/VFS catalogs) runs at admission of
  an idle run; a queued run starts without a fresh refresh.
- Workflow-level unit tests for the drive loop remain live-only (no replay
  test harness); the engine and live suites carry the coverage.

## Open questions

- Whether to keep a `grace` option on `CancelRun` for fleet/child cancels
  (recommendation: drop grace entirely now; reintroduce with a real purpose
  if a farewell turn is ever needed).
- Heartbeat interval for the LLM activity (trade-off: cancellation latency vs
  heartbeat traffic; a few seconds is fine).
