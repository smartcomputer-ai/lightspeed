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

One new model-visible Fleet tool:

```text
agent_wait   block until a target session's run reaches a terminal state,
             or an optional timeout elapses
```

Input shape:

```text
target_session_id
run_id           the run to wait on (REQUIRED — see "Which Run")
timeout_ms?      int   (default: none — wait until terminal, however long)
```

- The wait resolves when the named run reaches `Completed`, `Failed`, or
  `Cancelled` (`crates/engine/src/core/components/run.rs:88`). This is observed
  from the target itself (see Runtime Shape), so it works even if the child never
  calls `agent_send` or crashes mid-task and is retried.
- `timeout_ms` is **optional** with **no default**: absent, the wait is
  indefinite (no timer armed) and costs only an idle workflow plus a durable
  subscription. When set, it is backed by `ctx.timer` in the parent workflow —
  durable across restart, valid for days. On timeout the tool resolves with
  `outcome = "timeout"` (not an error), so the supervisor can wait again, read,
  or cancel.

`until = activity` (wake on mid-run progress, not just terminal) is **deferred**
— see Deferred. v1 waits for terminal only; a supervisor that wants intermittent
progress polls `agent_read`, or uses Mode I so the child pushes updates via
`agent_send`.

Output shape:

```text
target_session_id
run_id
outcome          terminal | timeout | error
run?             compact run summary (status, output ref) when terminal
error?           message, set when outcome = error
```

`outcome` is always one of the three; `agent_wait` never raises a tool *failure*
for these states (a supervisor can branch on `outcome` instead of catching an
error). `error` is for "the wait could not be established or observed," distinct
from `terminal` carrying a `Failed` run status (which is a normal, expected
result).

### Which Run

`agent_wait` **requires an explicit `run_id`**. A supervisor that spawned or
tasked a child already holds it (`agent_spawn` returns `child_run_id`;
`agent_send` returns `run_id`). Requiring it removes the race a "current active
run" default would introduce (the run completes and the target immediately starts
another, so a default latches the wrong one).

Behavior for edge states is defined, never a hang:

- named run already **terminal** at call time → `outcome = terminal`, resolve
  immediately with its summary, no parking;
- named run **unknown / never existed** on the target → `outcome = error`
  immediately (not a hang);
- target session **closing / closed** with the run not terminal → `outcome =
  terminal` with the run's last known status if it has one, else `outcome = error`;
- target workflow **errored / unreachable** → `outcome = error`.

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
signal_name              the signal to send on the subscriber
correlation_token        opaque, subscriber-chosen (echoed back; lets the
                         subscriber match the notification to its waiter)
run_id                   the specific run to fire on (required in v1)
```

- v1 semantics are **fire once on that run's terminal, then remove the
  subscription** — this is exactly what `agent_wait` needs.
- A future *persistent* mode (fire on every run terminal, never remove) is
  **deferred and not stored or tested in v1.** It would have to survive
  `continue_as_new` (see note below) and outlive runs, which is real extra
  machinery with no v1 user. The primitive is intentionally shaped so that adding
  a `mode` field later is additive, but v1 ships neither the field nor the
  behavior.

**Subscribe-after-terminal race.** The `subscribe_run` handler must check the run
*at registration time*: if the named run is **already terminal**, fire the
subscriber signal immediately and do **not** store the subscription. Only a
still-running target stores it for later. This closes the window where the run
goes terminal between the waiter's `Deferred` and the target processing
`subscribe_run` — otherwise the waiter would park forever.

On a run reaching terminal, the target workflow iterates its subscriptions and,
for each match, calls
`ctx.external_workflow(subscriber_workflow_id, None).signal(signal_name, { correlation_token, run_id, status, output_ref })`,
then removes the (once) subscription. Because this is workflow code it is durable:
a restart replays the terminal transition and re-emits the signal idempotently.

New signals on the session workflow:

```text
subscribe_run(RunSubscription)        register; fire-immediately if already terminal
unsubscribe_run(subscription_id)      remove it (waiter timed out / gave up)
```

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

### Engine: A Generic Deferred-Tool Primitive (Not Fleet-Specific)

`agent_wait` is a tool whose result is not produced inline by the tool activity —
it is produced later, when the subscription fires. Rather than hard-code Fleet
await semantics into the deterministic core, the engine gains a **generic
deferred-tool** capability keyed on tool-call metadata. `agent_wait` is one user
of it; nothing in the engine knows about Fleet.

- A tool invocation may resolve to
  `ToolOutcome::Deferred { call_id, resume_directive }` instead of a normal result.
  The engine records the call as parked and does **not** emit its result — the turn
  cannot complete until it is resumed.
  - `resume_directive` is an **opaque, runtime-owned blob ref** (an
    `api_kind` + versioned body, like P83's `ProviderParams`). The engine stores
    and replays it but **never interprets it**. The Fleet executor encodes the
    wait specifics into it (`target_session_id`, `run_id`, `timeout_ms`,
    `signal_name`); the parent workflow's Fleet handler decodes it to drive the
    subscribe/timer. This is what lets the workflow act on a deferral the *tool
    activity* decoded, without leaking Fleet vocabulary (`target`/`run_id`/`kind`)
    into the deterministic core. The engine sees only `{ call_id, opaque blob }`.
- A new command `ResumeToolCall { call_id, result }` (delivered via the existing
  `submit_admission` signal path) supplies the deferred result and lets the turn
  continue. This is the deferred sibling of `resume_tool_batch`
  (`crates/engine/src/core/drive.rs:162`).
- Durable reducer facts record "tool call deferred" (with the opaque directive)
  and "tool call resumed" so the
  parked turn replays.

The engine stays deterministic and wall-clock-free: it only knows *"this call is
deferred; resume it with a result."* Which tool, what it is waiting on, timers,
and subscriptions all live in the runtime/workflow.

### Flow

```text
parent model calls agent_wait
  -> tool execution returns ToolOutcome::Deferred { call_id, resume_directive }
     (the runtime decides to defer; directive encodes target/run/timeout)
  -> parent workflow, on seeing the deferred call, decodes resume_directive and:
       * subscribe_run on the TARGET workflow:
           ctx.external_workflow(target_id).signal(subscribe_run, {
             subscriber_workflow_id = parent_id, signal_name = "resume_wait",
             correlation_token = call_id, run_id })
       * records an ACTIVE WAIT in workflow state: { call_id, target_id, run_id,
         timer_handle? } (durable; see continue-as-new note)
       * if timeout_ms set: arms ctx.timer(timeout_ms) as a concurrent branch
  -> resolution, whichever fires first (see loop below):
       (a) resume_wait signal from target { correlation_token=call_id, status,
           output_ref } -> resolution = terminal
       (b) the armed timer future completes -> resolution = timeout,
           and the workflow unsubscribe_runs on the target
  -> the resolution becomes a local ResumeToolCall { call_id, result }, fed to
     drive.resume_tool_call; the deferred agent_wait resolves and the turn
     continues.
```

#### Workflow loop mechanics (the select, made explicit)

The current loop awaits only `pending_admissions`
(`crates/temporal-workflow/src/workflow.rs:69`). A timer firing is **not** an
admission, so the loop must be widened to observe both. The workflow keeps a
small set of **active waits** and a **pending-resolution queue**:

- For each active wait with a timeout, the workflow holds the `ctx.timer(..)`
  future. The top of the loop does a **select over: (1) the `wait_condition` for
  new `pending_admissions`, and (2) any armed timer future completing.** Both are
  `CancellableFuture`s, so they compose in a `select!`.
- A `resume_wait` **signal** is delivered like any other signal: its handler
  pushes a `(call_id, terminal-resolution)` onto the pending-resolution queue and
  marks the loop wakeable (same mechanism as `pending_admissions`).
- A **timer** branch winning the select pushes a `(call_id, timeout-resolution)`
  onto the same queue and cancels/forgets that wait's subscription.
- After the select returns, the loop drains the pending-resolution queue into
  `ResumeToolCall` admissions, then processes admissions as today.

So timer and signal funnel into **one queue**, the loop awaits **admissions OR
timers**, and there is exactly one place that converts a resolution into a
`ResumeToolCall`. This replaces the vague "start a timer then return to
wait_condition," which would never observe the timer.

`call_id`-keyed idempotency: a duplicate `resume_wait` (target replayed) or a
timer that fires after the wait was already resolved is dropped, because no active
wait with that `call_id` remains.

### Crash / Resume Safety (No DB, No Reconciler)

Every piece is durable by Temporal replay:

- the **deferred tool call** is a reducer fact in the parent session log;
- the **`RunSubscription`** is workflow state on the target, replayed on restart;
- the target's terminal-transition → subscriber-signal happens **in workflow
  code**, so a restart replays it and re-signals idempotently;
- the **timer** is a Temporal timer, durable by construction.

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
agent_wait { target_session_id: id, run_id } -> { outcome, run }
```

This keeps blocking semantics in exactly one place (`agent_wait` / the deferred
tool call) — one parked-turn code path to reason about, test, and make crash-safe.

## Are We Overbuilding? (`wait` vs `send` vs future `subscribe`)

Three mechanisms could look redundant. They are not — they differ on push-vs-pull
and on duration, and they share machinery rather than duplicating it:

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
  (`target_session_id`, required `run_id`, optional `timeout_ms`); add the optional
  `report_back` directive to `agent_spawn`. Strict schemas; tool names;
  `ToolSpecBundle` entries. No Postgres/Temporal deps here.
- `crates/engine/src/core/`:
  - Add the **generic deferred-tool** primitive: a tool result may be
    `ToolOutcome::Deferred { call_id, resume_directive }` where `resume_directive`
    is an **opaque runtime blob** the engine stores/replays but never interprets; a
    new `ResumeToolCall { call_id, result }` command (sibling of
    `resume_tool_batch`, `drive.rs:162`) resolves it and continues the turn; durable
    defer/resume reducer facts so the parked turn replays. No Fleet-specific or
    wait-specific logic in the engine.
- `crates/temporal-workflow/src/workflow.rs`:
  - **Widen the main loop** (`:69`) from `wait_condition(pending_admissions)` to a
    **select over `pending_admissions` and any armed `ctx.timer`**, draining a
    pending-resolution queue into `ResumeToolCall` admissions each pass.
  - **`RunSubscription`** support (`once`-only): `subscribe_run` /
    `unsubscribe_run` signals; store subscriptions in workflow state; the
    `subscribe_run` handler **fires immediately and does not store** if the named
    run is already terminal; on a run reaching terminal, iterate matching
    subscriptions, `ctx.external_workflow(subscriber).signal(..)`, and remove them.
    Generic — not tied to Fleet.
  - **`agent_wait` parking**: on the deferred `agent_wait` call, decode the opaque
    directive, `subscribe_run` on the target, record an active wait, optionally arm
    `ctx.timer(timeout_ms)`, and resolve via `ResumeToolCall` when the `resume_wait`
    signal or the timer wins (then `unsubscribe_run`).
  - **Continue-as-new guard**: extend `can_continue_as_new_at_idle` (`:724`) to be
    false while any `RunSubscription` or active wait exists, so idle compaction
    never drops parked-wait state.
- `crates/temporal-server/src/`:
  - `fleet.rs`: `agent_send` resolution (`parent` via link / `session` by id),
    `session_link` edge check (else `not_reachable`), envelope-encode
    `kind`/`payload`, admit recipient run with derived `submission_id`;
    `report_back` instruction injection (no config patch); the `agent_wait`
    deferral that triggers the workflow subscribe/timer path. The old `agent_task`
    path becomes a plain `agent_send` to a child.
  - `worker/session_tools.rs`: route `agent_send` and `agent_wait`; `agent_wait`
    returns `Deferred` rather than an inline result, so the batch path must handle
    a deferred call.
  - `gateway/service/workflow.rs`: reuse `signal_submit_admission` for the
    `agent_send` recipient run and `ResumeToolCall`.

## Implementation Steps

### S1. Tool Contracts + Naming Cleanup
- `agent_send` DTO (tagged `to`, `kind`, payload envelope) replacing `agent_task`;
  `agent_wait` DTO (required `run_id`, optional `timeout_ms`, terminal-only);
  optional `report_back` on `agent_spawn`. Strict schemas; Fleet bundle.
- Naming cleanup: rename `target_agent_id` / `agent_id` / `source_agent_id` /
  `from_agent_id` / `to_agent_id` to their `session_id` forms across the P83 tools.
- Regenerate committed contract artifacts once, covering both.

### S2. Engine Deferred-Tool Primitive
- `ToolOutcome::Deferred { call_id, resume_directive }` (opaque directive blob),
  `ResumeToolCall` command, `drive.resume_tool_call`, durable defer/resume reducer
  facts. Generic; no Fleet/wait coupling — the engine never decodes the directive.
  Engine unit tests for defer → resume → turn-continues, duplicate-resume no-op,
  and inline tools unaffected.

### S3. Workflow Select Loop + RunSubscription + Wait
- Widen the main loop to select over `pending_admissions` and armed timers, with a
  pending-resolution queue drained into `ResumeToolCall`.
- `subscribe_run` / `unsubscribe_run` signals and a `once`-only subscription
  registry in session workflow state; **subscribe-after-terminal** fires
  immediately without storing; terminal fan-out via `external_workflow().signal`
  then remove. (No `persistent` mode in v1.)
- Continue-as-new guard: block idle compaction while a subscription or active wait
  exists.
- `agent_wait` parking: decode the directive, subscribe on the target, optional
  `ctx.timer`, resolve via `ResumeToolCall`; timer-vs-signal race resolved by
  `call_id`.

### S4. agent_send + report_back
- `agent_send`: resolve `to`, require a `session_link` edge (`not_reachable`
  else), envelope-encode `kind`/`payload`, admit recipient run with derived
  submission id; the `to: child` case subsumes the old `agent_task`.
- `report_back`: inject the report-back instruction into the child input (no
  config patch).
- Idempotency by recipient `submission_id`.

### S5. Tests
- Engine unit: a deferred tool call parks the turn; `ResumeToolCall` resolves the
  exact call and the turn continues; duplicate `ResumeToolCall` is a no-op; a tool
  that returns inline is unaffected.
- Workflow subscription: `subscribe_run` fires exactly once on terminal and is
  removed; **`subscribe_run` against an already-terminal run fires immediately and
  stores nothing** (the race); terminal fan-out re-emits idempotently after a
  simulated restart; a registered subscription (or active wait) **blocks
  continue-as-new** at idle.
- Workflow loop/timer: the select resolves a `resume_wait` signal and an armed
  `ctx.timer` into `ResumeToolCall`; `ctx.timer` fires `timeout` only when
  `timeout_ms` is set, and **no timer is armed when it is absent** (indefinite
  wait); timer-vs-signal race resolves exactly once by `call_id`.
- `agent_wait` outcomes: requires `run_id`; already-terminal → `terminal`
  immediately without parking; unknown run / errored-or-unreachable target →
  `error`; closing session → `terminal` (last status) or `error`; never a hang.
- `agent_send` routing: `to: session` to a spawned child admits a run (old
  `agent_task` behavior); `to: parent` resolves the link and admits a parent run;
  `to: parent` from a root session and a send with no link edge both return
  `not_reachable`; `kind`/`payload` round-trip through the envelope; retried send
  does not double-admit.
- `report_back`: injects the instruction into the child input; does **not** mutate
  child tool config.
- Mode I in-process: parent spawns a child with `report_back`, goes idle, child
  `agent_send { to: parent }` wakes it as a fresh run.
- Mode W in-process: parent spawns a child, `agent_wait(run_id)`, child completes,
  the target's `RunSubscription` resolves the parent's parked turn with the run
  summary.
- Ignored Temporal/Postgres live: a real parent workflow parks on `agent_wait`, a
  child workflow completes and signals the parent via `RunSubscription`, the parent
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
- `agent_wait` resolves with `outcome ∈ { terminal, timeout, error }` — never a
  raised tool failure for those states. `timeout_ms` is optional with no default
  (indefinite when omitted); timeout is a normal outcome.
- `agent_wait` **requires `run_id`**; already-terminal / unknown / closing /
  errored targets resolve immediately with the defined `outcome`, never a hang.
  The **subscribe-after-terminal** race is closed (registering against an
  already-terminal run fires immediately).
- Waiting uses a **generic, fire-once `RunSubscription`** on the target workflow
  that any subscriber workflow can use — not an `agent_wait`-specific or
  session-only construct. No `persistent` mode is stored or tested in v1.
- The parent **workflow loop selects over admissions and armed timers** (not
  `pending_admissions` alone), draining resolutions into `ResumeToolCall`. A
  registered subscription or active wait **blocks idle continue-as-new** so parked
  state is never dropped.
- The engine gains only a **generic deferred-tool** primitive
  (`Deferred { call_id, opaque resume_directive }` + `ResumeToolCall`); the engine
  never decodes the directive, no Fleet or wait semantics live in the deterministic
  core, and no DB table or reconciler process is introduced.
- Crash safety is by Temporal replay alone: deferred-call reducer fact, target
  workflow subscription state, in-workflow terminal fan-out, and durable timer.
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
