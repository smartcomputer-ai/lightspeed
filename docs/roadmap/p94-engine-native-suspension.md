# P94: Engine-Native Suspension — Typed Awaits, Log-Backed Mailbox, Validated Wakes

**Status**
- Proposed 2026-07-08, while the core API is still in settling mode and
  breaking changes are free.
- Completed 2026-07-08: typed awaits, log-backed mailbox, validated
  `ResumeAwait`, and preflight deletion are implemented.
- Builds on **[P92](p92-unified-suspension.md)** (complete through step 5c,
  including the `RequestRun`/`DeliverMessage` command split) and finishes its
  central claim. P94 also renames `DeliverMessage` to `SubmitMessage`: the
  command records message submission/admission, while actual delivery to a
  parked run is performed by the validated await resume path. P92 §1 says
  semantic state lives in the session log and the workflow keeps only
  recomputable-or-transient transport. Three semantic decisions still live in
  the workflow today: what a parked run waits on (an opaque JSON directive
  only the workflow can decode), what wakes it (satisfaction logic in
  `awaits.rs`, duplicated in the await preflight), and what consumes a
  message (interception of `pending_admissions` before the engine ever sees
  the command). P94 moves all three into the engine.
- Orthogonal to **[P93](p93-fleet-safety.md)** (budgets and the tree index);
  P94 should land before clients build against `session/read`'s view of
  parked runs and pending messages, because it changes what that view can
  show.

## Goal

One invariant, finally true without remainder:

> **The workflow makes no decision that could not be recomputed from the
> session log.**

Concretely: the entire wake predicate for a parked run becomes a pure
function of `CoreAgentState` and the clock; the consume-vs-enqueue rule for
inbound messages becomes engine law rather than workflow behavior; and a
resume is a *claim the engine validates* rather than a result the engine
trusts. The workflow keeps exactly its transport duties — timers, signals,
blob I/O, the loop.

## Current State (Post-P92, The Vestigial Layer)

P84 built a generic deferred-tool-batch primitive: any tool could return
`ToolBatchOutcome::Deferred` with an opaque resume directive, and a generic
`ResumeToolBatch` command later fed its result. P92 then deleted every
deferring tool except `await` — and its own thesis guarantees no new ones
("every future async feature becomes a new *source* behind the same
primitive, never a new primitive": approvals, sub-workflows, timers, and
triggers become promise sources, all awaited through `await`). The generic
machinery now has exactly one client, forever, by design. The costs of
keeping the genericity:

- **The engine cannot read its own suspension state.**
  `batch.parked: Option<ToolBatchResumeDirective>` is
  `{ api_kind, version, body: Value }`. The workflow re-decodes the JSON on
  every loop iteration (`parked_await`), with a documented
  decode-failure-wedges-silently caveat. `session/read` cannot project "this
  run is waiting on promises X, Y with deadline T" without the same decode.
- **Satisfaction logic exists twice.** The await tool's preflight
  (`fleet.rs::await_promises`) replays the session log out-of-band to
  validate promise ids and short-circuit already-satisfied awaits; the
  workflow (`awaits.rs::await_resolution`) implements the same
  mode/deadline/snapshot semantics again for the parked case. Two
  implementations of one contract is drift waiting to happen.
- **The engine trusts the resume.** `ResumeToolBatch` carries a fully-formed
  `ToolInvocationBatchResult`; the engine validates batch identity and
  duplicate resumes but cannot check the *claim* — a buggy resume asserting
  a terminal outcome while every awaited promise still pends would be
  accepted and appended.
- **In-flight messages live outside the log.** A `DeliverMessage` destined
  for a parked mailbox await is intercepted in workflow `pending_admissions`
  and never admitted; the engine learns of it only through
  `consumed_message_submissions` piggy-backed on the resume — a field on the
  generic batch-result type that is only legal on deferred resumes, guarded
  by a runtime check (the representable-but-invalid pattern the 5c command
  split just eliminated one layer up). Until consumption, the message exists
  only in Temporal history: `session/read` cannot show a waiting message,
  and §5's "one rule" for delivery is workflow behavior, not engine law.
- **`ResumeToolBatch` is already await-only in practice.** Ordinary batches
  complete in-process through `resume_tool_batch_outcome`; the *command*
  exists solely for deferred resumes. The generic name describes a client
  that no longer exists.

## Design

### 1. The mailbox is engine state

`SubmitMessage` is the P94 name for P92's `DeliverMessage`. It is always
admitted; the engine decides its disposition:

```
SubmitMessage admission, engine branch:
  active run parked with mailbox: true  ->  Message(Buffered { submission_id, digest, input })
  otherwise                             ->  Run(Accepted { origin: message })
```

Buffered messages are session-log events — durable at delivery, rebuilt at
bootstrap, projectable in `session/read`. Each message has a typed
lifecycle, and every path out of it is an engine rule:

```
Buffered -> ConsumedByAwait(run_id)     // a mailbox wake delivered it (§3)
         -> PromotedToRun(run_id)       // flushed to a queued run
         -> Cancelled                   // force-close / session teardown
```

The flush rule: **when a run reaches any terminal state without consuming
the buffer, buffered messages promote to queued runs** (in arrival order,
preserving their submission ids and origin). This preserves today's
cancel-wins semantics — a wake with reason `Cancelled` leaves the buffer
intact, and the messages surface as runs once the run goes terminal —
as engine law instead of a property of which workflow queue the admission
happened to sit in. Force-close cancels buffered messages along with the
queue; no message outlives the session. No new close rule is needed: while
a run is active the close guard already refuses, and by the time close is
evaluable the buffer has flushed to queued runs, which are active work.

Consequences:

- `MessageSubmissionConsumed` and the `consumed_message_submissions`
  bookkeeping dissolve into the message lifecycle: consumption is a
  transition on the message, not a bolt-on record. Submission idempotency
  (log-backed since 5b/5c) is served by the buffer and its terminal states;
  the dedupe matchers gain a buffered arm and lose nothing.
- The workflow's `take_pending_mailbox_deliveries` and
  `mailbox_delivery_input` are deleted. There is no interception layer: the
  §5 delivery rule is decided at admission, deterministically, testable in
  engine unit tests.
- A recovered or force-closed session can no longer silently lose in-flight
  messages: they are in the log like everything else.

The cost is one small append per buffered message. Messages are semantic
state; per §1 they belong in the log. This was the one place P92 still
disagreed with itself.

### 2. Await is an engine primitive, invoked through a tool

The model interface stays a tool call — that is how models act. Everything
after argument parsing moves into the engine.

**Typed park, hoisted to the run.** `ToolBatchResumeDirective` (opaque
`api_kind`/`version`/JSON body) and `AWAIT_DIRECTIVE_KIND` are deleted. The
park becomes a first-class, typed field where §4's state machine already
put it conceptually:

```
ActiveRun.parked: Option<ParkedAwait {
  batch_id, call_id,
  spec: AwaitSpec { promise_ids, mode: any|all, deadline_at_ms?, mailbox: bool },
}>
```

`parked_await()` in the workflow becomes a field read. The
decode-failure-wedge class disappears, and `session/read` can project
"parked on promises X, Y until T" directly.

**The await executor becomes an argument parser.** The preflight's
out-of-band store replay is deleted. Validation happens at defer admission,
where the engine owns the promises:

- Unknown or foreign promise ids → the await **tool call fails** (an error
  result in the completed batch, no park). Could-never-resolve parks remain
  unrepresentable, now enforced by the owner of the truth.
- More than one await call in a batch → all await calls in that batch fail
  as tool errors and the batch completes without parking. One run parks on
  one await; a multi-wait is one await with several promise ids.
- Mixed batches keep P92 step 4 semantics: non-await results are recorded
  at deferral; the run parks on the await alone.

**The already-satisfied fast path is deleted.** An await over
already-terminal promises (or `timeout_ms: 0`) parks and wakes on the next
loop iteration through the same path as every other wake. One resolution
code path replaces two implementations of the same semantics. The cost is
one extra loop hop for the fast case; the benefit is that the satisfaction
contract exists in exactly one place:

**The wake predicate is one pure engine function.**

```
fn await_wake(state: &CoreAgentState, now_ms: u64) -> Option<WakeReason>

WakeReason = Cancelled | MailboxMessage | Timeout | Terminal
```

Precedence (unchanged from the verified P92 behavior): run
`Cancelling`/`CancellingGrace` wins over everything; then a non-empty
message buffer if `spec.mailbox`; then the deadline; then mode-satisfaction
over promise state. Every input — run status, buffer, deadline, promises —
is `CoreAgentState`, so the whole predicate is property-testable in the
engine with no workflow harness, and the workflow calls the same function
it will later be held to (§3).

### 3. `ResumeToolBatch` becomes `ResumeAwait`: workflow proposes, engine disposes

The resume command is renamed to what it already is, and its contract
inverts from "trust these results" to "validate this claim":

```rust
CoreAgentCommand::ResumeAwait(ResumeAwaitCommand {
    run_id,
    batch_id,
    claim: WakeReason,
    output: AwaitOutputRefs,   // output + summary blobs, pre-written by the runtime
})
```

At admission the engine recomputes `await_wake(state, observed_at_ms)` and
**rejects on mismatch** — if a cancel landed between the workflow computing
the claim and the admission, the resume is refused and the workflow simply
recomputes on its next iteration (the loop already has this shape, and the
cancelling watchdog still backstops the whole region). Duplicate resumes
stay idempotent no-ops. On acceptance the engine emits everything semantic
itself:

- the batch-resumed transition and the per-promise snapshot, derived from
  its own promise state (it holds the `payload_ref`s/`error_ref`s);
- for a `MailboxMessage` wake, the `ConsumedByAwait` transitions and the
  woken run's context entries, built from the buffered messages' input —
  whose content refs were written at `SubmitMessage` submission, so no new
  I/O is needed at wake time;
- the await tool-call completion referencing the runtime-supplied output
  blobs.

The runtime's one irreducible contribution is blob I/O: the machine-readable
`AwaitOutput` and the human-readable summary must exist in CAS before the
append that references them, and the engine does no I/O. Those blobs are
model-visible sugar; the engine validates the *semantic* claim, not the
prose. A stale blob after a claim mismatch is wasted CAS bytes, not a
correctness problem.

What this closes: today a resume asserting `Terminal` against pending
promises would be accepted. After P94 it is a rejected admission — the
engine is the arbiter of its own suspension semantics.

### 4. What is deleted, what is deliberately untouched

Deleted in full: `ToolBatchResumeDirective`, `AWAIT_DIRECTIVE_KIND`, the
`parked_await` JSON decode, the await preflight's store replay, the
duplicated satisfaction/snapshot logic, `mailbox_delivery_input`,
`take_pending_mailbox_deliveries`, the `consumed_message_submissions` field
on `ToolInvocationBatchResult` (and its only-on-resumes validation), the
`MessageSubmissionConsumed` bolt-on (subsumed by the message lifecycle),
and the `ResumeToolBatch` command.

Deliberately untouched:

- **The `ResolvePromise` funnel and the two-hop split.** Resolution must
  stay uniform across detached promises (which resume nothing) and awaited
  promises; P92's appendix defense stands, and the race-safety of the
  two-hop was verified. `ResolvePromise` still never assumes a batch to
  resume; `ResumeAwait` is still a separate, independently-guarded hop.
- **The `RequestRun`/message command pair** (5c). P94 preserves the split
  from P92, but renames `DeliverMessage` to `SubmitMessage` because the
  command records the inbound message fact; `ResumeAwait` performs actual
  delivery to a parked run when the mailbox wake is accepted.
- **Watchdogs, grace semantics, force-close, the reaper, CAN gating.** The
  buffer and the typed park are log state, so they are CAN-portable by
  construction — strictly better than today, where the parked directive
  survives CAN only as an opaque blob. Timer arming simplifies: the nearest
  await deadline is a typed field read.
- **The workflow's transport duties**: clock and timers, signal delivery,
  blob writes, the drive loop. That list is now exhaustive.

## Implementation Plan

Greenfield, breaking, P92 rules: each step deletes what it replaces in the
same change. Steps are independently shippable; 1 and 2 may be collapsed.

Status as of 2026-07-08: all three implementation steps are complete.

1. **Typed park + `ResumeAwait`.** Done. `AwaitSpec`/`ParkedAwait` on the run,
   defer admission validates promise ids and the one-await-per-batch rule,
   `ResumeAwait` with claim validation replaces `ResumeToolBatch`.
   The workflow now queues `ResumeAwait` with typed output refs and a wake
   observation timestamp.
2. **Engine mailbox.** Done. `Message(Buffered)` admission branch, the message
   lifecycle events, the terminal-flush and force-close rules; `ResumeAwait`
   sheds its message payload and consumes from the buffer; the workflow
   interception layer is deleted; dedupe matchers gain the buffered arm.
   Workflow status still exposes consumed submissions as a compatibility
   projection derived from engine message lifecycle.
3. **Preflight deletion.** Done. The await executor becomes an argument parser;
   the already-satisfied fast path and the store replay are removed;
   `await_wake` is the single satisfaction implementation, exported for the
   workflow's satisfied-check.

## Tests

- **`await_wake` property tests** (engine, no workflow harness): precedence
  order under every combination of cancelling status, buffered messages,
  expired deadline, and mode satisfaction; totality (a parked run with any
  wake-relevant state change produces `Some`); stability (no wake reason
  from irrelevant state changes).
- **Claim validation**: `ResumeAwait` with a stale claim (cancel landed
  after the workflow computed `Terminal`) is rejected and the next
  iteration resumes with `Cancelled`; duplicate resumes are no-ops; a
  fabricated `Terminal` claim against pending promises is rejected.
- **Defer admission**: unknown promise id fails the await call without
  parking; two awaits in one batch fail both without parking; mixed batches
  park on the await with other results recorded.
- **Message lifecycle**: delivery while parked-with-mailbox buffers; while
  active/idle becomes an origin-message run; buffer flushes to queued runs
  on run terminal without consumption (cancel-wins preserved); force-close
  cancels the buffer; no message reaches two terminal states (property:
  consumed ⊕ promoted ⊕ cancelled).
- **Idempotency across the buffer**: retried `SubmitMessage` against a
  buffered/consumed/promoted submission id is accepted idempotently;
  mismatched digest rejects — including after continue-as-new, from log
  state alone.
- **Log-rebuild equivalence** (extends P92's): typed park, buffer contents,
  and message lifecycle states reconstructed after CAN match pre-CAN state;
  `session/read` projects parked spec and pending messages from the log
  with no workflow query.
- **Fast-path removal regression**: `await` over an already-resolved
  promise and `timeout_ms: 0` both return correct total outcomes via the
  uniform park/wake path within one loop iteration.

## Key Decisions

- **Messages are semantic state, so they live in the log** — the buffer
  trades one append per message for §5-as-engine-law, `session/read`
  visibility, and loss-proof recovery (§1).
- **The message lifecycle subsumes consumption bookkeeping** — consumed-ness
  is a transition, not a side table (§1).
- **Await is the engine's suspension primitive; the tool is just its
  syntax** — typed spec on the run, validation where the promises live
  (§2).
- **One satisfaction implementation** — the preflight fast path is deleted;
  an extra loop hop for already-satisfied awaits buys a single, pure,
  property-testable wake predicate (§2).
- **Workflow proposes, engine disposes** — resumes carry claims, the engine
  recomputes and rejects mismatches; trust-the-blob resumes end (§3).
- **Blob I/O is the runtime's only irreducible role in a wake** — the
  engine validates semantics, never prose; stale blobs after a rejected
  claim are garbage, not corruption (§3).
- **The two-hop `ResolvePromise`/`ResumeAwait` split stays** — resolution
  is uniform across detached and awaited promises; P92's appendix argument
  is unchanged (§4).
- **Genericity is not kept for clients that cannot exist** — P92's thesis
  (new async features are promise sources, never new suspension primitives)
  is taken at its word; if it is ever revised, a typed enum variant beats
  an opaque JSON body (§4, Current State).
