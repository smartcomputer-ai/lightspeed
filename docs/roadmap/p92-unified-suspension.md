# P92: Unified Suspension — Promises, Await, And Cancellation

**Status**
- Proposed 2026-07-07.
- Motivated by a production incident on 2026-07-06 (see Background) in which a
  fleet subagent session deadlocked in `cancelling` and could never be closed.
- Builds on **P83 (Fleet Subagent Control Plane)**, **P84 (Fleet Wait,
  Subscriptions, And Send)** — including its generic deferred-tool-batch
  primitive — and **P86 (Durable Environment Jobs)**, whose job waits are the
  second consumer of that primitive.
- Supersedes the wait/cancel surfaces of P84 (`agent_wait`, `agent_cancel`,
  run subscriptions) and P86 (job wait/cancel) by folding them into one
  suspension-and-revocation mechanism. The `agent_send` graph-edge delivery
  of P84 is retained as mailbox-only fire-and-forget messaging; delegated
  work uses `agent_spawn`/`agent_request` promises instead.
- The fleet safety policy layer (spawn budgets, the tree/active-work index,
  tree observability) is split out to **[P93](p93-fleet-safety.md)**;
  this doc is the mechanism it builds on.
- **[P94](p94-engine-native-suspension.md)** finishes this doc's §1 split:
  it types the parked-await directive into the engine, moves the mailbox
  buffer and the wake predicate into engine state/law, and replaces the
  trust-the-blob `ResumeToolBatch` with a claim-validated `ResumeAwait`.
  Where P94 and the §5/appendix mechanics described here disagree, P94 is
  the settled direction.
- **Implementation: steps 1–6 are complete** (2026-07-07, branch
  `structured-concurrency`): unified promises/await/cancel, job waits folded
  in, cancellation grace and parked runs, deployment orphan reaper,
  `spawn/request/send` messaging cleanup, promise detach, mailbox wake, and
  continue-as-new portability. See the Implementation Plan below for per-step
  state.

## Goal

Give sessions **one** way to pause mid-run and wait for asynchronous results —
a subagent run, an environment job, a timer, and in the future a sub-workflow,
a human approval, or an external event — and **one** way to revoke async work,
with semantics that stay correct under cancellation, session close,
continue-as-new, and concurrent inbound messages.

We are still greenfield, and this lands as an **aggressive breaking
refactor**: every replaced surface — the wait tools, the per-domain cancel
tools, the wait records, the run-subscription signals — is deleted in the
same change that lands its replacement. No compat aliases, no dual wait
paths, no deprecation window; profiles, prompts, and tests update in
lockstep.

There are exactly two wait implementations and one mailbox path today, and
the first fleet-versus-cancellation race already produced an unrecoverable
session in production. This is the moment to settle the suspension model
once, before more wait consumers (triggers/P101, sub-workflows, approvals)
multiply the surface. Every future async feature becomes a new *source*
behind the same primitive, never a new primitive.

## Background: The 2026-07-06 Incident

A WhatsApp user asked a bridge-bound session (`assistant` profile, `fleet:
true`) a one-line question: *"what's going on in confluence"*. What followed,
entirely within ~90 seconds:

1. The parent session ran one Confluence search itself, then spawned **two**
   subagents in one tool batch (page summarizer, space-activity checker).
2. The second subagent — same profile, so fleet tools included — spawned **two
   more** subagents of its own (depth 3 from a one-line question).
3. One grandchild delivered every `agent_send` **twice** (duplicate tool calls
   in one batch), then sent a correction — which was also delivered twice. The
   middle agent accumulated six queued runs it never got to process.
4. The parent's `agent_wait` timed out, it inspected the children via
   `agent_read`, concluded the middle child was *"over-delegating"* (its
   literal cancel reason), and tried `agent_cancel scope=session` — rejected:
   `session cannot close with active work`. It then issued
   `agent_cancel scope=active_run`, reported "cancelled", answered the user
   directly, and moved on.
5. The middle child never terminated. The cancellation had landed **while its
   run was parked on a deferred `agent_wait` batch** (5s timeout, waiting on
   its remaining grandchild). The batch resumed on its timer, the wait tool
   call completed — and the run stayed in `cancelling` forever. Temporal showed
   the workflow idle and healthy: **0 pending activities, 0 timers, no expected
   signals**. The grandchild's session-closed notification arrived after the
   last processed workflow task and was never delivered.
6. Because the run never reached a terminal state, the six queued runs never
   started, and the session could never satisfy the "no active work" close
   guard. After an operator `temporal workflow terminate`, the registry row
   remains permanently `active` / `run_1: cancelling`: `session/close` still
   rejects with "active work" that no longer has a workflow to drain it.

Graph of what happened:
```
WhatsApp group 120363409803820781
│  20:33:35  Lukas: "what's going on in confluence"
▼
bridge_aj2w28…  (run_3)
│  20:33:46  searchConfluenceUsingCql (last 7 days) — itself
│  20:33:46  agent_spawn ×2 (one tool batch):
│
├─► agent_6e79c033…  "Summarize page '20_20 Enterprise Sim Test Env' (4947969)"
│     20:33:58  fetched page, agent_send → parent (queued as parent run_4)
│     20:34:02  runCompleted, sessionClosed ✔
│
├─► agent_10964eb0…  "Check recent structure/activity in TWC space"     ⚠ STUCK
│     20:34:00  agent_spawn ×2 (recursion, same profile):
│     │
│     ├─► agent_3122485f…  "inspect child page hierarchy under 98414/3309746"
│     │     20:34:07  agent_send "starting…"  → DELIVERED TWICE (runs 2+3 on its parent)
│     │     20:34:23  agent_send "Correction: duplicate note sent" → ALSO twice (runs 5+6)
│     │     20:34:45  agent_send final findings (run_7)
│     │     20:34:48  runCompleted, sessionClosed ✔
│     │
│     └─► agent_c04ed066…  "check recent page activity / empty placeholders"
│           20:34:21  agent_send findings (run_4 on its parent)
│           20:34:24  runCompleted, sessionClosed ✔
│
│     20:34:15  agent_wait both → error "must be only call in batch"; retry 5s → timeout
│     20:34:29  agent_read c04 ✔ → agent_wait on 3122 again (deferred)
│     20:34:32  ◄── parent's cancel lands: runCancellationRequested
│     20:34:37  deferred wait resumes… and then NOTHING. run_1 stuck in "cancelling"
│     queue: runs 2–7 (children's reports) accepted, never started
│
└─ parent, meanwhile:
      20:33:53  agent_wait both, 15s → timeout (6e79 done, 10964eb0 pending)
      20:34:11  agent_read both → sees child spawned grandchildren
      20:34:20  agent_cancel scope=session → REJECTED "cannot close with active work"
      20:34:27  agent_cancel scope=active_run → "cancelled"
      20:34:41  getConfluencePage itself, message_send answer to WhatsApp ✔
      20:34:41  agent_cancel scope=session again → REJECTED again
      20:34:46  processed 6e79's report (run_4), done
```

Two distinct bugs, one root cause:

- **Cancellation deadlock**: cancel racing a deferred wait batch has no path to
  a terminal run state (missing transition in an implicit state machine).
- **No recovery path**: the close guard consults state only the (dead) workflow
  could drain; there is no force-close.

And several design-level hazards the incident demonstrated in passing:
unbounded spawn recursion by default (addressed by the policy layer in
[P93](p93-fleet-safety.md)), zombie children outliving a cancelled parent,
unbounded mailboxes with no dedupe, and "active work" being an emergent rather
than defined concept.

## Superseded Baseline (Pre-P92)

Two parallel implementations of "park a tool batch on a durable condition",
both consuming the P84 deferred-batch primitive, both resuming through
`pending_tool_batch_resumes`:

| | Fleet waits (`fleet_waits.rs`) | Job waits (`job_waits.rs`) |
|---|---|---|
| Transport | push: cross-workflow `subscribe_run` signals, correlation tokens | poll: activity with 2s/15s/60s backoff + `EnvironmentJobChanged` nudge |
| Deadline handling | own copy | own copy |
| Resolution logic | all/any over handle records | mode + terminal policy over job handles |
| Cancellation integration | **none** | **none** |

Plus:

- `agent_wait` must be the only call in its tool batch — a special case that
  exists because deferral is bolted onto batches rather than being a run state.
- Cancellation is a per-domain afterthought: `agent_cancel` (scope
  `active_run` / `session`) and the job cancel tool sit beside the waits, and
  neither wait implementation participates in cancellation at all — the race
  that produced the incident.
- `agent_send` always materializes as a queued run on the receiver — the
  mailbox and the wait system are disjoint, so request/reply traffic floods the
  run queue and queued runs block session close.
- `can_continue_as_new_at_idle` requires both wait maps to be empty: a
  multi-hour job wait pins workflow history growth (P73/P74 territory) — a
  latent history-limit incident.

## Design

### 1. Promises: one registry, pluggable sources

One promise registry replaces `ActiveWaitRecord` and
`ActiveEnvironmentJobWait`:

```
Promise {
  promise_id,                     // minted in the creation event
  source,                         // Run | EnvJob | Timer | SubWorkflow | Approval | ...
  scope: run | session,           // ownership; see §4
  state: pending | resolved(payload_ref) | failed(error_ref) | cancelled,
  deadline_ms?,                   // reserved; await-level timeout_ms is active in v1
}
```

The registry is split across the two state domains this system already has,
along the line the deferred-batch mechanism already draws (`BatchDeferred` is
a session-log event; `ActiveWaitRecord` is workflow-only state):

- **Semantic state lives in the session log** as engine events: created /
  detached / resolved / failed / cancelled, with scope, source descriptor,
  and payload blob refs. Promises are part of `CoreAgentState`, rebuilt from
  the log like everything else, and `promise_id` is minted in the creation
  event — which makes ID assignment replay-deterministic for free.
- **Transport state lives in the workflow**: the outbound notification flush
  queue, poll schedules, watchdog timers. Poll schedules and timers are
  recomputable from the semantic state; the flush queue is transient and
  gates CAN instead (§6). None of it needs to survive on its own.

The split is mandatory, not layering taste, because three consumers can only
see the log: `session/read` is a store projection and never queries the
workflow, the close guard evaluates `CoreAgentState`, and a dead-workflow
recovery path has nothing else to consult. A workflow-only registry would
rebuild the incident's exact pathology — active work that only a live
workflow can enumerate.

Each source implements one trait with three duties:

- **deliver** — map native completion to a resolution. **Push is the default
  transport**: the observed side signals the holder's workflow on completion,
  so a parked session is truly idle — no history growth, no worker load, no
  poll cadence. A signal-parked workflow costs nothing in Temporal;
  hours/days-long parks are exactly what signal-waits are for. Poll (activity
  + backoff + nudge) is reserved for sources whose remote side cannot signal
  — today only EnvJob, which keeps P86's fixed 2s/15s/60s backoff; the
  `EnvironmentJobChanged` nudge makes the schedule a backstop, and per-source
  backoff configurability waits until a second poll source exists.
- **cancel** — propagate cancellation outward, best-effort (cancel the child
  run, cancel the job, no-op for timers).
- **rehydrate** — re-arm after continue-as-new: recompute poll schedules and
  watchdog timers from log-backed promise state. Push sources need
  nothing — notify-intents live in the *observed* session's log (below), and
  signals target the workflow id, which is stable across CAN on both sides.

Adding sub-workflows, approvals, webhooks (P101 triggers), or outbox delivery
confirmations later means implementing this trait — no new workflow
machinery.

**The edge event is the subscription.** P84's standalone subscription
machinery — the `subscribe_run`/`unsubscribe_run` signals and the
observed-side `run_subscriptions` map — is deleted, not generalized. A
subscription is someone else's intent, which the observed session cannot
recompute after its own CAN: exactly the class of state this section forbids
keeping workflow-only. Instead, the event that creates the edge records the
notify-intent in the observed session's log: a spawn admission carries "on
this run's terminal, signal `{parent workflow id}` with token
`{promise_id}`"; `agent_request` records the same intent at admission for
the existing linked session's requested run. Plain `agent_send` records no
notify-intent and creates no promise. The intent hangs off the run record it
targets (`notify_on_terminal: [{holder workflow id, token}]` in
`CoreAgentState`), so bootstrap rebuilds it with the run — including after
the observed session's own CAN. There is no unsubscribe protocol: the only
reasons to stop caring are that the promise resolved or was cancelled, and
an unwanted notification lands in the §3 funnel as a first-writer-wins
idempotent no-op. Nor does progress reporting grow this vocabulary —
promises are for completion; the mailbox (§5) is for everything in between.

### 2. Async tools return promises; `await` suspends, `cancel` revokes

Target non-blocking surface after the step 5b cleanup:

```
agent_spawn  -> { session_id, run_id, promise }                 // new child session; promise resolves from child run completion
agent_request -> { session_id, run_id, promise }                // existing linked session; promise resolves from requested run completion
agent_send   -> { status, submission_id }                       // fire-and-forget mailbox message; no promise
jobs run     -> { job_id, promise }
sleep {ms}   -> { promise }                                     // future surface; Timer source exists
detach { promises: [promise_id, ...] }                          // run-scope -> session-scope promotion
```

One suspension tool:

```
await {
  promises: [promise_id, ...],
  mode: any | all,                // n_of_m deferred: a total outcome plus
                                  // re-awaitable promises = an `any` loop
  timeout_ms?,
  mailbox: bool,                  // §5: also wake on next inbound message
}
```

Outcome is structured and *total*: per-promise `resolved | failed | cancelled |
pending`, plus `timeout` and `mailbox_message` outcomes. Timeout is a
successful return with partial results; remaining promises stay pending and
re-awaitable (today's `agent_wait` timeout semantics, which are right).

And one revocation tool, its dual:

```
cancel { promises: [promise_id, ...] }
```

Awaiting is watching; cancelling is revoking. An `await` that ends — timeout,
mailbox wake, the model simply moving on — cancels nothing: the promises stay
pending and the work continues. §4 defines what cancellation means per source
and why the tool needs no policy gating of its own.

Step 5 initially landed the more general `agent_send { expect: reply |
completion }` plus `agent_reply` shape. Before this surface becomes
compatibility-sensitive, step 5b collapses it: **completion is the response to
delegated work**, and mailbox sends are just messages. A correlated mid-run
request/reply protocol can be added later as its own explicit product feature
if we need true RPC semantics; it should not be hidden inside `agent_send`.

Consequences:

- The "only call in its batch" constraint dissolves: nothing else defers, by
  construction.
- `agent_wait`, `agent_cancel`, and the P86 job wait/cancel tools are deleted
  from the model-visible surface — no sugar aliases. One way to wait and one
  way to cancel means one doc, one prompt-shape, one test surface.
- The interleaving test matrix (see Test Matrix below) is written once,
  against one mechanism.

### 3. Cancellation is a resolution, not a race

The structural fix for the incident's bug class. Per await there is exactly
one resolution funnel; child-terminal, poll-ready, deadline, **and
cancellation** all go through it. First writer wins; later resolutions are
idempotent no-ops.

`cancel(run)` while parked:

1. resolves the active await with outcome `cancelled` (per-promise snapshot
   included),
2. grants the run one **bounded grace turn** in the state machine,
3. transitions the run to `cancelled`.

The grace turn is deliberately inert in the current implementation: it can
complete model text already in flight, but it does not execute tool batches
and therefore cannot create promises, detach promises, await, run jobs, or
send a final tool-mediated message. Allowing a narrow final-message allowlist
and a hard token cap is future work; the invariant is that grace can never
park again or create work that outlives the cancellation it is draining.
Force-close and session-level cancels
(operator/client-API surfaces) skip grace entirely — a "stop everything"
must not fan out a tree's worth of farewell LLM turns.

Every transitional state (`cancelling`, `closing`, the grace turn) carries a
durable watchdog timer that forces the transition on expiry. A missed edge
must degrade to a forced transition, never a wedge. This watchdog alone would
have prevented the incident's permanent hang.

### 4. Explicit run state machine + promise scope (structured concurrency)

Run states become first-class:

```
active(generating | executing_tools)
  -> parked(await_id)             // durable, visible in session/read
  -> active
  -> terminal(completed | failed | cancelled)
```

Promises are **run-scoped by default**: when a run reaches any terminal state,
its pending promises auto-cancel, cascading through their sources — child runs
get cancelled, jobs get cancelled. This kills the zombie-grandchild class (a
child kept working for 16s after its parent's cancellation, reporting into a
queue nobody would ever read).

`detach(promise)` promotes to session scope for deliberately longer-lived
work. Detach means **free from the current run**, not free from the parent
session or tree: the promise remains owned by the holder session, counts as
active work, and is cancelled by force-close/session teardown. Resolution of
a detached promise enqueues a run with the promise result as input, without a
separate `report_back` parameter or reply channel.

**Active work becomes a definition instead of an emergent property:**

```
active work ≡ active run ∪ queued runs ∪ pending session-scoped promises
```

— all enumerable, all cancellable. That makes `session/close { force: true }`
well-defined (cancel everything in the set, then close) and gives operators
the recovery path that was missing on 2026-07-06. Detached promises need no
special close rule: they are in the active-work set, so a normal
`session/close` is refused while they pend and a force-close cancels them
like everything else — no dead-letter path to a parent, which would be a new
delivery mechanism in a design whose thesis is one primitive.

**Cancellation is ownership-gated.** `cancel { promises }` (§2) reaches
exactly the promises the caller holds. No promise, no cancel — which yields
tree discipline for free: a session only ever cancels its own edges, so the
tool needs no ACL check of its own (what a session can *create* is what
P93's budgets gate). Every cancellation path — the `cancel` tool, the scope cascade,
force-close, the orphan reaper, client-API cancels — converges on the §3
funnel; there is no second cancellation semantics anywhere.

A run-backed promise is a stake in **the specific target run**, not in the
target session wholesale. A spawned child may outlive a cancelled run unless
its lifecycle says otherwise. Per source, `cancel(promise)`:

| Source | `cancel(promise)` does |
|---|---|
| Run | signal the target session: cancel *that run*. Queued → dequeued; active → the target's own §3 funnel + grace turn; parked → the target's await resolves `cancelled`; already terminal → idempotent no-op. Whether a spawned child *session* ends is the child's lifecycle config: `close_on_terminal` one-offs tear down with the run; persistent sessions lose the run and stay open. |
| EnvJob | cancel the environment job (P86 semantics). |
| Timer | discard the timer; nothing external to revoke. |
| SubWorkflow / Approval (future) | source-defined, same contract: revoke outward, resolve inward. |

**Teardown is emergent, not orchestrated.** A session never reaches past its
children. A cancels only its edge to B (`cancel(p_B)`); B's run goes terminal
through B's own funnel; *B's* scope rule then fires and cancels B's edge to
its own children; and so on down the tree. Recursive teardown falls out of
each level applying its own scope rule to its own edges. If a link cannot run
its cascade, the reaper below closes the gap.

**The backstop is a store-side orphan reaper.** Temporal signals to an
open workflow are never lost — they append durably to its history (the
incident's "undelivered" notification was a wedged wait loop, which the
watchdogs fix, not signal loss). The residual risk is a hard-terminated
session: `temporal workflow terminate` skips app code by definition — it is
how the incident actually ended — and it breaks the tree in both directions.
The current deployment-level reaper — a worker background task, not workflow
code — repairs promise edges: it resolves or fails pending run-backed
promises whose target run/session is terminal or gone, and it cancels source
work owned by terminal holder runs. It runs every five minutes by default
and is idempotent under multiple worker processes: the first successful
repair wins, and competing passes re-check log state before appending or
signaling. A full tree-root downward sweep ("cancel every child of a
hard-terminated parent even if no pending promise edge remains") is not yet
implemented and belongs with P93 topology/indexing work. The upward sweep is
possible only because promise state is log-backed (§1); the reaper reads the
store and never touches a workflow. Per-session liveness or completion
polling from inside workflows is rejected: an activity-on-a-timer per
session is constant history churn on otherwise-idle sessions, and it fights
CAN-at-idle.

The reaper is intentionally correctness-first in P92. Its current scan is
O(total session history) per universe pass: it pages sessions and replays
logs to discover pending promise edges. That is acceptable for the local
runtime and tests, but a real deployment needs an indexed pending-promise /
active-work view, closed-session skip, or incremental cursor before this is
treated as cheap background maintenance.

**Why not Temporal child workflows?** Temporal's native structured
concurrency — child workflows with `ParentClosePolicy: REQUEST_CANCEL` —
would give us the cascade server-side, surviving even operator terminate. We
stay on peer workflows deliberately: sessions are first-class and
user-addressable, they continue-as-new independently, they participate in
the P82 session graph, and `detach` changes ownership *after* spawn while
ParentClosePolicy is fixed at start. The price of peers is the app-level
cascade plus the reaper above; the price of children would be losing
detach-after-spawn and session independence.

### 5. Mailbox unification

Two changes to the messaging surface:

1. **Work and messages split.** Delegated work returns promises; messages do
   not. `agent_spawn` creates a child session and returns a promise for the
   child run's terminal output. `agent_request` queues work in an existing
   linked session and returns a promise for that requested run's terminal
   output. `agent_send` is fire-and-forget mailbox delivery only. This keeps
   the response model simple: **the completion of delegated work is the
   response**. A worker does not need Fleet tools just to return a final
   answer, and a receiver completing the run is never confused with an
   explicit reply tool call. `agent_request` keeps the shared target shape but
   rejects `to: { kind: "parent" }`; child-to-parent communication uses
   `agent_send` so it can wake a parked mailbox instead of creating a
   parent/child request cycle.

   Step 5's first-cut `agent_send { expect: reply | completion }` and
   `agent_reply` implementation was removed in step 5b. The
   completion-tracked form becomes `agent_request`; `expect: reply` and the
   reply correlation machinery disappear. If we later need true correlated
   mid-run RPC, it should land as an explicit `agent_request`/`agent_reply`
   protocol with its own semantics, not as a second meaning of `agent_send`.

2. **The mailbox is awaitable.** `await { ..., mailbox: true }` also wakes on
   the next inbound message. A bridge session parked on child agents stays
   interruptible: the user's "cancel that" wakes the parked run rather than
   queueing behind it. Head-of-line blocking — the real cost of parking,
   since a session has one active run — becomes a select arm instead of a
   bug.

   Precise semantics, so delivery never forks on receiver state: a
   `DeliverMessage` arriving while parked with `mailbox: true` resolves the
   await with outcome `mailbox_message`; everything queued at that moment is
   delivered to the woken run as context entries in arrival order and is
   **consumed** — it does not also enqueue runs. This includes fleet sends
   and client-originated `session/messages/submit` messages; `RequestRun`
   admissions are excluded because delegated work and `session/runs/start`
   must resolve from run completion. Consumed message submissions with stable
   `submission_id`s are recorded in the session log with the consuming
   `run_id`, so duplicate retries remain idempotent after continue-as-new
   instead of being redelivered as fresh queued runs. Messages arriving while
   the run is active (or parked without `mailbox`) follow the normal enqueue
   rules by becoming input-origin runs. One rule, no double delivery.

   `mailbox: false` remains meaningful: it asks for promise/timeout/cancel
   only, leaving inbound messages to queue as future runs. Interactive
   supervisors should usually use `mailbox: true`; transactional or batch
   workflows that want deterministic "resume only when the awaited condition
   is satisfied" can use `mailbox: false`.

Unsolicited sends submit `DeliverMessage` commands; delegated work submits
`RequestRun` commands. The receiver workflow still decides whether a message
is consumed by a parked mailbox or becomes a queued run, which avoids the
sender-side TOCTOU where a target changes state between inspection and
delivery. `RequestRun` remains work-only: it may start from new input or
from already-staged context keys, may carry terminal notify intents, and
always returns or creates a run. `DeliverMessage` is input-only, carries no
run config and no terminal notify intents, and returns only acceptance; if it
falls through to a queued run, that run uses the receiver session's current
default run config at admission time. Context-backed mailbox wake is not part
of P92; if "wake because context changed" becomes real, it should be an
explicit third command rather than another `RequestRun` mode.

The old implementations inferred message-ness from
`RequestRun + Input + empty notify_on_terminal`, then made it explicit with a
`delivery: message | run` field. Both are superseded by the command split:
the command name is now the intent signal, and run records keep only an
origin marker (`requested` or `message`) for observability/idempotency. The
sender does not receive a promise or a stable target run id for `agent_send`
or `session/messages/submit`. The current queue cap is still snapshot-based at
send time and returns `queue_full` under obvious backpressure; pushing that
cap fully into receiver admission is future queue-policy work. Sends to
terminal/closing sessions fail fast instead of enqueueing into the void.
Identical `agent_send` calls within one tool batch are rejected at the tool
layer: the incident's duplicate deliveries were duplicate calls in a single
batch, and an optional `dedupe_key` is no defense against a model that does
not set it. TTL on queued runs plus optional `dedupe_key` and
`coalesce: latest` remain follow-up message queue-policy knobs once queued-run
metadata exists; they are not needed for the P92 safety boundary.

### 6. Continue-as-new portability

Nothing about promises goes into the continue-as-new payload. Semantic
promise state lives in the session log (§1), so the new run's bootstrap
rebuilds it with the rest of `CoreAgentState`, and each source's `rehydrate`
re-arms transport from it: push sources need nothing (notify-intents are
log-backed on the observed side; signals target the CAN-stable workflow id),
poll schedules are recomputed rather than carrying attempt counters forward,
and watchdog/deadline timers are re-armed from promise state. The CAN
payload stays exactly what it is today — the session args — with no growth
pressure against Temporal's payload limits (P73 territory).

Consumed message submissions are treated the same way: the consumption edge
is a run event in the session log, not workflow-local cache. A post-CAN
retry with the same `submission_id` and message input is accepted
idempotently; a retry with the same id for a different command kind or
different input rejects as a duplicate-submission bug.

Long waits must not block CAN — today they do
(`workflow_state_allows_continue_as_new` requires empty wait maps), which
combined with multi-hour job waits is a latent history-limit incident. The
new guard requires quiescence of in-flight tool batches, admissions, and the
outbound notify flush queue — never of pending promises. The flush-queue
gate is what makes delivery reconstruction unnecessary: the queue is
transient by construction (signals to an existing workflow succeed
immediately; a missing target fails terminally, drops the entry, and leaves
its holder to the reaper), draining in seconds where waits pend for days.
Net delivery model: at-least-once, idempotent receive keyed by promise id.
Fired intents on completed runs never re-fire after CAN, because
notifications enqueue on the terminal *transition* — a fresh event append,
the property today's `queue_terminal_notifications_for_entries` already has
— not on state inspection at bootstrap.

A parked session still accrues history from EnvJob poll ticks (a handful of
history events per poll), so CAN-at-idle must fire on the history threshold
*while promises pend* — which the relaxed guard provides.

## Implementation Plan

Greenfield, breaking: each step deletes the surface it replaces in the same
change — no compat aliases, no dual wait paths, no deprecation window.
Each step is independently shippable and testable:

1. **[DONE] Immediate bug fixes:** watchdog timer on `cancelling`
   (force-terminal on expiry, 60s) and `session/close { force: true }`.
   Removes the "stuck forever" class while the rest lands, and recovers the
   orphaned session row from the incident. Force-close is a
   gateway/registry-level path (`force_close_session_in_store`) that
   reconciles the row directly when no workflow exists — the incident's end
   state is precisely a registry row with no workflow behind it, which a
   workflow-signal implementation could never recover. Landed alongside the
   engine `ForceCancelRun`/`ForceCancelled`/`QueuedCancelled` run events, the
   planner fix that completes an already-terminal batch under a `cancelling`
   run (the direct incident deadlock), and a guard so a rejected tool-batch
   resume records a failure instead of wedging the workflow loop.
2. **[DONE] Promise registry + `await`** (push source): promise lifecycle
   events in the engine/session log (`Promise(Created/Resolved/Failed/
   Cancelled)`, `ResolvePromise` command, first-writer-wins), promises minted
   via a `promise_create_effect` tool effect scoped to the calling run,
   notify-intent recorded on the run record at spawn admission. `agent_spawn`
   returns a promise; `agent_wait` is replaced by `await { promises, mode,
   timeout_ms }`. Deleted `agent_wait`, `ActiveWaitRecord`, the
   `subscribe_run`/`unsubscribe_run`/`run_terminal` signals, the
   `run_subscriptions` map, and `fleet_waits.rs`. Push transport is the
   `resolve_promise` signal + a transient `PendingPromiseNotification` flush
   queue. **Implementation refinement:** parked awaits are *derived from*
   `batch.parked` in core state on every loop rather than stored in a
   workflow-resident registry — so they are continue-as-new-portable by
   construction and step 6 is largely already satisfied for the push source.
   CAN-at-idle is gated on transport quiescence (admissions, unresumed
   batches, the flush queue), not on pending promises.
3. **[DONE] Job waits fold in** (poll source). Deleted the P86 model-visible
   job wait tool, `ActiveEnvironmentJobWait`, `job_waits.rs`, and the second
   deadline/resume implementation. EnvJob promises now reconcile into
   workflow promise-source polls backed by `check_promise_source`, with
   `EnvironmentJobChanged` nudging matching polls due. Job-start results
   carry a promise, and CAN-at-idle is no longer blocked by long-lived job
   promises.
4. **[DONE] Cancellation-as-resolution + grace turn + run `parked` state.**
   `BatchDeferred` parks the active run, resumes restore
   it to active, and deferred outcomes may include completed non-await tool
   results so mixed batches work. Cancellation of active or parked runs
   resolves the parked await with `cancelled`, drains active work, and then
   reaches terminal `cancelled`; the watchdog covers both `cancelling` and
   grace. Pending run-scoped promises auto-cancel on terminal run events and
   cascade outward through promise-source cancellation (`CancelRun` for run
   promises, environment job cancel activity for EnvJob, no-op for timers).
   Deleted `agent_cancel` and the P86 model-visible job cancel tool in favor
   of unified `cancel { promises }`. Current grace v1 is non-parking and
   does not execute tool batches; a narrow final-message send allowlist and
   explicit token cap remain future work.
4a. **[DONE] Deployment-level orphan reaper.** The in-band cascade covers
    normal terminal transitions and force-close reconciliation covers a dead
    workflow when a caller explicitly closes the session with `force: true`.
    A worker-hosted store-side reaper now scans every universe's session logs,
    repairs pending promise/source mismatches, re-signals live holder/target
    workflows, and falls back to expected-head-protected direct log repair when
    a workflow is absent. Covered repairs include terminal/gone run promises
    resolving or failing holder promises and terminal owners cancelling their
    owned run and EnvJob sources. The reaper is still a full-log scan and does
    not yet implement a tree-root downward sweep without a promise edge.
5. **[DONE; superseded by 5b] Tracked sends + `agent_reply` + awaitable
   mailbox + mailbox bounds.** The first implementation made
   `agent_send { expect: reply | completion }` create `PromiseSource::Send`
   promises with run-terminal notify intents; reply-tracked sends failed when
   the receiver run terminated without a reply, while completion-tracked
   sends resolved from the receiver run terminal state. `agent_reply`
   resolved the sender's promise directly and duplicate or stale replies were
   idempotent. This surface is no longer current. Step 5b deletes it in favor
   of `agent_request` for completion-tracked work and plain `agent_send` for
   fire-and-forget mailbox delivery. The mailbox and duplicate-send pieces
   survive in the 5b shape.
5b. **[DONE] Greenfield messaging cleanup + detach.** Replaced the first-cut
    tracked-send/reply surface with the cleaner `spawn/request/send` split
    before clients depend on it: `agent_spawn` remains child-session
    delegation with a promise for child run completion; new `agent_request`
    targets an existing linked session, queues a run there, and returns a
    promise for that run's terminal output; `agent_send` is fire-and-forget
    mailbox delivery only. Deleted `agent_reply`, `expect: reply`, reply
    correlation ids, `FailOnTerminal`, `PromiseSource::Send`, and send
    expectations. Collapsed run-backed promise sources to
    `PromiseSource::Run`; cancellation owns the specific target run for both
    spawn and request promises. Added
    `detach { promises }` as a promise-scope promotion tool/effect:
    run-scoped pending promises become session-scoped, are no longer
    auto-cancelled when the creating run completes, still count as active work
    for session close, are cancelled by force-close/session teardown, and
    enqueue a stable follow-up run with the promise result when they resolve
    without a parked await consuming them. This briefly used an explicit
    `delivery: message | run` admission property plus log-backed consumed
    message submissions; step 5c replaces the temporary delivery axis with
    separate commands.
5c. **[DONE] Split run requests from message delivery.** Before P92 closes,
    remove the temporary `delivery: message | run` axis from `RequestRun`.
    The core command vocabulary becomes `RequestRun` for work and
    `DeliverMessage` for mailbox-eligible input. `session/runs/start` always
    requests a run and returns a `RunView`; message delivery uses
    `session/messages/submit` with an acceptance-only response. `DeliverMessage`
    carries input and submission id only: no context source, no run config,
    and no terminal notify intents. If a message is not consumed by a parked
    mailbox and becomes a queued run, the receiver session's current default
    run config is used at admission time. Run records keep `origin:
    requested | message` for idempotency and observability.

    Naming follow-up: P94 records a planned rename from `DeliverMessage` to
    `SubmitMessage`, because the command records message submission/admission;
    actual delivery to a parked run becomes part of engine-native await
    resume.
6. **[DONE] CAN portability** for pending promises. The CAN payload remains
   session args only: promise state, parked awaits, await deadlines, and
   detached/session-scoped work all rebuild from the session log. The workflow
   CAN guard now requires quiescence only for transient transport queues
   (pending admissions, tool-batch resumes, promise notifications, and promise
   cancellations); pending promises and parked awaits do not block CAN. Poll
   sources (`EnvJob`, timers) re-arm from rebuilt core state, while push
   sources (`Run`) remain signal-driven and create no poll records. Unit
   coverage exercises pending source portability, parked
   mailbox/timeout awaits, detached/session-scoped promises, and poll-source
   rehydration.

The fleet safety policy layer (spawn budgets, the tree/active-work index,
observability) follows as **[P93](p93-fleet-safety.md)**.

## Known Residuals And Follow-Ups

These are accepted P92 edges, not missed invariants:

- **Cross-session await cycles can deadlock in principle, but are not
  constructible today.** Two linked non-parent sessions could
  `agent_request` each other, both await with `mailbox: false` and no
  timeout, and remain healthy forever; v1 does not attempt cycle detection.
  Under the current linking model this cannot happen: the only link edges
  are spawn edges and `agent_request` rejects parent targets, so request
  edges point strictly downward in a forest. P93 pins this with a structural
  test; the residual becomes real only if a future feature creates
  non-spawn link edges, and the cycle policy belongs on that feature.
- **Detached follow-up delivery is resolution-time based.** If a
  session-scoped promise resolves while no parked await is consuming it, the
  workflow enqueues the stable detached-promise follow-up run immediately. A
  model that later awaits the same already-terminal promise can still receive
  the result through `await`; this is duplicate visibility, not duplicate
  resolution. If the session is parked with `mailbox: true`, the resolution
  can instead be consumed as a mailbox message even when that await did not
  name the promise; in that case no separate follow-up run is queued.
- **Detached follow-ups require a live workflow path today.** The reaper can
  direct-append a resolution into a workflow-less session log, but the
  follow-up run is queued by the live workflow append path. A bootstrap
  reconciliation for resolved-but-unconsumed session-scoped promises would
  close that gap.
- **Timer/deadline surface is not complete.** The Timer source exists for
  promise transport, but no model-visible `sleep` tool creates one yet, and
  `Promise.deadline_ms` is reserved rather than armed. `await.timeout_ms` is
  the implemented timeout mechanism.

## Test Matrix (Written Once, Against One Mechanism)

Replay/deterministic coverage for the product space that produced the incident:

```
await(mode, timeout?) ×
  { promise resolves, promise fails, deadline fires,
    cancel(run), cancel(session), close(session), force-close,
    inbound mailbox message, grandchild terminal after parent cancel,
    continue-as-new while parked, consumed message submission retried after CAN,
    duplicate/late resolution signal,
    notify delivery retried after transient signal failure,
    source workflow already terminated,
    request target terminal, request target gone,
    detach then creating run completes, detach then session close/force-close,
    detached promise resolution enqueues follow-up run,
    reaper edge repair: source work owned by a terminal holder cancelled,
    reaper upward: promise watching a hard-terminated child failed }
```

Plus property tests: no reachable in-session state without an exit edge
(every non-terminal run transition has a watchdog or deterministic
resolution path); resolution idempotency; scope cascade (run terminal ⇒ no
pending run-scoped promises); log-rebuild equivalence (promise registry
reconstructed from the session log after CAN matches pre-CAN semantic state,
and transport re-arm is a pure recompute). Cross-session await cycles are
the explicit residual above, not covered by the in-session exit-edge claim.

Plus admission tests: `agent_request` requires a reachable linked target,
rejects parent targets with guidance to use `agent_send`, and returns a
promise for the admitted run; duplicate request submissions are idempotent by
stable submission id; `agent_send` never returns a promise and plain sends to
terminal/closing sessions fail fast; duplicate `agent_send` calls in one
batch rejected (§5). Detach admission rejects unknown or terminal promises,
accepts only promises held by the current run, and is
idempotent for already session-scoped promises.

## Key Decisions

Deliberate choices, argued in the referenced sections:

- **Log-backed promises**: semantic lifecycle in the session log; the
  workflow keeps only recomputable-or-transient transport (§1).
- **Push observation, never poll**: parked sessions are truly idle; poll
  only for EnvJob, keeping P86's fixed backoff until a second poll source
  exists (§1).
- **The edge event is the subscription**: P84's subscription machinery is
  deleted; notify-intents ride the spawn/request admission events (§1).
- **`any | all` only**: `n_of_m` deferred — expressible as an `any` loop
  over re-awaitable promises (§2).
- **One way to cancel, ownership-gated**: no tier/ACL on the tool itself;
  session-level cancellation stays an operator/client-API surface (§2, §4).
- **Grace turn**: fixed budget, promise-inert, can never park, skipped on
  force/session-level cancels (§3).
- **Detached promises cancel with the session**: no dead-letter path (§4).
- **Bidirectional store-side orphan reaper**: no per-session polling (§4).
- **Peer workflows, not Temporal child workflows**: detach-after-spawn and
  session first-classness rule out `ParentClosePolicy` (§4).
- **Completion is the response to delegated work**: `agent_spawn` and
  `agent_request` return promises for run terminal output; `agent_send` is
  mailbox-only fire-and-forget. No `agent_reply` in the P92 v1 surface (§5).

## Appendix: Things To Look Into

Non-blocking notes surfaced during implementation; revisit, don't gate on.

- **`ResolvePromise` vs `ResumeToolBatch` — collapse the two-hop resume?**
  `ResolvePromise` (promise lifecycle: pending → terminal, the §3 funnel) and
  `ResumeToolBatch` (P84's deferred-batch primitive: un-park a parked tool
  call and feed its result) are deliberately distinct layers, and both stay —
  a resolution updates promise state, a resume wakes a parked run. In the
  `await` flow they fire in sequence across two loop iterations: iteration N
  admits `ResolvePromise` (promise goes terminal); iteration N+1 sees the
  await satisfied, pushes a `PendingToolBatchResume`, and admits
  `ResumeToolBatch`. The two hops could in principle be collapsed into one
  iteration. It was left as two passes on purpose: the resolution funnel must
  stay uniform across *detached* promises (session-scoped, resolve with **no**
  parked batch — they enqueue a run) and *awaited* promises (run-scoped,
  resume a batch). Coupling resolve-and-resume would special-case the funnel.
  Worth revisiting only if the extra loop hop ever shows up as latency; the
  invariant to preserve is that `ResolvePromise` never assumes a batch to
  resume. **[P94](p94-engine-native-suspension.md) keeps the two-hop split**
  and renames the second hop `ResumeAwait`.
- **The deferred-batch genericity is vestigial → superseded by
  [P94](p94-engine-native-suspension.md).** `await` is the only deferring
  tool and — per this doc's own thesis (new async features become promise
  *sources*, never new suspension primitives) — the only one there will be.
  The opaque `ToolBatchResumeDirective`, the preflight/workflow duplication
  of satisfaction logic, the workflow-side mailbox interception, and the
  engine-trusted resume are all resolved there: typed `AwaitSpec` on the
  run, a log-backed message buffer with an engine-law delivery rule, one
  pure `await_wake` predicate, and claim-validated resumes.
