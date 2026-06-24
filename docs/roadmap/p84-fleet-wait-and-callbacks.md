# P84: Fleet Wait, Subscriptions, And Send

**Status**
- Proposed 2026-06-24.
- Builds on **P83 (Fleet Subagent Control Plane)** — the `agent_*` tool surface,
  the `FleetService`/`FleetToolExecutor`, hosted `SessionTools` routing, and the
  parent->child links. It also builds on the Temporal-backed
  `AgentSessionWorkflow` (run admission via the `submit_admission` signal).
- This doc owns two P83-deferred items: *"Completion and important-update
  notifications back to the parent; rich `wait_agent` semantics."*

## Goal

Give a supervisor agent two missing capabilities:

1. **Wait** — block until a target run reaches a terminal state, or an optional
   (possibly multi-day) timeout elapses — instead of open-coded polling of
   `agent_read`.
2. **Send** — let one agent deliver a message to another (a child reporting to a
   parent, a parent tasking a child, a peer pinging a peer) so the recipient
   wakes and reacts.

Both must slot cleanly into the Temporal runtime, support **hours-to-days** waits
cheaply, and keep wall-clock / side-effect logic out of the deterministic
`engine` core.

## Temporal Primitives We Rely On (Confirmed)

The Temporal Rust SDK (`temporalio-sdk` 0.4.0) exposes, **from workflow code**,
all the durable primitives this design needs — the repo simply has not used them
yet:

- `ctx.timer(TimerOptions { duration, .. })` — a durable timer; survives worker
  restart by replay; can sleep for days/months; resource-light
  (`workflow_context.rs:716`).
- `ctx.external_workflow(workflow_id, run_id).signal(SignalDef, input)` — a
  workflow can **signal another workflow** directly and durably
  (`workflow_context.rs:799`, `:2007`).
- `ctx.child_workflow(...)` — child workflows (`workflow_context.rs:748`).
- `ctx.wait_condition(..)`, `ctx.continue_as_new(..)` — already used today.

So waits and cross-workflow wakeups are **Temporal-native and durable-by-replay**.
No new database table, no reconciler process, and no long-blocking tool activity
is required. (An earlier draft mistakenly assumed timers/cross-signal were
unavailable and proposed either a DB watcher or a bounded poll; both are
unnecessary.)

## Two Ways To Wait — Both Are First-Class

A top-level agent kicks off large tasks that can take hours or days. There are
two ways for it to wait, and we support both:

| | **Idle-and-be-woken** (Mode I) | **Explicit `agent_wait`** (Mode W) |
|---|---|---|
| Parent state | run goes **terminal**; workflow parks idle | run **parked mid-turn** on a subscription |
| Waiting "on" | nothing specific — any admission resumes it | a named target run reaching terminal |
| Who wakes it | the child's `agent_send` (or any business event) | the target workflow, via a `RunSubscription` |
| Child must cooperate? | **yes** — child must be told to report back | **no** — the target fires on its own terminal |
| Cost while waiting | free (idle workflow) | free (idle workflow + at most one durable timer) |
| Result lands as | a **new run** on the parent | resolution of the **same parked turn** |
| Best for | long fan-out; parent free to do other work | "I can't continue until this finishes" |

The enabling runtime fact (already true today): **an idle session is just a
workflow parked on `wait_condition(pending_admissions)`
(`crates/temporal-workflow/src/workflow.rs:69`). It holds no worker, lives
indefinitely across `continue_as_new`, and any `submit_admission` wakes it —
hours or days later.** Mode I rides this directly; Mode W adds a durable
subscription and (optionally) one durable timer.

- **Mode I (default for big async tasks):** spawn the child with a `report_back`
  directive, let the parent run go idle, and be woken by the child's `agent_send`.
  Cheaper and more flexible — the parent can do other work meanwhile, and the
  callback can carry a result or a mid-task question.
- **Mode W (for tight dependencies):** call `agent_wait` to park the current turn
  until the named target run is terminal. The result is delivered back *into the
  same turn*.

## Part A: Wait

### Tool Surface

One new model-visible Fleet tool. `agent_wait` is the **structured-concurrency
join** over runs: a run handle is `{ target_session_id, run_id }` (the
`JoinHandle`), and `agent_wait` is `join` over one or many of them. This is
deliberately the Tokio / async-task model — `agent_spawn` / `agent_send` are
`spawn` (they hand back a `run_id`), and `agent_wait` is `join` / `join_all` /
`select`.

```text
agent_wait   join one or more in-flight runs: block until they reach terminal
             (all of them, or the first), or an optional timeout elapses
```

Input shape:

```text
waits        [ { target_session_id, run_id }, ... ]   one or more run handles
mode         all | any                                (default: all)
timeout_ms?  int                                      (default: none)
```

- `mode = all` (default) — `join_all`: resolve when **every** named run is
  terminal.
- `mode = any` — `select`: resolve when the **first** named run is terminal
  (the result reports which).
- Each run resolves on `Completed`, `Failed`, or `Cancelled`
  (`crates/engine/src/core/components/run.rs:88`), observed from the target itself
  (see Runtime Shape) — so it works even if a child never calls `agent_send` or
  crashes mid-task and is retried.
- `timeout_ms` is **optional** with **no default**: absent, the wait is indefinite
  (no timer armed) and costs an idle workflow plus the durable subscriptions. When
  set, it is backed by a single `ctx.timer` keyed to an absolute deadline (see
  "Timer"). On timeout the tool resolves with `outcome = "timeout"` carrying
  whatever partial results have arrived.

`waits` validation (strict schema, rejected before deferring):

- **`minItems = 1`** — an empty join is meaningless.
- **No duplicate handles** — duplicate `{target_session_id, run_id}` entries are
  rejected (they would create two subscriptions for one run and double-count
  arrivals).
- **`maxItems` fan-in cap** (e.g. 32) — bounds the number of subscriptions a
  single wait installs and the workflow-history cost of fanning out/​collecting
  them. A supervisor needing more joins in batches.

Each handle requires an explicit `run_id`. A supervisor already holds it
(`agent_spawn` returns `child_run_id`; `agent_send` returns `run_id`). Requiring
it avoids the race a "current active run" default would introduce (a run completes
and the target immediately starts another, so a default latches the wrong one).

Mid-run progress (`until = activity`) is **deferred** — v1 joins on terminal only;
a supervisor wanting intermittent progress polls `agent_read` or uses Mode I so
the child pushes updates via `agent_send`.

Output shape:

```text
outcome      terminal | timeout | error      overall result of the join
results      [ { target_session_id, run_id, status, run?, error? }, ... ]
             per-handle results (status: terminal | pending | error)
```

- `mode = all` + every handle terminal → `outcome = terminal`.
- `mode = any` + at least one terminal → `outcome = terminal`; the resolved
  handle's `results` entry is `terminal`, others may be `pending`.
- `outcome = timeout` → the timer won first; `results` carries partial state
  (some `terminal`, some `pending`).
- `outcome = error` → the join could not be established/observed for at least one
  handle in a way that prevents a meaningful result; per-handle `error` says which.
- `agent_wait` never raises a tool *failure* for these states — the supervisor
  branches on `outcome` / per-handle `status`. A handle that is `terminal` with a
  `Failed` run status is a normal result, **not** `error` (which is reserved for
  "could not establish/observe the wait").

### Already-Finished Runs Resolve Immediately (Join A Completed Handle)

A child can be **fast enough to finish before `agent_wait` is even called** — the
async equivalent of joining an already-completed `JoinHandle`. This must return
immediately, never park forever. So `agent_wait` **preflights** every handle
synchronously in the tool activity (reading each target's run status via the
existing projection/query) **before deferring**:

- handles already **terminal** at preflight → recorded as done immediately, no
  subscription created;
- **unknown / never-existed** run, or **errored / unreachable** target → that
  handle's result is `error`, no subscription;
- a **closing / closed** target with the run not terminal → `terminal` with its
  last known status if any, else `error`.

If, after preflight, the join is **already satisfied** (`all` and every handle
terminal/error; or `any` and at least one terminal), `agent_wait` returns
**inline** and never defers. Otherwise it defers, subscribing only to the handles
still running. The narrow race — a run going terminal *between* preflight and the
target processing `subscribe_run` — is closed by the subscribe handler firing
immediately for an already-terminal run (below).

### Runtime Shape: `RunSubscription` (Generic, Target-Signals-Subscriber)

The target session workflow is the thing that **knows best** when its run is
terminal — so it is the thing that notifies waiters. A waiter registers a durable
**`RunSubscription`** on the target; when a run reaches terminal, the target
signals each matching subscriber. No separate watcher process, no polling, no DB.

This is built as a **general primitive**, not an `agent_wait`-specific one, so
other workflows (future supervisors, goal-mode runners, external orchestrators)
can subscribe to a session's run lifecycle the same way.

`RunSubscription` record (held in the target session workflow state, durable by
replay). v1 is **`once`-only**:

```text
subscription_id          deterministic id (subscriber-derived)
subscriber_workflow_id   any workflow id (not necessarily a session)
correlation_token        opaque, subscriber-chosen (echoed back; lets the
                         subscriber match the notification to a specific wait/handle)
run_id                   the specific run to fire on (required in v1)
```

The callback uses a **single fixed, typed signal** (`run_terminal`) on the
subscriber, not a per-subscription `signal_name`. The SDK's
`external_workflow().signal(SignalDef, input)` wants a typed `SignalDefinition`;
a free-string signal name would need an untyped signal mechanism we do not have a
real second user for yet. So v1 fixes the callback signal and keeps the *target*
generic (any `subscriber_workflow_id`, opaque `correlation_token`). A
parameterized signal name can come with the first non-session subscriber that
needs a different one (Deferred).

- v1 semantics are **fire once on that run's terminal, then remove the
  subscription** — exactly what a join handle needs.
- A future *persistent* mode (fire on every run terminal, never remove) is
  **deferred and not stored or tested in v1.** It would have to survive
  `continue_as_new` (see note below) and outlive runs — real extra machinery with
  no v1 user. Adding a `mode` field later is additive; v1 ships neither.

**Subscribe-after-terminal race.** The `subscribe_run` handler checks the run *at
registration time*: if the named run is **already terminal**, fire the
`run_terminal` signal immediately and do **not** store the subscription. Only a
still-running target stores it. This closes the window where the run goes terminal
between the waiter's preflight and the target processing `subscribe_run`.

On a run reaching terminal, the target workflow iterates its subscriptions and,
for each match, calls
`ctx.external_workflow(subscriber_workflow_id, None).signal(run_terminal, { correlation_token, run_id, status, output_ref })`,
then removes the (once) subscription. Because this is workflow code it is durable:
a restart replays the terminal transition and re-emits the signal idempotently. A
**fan-out signal failure must not fail the target run** — the target attempts the
signal best-effort and proceeds; a subscriber that never hears back relies on its
own timeout (below).

New signals on the session workflow:

```text
subscribe_run(RunSubscription)        register; fire-immediately if already terminal
unsubscribe_run(subscription_id)      remove it (waiter timed out / gave up)
```

**Signal-failure behavior** (all three made explicit so implementation does not
guess):

- **Parent `subscribe_run` send fails** (target unreachable / not found): the
  waiter cannot establish that handle's wait, so it resolves that handle as
  `error` (and, for `mode = all`, that makes the overall `outcome = error`). It
  does not park hoping the target appears.
- **Target `run_terminal` fan-out fails:** best-effort, **must not fail the target
  run** — the target logs and proceeds. Crucially, the SDK's
  `external_workflow().signal` to a *live* subscriber is durable (Temporal retries
  delivery), so the only way a fan-out is permanently lost is if the **subscriber
  workflow is gone or cancelled** — in which case the wait is moot anyway (no one
  is parked). So a fan-out failure does **not** silently strand a healthy
  indefinite waiter; this does **not** require the waiter to set a timeout. The one
  residual case — a genuinely lost notification to a still-parked indefinite waiter
  — is not auto-recovered; the supervisor's recourse is `agent_cancel` on its own
  wait or re-issuing `agent_wait` (which re-preflights and sees the now-terminal
  run immediately). (Earlier wording said "the waiter falls back on its timeout,"
  which wrongly implied a timeout is required; indefinite waits remain
  first-class.)
- **Timeout `unsubscribe_run` fails:** best-effort and ignored — a stale
  subscription on the target is harmless (it is `once`; it fires or is GC'd when
  that run is long terminal), and the waiter has already resolved.

**Continue-as-new safety.** A live subscription is workflow state that is **not**
in `AgentSessionArgs`, so the current `continue_as_new(&args)` at idle
(`workflow.rs:78`) would silently drop it. v1 must prevent that: extend
`can_continue_as_new_at_idle` (`workflow.rs:724`) to be false while any
`RunSubscription` is registered (and, on the waiter side, while any active wait is
parked). Because `once` subscriptions are short-lived (one run to terminal), this
only briefly defers history compaction — acceptable. This is also a second reason
`persistent` is deferred: an indefinitely-living subscription would either block
continue-as-new forever or require threading subscription state through
continue-as-new.

### Engine: A Generic Deferred-Tool-Batch Primitive (Not Fleet-Specific)

`agent_wait` is a tool whose result is not produced inline by the tool activity —
it is produced later, when the joined runs reach terminal. The engine gains a
**generic deferred capability**, keyed on tool-call metadata; `agent_wait` is one
user and nothing in the engine knows about Fleet.

This must fit the engine's existing batch contract precisely
(`crates/engine/src/core/`):

- A run has **one `active_tool_batch_id` at a time** (`run.rs:70`), and a batch
  completes **all-or-nothing**: `validate_tool_batch_result` (`drive.rs:579`)
  requires **every** call in the batch to have a *terminal* `ToolCallStatus`
  (`Succeeded | Failed | Cancelled`; `tooling.rs:788`) before the turn proceeds.
  There is no `Deferred` call status, and adding one would mean teaching
  `validate` and the turn planner about a non-terminal-but-ok call.

So v1 models deferral at the **batch** level, not by inventing a `Deferred` call
status — which keeps the per-call terminal contract intact:

- **`agent_wait` must be the only call in its batch.** When the model emits
  `agent_wait` alongside other tool calls, the runtime resolves the `agent_wait`
  call as `Failed` with a model-visible message ("`agent_wait` must be the only
  call in its batch"), and the batch completes normally; the model re-issues the
  wait alone. (Waiting on *many* runs is the `waits: [...]` argument, not multiple
  batched calls — so this restriction does not block the common fan-out join.)
- A **single-call `agent_wait` batch** may return
  `ToolBatchOutcome::Deferred { batch_id, resume_directive }` instead of a result.
  The engine records the **batch as parked**: `active_tool_batch_id` stays set, no
  `CallCompleted` is emitted yet, and the turn cannot advance — reusing the
  existing "a batch is in flight" state rather than a new call status.
  - **Parked invariant (no re-emit).** A parked batch must not be re-issued. Today
    the planner skips planning while `active_tool_batch_id.is_some()` (`turn.rs`),
    but it also must not re-emit the *original* `InvokeTools` for this batch. Add an
    explicit **`parked` flag (carrying the `resume_directive`) on `ActiveToolBatch`**
    (`run.rs`): while set, `drive.next_action` neither re-emits the invocation nor
    treats the batch as completable — it can only be cleared by `ResumeToolBatch`.
    This is the durable marker that distinguishes "batch parked, awaiting resume"
    from "batch in flight in an activity."
  - `resume_directive` is an **opaque, runtime-owned blob** (`api_kind` +
    versioned body, like P83's `ProviderParams`). The engine stores/replays it but
    **never interprets it**. The Fleet executor encodes the wait specifics
    (`waits[]`, `mode`, `timeout_ms`); the parent workflow's Fleet handler decodes
    it. So the workflow can act on a deferral the *tool activity* decoded, with no
    Fleet vocabulary in the core.
- A new command `ResumeToolBatch { batch_id, result }` (delivered via the existing
  `submit_admission` path) supplies the parked batch's single-call terminal result
  and lets the turn proceed through the **normal** `tool_batch_result_proposals`
  path (`drive.rs:544`) — the eventual result *is* terminal, so `validate` is
  satisfied. This is the deferred sibling of `resume_tool_batch` (`drive.rs:162`).
- Durable reducer facts record "tool batch deferred" (with the opaque directive)
  and "tool batch resumed" so the parked turn replays.

The engine stays deterministic and wall-clock-free: it knows only *"this batch is
parked; resume it with a result."* Joins, timers, and subscriptions live in the
runtime/workflow.

### Flow

```text
parent model calls agent_wait { waits: [h1..hN], mode, timeout_ms? }
  -> tool activity PREFLIGHTS each handle (reads target run status):
       * if the join is already satisfied -> return an INLINE batch result
         (no deferral); else -> ToolBatchOutcome::Deferred { batch_id,
         resume_directive }, directive encoding the still-running handles, the
         per-handle preflight errors, mode, and timeout_ms (NOT an absolute
         deadline — the activity has no deterministic workflow clock)
  -> parent workflow, on the deferred batch, decodes resume_directive and:
       * FIRST records the durable ACTIVE WAIT (before any subscribe):
           { batch_id, mode, handles[], arrived[], errored[], deadline_ms? }
         where deadline_ms = workflow_time + timeout_ms (the WORKFLOW computes the
         absolute deadline; see Timer). Recording first means a synchronous
         run_terminal for an already-terminal handle is matched, not dropped as
         unknown.
       * THEN, for each still-running handle h: subscribe_run on h.target
           ctx.external_workflow(h.target).signal(subscribe_run, {
             subscriber_workflow_id = parent_id,
             correlation_token = (batch_id, h), run_id = h.run_id })
         a subscribe send-failure marks h errored (see error rules).
  -> resolutions accumulate (see loop), applying the mode-specific rules below:
       (a) run_terminal signal { correlation_token=(batch_id,h), status,.. }
           -> mark h arrived
       (b) subscribe failure / handle error -> mark h errored
       (c) deadline timer fires -> satisfied as timeout (partial results)
       After each, re-evaluate satisfaction by mode (see "Error propagation").
  -> on satisfied: unsubscribe_run any still-open handles (best-effort), build
     the agent_wait result, emit ResumeToolBatch { batch_id, result }; the parked
     batch completes and the turn continues.
```

#### Error propagation is mode-specific

A handle can fail to even establish (unknown run, unreachable target, failed
`subscribe_run` send) — distinct from a run completing with a `Failed` status.
Satisfaction is re-evaluated after every arrival/error/timeout:

- **`mode = all`**: any handle that **errors** resolves the whole join as
  `outcome = error` **immediately** — `all` cannot succeed if one leg is
  unobservable, so there is no point waiting on the rest. (A handle that *arrives*
  terminal-with-`Failed`-status is **not** an error; it is a normal arrival, and
  `all` still needs the others.)
- **`mode = any`**: a handle error is **not** fatal while other handles are still
  viable (running or not-yet-errored) — `any` only needs one to succeed. The join
  resolves `outcome = error` only when **every** handle has errored (no viable
  handle remains). The first handle to *arrive* terminal resolves `outcome =
  terminal` and wins regardless of other handles' errors.
- **timeout** (either mode) resolves `outcome = timeout` with whatever partial
  `results` exist (some arrived, some errored, some still pending), independent of
  the above.

#### Workflow loop mechanics (the select, made explicit)

The current loop awaits only `pending_admissions`
(`crates/temporal-workflow/src/workflow.rs:69`). A timer firing is **not** an
admission, so the loop must be widened to observe both. The workflow keeps a set
of **active waits** and a **pending-resolution queue**:

- The top of the loop does a **select over: (1) the `wait_condition` for new
  `pending_admissions`, and (2) the nearest active wait's deadline timer.** Both
  are `CancellableFuture`s, so they compose in a `select!`.
- A `run_terminal` **signal** is delivered like any other signal: its handler
  records `(batch_id, handle)` as arrived in the active wait, and marks the loop
  wakeable. When that wait becomes satisfied (`any` → first arrival; `all` → all
  arrived), it pushes a `(batch_id, resolution)` onto the pending-resolution queue.
- A **deadline timer** winning the select pushes a `(batch_id, timeout)`
  resolution with whatever partial results have arrived.
- After the select returns, the loop drains the pending-resolution queue into
  `ResumeToolBatch` admissions (unsubscribing any still-open handles best-effort),
  then processes admissions as today.

So timer and signal funnel into **one queue**, the loop awaits **admissions OR the
deadline**, and there is exactly one place that converts a resolution into a
`ResumeToolBatch`.

##### Timer: workflow-computed absolute deadline, not stored as a future

`deadline_ms` is computed **by the workflow**, not the tool activity: the activity
returns only `timeout_ms` in the directive (it has no deterministic workflow
clock), and the workflow sets `deadline_ms = workflow_time() + timeout_ms`
(`workflow.rs:688`'s `workflow_time_ms`) when it records the active wait. This
keeps the absolute deadline on the deterministic side.

The active-wait record stores **durable metadata only** — `{ batch_id, mode,
handles[], arrived[], errored[], deadline_ms? }` — and **not** a `ctx.timer`
future (a future is not serializable durable state). On each loop pass, if a wait
has a `deadline_ms`, the select arms `ctx.timer(deadline_ms - now)` for the
**nearest** deadline. Because the timer is keyed to an *absolute* deadline:

- a worker restart re-derives the remaining duration from `deadline_ms` on replay
  — the timeout does not reset;
- an **unrelated admission** (a signal, a new run) that wakes the loop does **not**
  re-arm a fresh full-duration `timeout_ms` — it re-arms toward the same
  `deadline_ms`, so the deadline never drifts outward.

`batch_id`/handle-keyed idempotency: a duplicate `run_terminal` (target replayed)
or a deadline firing after the wait was already satisfied is dropped, because the
active wait is gone (or the handle is already marked arrived).

### Crash / Resume Safety (No DB, No Reconciler)

Every piece is durable by Temporal replay:

- the **parked tool batch** (and its opaque resume directive) is a reducer fact in
  the parent session log;
- the **active wait** `{ batch_id, mode, handles[], arrived[], errored[],
  deadline_ms? }` is parent workflow state, replayed on restart;
- the **`RunSubscription`** is workflow state on the target, replayed on restart;
- the target's terminal-transition → `run_terminal` signal happens **in workflow
  code**, so a restart replays it and re-signals idempotently;
- the **timer** is a Temporal timer derived from the absolute `deadline_ms`, durable
  by construction and non-drifting.

This is the fix for the watcher-durability gap an earlier draft had ("an activity
registers a watcher" — a completed activity does not rerun on replay): the watch
is not an activity, it is the target workflow's own durable terminal handler.

## Part B: Sending Between Agents (`agent_send`)

### One Mechanism, So One Tool — Because The Topology Is A Graph

`agent_task` (parent -> child) and a child -> parent callback are **the same
runtime primitive**: admit a `RequestRun` on another session via
`signal_submit_admission` (`crates/temporal-server/src/gateway/service/workflow.rs:28`).
There is no second transport.

Two direction-locked tools (`agent_task` down, `agent_notify` up) bake a **tree**
into the surface: every edge is parent->child or child->parent. But real agent
structures are a **general graph**. A spawns B and C, then tells B to message C (a
sibling edge), or arranges for C to report to A. With direction-locked tools, "B
messages C" is not expressible and "C messages A" only works when A is literally
C's parent.

So P84 collapses inter-agent delivery into **one tool whose recipient is just a
session you are allowed to reach** — the edge is data, not vocabulary:

```text
agent_send   deliver a message to another session, admitting a run on it
```

This subsumes and **replaces P83's `agent_task`**, and absorbs the child->parent
callback. "Task a child", "report to a parent", and "ping a sibling" are now the
same verb with a different `to` and `kind`.

Input shape:

```text
to        tagged enum (see below)                addressing
text                                             human/agent-readable message
input?    structured input items                 run input (defaults to text)
payload?  opaque JSON                             structured data for the receiver
kind?     task | progress | question | result    sender-chosen framing (default: task)
```

Output shape:

```text
target_session_id      resolved recipient session id
run_id?                the run admitted on the recipient (when delivered)
status                 delivered | not_reachable
```

### Addressing (`to`) Is A Tagged Enum Over `session_id`

A Fleet agent *is* a session (`agent_id == session_id`, P83 "Identity Model"),
and there is no separate agent identity in v1. So the Fleet surface uses
**`session_id` everywhere** — no second `agent_id`-flavored name for the same
value. (P83's shipped DTOs drifted into a mix of `target_agent_id` / `agent_id` /
`from_agent_id`; that is being corrected to `session_id` across all agent tools —
see "Naming Cleanup". P84 is written in the target convention.)

`to` is tagged (mirroring P83's `source` enum) so a real id named `parent` is
never ambiguous:

```json
{ "kind": "parent" }
{ "kind": "session", "target_session_id": "..." }
```

- `session(target_session_id)`: any session the caller may reach (see Permission).
  This is the general case — child, parent, or peer, all the same shape.
- `parent`: **convenience sugar** that resolves the caller's parent via the P83
  parent->child link, so a child need not carry its parent's id. It is *not* a
  privileged direction — just a named lookup of one edge. A root session sending
  `to: parent` gets `status = not_reachable`, not an error.

### Permission Is "Is There An Edge?", Not "Which Way Does It Point?"

`agent_send` is permitted to any session the caller has a `session_link` to —
**any direction, no up/down asymmetry**. The topology lives entirely in the link
graph; the tool just rides the edges that exist.

- send is allowed iff a `session_link` connects caller and `target_session_id`
  (the spawn parent->child edge, or any other link that exists);
- `kind` (`task | progress | question | result`) is **pure sender-chosen
  framing** — the runtime never forces or rewrites it based on direction. A peer
  may `task` a peer; a child may send a `result` up. The receiver decides how to
  react.

There is no built-in "subordinate must not command supervisor" in v1, because
that is a *policy over edges*, not a property of the send tool. Directional edge
rights and per-relationship grants are the **P83-deferred capability-policy
work**; v1 trusts a present edge, exactly as P83 `agent_read` / `agent_cancel`
already trust a named target id.

### v1 Wires Only Spawn Edges (Graph Is Expressible, Not Yet Fully Reachable)

The send *semantics* are graph-general, but v1 only auto-creates **one** kind of
edge: the parent->child link on `agent_spawn`. So a caller can send to its spawned
children and (via `to: parent` / `report_back`) to its parent, but an A-arranged
**B->C sibling** edge is not yet reachable: nothing in v1 installs a link from B
to C. This is a *reachability* gap, not a design compromise — the moment a
link-installation action exists, arbitrary topologies light up with no change to
`agent_send`. Listed in Deferred.

### Framing (`kind`) Tells The Receiver How To Read It

The admitted run carries a `kind` (`task | progress | question | result`) and an
origin marking it a Fleet send. The receiver reads `task` as new work and
`progress` / `question` / `result` as an incoming report. This is the *only*
semantic difference between "tasking" and "notifying" — a sender-chosen field,
not a separate tool and not a direction the runtime infers.

### Send Metadata Storage: A Text Envelope (No API Change)

`RunStartParams.input` carries only `text` / `textRef` / `media`; it has no slot
for `kind` / `payload`. v1 encodes them into a **structured envelope inside a
single prepended text item**, which the receiver parses — so `agent_send` ships on
the existing run-admission contract with **no `api` wire-type change**:

```text
input = [
  text-item: envelope { fleet_send: { from_session_id, kind, payload? },
                        text: "<the message>" },   // ALWAYS prepended, exactly one
  ...any items/media from the caller's `input?`, unchanged, in order
]
```

Composition rule, stated to avoid drift: the envelope is **always exactly one
text item, prepended to the front of the run input.** If the `agent_send` caller
also supplied a structured `input?` (multiple items, media), those follow the
envelope item verbatim; the envelope never merges into or wraps them. A receiver
parses the first item as the envelope and treats the remainder as ordinary input.
`text` is the convenience case (no `input?`): it becomes the envelope's `text`
field, and there are no trailing items.

A first-class structured input item (`InputItem::AgentMessage { from, kind,
payload }`) is the cleaner long-term shape but is deferred — it would touch `api`
wire types, contract artifacts, and projection.

### Resolution And Idempotency

The Fleet service resolves `to` to a recipient session id (`parent` via the link,
`session` by id), checks a `session_link` edge connects caller and recipient
(else `status = not_reachable`), then admits a `RequestRun` on it via the normal
hosted run path with the envelope input and origin metadata. The recipient wakes
as an ordinary new run.

The recipient run's `submission_id` is derived from the sender
session/run/turn/tool-call identity (P83's scheme), so a tool-activity retry
re-admits the *same* recipient run instead of delivering twice.

### Why Not Reuse The Messaging Outbox

The messaging outbox (P71) is **outbound-only**: rows key on a `session_id` and
are drained by *external channel bridges*; nothing routes an outbox row back into
another session's workflow, and there is no inbound/inbox store
(`crates/messaging/src/lib.rs:1`). Repurposing it for agent->agent delivery would
require a brand-new inbound consumer that admits recipient runs — i.e. exactly the
run-admission call `agent_send` already makes, plus an extra queue hop and a new
store. The outbox stays the external-channel spine; internal agent->agent
signalling uses admission directly.

### The Child Must Be *Told* To Report Back (`report_back`)

Permission to message the parent already exists by construction: the spawn creates
the parent->child edge, and `agent_send` may ride any edge. So `report_back` is
not about *permission* — it is about *expectation*. A callback only happens if the
child knows it is expected to make one, and when.

`report_back` directive (optional on `agent_spawn`, and on an `agent_send` that
kicks off a child's work):

```text
report_back?  {
  on            terminal | milestones | question   (what warrants a report)
  instructions? string                             (extra guidance for the child)
}
```

When present, the Fleet service **injects a report-back instruction** into the
child's input — a short note like *"You are a subagent of <parent>. `agent_send
{ to: parent }` when you finish (and if you hit a blocking question), with a
concise result."* parameterized by `on` and `instructions`.

`report_back` is **instruction-injection only — it does not patch the child's tool
config.** P83 deliberately inherits the source's config and avoids config
patching, and `tools.fleet` today gates the whole Fleet surface at once. So the
child has `agent_send` iff its inherited config already enables `tools.fleet`; if
it does not, `report_back` simply has no tool to lean on. A narrower per-capability
Fleet gate (e.g. a send-only sub-capability) is part of the P83-deferred policy
work and is not introduced here.

`report_back` is the *declared-cooperation* path. It is complementary to
`agent_wait`, the *observed* path (the target fires on its own terminal, no child
cooperation needed). A robust supervisor often uses both: `report_back` so the
child can volunteer progress and questions, plus a terminal wait as the backstop
in case the child finishes without reporting.

### Send + Wait Compose

A child `agent_send { to: parent }` lands as a new parent run — so an **idle**
parent (Mode I) wakes and reacts. A parent that instead used `agent_wait`
(Mode W) is resolved by the target's terminal, independently of whether the child
also sent anything. The two are complementary, through the same admission/run
spine, with no special wiring.

## agent_send Is Async; Synchronous Is Composed

`agent_send` admits a run and returns the handle immediately. It does **not** grow
a blocking `wait` flag. "Send work and get the result back" is composed:

```text
agent_send { to: { kind: session, target_session_id: id }, text } -> { run_id }
agent_wait { waits: [ { target_session_id: id, run_id } ] } -> { outcome, results }
```

Fan-out then join is the same shape with more handles:

```text
r1 = agent_send { to: session(B), text }   -> run_id
r2 = agent_send { to: session(C), text }   -> run_id
agent_wait { waits: [ {B, r1}, {C, r2} ], mode: all }   // join_all
```

This keeps blocking semantics in exactly one place (`agent_wait` / the deferred
tool batch) — one parked-turn code path to reason about, test, and make
crash-safe.

## Are We Overbuilding? (`wait` vs `send` vs future `subscribe`)

The clarifying frame is **structured concurrency over runs** — this is the agent
analogue of threads / async tasks, and the pieces map onto a well-understood model
rather than being novel invention:

| async tasks | Fleet |
|---|---|
| `spawn(task)` → `JoinHandle` | `agent_spawn` / `agent_send` → `run_id` |
| `handle.await` / `join` | `agent_wait { waits: [h] }` |
| `join_all` / `select!` | `agent_wait { mode: all | any }` |
| join a completed handle | `agent_wait` preflight resolves inline |
| a channel `send` between tasks | `agent_send` |

So the surface is "spawn, join, message" — the same trio every task runtime has.

Three mechanisms could still look redundant. They are not — they differ on
push-vs-pull and on duration, and they share machinery rather than duplicating it:

| | direction | initiated by | fires | carries |
|---|---|---|---|---|
| `agent_send` | push | sender | when the sender decides | a message / payload |
| `agent_wait` | pull (block) | waiter | target run terminal | nothing — just unblocks |
| `agent_subscribe` (future) | pull (standing) | subscriber | every terminal | a terminal notification |

Key point: **`agent_wait` and a future `agent_subscribe` are the same
`RunSubscription` primitive at different durations** — `agent_wait` is "subscribe,
block, fire-once, unsubscribe"; a future tool would be "subscribe persistently, do
not block, get woken each time." We build one primitive now (fire-once) and expose
the standing-subscription variant later — additively, not as a second mechanism.
`agent_send` is the distinct **push** path (a child volunteering content) vs. the
**pull** lifecycle notifications. They are complementary; a child that finishes and
wants to hand back a *result* uses `agent_send`, while a parent that just needs to
know *when* uses the subscription. So: not overbuilt, provided we ship only
`agent_wait` (fire-once) now and keep the `RunSubscription` surface minimal.

## Tool Surface After P84

```text
agent_spawn    (P83)            create a child; optional report_back
agent_list     (P83)
agent_read     (P83)
agent_cancel   (P83)
agent_send     (P84, replaces agent_task) deliver to parent | session(target_session_id)
agent_wait     (P84)            block until a target run is terminal (or timeout)
```

Six tools (P83's `agent_task` folds into `agent_send`), all still gated behind the
per-session `tools.fleet` flag (P83 "Policy"). No generic session/run API is
exposed.

## Naming Cleanup: `session_id` Everywhere (Applies To All Agent Tools)

A Fleet agent *is* a session — `agent_id == session_id`, with no separate agent
identity in v1 (P83 "Identity Model"). The shipped P83 DTOs nonetheless drifted
into a second, `agent_id`-flavored name for the same value on the
address-an-existing-agent surface. P84 standardizes the whole Fleet surface on
**`session_id`** and corrects the existing P83 tools to match.

### Rule

- The id of any agent/session is always **`session_id`** (and `target_session_id`
  when it is the thing being addressed). Never `agent_id`, `target_agent_id`,
  `source_agent_id`, `from_agent_id`, `to_agent_id`.
- Creation-side names already conform (`child_session_id`, `source { session_id }`).
- If/when a real distinct agent identity ever exists (it does not in v1), it gets
  its own field then — not by overloading the session id with an alias now.

### Concrete renames in shipped P83 (`crates/tools/src/fleet/mod.rs`)

| Current field | Rename to | Where |
|---|---|---|
| `target_agent_id` | `target_session_id` | `agent_task`/`agent_read`/`agent_list`/`agent_cancel` args + outputs + schemas (`:118`, `:125`, `:136`, `:154`, `:194`, `:237`, `:262`, and the schema/`required` strings) |
| `agent_id` | `session_id` | `AgentListItem` (`:222`), `AgentReadOutput` (`:246`) |
| `source_agent_id` | `source_session_id` | `AgentLineageView` (`:203`) — matches P83 lineage prose, which already says `source_session_id` |
| `from_agent_id` / `to_agent_id` | `from_session_id` / `to_session_id` | `AgentLinkView` (`:211`, `:212`) |

This is a **wire-contract change**: it touches the strict JSON schemas, the
committed contract artifacts under `interop/contract/`, the hosted Fleet executor
in `crates/temporal-server/src/fleet.rs`, and the deterministic Fleet tests that
assert field names. Regenerate artifacts via `cargo run -p api --bin
export-schema` and update P83's tool-contract prose. Bundled into P84's S1 so the
rename and the new tools land in one contract revision.

## Implementation Map

- `crates/tools/src/fleet/`: add the `agent_send` DTO (tagged `to`, `kind`,
  `text`/`input`/`payload`) replacing `agent_task`, and the `agent_wait` DTO
  (`waits: [{target_session_id, run_id}]`, `mode: all|any`, optional `timeout_ms`);
  add the optional `report_back` directive to `agent_spawn`. Strict schemas; tool
  names; `ToolSpecBundle` entries. No Postgres/Temporal deps here.
- `crates/engine/src/core/`:
  - Add the **generic deferred-tool-batch** primitive that fits the existing batch
    contract: a single-call batch may return
    `ToolBatchOutcome::Deferred { batch_id, resume_directive }` (opaque runtime blob
    the engine stores/replays but never interprets); add a **`parked` flag (holding
    the directive) on `ActiveToolBatch`** (`run.rs`) so `drive.next_action` neither
    re-emits the `InvokeTools` invocation nor treats the batch as completable while
    parked — no new `ToolCallStatus`; a new `ResumeToolBatch { batch_id, result }`
    command clears `parked` and resolves through the normal
    `tool_batch_result_proposals` path (`drive.rs:544`), whose terminal result
    satisfies `validate_tool_batch_result` (`drive.rs:579`); durable
    deferred/resumed reducer facts. No Fleet/wait logic in the engine.
- `crates/temporal-workflow/src/workflow.rs`:
  - **Widen the main loop** (`:69`) from `wait_condition(pending_admissions)` to a
    **select over `pending_admissions` and the nearest active-wait deadline timer**,
    draining a pending-resolution queue into `ResumeToolBatch` admissions each pass.
  - **`RunSubscription`** support (`once`-only, fixed typed `run_terminal` signal):
    `subscribe_run` / `unsubscribe_run` signals; store subscriptions in workflow
    state; the `subscribe_run` handler **fires immediately and does not store** if
    the run is already terminal; on a run reaching terminal, iterate matching
    subscriptions, `ctx.external_workflow(subscriber).signal(run_terminal, ..)`
    best-effort, and remove them. Generic — not tied to Fleet.
  - **`agent_wait` parking**: on the deferred batch, decode the directive,
    **record the durable active wait first** `{ batch_id, mode, handles[],
    arrived[], errored[], deadline_ms? }` (deadline computed by the workflow =
    `workflow_time + timeout_ms`; **no timer future stored**), **then**
    `subscribe_run` on each still-running handle (record-before-subscribe so a
    synchronous `run_terminal` is matched, not dropped); apply the mode-specific
    error rules; resolve via `ResumeToolBatch` on satisfied (`all`/`any`/timeout);
    unsubscribe open handles best-effort.
  - **Continue-as-new guard**: extend `can_continue_as_new_at_idle` (`:724`) to be
    false while any `RunSubscription` or active wait exists.
- `crates/temporal-server/src/`:
  - `fleet.rs`: `agent_send` resolution (`parent` via link / `session` by id),
    `session_link` edge check (else `not_reachable`), envelope-encode
    `kind`/`payload`, admit recipient run with derived `submission_id`;
    `report_back` instruction injection (no config patch); `agent_wait` **validate**
    (`minItems`, dedupe handles, `maxItems` fan-in) + **preflight** (read each
    handle's run status; return inline if already satisfied, else `Deferred`) and
    the encode of the resume directive (`waits`, per-handle preflight errors,
    `mode`, `timeout_ms` — **not** an absolute deadline; the workflow computes that).
    The old `agent_task` path becomes a plain `agent_send` to a child.
  - `worker/session_tools.rs`: route `agent_send` and `agent_wait`; **reject** an
    `agent_wait` batched with other calls (model-visible `Failed`); a lone
    `agent_wait` may return `Deferred`, so the batch path must handle a parked batch.
  - `gateway/service/workflow.rs`: reuse `signal_submit_admission` for the
    `agent_send` recipient run and `ResumeToolBatch`.

## Implementation Steps

### S1. Tool Contracts + Naming Cleanup
- `agent_send` DTO (tagged `to`, `kind`, payload envelope) replacing `agent_task`;
  `agent_wait` DTO (`waits: [{target_session_id, run_id}]`, `mode: all|any`,
  optional `timeout_ms`, terminal-only); optional `report_back` on `agent_spawn`.
  Strict schemas; Fleet bundle.
- Naming cleanup: rename `target_agent_id` / `agent_id` / `source_agent_id` /
  `from_agent_id` / `to_agent_id` to their `session_id` forms across the P83 tools.
- Regenerate committed contract artifacts once, covering both.

### S2. Engine Deferred-Tool-Batch Primitive
- `ToolBatchOutcome::Deferred { batch_id, resume_directive }` (opaque blob); a
  **`parked` flag on `ActiveToolBatch`** so `drive.next_action` does not re-emit the
  invocation or complete the batch while parked (no new `ToolCallStatus`);
  `ResumeToolBatch` clears `parked` and resolves through the normal
  `tool_batch_result_proposals` path; durable deferred/resumed reducer facts.
  Generic; engine never decodes the directive. Unit tests: lone-call batch parks →
  `next_action` does **not** re-emit it → `ResumeToolBatch` continues the turn;
  duplicate-resume no-op; `validate` still passes on the resumed terminal result;
  inline batches unaffected.

### S3. Workflow Select Loop + RunSubscription + Wait
- Widen the main loop to select over `pending_admissions` and the nearest
  active-wait **deadline** timer (workflow-computed absolute `deadline_ms`, not
  stored as a future), draining a pending-resolution queue into `ResumeToolBatch`.
- `subscribe_run` / `unsubscribe_run` signals (fixed typed `run_terminal` callback)
  and a `once`-only subscription registry; **subscribe-after-terminal** fires
  immediately without storing; terminal fan-out best-effort then remove; the three
  signal-failure behaviors. (No `persistent` mode in v1.)
- Continue-as-new guard: block idle compaction while a subscription or active wait
  exists.
- `agent_wait` parking: **record the active wait first** (deadline =
  `workflow_time + timeout_ms`), **then** subscribe each still-running handle;
  accumulate arrivals/errors; resolve on `all`/`any`/deadline via `ResumeToolBatch`
  applying the **mode-specific error rules**; race/duplicate resolved by `batch_id`
  + handle.

### S4. agent_send + report_back
- `agent_send`: resolve `to`, require a `session_link` edge (`not_reachable`
  else), envelope-encode `kind`/`payload`, admit recipient run with derived
  submission id; the `to: child` case subsumes the old `agent_task`.
- `report_back`: inject the report-back instruction into the child input (no
  config patch).
- Idempotency by recipient `submission_id`.

### S5. Tests
- Engine unit: a lone-call batch returning `Deferred` parks the turn
  (`active_tool_batch_id` set, `parked` flag set, no `CallCompleted`); **`next_action`
  does not re-emit the parked invocation**; `ResumeToolBatch` clears `parked`,
  resolves it, and the turn continues; the resumed terminal result passes
  `validate_tool_batch_result`; duplicate `ResumeToolBatch` is a no-op; inline
  batches unaffected.
- Mixed-batch rejection: an `agent_wait` emitted alongside other calls resolves the
  `agent_wait` call as model-visible `Failed`; the rest of the batch completes
  normally.
- `agent_wait` validation: empty `waits` rejected (`minItems`); duplicate
  `{target_session_id, run_id}` rejected; over-cap fan-in rejected.
- Workflow subscription: `subscribe_run` fires exactly once via the `run_terminal`
  signal on terminal and is removed; **`subscribe_run` against an already-terminal
  run fires immediately and stores nothing** (the race); **record-before-subscribe**
  — a synchronous `run_terminal` arriving for an already-terminal handle is matched
  to the active wait, not dropped as unknown; terminal fan-out re-emits idempotently
  after a simulated restart; a fan-out signal failure does **not** fail the target
  run; a registered subscription (or active wait) **blocks continue-as-new** at idle.
- Workflow loop/timer: the select resolves a `run_terminal` signal and a deadline
  timer into `ResumeToolBatch`; **the workflow computes `deadline_ms` from
  `timeout_ms`** and the timer is derived from it (an unrelated admission does
  **not** extend it; restart does **not** reset it); **no timer armed when
  `timeout_ms` absent**; race resolves once by `batch_id`.
- `agent_wait` join + error semantics: `mode = all` resolves only when every handle
  is terminal, and **any handle error resolves overall `error` immediately**;
  `mode = any` resolves on the first arrival, and **a handle error is non-fatal
  until no viable handle remains** (one errored + one running still defers; all
  errored → `error`); **preflight short-circuit** returns inline when all handles
  are already terminal (fast-child case); `timeout` carries partial results; a
  handle terminal-with-`Failed`-status is a normal arrival, not `error`.
- `agent_send` routing: `to: session` to a spawned child admits a run (old
  `agent_task` behavior); `to: parent` resolves the link and admits a parent run;
  `to: parent` from a root session and a send with no link edge both return
  `not_reachable`; `kind`/`payload` round-trip through the prepended envelope item;
  retried send does not double-admit.
- `report_back`: injects the instruction into the child input; does **not** mutate
  child tool config.
- Mode I in-process: parent spawns a child with `report_back`, goes idle, child
  `agent_send { to: parent }` wakes it as a fresh run.
- Mode W in-process: parent spawns two children, `agent_wait { waits:[h1,h2],
  mode: all }`, both complete, the targets' `RunSubscription`s resolve the parent's
  parked batch with both run summaries.
- Ignored Temporal/Postgres live: a real parent workflow parks on `agent_wait`, a
  child workflow completes and signals the parent via `run_terminal`, the parent
  resumes; separately, a child `agent_send { to: parent }` wakes an idle parent.

## Deferred

- **`until = activity` / mid-run progress waits.** v1 waits for terminal only;
  intermittent progress is covered by polling `agent_read` or by Mode I
  (child-pushed `agent_send`). A later `agent_wait` mode (or `agent_subscribe`)
  can wake on activity, with the per-waiter event cursor living on the watcher
  side.
- **`agent_subscribe` (standing `RunSubscription`).** v1 is fire-once only — no
  `mode` field is stored or tested. A future tool adds a persistent variant (fire
  on every run terminal, survive continue-as-new) so an agent can stand-subscribe
  to another's run lifecycle (every completion, goal-mode terminal, etc.); adding
  it is additive to the v1 record.
- **Graph wiring / link installation.** A v2 `agent_link` action (or a spawn-time
  `links:` field) that installs `session_link` edges to named peers, so a
  supervisor can wire B->C and C->A and make those sends reachable. v1
  auto-creates only the spawn parent->child edge.
- **Directional edge policy.** Command-vs-report rights, "subordinate may not task
  supervisor", per-relationship grants, and a narrower send-only Fleet
  sub-capability — the P83-deferred capability work.
- **First-class `InputItem::AgentMessage`** typed send metadata (replacing the v1
  text envelope) — touches `api` wire types, contract artifacts, and projection.
- Structured typed message channels / schemas beyond `text` + opaque `payload`.
- Back-pressure / rate limits on `agent_send` (reuse the messaging rate-cap shape
  if abuse appears).
- Cancelling a parked `agent_wait` explicitly (today: it resolves on timeout, on
  terminal, or when the parent is cancelled).

## Acceptance Criteria

- **Both wait modes work and hold for hours-to-days cheaply.** Mode I (idle +
  child `agent_send`) and Mode W (`agent_wait`) each cost only an idle workflow
  (Mode W plus at most one durable timer), and survive worker restart by replay.
- `agent_wait` parks the parent turn without holding a Temporal worker slot or a
  blocking tool activity; there is no ~6-minute ceiling.
- `agent_wait` is a **join over run handles**: `waits: [{target_session_id,
  run_id}]` (validated: `minItems=1`, no duplicate handles, bounded `maxItems`)
  with `mode: all` (join_all) / `any` (select). It resolves with
  `outcome ∈ { terminal, timeout, error }` plus per-handle `results` — never a
  raised tool failure for those states. `timeout_ms` is optional with no default
  (indefinite when omitted).
- **Error propagation is mode-specific**: `all` resolves `error` as soon as any
  handle errors; `any` resolves `error` only when every handle has errored (one
  errored + one running still defers).
- **Already-finished runs resolve immediately**: `agent_wait` preflights every
  handle and returns inline (no parking) when the join is already satisfied — the
  fast-child / join-a-completed-handle case. The parent **records the active wait
  before subscribing**, and the subscribe handler fires immediately for an
  already-terminal run, so a synchronous `run_terminal` is never dropped.
- A handle that is `terminal` with a `Failed` run status is a normal result;
  `error` is reserved for "could not establish/observe the wait" (unknown run,
  unreachable target, failed subscribe).
- **`agent_wait` must be the only call in its batch**; emitted alongside other
  tool calls it returns a model-visible `Failed` ("must be called alone"). Waiting
  on many runs is the `waits: [...]` argument, not multiple batched calls.
- Waiting uses a **generic, fire-once `RunSubscription`** (fixed typed
  `run_terminal` callback) on the target workflow that any subscriber workflow can
  use — not an `agent_wait`-specific or session-only construct. No `persistent`
  mode is stored or tested in v1.
- The parent **workflow loop selects over admissions and the deadline timer** (not
  `pending_admissions` alone), draining resolutions into `ResumeToolBatch`. The
  **workflow computes `deadline_ms = workflow_time + timeout_ms`** (the activity
  passes only `timeout_ms`); the timer does not drift on unrelated admissions or
  reset on restart. A registered subscription or active wait **blocks idle
  continue-as-new** so parked state is never dropped.
- The engine gains only a **generic deferred-tool-batch** primitive
  (`ToolBatchOutcome::Deferred { batch_id, opaque resume_directive }` +
  `ResumeToolBatch`), modeled as a **parked batch** with an explicit `parked` flag
  on `ActiveToolBatch` (no new `ToolCallStatus`); while parked, `next_action`
  **does not re-emit the invocation**, and resolution flows through the normal
  terminal-result path. The engine never decodes the directive; no Fleet/wait
  semantics in the core; no DB table or reconciler.
- The three signal-failure behaviors hold: parent subscribe-failure → handle
  `error`; target fan-out failure → does not fail the target run; timeout
  unsubscribe → best-effort.
- Crash safety is by Temporal replay alone: deferred-batch reducer fact, target
  workflow subscription state, in-workflow terminal fan-out, and absolute-deadline
  timer.
- The entire Fleet surface uses **`session_id`**; no tool exposes `agent_id` /
  `target_agent_id` / `*_agent_id`. The P83 tools are renamed in the same contract
  revision.
- Inter-agent delivery is **one tool, `agent_send`**, addressing any linked session
  (`to: parent | session(target_session_id)`), `kind` sender-chosen, no up/down
  asymmetry; it replaces P83's `agent_task` and the child->parent callback.
  `kind`/`payload` ride a text envelope with no `api` wire change.
- A send to a session with no link edge returns `not_reachable`; arbitrary A-wired
  B->C topologies are expressible by the design and unlocked by the deferred
  link-installation action.
- `report_back` injects the report-back instruction only and does **not** patch
  child tool config.
- `agent_send` is async; synchronous behavior is composed from `agent_send` +
  `agent_wait`.
- The model-visible Fleet surface stays small (6 tools) and exposes no generic
  session/run API.
