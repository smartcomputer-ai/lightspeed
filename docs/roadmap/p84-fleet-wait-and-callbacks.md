# P84: Fleet Wait And Callbacks

**Status**
- Proposed 2026-06-24.
- Builds on **P83 (Fleet Subagent Control Plane)** — the `agent_*` tool surface,
  the `FleetService`/`FleetToolExecutor`, hosted `SessionTools` routing, and the
  parent->child links. It also builds on the Temporal-backed
  `AgentSessionWorkflow` (run admission via the `submit_admission` signal) and the
  messaging outbox spine (P71).
- This doc owns two P83-deferred items: *"Completion and important-update
  notifications back to the parent; rich `wait_agent` semantics."*

## Goal

Give a supervisor agent two missing capabilities:

1. **Wait** — block until the target run reaches a terminal state, or the target
   produces new activity, or an (optional, possibly multi-day) timeout elapses —
   instead of open-coded polling of `agent_read`.
2. **Callbacks** — let a child agent push a note back to its parent (progress, a
   question, a result) so the parent wakes and reacts, instead of the parent
   having to poll.

Both must slot cleanly into the Temporal runtime, support **hours-to-days**
waits cheaply, and must not put blocking, side-effectful, or wall-clock logic
into `engine`.

## Two Ways To Wait — Both Are First-Class

A top-level agent kicks off large tasks that can take hours or days. There are
two fundamentally different ways for it to wait for the result, and we support
both because they fit different situations:

| | **Idle-and-be-woken** (Mode I) | **Explicit `agent_wait`** (Mode W) |
|---|---|---|
| Parent state | run goes **terminal**; workflow parks idle | run **parked mid-turn** on a declared await |
| Waiting "on" | nothing specific — any business event resumes it | a named target + condition + optional bound |
| Who delivers | child `agent_send { to: parent }` (or any future event) | runtime watcher / workflow timer |
| Child must cooperate? | **yes** — child must be told to report back | **no** — runtime observes the child's log |
| Cost while waiting | free (idle workflow) | free (idle workflow + one durable timer) |
| Result lands as | a **new run** on the parent | resolution of the **same parked turn** |
| Best for | long autonomous fan-out; parent free meanwhile | "I can't continue until I have this answer" |

The enabling runtime fact (already true today): **an idle session is just a
workflow parked on `wait_condition(pending_admissions)`
(`crates/temporal-workflow/src/workflow.rs:69`). It holds no worker, lives
indefinitely across `continue_as_new`, and any `submit_admission` wakes it —
hours or days later.** So both modes are cheap for arbitrarily long waits: Mode I
costs an idle workflow, Mode W costs an idle workflow plus one durable Temporal
timer.

Both modes funnel through **one delivery spine** — an admission against the
parent session. The parent picks its mode per task:

- **Mode I (default for big async tasks):** spawn the child with a `report_back`
  directive, let the parent run go idle, and be woken by the child's
  `agent_send { to: parent }`. Cheaper and more flexible — the parent can do other
  work, the callback can carry a rich payload or a mid-task question, and it
  survives compaction / continue-as-new naturally.
- **Mode W (for tight dependencies):** call `agent_wait`, parking the turn until a
  declared condition fires. The result is delivered back *into the same turn*.

Mode W's `until = activity` resolves on exactly the kind of event Mode I's
`agent_send { to: parent }` produces, so the two interoperate with no special
wiring.

## The Core Insight

The two features look different but reduce to **one primitive that already
exists**: `submit_admission(AgentAdmission { command })`, the single signal that
wakes an `AgentSessionWorkflow` and feeds it a `CoreAgentCommand`
(`crates/temporal-workflow/src/workflow.rs:83`,
`crates/temporal-server/src/gateway/service/workflow.rs:28`).

- A **callback** is the child's runtime admitting a `RequestRun` on the *parent*
  session. The parent wakes as a normal new run. No new transport.
- A **wait** is the parent parking the current turn until a future admission
  resumes it. The thing that resumes it is a small new admission command
  (`ResumeAwait`) delivered by the same signal path — emitted either by the
  child reaching terminal state or by a Temporal timer firing for the timeout.

So both features share the admission spine. We are not inventing a callback bus
or a second messaging system; we are applying the existing wake primitive in two
directions (child->parent for callbacks, runtime->self for wait resumption).

## Why Not A Blocking Tool Activity

The obvious shortcut — make `wait_agent` a tool that long-polls the child inside
its tool activity — is rejected:

- Tool calls run as one batched Temporal activity with a **360s start-to-close
  timeout** and no heartbeat (`crates/temporal-workflow/src/config.rs:11`,
  `:53`). A wait longer than ~6 minutes cannot complete in one activity, and any
  wait holds a worker slot for its full duration.
- The runtime uses **no Temporal timers and no cross-workflow signaling today**;
  the workflow blocks only on `wait_condition(pending_admissions)`. A
  long-blocking activity is exactly the anti-pattern the P83 reentrancy rules
  warn against ("starting/awaiting other work is a side effect; it belongs in the
  workflow, not inside a tool activity that pretends to be synchronous").

The user-selected design instead makes the *workflow* await, with the tool call
returning a parked result and the turn resuming on a signal or timer. This keeps
waits unbounded in wall-clock time, cheap (no held worker), and Temporal-native.

## Part A: Wait (Workflow-Level Await)

### Tool Surface

One new model-visible Fleet tool:

```text
agent_wait   block until a target agent's run is terminal, it produces new
             activity, or a timeout elapses
```

An explicit wait must declare two things: **what** it is waiting for (the resolve
condition) and **how long** it is willing to wait (an optional, possibly
multi-day, bound). Input shape:

```text
target_session_id
run_id?          pin the wait to a specific run (see "Which run")
until            terminal | activity            (default: terminal)
since_seq?       int (event cursor; activity mode only)
timeout_ms?      int                            (default: none — wait indefinitely)
```

- `until = terminal`: resume when the target run reaches `Completed`, `Failed`,
  or `Cancelled` (`crates/engine/src/core/components/run.rs:88`). The runtime
  *observes* this from the target's log, so it works even if the child never
  cooperates (never sends `to: parent`, or crashes mid-task).
- `until = activity`: resume when the target's session event log advances past
  `since_seq` (any new event), so a supervisor can react to incremental progress,
  not just completion. `since_seq` defaults to the target head observed at call
  time.
- `timeout_ms`: **optional**, with **no default — the wait is indefinite** unless
  a bound is given. This matches hours-to-days tasks: the parent stays
  parked-but-free until the condition fires. A bound, when set, is backed by a
  durable Temporal timer (also free; survives restart; fires after arbitrary
  duration). On timeout the tool resolves with `outcome = "timeout"` rather than
  erroring, so the supervisor decides whether to wait again, read, or cancel.

There is **no ~6-minute ceiling**: the wait is a parked workflow turn plus an
optional timer, not a blocking tool activity. (An earlier draft floated a 5-min
cap inherited from the rejected blocking-activity design; it does not apply.)

Output shape:

```text
target_session_id
outcome          terminal | activity | timeout
run?             compact run summary (status, output ref) when known
last_seq         event cursor reached (for chaining the next agent_wait)
```

Because timeout is a normal outcome and `last_seq` is returned, "wait longer" is
just another `agent_wait` with the new cursor.

### Which Run

`agent_wait` waits on the target's **current active run** by default. A supervisor
that spawned/tasked a child holds the `child_run_id` / `run_id` from
`agent_spawn` / `agent_send`; an optional `run_id` selector pins the wait to that
specific run so a race (run completes, target immediately starts another) cannot
make the wait latch onto the wrong run. If the named run is already terminal at
call time, `agent_wait` returns immediately — no parking.

### Runtime Shape (Parked Turn)

The engine gains one new action and one new resume command. This is the same
async-effect pattern the drive already uses for `InvokeTools`
(`crates/engine/src/core/drive.rs:25`, `:162`), generalized so the *resumer* is
an external admission rather than the tool activity's return value.

New `CoreAgentAction` variant:

```text
AwaitExternal {
    await_id            deterministic id (parent session/run/turn/tool-call)
    awaited             { session_id, run_id?, until, since_seq? }
    timeout_ms
}
```

New `CoreAgentCommand` variant (delivered via `submit_admission`):

```text
ResumeAwait {
    await_id
    outcome             terminal | activity | timeout
    run_summary?        opaque blob ref
    last_seq?
}
```

Flow:

```text
parent model calls agent_wait
  -> CoreAgent plans an AwaitExternal action (NOT InvokeTools)
  -> workflow loop sees AwaitExternal:
       * records the await as pending (in workflow state + durable event)
       * registers a watcher (see below) — a SIDE EFFECT, done in the workflow,
         not the engine
       * does NOT block the loop; returns to wait_condition
  -> later, a ResumeAwait admission arrives via submit_admission
  -> drive.resume_await(cmd) emits the tool result for the original agent_wait
     call (Succeeded, output = outcome/run/last_seq) and continues the turn
```

Key point: `AwaitExternal` is the engine telling the runtime *"I am parked on
await_id; wake me with a ResumeAwait."* The engine stays deterministic and
wall-clock-free. Producing the resume is the runtime's job.

### Producing The Resume (Watcher + Timer)

The hosted runtime owns two resume sources for each parked await. Both end in the
same `submit_admission(ResumeAwait)` call against the **parent** workflow
(`workflow_id == parent_session_id`):

1. **Completion / activity watcher.** A runtime task (in `temporal-server`, not
   the workflow) watches the target via the existing long-poll
   (`read_session_events` with `wait_ms`, already capped and blessed —
   `gateway/service/mod.rs`). When the target run goes terminal (`until =
   terminal`) or any event lands past `since_seq` (`until = activity`), it signals
   `ResumeAwait { outcome: terminal|activity, ... }` to the parent.

2. **Timeout timer (only when `timeout_ms` is set).** Implemented as a Temporal
   timer in the *parent* workflow (this introduces the first use of
   `ctx.timer`/sleep in this codebase). When the timer fires first, the workflow
   itself produces `ResumeAwait { outcome: timeout }` locally — no signal
   round-trip, because the workflow is already running. When `timeout_ms` is
   absent (the default), **no timer is armed and the await is indefinite** — it
   resolves only via the watcher. An indefinite await costs exactly an idle
   workflow plus a persisted pending-await record; it can park for days. Whichever
   source fires first wins; `await_id` makes the resume idempotent so the loser is
   dropped.

Putting the timeout on a workflow timer (deterministic, replay-safe) and the
completion watch on a runtime task (side-effecting I/O) respects the
engine/runtime boundary: the workflow decides *when to give up*, the runtime
observes *what the child did*.

Idempotency: `await_id` is derived deterministically from the parent
session/run/turn/tool-call identity (the same scheme P83 uses for child ids,
P83 "Identity Model"). A duplicate `ResumeAwait` for an already-resolved await is
a no-op. A watcher that restarts re-reads from the persisted `since_seq`.

### Crash / Resume Safety

The pending await is a durable workflow-state fact (and a session-log event), so
a worker restart replays it and re-registers the watcher. The watcher is
stateless beyond `(target, since_seq, until)`, all of which are in the persisted
await record. The Temporal timer survives restart by construction. There is no
in-memory-only wait state.

## Part B: Sending Between Agents (`agent_send`)

### One Mechanism, So One Tool — Because The Topology Is A Graph

`agent_task` (parent -> child) and a child -> parent callback are **the same
runtime primitive**: admit a `RequestRun` on another session via
`signal_submit_admission` (`crates/temporal-server/src/gateway/service/workflow.rs:28`).
There is no second transport.

Two direction-locked tools (`agent_task` down, `agent_notify` up) bake a **tree**
into the surface: every edge is parent->child or child->parent. But real agent
structures are a **general graph**. A spawns B and C, then tells B to message C (a
sibling edge), or arranges for C to report to A (which C's own parent might not
be). With direction-locked tools, "B messages C" is not expressible and "C
messages A" only works when A is literally C's parent.

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
see "Naming Cleanup" below. P84 is written in the target convention.)

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
  `to: parent` gets `status = not_reachable` (no parent edge), not an error.

### Permission Is "Is There An Edge?", Not "Which Way Does It Point?"

`agent_send` is permitted to any session the caller has a `session_link` to —
**any direction, no up/down asymmetry**. The topology lives entirely in the link
graph; the tool just rides the edges that exist. Concretely:

- send is allowed iff a `session_link` connects caller and `target_session_id`
  (the spawn parent->child edge, or any other link that exists);
- `kind` (`task | progress | question | result`) is **pure sender-chosen
  framing** — the runtime never forces or rewrites it based on edge direction. A
  peer may `task` a peer; a child may send a `result` up; a parent may ask a
  `question` down. The receiver decides how to react.

This is the deliberate consequence of treating the structure as a graph: there is
no built-in notion of "subordinate must not command supervisor" in v1, because
that is a *policy over edges*, not a property of the send tool. Hardening edges
with directionality, command-vs-report rights, and per-relationship grants is the
**P83-deferred capability-policy work**; v1 trusts a present edge, exactly as P83
`agent_read` / `agent_cancel` already trust a named target id.

### v1 Wires Only Spawn Edges (Graph Is Expressible, Not Yet Fully Reachable)

The send *semantics* are graph-general, but v1 only auto-creates **one** kind of
edge: the parent->child link on `agent_spawn`. So in v1 a caller can send to its
spawned children and (via `to: parent`, or `report_back`) to its parent, but an
A-arranged **B->C sibling** edge is not yet reachable: nothing in v1 installs a
link from B to C.

This is a *reachability* gap, not a design compromise — the moment a
link-installation action exists, arbitrary topologies light up with no change to
`agent_send`. A v2 `agent_link` / spawn-time `links:` field (deferred) lets A
install B->C and C->A edges so the supervisor composes the graph and the children
just send along it. Listed in Deferred.

### Framing (`kind`) Tells The Receiver How To Read It

The admitted run carries a `kind` (`task | progress | question | result`) and an
origin marking it a Fleet send. The receiver reads `task` as new work and
`progress` / `question` / `result` as an incoming report. This is the *only*
semantic difference between "tasking" and "notifying" now — a sender-chosen field,
not a separate tool and not a direction the runtime infers.

### Resolution And Idempotency

The Fleet service resolves `to` to a recipient session id (`parent` via the link,
`session` by id), checks a `session_link` edge connects caller and recipient
(else `status = not_reachable`), then admits a `RequestRun` on it via the normal
hosted run path, with input carrying `text` / `input` / `payload` and the
`kind`/origin metadata. The recipient wakes as an ordinary new run.

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

Permission to message the parent already exists by construction: the spawn
creates the parent->child edge, and `agent_send` may ride any edge. So
`report_back` is not about *permission* — it is about *expectation*. A callback
only happens if the child knows it is expected to make one, and when; otherwise a
child with the fleet tools enabled simply might not think to report.

`report_back` directive (optional on `agent_spawn`, and on an `agent_send` that
kicks off a child's work):

```text
report_back?  {
  on            terminal | milestones | question   (what warrants a report)
  instructions? string                             (extra guidance for the child)
}
```

When present, the Fleet service, as it admits the child run:

1. **Ensures `agent_send` is enabled** on the child (it usually already is via the
   `tools.fleet` gate; `report_back` makes the dependency explicit).
2. **Injects a report-back instruction** into the child's input — a short note
   like *"You are a subagent of <parent>. `agent_send { to: parent }` when you
   finish (and if you hit a blocking question), with a concise result."*
   parameterized by `on` and `instructions`.

So the child learns both **how** (the tool, addressing `to: parent`) and **when**
(the injected instruction). This makes Mode I (idle-parent callbacks) reliable
instead of relying on the child's unprompted judgment.

`report_back` is the *declared-cooperation* path. It is complementary to
`agent_wait until=terminal`, the *observed* path (runtime watches the child's
log, no child cooperation needed). A robust supervisor often uses both:
`report_back` so the child can volunteer progress and questions, plus a terminal
wait/observe as the backstop in case the child finishes without reporting.

### Send + Wait Compose

A `to: parent` `agent_send` lands as a new parent run — exactly the "new
activity" an `agent_wait(until = activity)` is parked on. So a parent can spawn a
child with `report_back`, `agent_wait` on it, and the child's send resolves the
wait via the activity watcher — through the same admission/event spine, with no
special wiring between the features.

## Naming Cleanup: `session_id` Everywhere (Applies To All Agent Tools)

A Fleet agent *is* a session — `agent_id == session_id`, with no separate agent
identity in v1 (P83 "Identity Model"). The shipped P83 DTOs nonetheless drifted
into a second, `agent_id`-flavored name for the same value on the
address-an-existing-agent surface. P84 standardizes the whole Fleet surface on
**`session_id`** and corrects the existing P83 tools to match, so the model never
sees two names for one identity.

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
in `crates/temporal-server/src/fleet.rs` that builds these DTOs, and the
deterministic Fleet tests that assert field names. Regenerate artifacts via
`cargo run -p api --bin export-schema` and update P83's tool-contract prose
(`agent_task` etc.) to the new names. P84's `agent_send` / `agent_wait` are
authored in the target convention from the start, so no P84 rename is needed.

This cleanup is bundled into P84's S1 (contracts) so the rename and the new tools
land in one contract revision rather than two.

## agent_send Is Async; Synchronous Is Composed

`agent_send` admits a run and returns the handle immediately. It does **not**
grow a blocking `wait` flag. "Send work and get the result back" is composed:

```text
agent_send { to: { kind: session, target_session_id: id }, text } -> { run_id }
agent_wait { target_session_id: id, run_id, until: terminal } -> { outcome, run }
```

This keeps blocking semantics in exactly one place (`agent_wait` /
`AwaitExternal`) — one parked-turn code path to reason about, test, and make
crash-safe, not two.

## Tool Surface After P84

```text
agent_spawn    (P83)            create a child; optional report_back
agent_list     (P83)
agent_read     (P83)
agent_cancel   (P83)
agent_send     (P84, replaces agent_task) deliver to parent | session(target_session_id)
agent_wait     (P84)            block on target terminal/activity/timeout
```

Six tools (P83's `agent_task` folds into `agent_send`), all still gated behind
the per-session `tools.fleet` flag (P83
"Policy"). No generic session/run API is exposed.

## Implementation Map

- `crates/tools/src/fleet/`: add the `agent_send` DTO (tagged `to` enum, `kind`,
  `text`/`input`/`payload`) — replacing P83's `agent_task` — and the `agent_wait`
  DTO; add the optional `report_back` directive to `agent_spawn`. Strict schemas
  (deny unknown fields; do not advertise selectors not yet implemented), tool
  names, `ToolSpecBundle` entries. No Postgres/Temporal deps here.
- `crates/engine/src/core/`:
  - Add `CoreAgentAction::AwaitExternal` (`drive.rs:25`) and a planner path that
    emits it when an `agent_wait` tool call is admitted instead of running it as a
    tool batch. The parked tool call's result is *deferred*, not produced inline.
  - Add `CoreAgentCommand::ResumeAwait` (`components/command.rs:46` neighborhood)
    and `drive.resume_await(...)` (sibling of `resume_tool_batch`,
    `drive.rs:162`) that emits the deferred tool result for the original
    `agent_wait` call and continues the turn.
  - Add durable run/turn events for "await opened" / "await resolved" so the
    parked state replays. Keep these reducer facts minimal.
  - Engine stays deterministic: no I/O, no timers, no watching. It only emits
    `AwaitExternal` and consumes `ResumeAwait`.
- `crates/temporal-workflow/src/workflow.rs`:
  - Handle `AwaitExternal`: record the pending await in workflow state, start a
    Temporal timer for `timeout_ms`, and request the runtime watcher (via an
    activity) — without blocking the admission loop.
  - On timer fire, queue a local `ResumeAwait { outcome: timeout }`.
  - `ResumeAwait` arrives through the existing `submit_admission` signal or the
    local timer path and flows into `drive.resume_await`.
- `crates/temporal-server/src/`:
  - `fleet.rs`: `agent_send` resolution — resolve the `to` enum (`parent` via the
    P83 link, `session` by `target_session_id`), check a `session_link` edge exists
    between caller and recipient (else `not_reachable`; no up/down classification),
    admit a recipient run with derived `submission_id` and sender-chosen
    `kind`/origin framing; `report_back` application on `agent_spawn` (inject the
    report-back instruction, ensure `agent_send` enabled); the await **watcher**
    task that long-polls the target and signals `ResumeAwait` to the parent
    workflow. The existing `agent_task` path becomes a plain `agent_send` to a child.
  - `worker/session_tools.rs`: route `agent_send` and `agent_wait` like the other
    Fleet tools; `agent_wait` produces an `AwaitExternal` plan rather than a normal
    tool result, so the fast-path must recognize it does not return inline.
  - `gateway/service/workflow.rs`: reuse `signal_submit_admission` for both the
    `agent_send` recipient run and the `ResumeAwait` resume.

## Implementation Steps

### S1. Tool Contracts + Naming Cleanup
- `agent_send` DTO (tagged `to`, `kind`, payloads) replacing `agent_task`;
  `agent_wait` DTO; optional `report_back` on `agent_spawn`. Strict schemas; add
  to the Fleet bundle.
- Naming cleanup: rename `target_agent_id` / `agent_id` / `source_agent_id` /
  `from_agent_id` / `to_agent_id` to their `session_id` forms across the existing
  P83 tools (see "Naming Cleanup"), updating the Fleet executor DTOs, P83 prose,
  and tests.
- Regenerate committed contract artifacts (`cargo run -p api --bin export-schema`)
  once, covering both the new tools and the rename.

### S2. Engine Await Primitive
- `AwaitExternal` action, `ResumeAwait` command, `resume_await`, durable
  await-open/await-resolve events, deferred tool-result emission for the parked
  `agent_wait` call. Deterministic; no runtime deps.

### S3. Workflow Await Handling
- Handle `AwaitExternal` (record pending, start timeout timer, request watcher);
  feed `ResumeAwait` (signal or timer) into `resume_await`; replay-safe pending
  await on restart.

### S4. Runtime Watcher + agent_send + report_back
- The watcher task (long-poll target, signal `ResumeAwait` on terminal/activity).
- `agent_send`: resolve `to` (`parent` | `session(target_session_id)`), require a
  `session_link` edge (any direction; `not_reachable` otherwise), admit recipient
  run with derived submission id and sender-chosen `kind`/origin framing (a send to
  a child subsumes the old `agent_task`).
- `report_back` application: inject the report-back instruction into the child
  input and ensure `agent_send` is enabled.
- Idempotency by `await_id` and recipient `submission_id`.

### S5. Tests
- Engine unit: `AwaitExternal` is planned for `agent_wait`; `ResumeAwait`
  resolves the exact parked call; duplicate `ResumeAwait` is a no-op; a wait on an
  already-terminal run resolves immediately without parking.
- Workflow: timeout timer fires `ResumeAwait { timeout }` when `timeout_ms` is
  set; **no timer is armed when it is absent** and the await stays pending
  indefinitely until the watcher resolves it; watcher resume beats the timer (and
  vice versa) deterministically by `await_id`; pending await replays after a
  simulated restart.
- `agent_send` routing: `to: session` to a spawned child admits a recipient run
  (the old `agent_task` behavior); `to: parent` resolves the link and admits a
  parent run; `to: parent` from a root session returns `not_reachable`; a send to a
  session the caller has **no** link to returns `not_reachable`; `kind` is carried
  through verbatim (a child may send `kind: task` up — not rejected in v1).
- Mode W in-process: parent spawns a child, `agent_wait(until=terminal)`, child
  completes, parent's wait resolves with the child's run summary.
- Mode I in-process: parent spawns a child with `report_back`, parent run goes
  **idle** (no `agent_wait`), child `agent_send { to: parent }` admits a fresh
  parent run that wakes the idle parent.
- `report_back`: a child spawned with `report_back` has `agent_send` enabled and a
  report-back instruction in its input; a child spawned without it has no injected
  instruction (but, having the spawn edge, could still send to its parent if it
  chose to).
- Cross-mode: a child `agent_send { to: parent }` also resolves a parent
  `agent_wait(until=activity)`.
- Idempotency: a retried `agent_send` does not double-admit the recipient run;
  retried `agent_wait` re-registers the same `await_id`.
- Ignored Temporal/Postgres live: real parent workflow parks on `agent_wait`,
  child workflow completes, parent resumes; separately, a child
  `agent_send { to: parent }` wakes the parent workflow with a new run.

## Deferred

- Fan-in waits (`agent_wait` on a *set* of targets, resolve on first/all). v1
  waits on one target; a supervisor loops.
- **Graph wiring / link installation.** A v2 `agent_link` action (or a spawn-time
  `links:` field) that installs `session_link` edges to named peers, so a
  supervisor A can wire B->C and C->A and have those sends reachable. v1 only
  auto-creates the spawn parent->child edge, so A-arranged sibling sends are
  *expressible but not yet reachable*. This is the main follow-up that turns the
  flat send model into the full arbitrary-topology graph.
- Directional edge policy: command-vs-report rights, "subordinate may not task
  supervisor", per-relationship grants — the P83-deferred capability work. v1
  permits a send along any existing edge with sender-chosen `kind`.
- Structured typed message channels / schemas beyond `text` + opaque `payload`.
- Back-pressure / rate limits on `agent_send` (reuse the messaging rate-cap shape
  if abuse appears).
- Push notifications to external channels on child terminal (that is the
  messaging outbox's job, wired separately).
- Cancelling a parked await explicitly (today: it resolves on timeout or the
  parent is cancelled). A dedicated cancel can come with capability policy.

## Acceptance Criteria

- **Both wait modes work.** A parent can either (Mode I) go idle and be woken by a
  child callback, or (Mode W) `agent_wait` to park its current turn — and both
  hold for hours-to-days at the cost of an idle workflow (plus, for a bounded
  Mode W wait, one durable timer).
- A parent can `agent_wait` on a child and the parent turn parks without holding a
  Temporal worker slot or a tool activity. There is no ~6-minute ceiling.
- `agent_wait` resolves on the target run reaching terminal state, on new target
  activity past a cursor, or on timeout. **`timeout_ms` is optional with no
  default; when omitted the wait is indefinite** (no timer armed). Timeout is a
  normal outcome, not an error, and returns a cursor for chaining.
- The await survives a worker restart (durable pending await + replayable timer +
  stateless watcher).
- The entire Fleet surface uses **`session_id`** for agent identity; no tool
  exposes `agent_id` / `target_agent_id` / `*_agent_id`. The P83 tools are renamed
  accordingly in the same contract revision.
- Inter-agent delivery is **one tool, `agent_send`**, addressing any session the
  caller has a `session_link` to — **no up/down asymmetry**; `kind` is
  sender-chosen framing. It replaces P83's `agent_task` and the child->parent
  callback. The topology is the link graph, not the tool vocabulary.
- A send to a session with no link edge returns `not_reachable`; v1 auto-wires
  only the spawn parent->child edge, and arbitrary A-wired B->C topologies are
  expressible by the design and unlocked by the deferred link-installation action.
- `report_back` makes a child *report* (injects the when/how instruction); the
  child can reach its parent regardless, because the spawn edge exists.
- A child can `agent_send { to: parent }`; the parent wakes as a normal new run
  (Mode I) or resolves a parked `agent_wait(until=activity)` (cross-mode); a root
  session's `to: parent` returns `not_reachable` instead of erroring.
- `agent_send` is async; synchronous behavior is composed from `agent_send` +
  `agent_wait`.
- No wall-clock, blocking, or watcher logic lives in `engine`; resumption is
  always a `ResumeAwait` admission delivered by the runtime or a workflow timer.
- The model-visible Fleet surface stays small (6 tools) and exposes no generic
  session/run API.
