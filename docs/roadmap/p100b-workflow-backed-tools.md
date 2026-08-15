# P100b: Workflow-Backed Tools — Bound Receivers, Keyed Promise Sets, And Start-On-Call Workflows

**Status**
- Follow-on [P106](p106-joined-workflow-tools.md) proposed 2026-07-30 after
  production Channels usage exposed the missing single-result completion
  form. P106 preserves P100b's Promise transport and source protocol but
  supersedes two scope decisions for new bindings: bound dispatch becomes
  explicit rather than derived from completion, and `Joined` may durably park
  the original workflow-tool call without a model-authored `await`. This
  document remains the historical P100b baseline.
- Controller self-receiver addendum implemented 2026-07-28 for the first
  concrete re-entrant consumer, Channels:
  - replaced the blanket rejection of `Bound + Promises` targeting the
    session's lifecycle controller with the explicit self-receiver progress
    contract in this document;
  - requires a non-zero hard Promise deadline for that topology, so a broken
    receiver cannot park its own managed run forever;
  - requires the controller to accept, durably deduplicate, and process pushed
    invocations independently of run-terminal handling and without starting
    or awaiting new work in the emitting session;
  - adds a live proof in which one workflow is both lifecycle controller and
    tool receiver, resolves a provider-acknowledged delivery Promise while
    the run is active, and subsequently observes the run-terminal notification.
  B7 removes the blanket admission rejection, enforces the deadline during
  admission and durable replay, and extends the serial live plugin suite to
  13 scenarios. The Channels-shaped controller proof covers mid-run delivery
  and resolution, duplicate push and resolution, controller continue-as-new,
  terminal notification ordering, a stalled controller deadline, and an
  absent controller receiver.
- Environment-job contract addendum implemented 2026-07-28 after auditing
  whether the core job system should become the first production plugin:
  - `WorkflowToolCompletion::Promises` now has one required declarative
    `key_source`: `Reply`, `StringArray { pointer }`, or
    `ArrayIndices { pointer, prefix }`. The last shape lets a validated array
    of objects create one Promise per item without a redundant model-supplied
    key array or domain-specific key derivation in the session worker.
  - The binding fingerprint domain was v2 when the key-source declaration
    landed; P103's removal of the unused `FunctionToolSpec.model_name` alias
    advances the greenfield binding encoding/domain to v3.
  - Start-on-call acknowledgement is explicitly an asynchronous workflow
    acceptance boundary. It returns generic invocation/execution identities
    and keyed Promises; it does not wait for a provider or return
    domain-specific handles. An environment-job workflow may return stable
    handles and results through per-item Promise payloads and the core job
    read/list APIs.
  - A started workflow must accept per-key cancellation emissions,
    map them to its own work items, and make execution cancellation clean up
    external resources, including start-versus-cancel races. P100b owns
    cancellation delivery and exact-execution cancellation; the producing
    workflow owns domain cleanup.
  - Durable core resources that remain addressable outside a session may
    store an opaque controller reference. That locator is domain/runtime
    state, not a new P100b protocol or a public resource-handle field.
- Core environment-job adoption implemented 2026-07-28 as a follow-on use of
  the completed P100b primitives: model-visible `job_submit` is an immutable
  core-owned start binding with `ArrayIndices { pointer: "/jobs", prefix:
  "job-" }`; `EnvironmentJobWorkflow` accepts the generic start envelope,
  resolves and recovers keyed Promises, and handles per-key and exact-execution
  cancellation. The obsolete `PromiseSource::EnvJob`, source subscription,
  polling/cancellation activities, and direct session-worker start path were
  deleted. Job storage, APIs, host integration, and workflows remain core;
  the workflow's short, idempotent environment-adapter calls run as local
  activities on that co-located core worker. Config-only Fleet clones preserve
  the immutable managed workflow-tool creation event and restart with that
  exact declaration, so cloned tool surfaces cannot lose their durable
  bindings.
  The live host-bridge proof starts two jobs in one call, awaits both keyed
  Promises, recovers both stable handles, and reads both terminal outputs.
- Proposed 2026-07-25.
- Revised 2026-07-25 after design review; the following are settled design
  decisions, not open questions:
  - Vocabulary is unified with P100's rename: one **workflow tool** family
    (`WorkflowTool*`), no "port" noun.
  - `BoundDeliveryMode` is deleted. **Completion determines delivery**:
    `Accepted` tools are pull-consumed at the run boundary; promise-bearing
    tools are pushed. Push-notify and ordered pushed notify streams are
    deferred until a mid-run notify consumer exists.
  - Completion is a **keyed promise set**: one invocation atomically creates
    zero or more promises keyed by completion keys derived from validated
    arguments. Request/reply is the single-key case (reserved key `reply`).
    The per-invocation key-count cap is an admission-time resource guardrail,
    not a semantic.
  - The two previously proposed promise sources merge into one
    `PromiseSource::Workflow { producer, invocation_id, key }`. Cancellation
    behavior is derived from the durable binding's target lifecycle, not from
    a second source variant.
  - v1 push is slimmed to independent per-emission delivery with bounded
    retry and terminal failure. Per-receiver FIFO, head-of-line retry, and
    the log-sequence high-water dedup rule move to the deferred
    ordered-notify-stream mode.
- Design-first: accepting the primitive contract is an explicit gate before
  any runtime implementation begins.
- B1/B2/B3 implemented 2026-07-26 (bound targets end to end; start targets
  admit but are not yet invocable):
  - Engine: `WorkflowToolTarget` (Bound/Start), `WorkflowToolCompletion`
    (Accepted/Promises with `reply_schema_ref`, `deadline_after_ms`,
    `max_promises`, and a declarative completion-key source), fingerprint encoding
    bumped to v2 with goldens repinned, deterministic per-key promise ids
    (`wtp:sha256:` over invocation id + key), unified
    `PromiseSource::Workflow`, atomic
    `Tool::CallCompleted + Promise::Created × N + Emitted` append with the
    Emitted apply re-verifying every keyed promise and source, and the
    `FailWorkflowToolDelivery` command (terminal `DeliveryFailed` + fail
    still-pending promises in one append, idempotent retry).
  - Tools: declarative key derivation from validated arguments, a keyed
    `promises` map in the acknowledgement, and absolute deadline conversion
    at invocation; start-target invocation rejected until B4.
  - Temporal: promise-bearing `Emitted` entries enqueue push delivery on
    the existing flush spine with independent per-entry deterministic
    bounded retry (5 attempts, linear backoff) and terminal failure;
    received source-resolutions defer to a main-loop stage that authorizes
    the exact stored producer and validates optional reply schemas through
    the new bounded `validate_workflow_tool_reply` activity before ordinary
    `ResolvePromise` admission; per-key cancellation rides a new
    `EmissionBody::InvocationCancellation` best-effort fact to the bound
    receiver. Continue-as-new gates on all new pending state.
  - API/projection: `workflowToolEmitted` exposes the keyed
    `completionPromises` map; contract artifacts and both TypeScript
    consumers regenerated and verified.
  - B4 implemented 2026-07-26 (same review cycle): start-on-call end to
    end. Engine: `WorkflowTool::StartRequested { invocation, execution_id }`
    (execution id re-validated against the canonical
    `wtx:sha256:`(invocation id + recipe fingerprint) derivation at apply)
    and terminal `StartFailed`; durable `start_requests`/`start_failures`
    state; the shared per-run/per-tool cap covers both emissions and start
    intents; keyed promise sources for start targets name the derived
    execution with kind `workflow_tool.execution`; the
    `FailWorkflowToolStart` command fails still-pending promises atomically
    and is idempotent. Temporal: pending start work is **recomputed from
    durable state** (start intents with pending promises and no terminal
    failure), so replay and continue-as-new rebuild it with no transport
    bookkeeping; deterministic re-issue treats `AlreadyStarted` as success;
    bounded retry then terminal failure; a slow (10s) recovery poll
    describes the execution and, when closed, recovers keyed results
    through the fixed versioned `workflow_tool_recovery` query — completion
    without a valid keyed result fails the promise as a contract violation;
    when the last pending key of an owned execution turns terminal, the
    exact execution is cancelled best-effort. Server: recipe format v1
    (`{workflowType, taskQueue}` JSON, CAS-backed, fingerprint-verified
    with `wtr:sha256:` over the recipe bytes) resolved in the generic
    adapter; the start uses the untyped client API, so **no plugin workflow
    type is compiled into the session worker**. Tools: start-target calls
    acknowledge `{accepted, invocationId, executionId, promises}`.
  - B5/B6 closed 2026-07-26 with the live plugin suite
    (`workflow_tool_plugins_live.rs`, eleven serial scenarios, all green
    alongside `temporal_live` and `environment_provider_live`); closing B6
    added hard-promise-deadline enforcement to the session workflow and the
    concurrency-toolset derivation for promise-bearing bindings. The original
    B1-B6 scope was complete at that point; per-item descopes are recorded in
    B6, and the receiver-side plugin helper surface remains a follow-on (the
    test plugins used plain workflow code against the fixed contracts).
- Follow-on to **P100 (Workflow Emissions And Workflow Tools)**. P100 remains
  the complete notify/pull substrate; this proposal owns push delivery,
  promise-bearing completion, and start-on-call workflow tools.
- Builds on **P92 (Unified Suspension)** for Promises, `await`, cancellation,
  detach, deadlines, and the single `ResolvePromise` admission funnel.
- Uses the existing P100 `EmissionEnvelope`, `WorkflowToolInvocation`,
  immutable trusted bindings, fixed `deliver_emission` signal, source-log
  identity, and terminal `DeliveryFailed` event. It does not create a second
  workflow communication protocol.
- P101 Durable Work does not depend on P100b. Its `work_report` tool remains a
  pull-consumed P100 notify tool.
- P100b migrates no existing feature or tool family. The deferred
  **[P102 (Workflow Plugin Extractions)](p102-workflow-plugin-extractions.md)**
  owns later migrations of genuinely independent plugin domains. The
  environment system and its jobs remain core after the 2026-07-28 boundary
  review. Jobs are nevertheless the primary design pressure on the completion
  contract (see the keyed promise set section). The 2026-07-28 internal
  adoption removed bespoke `PromiseSource::EnvJob` and session routing without
  moving job workflows, storage, APIs, or host integration out of core.

## Goal

Let trusted workflow plugins expose tools to managed agent sessions without
compiling plugin-specific implementations into the session worker and without
adding an HTTP, dynamic-library, WASM, or arbitrary-activity plugin protocol.

A plugin runs in its own Temporal worker:

- bound tools send typed invocations to a workflow execution the plugin
  created before session admission;
- start-on-call tools start a workflow type registered by the plugin worker;
- the plugin workflow performs side effects through its own activities;
- results return through P92 Promises and P100 emissions.

The session core understands only tool declarations, immutable authority,
invocation identity, workflow endpoints, workflow-start intent, Promises, and
terminal facts. It does not understand messaging, approvals, environment jobs,
or any later plugin domain.

The explicit architectural goal is to make later deletion of feature-specific
session-core and session-worker code possible. P100b proves that generic
boundary with minimal test plugins; it does not mix the primitive design with
the behavior-preservation work of migrating existing systems.

## Decision

Workflow-backed tools have two independent axes.

### Axis 1: target lifecycle

1. **Bound workflow execution** — a specific workflow execution already exists
   before the tool call. Its opaque `WorkflowEndpointRef` is fixed by trusted
   managed-session admission. The call sends an invocation to that execution.
2. **Start new workflow execution** — no receiver execution exists for this
   call. The admitted binding contains a trusted, substrate-neutral start
   reference. The workflow adapter starts one execution for the invocation.

“Bound” refers to a workflow **execution**, not a workflow type compiled into
the main worker.

### Axis 2: completion

1. **Accepted** — the tool completes when its durable invocation or start
   intent is recorded. The receiver reacts independently.
2. **Promises** — the same append also creates a keyed set of P92 Promises
   (possibly a single one). The receiver or newly started workflow eventually
   resolves each keyed promise. The agent may use the ordinary `await`,
   `cancel`, or `detach` tools per promise.

The conceptual law: **completion is a set of keyed promises, possibly
empty.** Notify is the empty set; request/reply is the singleton set with the
reserved key `reply`; job-like tools carry one key per validated work item.
The explicit `Accepted` variant is kept in the durable declaration so notify
bindings state their intent, but no third completion shape exists.

### Delivery is derived, not declared

There is no per-binding delivery mode. `Accepted` tools are consumed by
P100's pull read at the run-terminal boundary. Promise-bearing tools are
pushed, because the agent may park mid-run on a promise the receiver can only
resolve if it sees the invocation mid-run; pull-at-boundary would deadlock.
Push for `Accepted` tools (mid-run FYI) has no consumer and is deferred with
the ordered-notify-stream mode.

The resulting matrix is:

| Target lifecycle | Accepted | Promises |
|---|---|---|
| Bound workflow execution | P100 notify tool (pull) | P100b request/reply and keyed-set tools (push) |
| Start new workflow execution | valid shape, deferred | P100b workflow-as-tool (start args) |

P100b implements the two promise-bearing cells and the push transport
required by bound promise-bearing tools. Fire-and-forget workflow start
remains deferred until it has a consumer. Start-target invocations do not use
push at all: the invocation travels in the start arguments.

## Why There Is No Generic Plugin Executor

A “generic executor” must run somewhere:

1. compiled into the session worker;
2. loaded through a dynamic code ABI;
3. contacted over a new RPC protocol;
4. scheduled as a dynamically addressed activity; or
5. hosted behind a workflow.

The first violates the stable-session-worker goal. The next three create
another plugin protocol and authorization surface. P100b chooses the fifth:
the public plugin boundary is a workflow; activities remain private
implementation details of the plugin.

```text
AgentSessionWorkflow                 plugin Temporal worker
--------------------                 ----------------------
typed admitted tool
  -> P100 emission ----------------> bound PluginWorkflow
                                      -> plugin activity
                                      -> keyed reply emission(s)

typed admitted start
  -> workflow start ---------------> new PluginWorkflow execution
                                      -> plugin activities
                                      -> keyed terminal reply emission(s)
```

Temporal is the only cross-process workflow protocol. A plugin may use its own
database, provider APIs, or Lightspeed capability services inside activities,
but the session worker never speaks a plugin-specific transport.

### Why start-on-call is not a Temporal child workflow

Considered and rejected (2026-07-25): modeling start-on-call as a Temporal
child workflow on the plugin's task queue. Child workflows would provide
already-started deduplication and parent-close cancellation natively, but
they deliver results through a second mechanism (child completion futures in
the parent's history) beside the one emission spine, couple the session
workflow's history to child lifetimes across continue-as-new, and are
Temporal-specific in exactly the place the engine contract must stay
substrate-neutral. External start with a deterministic execution id, terminal
result emission over the shared spine, and explicit cancellation of the exact
derived execution reproduces the useful properties with one result path.

## Binding Model

The model sees only the admitted tool name, description, and input schema. It
never supplies a workflow id, workflow type, task queue, start recipe,
completion mode, completion key, Promise source, universe, or signal name.

Conceptually, a workflow-backed tool binding adds two fields to P100's durable
declaration:

```rust
pub enum WorkflowToolTarget {
    Bound {
        receiver: WorkflowEndpointRef,
    },
    Start {
        start: WorkflowStartRef,
    },
}

pub enum WorkflowToolCompletion {
    Accepted,
    Promises {
        /// Schema every keyed resolution payload must satisfy.
        reply_schema_ref: Option<BlobRef>,
        /// Trusted relative hard deadline applied to each created promise.
        deadline_after_ms: Option<u64>,
        /// Admission-time resource guardrail (>= 1, deployment-ceilinged),
        /// not a semantic. Request/reply tools declare 1.
        max_promises: u32,
        /// Declarative key derivation from schema-validated arguments. This
        /// keeps derivation data-driven: no plugin code in the session
        /// worker, and the model influences the key count only through
        /// validated arguments.
        key_source: WorkflowToolCompletionKeySource,
    },
}

pub enum WorkflowToolCompletionKeySource {
    Reply,
    StringArray { pointer: String },
    ArrayIndices { pointer: String, prefix: String },
}

pub struct WorkflowStartRef {
    /// Versioned recipe encoding identifier. Identifies the generic recipe
    /// codec, never a feature, service, or workflow plugin.
    pub recipe_format: u32,
    pub revision: u32,
    pub recipe_ref: BlobRef,
    pub recipe_fingerprint: String,
}
```

The exact Rust shape preserves P100's existing
`WorkflowToolDefinition`/`WorkflowToolBinding` types and extends them. The
durable semantics are normative:

- `Bound + Accepted` is exactly P100 v1 (pull);
- `Bound + Promises` is pushed request/reply or keyed-set completion;
- `Start + Promises` creates a workflow start intent rather than a push
  delivery;
- `Start + Accepted` is a valid shape, deferred without a consumer;
- binding fingerprints cover target lifecycle, completion mode, reply schema,
  `max_promises`, deadline, start reference, and all existing P100
  definition fields;
- every field is immutable for the managed session's lifetime.

`WorkflowStartRef` is substrate-neutral in `engine`. A Temporal adapter
validates and resolves a versioned recipe containing the workflow type and
task queue. `recipe_format` identifies the generic recipe codec, never a
feature, service, or workflow plugin. P100b v1 has one Temporal recipe
format; adding another plugin creates data using that format and never adds a
compiled registry branch.

P100b may add one generic runtime dispatch shape and one generic Temporal
recipe resolver for start-on-call bindings. That is substrate infrastructure
added once; installing another plugin must not add another dispatch variant,
tool-name branch, recipe format, or resolver branch. Bound tools continue to
use the generic workflow-tool dispatch.

The binding is admitted only by the same trusted managed-session creation
boundary as P100 tools. Ordinary config writes cannot add, change, or
retarget it.

## Keyed Promise Sets

The completion contract in full:

1. At invocation, the tool runtime derives the **completion keys** from the
   binding's declarative `key_source`. `Reply` yields the reserved singleton
   key `reply`; `StringArray` reads unique strings at a JSON Pointer; and
   `ArrayIndices` derives `prefix + zero-based-index` for every item in a
   schema-validated array. Keys are bounded ASCII identifiers, unique within
   the invocation. The typed internal effect enumerates them; the model
   influences the key count only through schema-validated arguments.
2. The engine validates key uniqueness and `count <= max_promises`. A
   violating call is converted to an ordinary failed tool result before
   `Tool::CallCompleted` is proposed — it creates no emission and no
   promises, exactly like P100's schema and emission-cap failures.
3. One append atomically records `Tool::CallCompleted`,
   `WorkflowTool::Emitted`, and `Promise::Created` for every key. Each
   promise id is deterministically derived from the invocation id and its
   key, so replay and retry re-derive identical ids.
4. The acknowledgement returns the key→promise map:

```json
{
  "accepted": true,
  "invocationId": "wti:sha256:...",
  "promises": { "reply": "promise_..." }
}
```

5. Each created promise is an ordinary P92 promise — individually awaitable,
   cancellable, detachable, deadline-bearing, run-scoped by default. No new
   suspension vocabulary exists.
6. Each promise's source is the unified workflow source (next section). The
   receiver resolves promises one at a time by naming the promise id; keys
   correlate, they do not route.

**Why `max_promises` is a guardrail, not a semantic.** Nothing in the
completion semantics changes if the bound is removed; it exists because
promise count is argument-driven and promises are durable reducer state the
session tracks until terminal and iterates for run-scoped auto-cancel. It is
the same class of limit as P100's per-run/per-tool emission cap and is
enforced the same way.

**Why the set exists at all.** The one-invocation/one-promise draft could not
express P86 environment jobs — one `job_submit` call starts several jobs, each
with its own promise, resolved over time by one group workflow. The bound
launcher pattern cannot rescue that shape: receivers can only *resolve*
promises, never create them in the session, so per-job promises simply could
not exist. Rather than discover this at internal-adoption time, the completion
contract carries the keyed set from day one; request/reply degenerates to one
key and pays nothing for the generality.

## Workflow Identity

### Bound target

The plugin owner chooses any valid bound workflow id. P100 and P100b treat it
as opaque. It may identify a per-session, per-channel, per-account, sharded,
or deployment-scoped execution. Universe scope is carried separately by the
managed-session binding and emission envelope.

The bound workflow must be open and able to consume P100 envelopes before the
session is admitted. If it is absent or closed when push delivery is
attempted, normal bounded retry and terminal failure apply.

### Start target

Direct start-on-call gives the session structured-concurrency ownership of the
new execution. Its execution id is therefore system-derived from:

- source universe;
- source session, run, turn, batch, and tool-call identity;
- binding fingerprint; and
- workflow-start recipe fingerprint.

The same invocation always derives the same execution id. Retry treats
Temporal `AlreadyStarted` as success only for that exact identity. A
conflicting execution is an invariant violation and fails the invocation's
promises.

A plugin that requires a domain-chosen workflow id — or wants to coalesce
many calls into one long-lived execution — uses a **bound** tool against a
workflow it created itself, and may start child workflows under its own state
machine. P100b does not add workflow-id templates or model-authored ids.

## Invocation Semantics

### Bound + Accepted

This is the P100 notify path, unchanged:

```text
tool call
  -> validate arguments
  -> same append:
       Tool::CallCompleted
       WorkflowTool::Emitted
  -> immediate { accepted, invocationId }
  -> receiver consumes by pull at the run-terminal boundary
```

### Bound + Promises

A notify emission plus keyed promises in the same append:

```text
tool call
  -> validate arguments, derive completion keys
  -> same append:
       Tool::CallCompleted
       WorkflowTool::Emitted { completion_promises }
       Promise::Created x N {
         source: Workflow {
           producer: bound receiver endpoint,
           invocation_id,
           key
         }
       }
  -> immediate { accepted, invocationId, promises }
  -> push ToolInvocation envelope to the bound receiver
  -> receiver emits SourceResolution per keyed promise
  -> validate producer and optional reply schema
  -> ResolvePromise admission
  -> agent may await/cancel/detach each promise through P92
```

The tool activity never waits for the handler. The immediate tool output is a
transport acknowledgement; the semantic results are the promise resolutions.
There is no `workflow_tool_wait` tool and no implicit await.

The receiver may resolve each keyed promise as:

- `Resolved { payload_ref }`;
- `Failed { error_ref }`; or
- `Cancelled`.

First terminal resolution per promise wins. Duplicate terminal emissions are
no-ops under the existing P92 rule.

### Start + Promises

Workflow-as-tool records intent before performing the workflow start:

```text
tool call
  -> validate arguments, derive completion keys
  -> same append:
       Tool::CallCompleted
       WorkflowTool::StartRequested
       Promise::Created x N {
         source: Workflow {
           producer: derived execution endpoint,
           invocation_id,
           key
         }
       }
  -> immediate { accepted, invocationId, executionId, promises }
  -> replay-rebuilt start queue issues deterministic workflow start
  -> plugin workflow receives invocation + holder endpoint + promise keys
  -> plugin workflow performs activities
  -> plugin emits SourceResolution per keyed promise as work completes
  -> ResolvePromise admission
```

There is no successful `Started` event in the session log. Temporal history is
the durable record of the start command, the deterministic execution id makes
retry safe, and `AlreadyStarted` is the recovery path. An exhausted or
non-retryable start failure appends terminal `StartFailed` and fails all of
the invocation's still-pending promises atomically.

The v1 plugin contract produces one terminal `SourceResolution` per key; the
delivery and recovery rules are specified below. P100b does not infer
semantics from arbitrary workflow return values. A future early-reply mode,
where the workflow continues after its semantic results, must be explicit.

## Reply Payloads And Schemas

The provider-visible function tool's immediate result remains the
transport-owned acknowledgement. Promise-bearing bindings therefore carry a
separate optional `reply_schema_ref` that every keyed resolution payload must
satisfy.

Before admitting `ResolvePromise::Resolved`, the session workflow adapter:

1. verifies the resolving producer is authorized for the Promise source;
2. loads the CAS payload through a bounded activity when a reply schema
   exists;
3. validates it against the immutable admitted schema; and
4. resolves the Promise or fails it with a stable validation error.

The deterministic engine never performs CAS I/O. A receiver cannot bypass
reply validation by returning an arbitrary blob reference.

Failure detail remains CAS-backed and bounded. Neither request arguments nor
reply payloads are copied into delivery bookkeeping or projection summaries.

## Producer Authorization

Possession of a Promise id is not authority to resolve it.

Every workflow-tool promise uses one source:

```rust
PromiseSource::Workflow {
    /// The only endpoint authorized to resolve this promise: the admitted
    /// bound receiver, or the system-derived started execution.
    producer: WorkflowEndpointRef,
    invocation_id: WorkflowToolInvocationId,
    key: String,
}
```

For bound tools, `producer` is the exact receiver endpoint fixed at
managed-session admission. For start-on-call tools, it is the system-derived
execution endpoint. An accepted resolution must come from the workflow id in
the stored `producer` and must name the exact Promise id; the adapter follows
the stored source rather than trusting caller-supplied invocation metadata.
`workflow_kind` remains diagnostic admission metadata, as in P100. Whether
cancellation targets a shared receiver or an owned execution is derived from
the durable binding's target lifecycle — it is not a second source variant.

The session workflow rejects mismatched producers before creating a
`ResolvePromise` admission. This check belongs to the workflow adapter because
the engine receives only trusted commands; reducer invariants still verify the
Promise exists and is pending.

This is source correlation inside a trusted Temporal namespace, not a sandbox
against a malicious worker: `EmissionProducer::Workflow` is asserted in the
envelope because Temporal signals do not provide authenticated caller
identity. P100b admits only trusted plugin workers. Running mutually untrusted
plugins requires namespace isolation or a later authenticated workflow SDK
with a real resolution capability; it must not be implied by these primitives.

## Reply Delivery And Recovery

The producing plugin workflow owns a terminal resolution until the fixed
`deliver_emission` command is durably accepted by the holder workflow. It
retains the resolution in workflow state, retries with the deterministic
emission id, and may discard it only after that acceptance or explicit
cancellation. Duplicate delivery is harmless because each Promise is the
first-terminal-writer idempotency boundary.

A bound receiver can remain alive after emitting replies. A start-on-call
workflow emits its keyed terminal resolutions as work completes and normally
emits the last immediately before completion. It also exposes the same
bounded per-key resolutions through the fixed P100b recovery query so the
generic holder-side monitor can distinguish:

- valid results whose terminal signals were not observed;
- workflow failure or cancellation; and
- contract violation: workflow completion while keyed promises remain
  pending without valid results — which fails those promises.

The recovery query is one versioned transport contract shared by every
start-on-call plugin, not a plugin-defined query or arbitrary workflow return
decoder. Its result re-enters through the same `ResolvePromise` admission
funnel.

## Push Delivery

P100b owns the deferred P100 push slice, slimmed to what promise-bearing
completion actually needs. It reuses the existing shared emission spine:

1. `WorkflowTool::Emitted` for a promise-bearing bound tool creates a
   transient, replay-rebuilt delivery entry.
2. The session workflow sends the existing fixed `deliver_emission` signal to
   the admitted receiver endpoint.
3. Each envelope is delivered independently with deterministic bounded
   retry. There is no cross-invocation ordering guarantee: promise-bearing
   invocations are independent, correlated only by their own invocation id
   and keys.
4. Receivers need no dedicated dedup state: domain state is keyed by
   invocation id, so duplicate delivery is a no-op, and resolution is
   idempotent at the promise.
5. Flush-queue quiescence gates session continue-as-new.
6. Exhausted or non-retryable failure appends
   `WorkflowTool::DeliveryFailed`, removes the queue entry, and fails the
   invocation's still-pending promises in the same append. A dead receiver
   must never leave an unresolvable pending Promise.

`Accepted` bindings do not enter push state. P101 continues to receive
run-terminal and then read earlier reports from the source log.

Deferred with the ordered-notify-stream mode (no current consumer):
per-receiver FIFO in source-log order, head-of-line retry with
order-preserving backoff, and the per-producer `log_seq` high-water dedup
mark.

## Cancellation, Deadlines, And Detach

P92 remains the sole ownership and suspension model.

### Bound promise

Cancelling a keyed workflow-tool Promise first resolves it as cancelled in
the session. The workflow adapter then sends a best-effort, fixed
cancellation fact naming the invocation, key, and Promise to the bound
receiver through the same authorized endpoint. It does not cancel the shared
receiver workflow execution.

The receiver may stop the domain work behind that key. A later reply is
ignored because the Promise is already terminal.

### Started workflow

Cancelling a keyed Promise on a started execution first resolves it as
cancelled in the session, then sends the same best-effort per-key
cancellation fact to the execution. When the last still-pending keyed Promise
of a session-owned started execution reaches a terminal state — by
resolution, failure, or cancellation — the adapter requests cancellation of
the exact system-derived workflow execution; this default is explicit, not
emergent. The plugin workflow owns cleanup of its activities and external
resources.

### Scope and deadlines

- run-scoped workflow Promises auto-cancel when their owning run terminates;
- detached Promises become session-scoped exactly as in P92;
- a binding may declare a trusted relative hard deadline, converted to an
  absolute deadline on each created Promise at invocation;
- an `await` timeout never changes the underlying Promise;
- session close cancels remaining owned workflow Promises.

No new cancel, wait, detach, or timeout vocabulary is added.

Hard-deadline expiry is deliberately a failed Promise resolution owned by the
holder, not a cancellation. The current adapter therefore emits no
`invocation_cancellation` fact for it: that envelope is reserved for actual
`PromiseEvent::Cancelled` entries. Deadline-expiry metrics must be recorded in
the holder where the deadline is authoritative. If a receiver also needs a
best-effort instruction to stop external work after holder expiry, add a
distinct reasoned terminal notification rather than labeling run cancel,
detach, or session close as deadline expiry.

## Re-Entrancy Law

The edge direction is part of admission safety:

```text
bound notify:            session -> bound workflow          non-blocking
bound promise-bearing:   session -> bound workflow          upward wait edge
controller self-receiver:session -> lifecycle controller    guarded self-edge
workflow-as-tool:        session -> new child workflow      downward wait edge
```

P100b initially rejected `Bound + Promises` when the receiver was the
session's own lifecycle controller. That rejection prevents one real deadlock:
the controller waits for run terminal, or requires a new run in the same
session to compute a reply, while the active run waits on the controller's
Promise.

Endpoint inequality is not the correct permanent safety boundary, however. A
different receiver workflow can still re-enter the emitting session and form
the same cycle, while a lifecycle controller can safely resolve a pushed
invocation entirely from its own state and activities. Workflow identity is
therefore neither necessary nor sufficient evidence of acyclic progress. The
durable dependency contract is what matters.

`Bound + Promises` may target the lifecycle controller when all of the
following **self-receiver progress contract** holds:

1. The binding declares a non-zero `deadline_after_ms`. A missing or zero
   hard deadline is rejected at managed-session admission for this topology.
2. The controller's fixed `deliver_emission` handler accepts and durably
   deduplicates invocations while the emitting run is active, including while
   that run is parked in `await`.
3. Invocation processing is scheduled independently from run-terminal
   notification handling. Waiting for the matching terminal notification is
   never a precondition for handling or resolving the invocation.
4. The controller resolves or fails each keyed Promise from its own durable
   state and external activities. It must not start, await, or otherwise
   require new work in the emitting session to compute that resolution.
5. Duplicate delivery and replay converge by invocation id, and duplicate
   resolution remains a first-terminal-writer no-op at the Promise.

No new wire flag is required. The trusted immutable binding already identifies
that receiver and lifecycle controller are the same endpoint; admitting that
shape is the controller's declaration that it implements this contract. The
hard deadline is a machine-checkable backstop: a faulty implementation may
delay the run, but cannot leave a permanent self-deadlock.

Every promise-bearing receiver must be able to resolve or fail its keyed
promises from its own state and external dependencies. Starting a new run in
the emitting session may be a consequence of handling the request, never a
prerequisite for a reply.

A start-on-call workflow must not require re-entry into its holder session to
reach terminal results. Cross-session work continues to use Fleet admissions
and Fleet Promises rather than workflow-tool backchannels.

### First consumer: Channels

A per-channel-session `ChannelSessionWorkflow` may be both lifecycle
controller and receiver for promise-bearing `message_send`, `message_edit`,
and `message_react` tools:

```text
ChannelSessionWorkflow starts managed run
  -> agent calls message_send
  -> session atomically records invocation + delivery Promise
  -> pushed invocation reaches the same ChannelSessionWorkflow
  -> controller schedules an account-affine provider activity
  -> connector sends to Telegram/WhatsApp using its live client
  -> provider acknowledges and returns its message id
  -> controller resolves Promise with the provider delivery receipt
  -> agent run can continue or terminate
  -> controller independently receives run-terminal notification
```

The provider activity depends only on the invocation, the controller's durable
route, and the live account connector; it never depends on the active run
reaching terminal. Temporal history is the delivery authority: it durably
records the activity intent, retry, cancellation, and result. The provider
message id is the successful Promise result. Providers without an idempotency
key retain the unavoidable send-before-activity-ack at-least-once boundary;
adding an application outbox would not remove that ambiguity. This is safe
re-entrancy and avoids both a second delivery ledger and a per-session delivery
workflow whose only purpose would be satisfying the former endpoint inequality
check.

## Plugin Worker Contract

A workflow plugin ships a Temporal worker that registers:

- one or more bound receiver workflow types, start-on-call workflow types, or
  both;
- the fixed P100 `deliver_emission` handler for bound receivers;
- P100b per-key cancellation handling where promise-bearing completion is
  supported;
- keyed terminal result emission to the supplied holder endpoint;
- the fixed terminal-result recovery query for start-on-call workflows;
- cooperative execution cancellation for start-on-call workflows: Temporal
  workflow cancellation is a request, so the plugin must race its domain
  work against the context's cancellation future and end as cancelled —
  the holder cancels the exact owned execution when its last pending key
  turns terminal; and
- any private activities needed for database, provider, filesystem, or network
  side effects.

Receiver-side dedup is invocation-id keying of the plugin's own domain state;
no substrate dedup helper is required in v1.

The main session worker registers none of the plugin's domain workflow or
activity implementations. It needs only the generic P100b Temporal adapter
capable of:

- signaling an admitted bound endpoint;
- starting an admitted versioned workflow recipe;
- cancelling a system-owned started execution; and
- validating and admitting keyed replies.

Adding a plugin may add deployment data and another plugin worker. It must not
add an `is_<plugin>_tool` branch, a new `ToolDispatchMode` variant, a signal
name, or a workflow type registration to the session worker.

## Migration Boundary

P100b migrates no existing production tool or subsystem. Environment jobs,
messaging, Fleet, timers, MCP, and every other current behavior are
compatibility suites for the new primitives, not consumers to rewrite here.

P86 environment jobs and the messaging outbox remain useful design pressure:
they expose multi-Promise, cancellation, durable-enqueue, idempotency, and
plugin-isolation questions that the generic contracts must be capable of
answering. The keyed promise set exists precisely because the env-job shape
stressed the draft contract and the answer belonged at this design gate
rather than at migration time. Design pressure still does not authorize
domain-specific fields, branches, adapters, or migration work in P100b.

The deferred [P102](p102-workflow-plugin-extractions.md) owns the inventory,
ordering, and equivalence gates for extracting genuinely independent systems
after P100b is accepted. Environment jobs are explicitly not such an
extraction candidate: any use of P100b there is an internal core refactor,
judged by removal of bespoke Promise and dispatch paths rather than by moving
the environment domain to another package or deployment.

## Design Gate Before Implementation

P100b is deliberately design-first. The implementation plan below does not
begin until a review accepts one coherent contract for:

1. the durable target-lifecycle and completion model, including the keyed
   promise set and its admission cap;
2. the unified `PromiseSource::Workflow` identity and exact producer
   authorization;
3. atomic append boundaries for notify, promise-bearing, and start-on-call
   invocations;
4. deterministic start identity, retry, recovery, per-key cancellation,
   last-key execution cancellation, and terminal failure; and
5. the division between deterministic engine facts, generic Temporal adapter
   behavior, trusted admission, and plugin-owned workflow/activity code.

The 2026-07-25 review settled the shape of items 1, 2, and the delivery
derivation; the gate confirms the full written contract, it does not reopen
those decisions without new evidence.

The review uses only minimal generic workflows. Existing feature shapes may
challenge a primitive, but they do not add domain vocabulary to it. If a later
P102 extraction reveals a genuinely missing capability, that capability
returns as a separately justified primitive revision rather than being hidden
inside migration code.

## Scope

P100b includes:

1. The target-lifecycle/completion matrix, derived delivery, and precise
   bound-workflow terminology.
2. Push delivery for promise-bearing P100 tool invocations.
3. Immutable keyed-promise completion declarations and reply schemas.
4. Atomic `Emitted + Promise::Created × N`.
5. Producer-authorized keyed reply resolution through the existing emission
   and admission funnels.
6. Per-key cancellation and terminal-delivery failure behavior for bound
   promise-bearing tools.
7. Trusted, substrate-neutral workflow-start references.
8. Deterministic start-on-call execution identity and replay-safe start.
9. The unified generic workflow Promise source, cancellation, deadlines, and
   terminal resolution.
10. Plugin-worker execution rules and an internal receiver helper surface.
11. Protocol, reducer, restart, continue-as-new, and live Temporal coverage
    using minimal generic test workflows.

## Explicit Non-Goals

P100b does not add:

- an HTTP/gRPC plugin executor;
- a dynamic-library or WASM plugin ABI;
- arbitrary plugin activities addressed by session tools;
- a compiled feature/service-kind endpoint registry;
- plugin discovery, marketplace, installation, or deployment management;
- public config fields accepting raw workflow ids, workflow types, task
  queues, or start recipes;
- model-selectable routing, workflow identity, or completion keys;
- a per-binding delivery-mode declaration;
- push delivery for `Accepted` tools, ordered pushed notify streams,
  per-receiver FIFO, head-of-line retry, or high-water dedup helpers;
- a second waiting primitive or implicit wait on tool invocation;
- synchronous workflow-handler completion inside `tool_invoke_batch`;
- arbitrary plugin-authored `ToolEffect`s;
- replacement of Fleet session-to-session commands;
- arbitrary workflow return-value decoding;
- execution of mutually untrusted plugin workers in one Temporal namespace;
- external webhook, schedule, email, or bridge ingress;
- migration or deletion of any existing feature-specific implementation,
  Promise source, tool branch, workflow, activity, or public API;
- changes to P101 Work's pull-at-terminal behavior; or
- fire-and-forget start-on-call workflows without a concrete consumer.

## Crate And Module Shape

Expected changes:

```text
crates/engine/
  keyed completion and start ids, binding contracts, deterministic
    per-key promise-id derivation
  unified PromiseSource::Workflow
  promise-bearing/start events and deterministic reducer invariants

crates/tools/
  promise-bearing workflow-tool acknowledgement + keyed promise effects
  start-on-call invocation adapter
  input/reply schema helpers

crates/temporal-workflow/
  pushed ToolInvocation delivery entries on the shared flush spine
  generic workflow start/cancel adapter contracts
  per-key cancellation and reply emission helpers
  unified workflow Promise source handling

crates/temporal-server/
  trusted start-recipe admission and resolution
  bounded reply-schema validation activities
  generic start failure/retry integration

crates/api/ and api-projection/
  read-only invocation/promise/failure projections where operator value is
  demonstrated; no public mutation surface
```

The engine remains deterministic and substrate-neutral. Temporal workflow
types, task queues, signals, activities, and clients stay outside it.

## Implementation Plan

### B1. Contracts and terminology

- [x] Add the target-lifecycle/completion matrix to durable binding
      validation, with delivery derived from completion.
- [x] Preserve P100 bound-notify fingerprints and behavior where possible;
      explicitly version any canonical fingerprint encoding that changes
      (encoding bumped to v2; goldens repinned — greenfield identity
      change).
- [x] Add completion keys, `max_promises`, reply schema, start reference,
      and deterministic per-key promise-id derivation.
- [x] Add the unified `PromiseSource::Workflow { producer, invocation_id,
      key }`.
- [x] Reject `Bound + Promises` to the lifecycle controller in initial v1
      (superseded by the B7 self-receiver progress contract).
- [x] Keep all new bindings behind trusted managed-session admission.

### B2. Push delivery

- [x] Move promise-bearing tool emissions onto the shared replay-rebuilt
      flush spine.
- [x] Deliver each envelope independently with bounded deterministic retry;
      no cross-invocation ordering.
- [x] Append terminal `DeliveryFailed` only after exhausted/non-retryable
      delivery, failing the invocation's still-pending promises in the same
      append; never append successful-delivery bookkeeping.
- [x] Gate continue-as-new on flush-queue quiescence.

### B3. Bound promise-bearing tools

- [x] Derive completion keys from validated arguments; enforce uniqueness
      and `max_promises` before proposing a successful tool result.
- [x] Atomically append successful tool completion, `Emitted`, and
      `Promise::Created × N`.
- [x] Return the stable immediate acknowledgement with invocation id and the
      key→promise map.
- [x] Carry `completion_promises` in the existing P100 invocation envelope.
- [x] Authorize resolution by exact stored producer and promise.
- [x] Validate optional reply schemas before `ResolvePromise`.
- [x] Fail still-pending promises when push delivery reaches terminal
      failure.
- [x] Route best-effort per-key cancellation without cancelling the shared
      receiver execution.

### B4. Start-on-call workflow tools

- [x] Admit versioned substrate-neutral start references and resolve Temporal
      recipes outside `engine`.
- [x] Derive one deterministic execution id per invocation and binding.
- [x] Atomically append `StartRequested + Promise::Created × N` with ordinary
      tool completion.
- [x] Rebuild pending start work from durable state across replay and
      continue-as-new.
- [x] Treat exact `AlreadyStarted` as success; reject conflicting identity.
- [x] Append `StartFailed` and fail all pending promises on terminal start
      failure.
- [x] Resolve keyed promises from, and cancel, only the exact started
      execution; cancel the execution when its last pending key becomes
      terminal.
- [x] Recover emitted terminal results through the fixed query and fail
      promises whose workflow completes without valid results.

### B5. Generic plugin proofs

Implemented 2026-07-26 as the live suite
`crates/temporal-server/tests/workflow_tool_plugins_live.rs`: minimal
generic plugin workflows (`TestBoundPluginWorkflow`,
`TestStartPluginWorkflow`, `TestSilentStartPluginWorkflow`) registered on a
**second worker with its own task queue**; the session worker uses the
production `worker_with_activities` registration list unchanged. The suite
also surfaced and fixed a derivation gap: promise-bearing workflow-tool
bindings now pull the concurrency toolset (`await`/`cancel`/`detach`) into
the derived toolset (`enable_concurrency_for_workflow_tools`) — a promise
the agent cannot await is unusable.

- [x] Add unified workflow-source Promise routing.
- [x] Add a minimal bound promise-bearing receiver in a plugin worker and
      prove request, keyed replies, duplicate delivery, per-key
      cancellation, and receiver restart (per-key cancellation-fact and
      restart legs remain open in B6).
- [x] Add a minimal start-on-call workflow in a plugin worker and prove
      deterministic start, multi-key terminal results, cancellation, and
      worker restart (cancellation and restart legs remain open in B6).
- [x] Prove neither plugin adds a tool-name branch, workflow registration, or
      activity implementation to the session worker.

### B6. Failure and live coverage

Closed 2026-07-26. `workflow_tool_plugins_live.rs` grew to eleven serial
scenarios; closing this slice also added one missing substrate behavior:
**hard promise deadlines are now enforced** — the session workflow wakes at
the earliest pending `deadline_ms` and fails overdue promises through the
ordinary `ResolvePromise` funnel (first-writer-wins keeps a racing real
resolution harmless). Deliberate descopes are recorded per item; each is a
consequence of the design rather than untested behavior.

- [x] Crash-point coverage by construction rather than fault injection:
      pending start work and push state are recomputed from durable state
      (proven by the continue-as-new scenario), delivery is at-least-once
      with invocation-id idempotency, and replies are
      first-terminal-writer-wins. A precise crash-injection harness (kill
      between signal and apply) is descoped as disproportionate to what it
      would add over these invariants.
- [x] Duplicate push (receiver invocation-id dedup), duplicate reply, stale
      reply after completion, wrong producer (dropped with a recorded
      reason), schema-invalid reply (fails the promise with a CAS-backed
      error; malformed JSON takes the same validator path), and unknown
      completion key / non-canonical maps (unit/protocol level).
- [x] Key-count cap violations create no emission and no promises.
- [x] Receiver absent and receiver closed (terminal `DeliveryFailed` fails
      the promise, projected), start worker absent / start timeout (the
      unserved-queue start parks until the binding's hard deadline fails
      the promise), exact `AlreadyStarted` (deterministic re-issue after
      continue-as-new). Conflicting workflow identity is unconstructible by
      design — the execution id is a digest of the invocation and recipe
      identity, so no caller can race a different execution onto the same
      id.
- [x] Source continue-as-new (holder CANs during a parked await; rebuilt
      start work re-issues and the resolution still lands). Receiver-side
      continue-as-new is descoped: delivery targets stable workflow ids,
      never run ids, so receiver CAN is invisible to the substrate;
      `EnvironmentJobWorkflow` exercises that pattern in production suites.
- [x] Run-scoped auto-cancel (delivers the per-key cancellation fact to the
      bound receiver), last-key execution cancel (a running owned execution
      ends `Canceled` after its last pending key turns terminal), and hard
      deadline. Explicit `cancel`-tool and detach legs ride the same
      engine paths as P92's existing coverage; session-close cancellation
      is engine-level P92 behavior and stays there.
- [x] Preserve P100 pull, P101 Work, Fleet, timers, messaging, MCP, standalone
      sessions, and public environment-job behavior (`temporal_live` 17/17
      and `environment_provider_live` 5/5 green on 2026-07-26).

Live Temporal tests must source `dev/env.sh` and run serially:

```bash
source dev/env.sh
cargo test -p temporal-server --test workflow_tool_plugins_live -- --ignored --test-threads=1
cargo test -p temporal-server --test temporal_live -- --ignored --test-threads=1
cargo test -p temporal-server --test environment_provider_live -- --ignored --test-threads=1
```

### B7. Lifecycle-controller self-receiver

- [x] Remove the blanket admission rejection for a promise-bearing bound
      receiver equal to the lifecycle controller.
- [x] Reject that topology unless `deadline_after_ms` is present and non-zero;
      keep the existing rule for all other completion validation.
- [x] Prove one workflow can process `deliver_emission` independently while
      awaiting the managed run's terminal notification, without adding a new
      signal, Promise source, or delivery mode.
- [x] Add a live Channels-shaped proof: idempotent external delivery
      acceptance keyed by invocation id, Promise resolution before run
      terminal, then terminal notification consumption by the same workflow.
- [x] Cover duplicate push, duplicate resolution, controller
      continue-as-new/replay, receiver absence, and the hard-deadline backstop.
- [x] Preserve the general prohibition on computing a reply by starting or
      awaiting new work in the emitting session.

## Acceptance Criteria

P100b is complete when:

1. The code and docs use “bound workflow execution” precisely, model target
   lifecycle independently from completion, and derive delivery from
   completion with no per-binding delivery declaration.
2. A bound promise-bearing receiver reacts mid-run through P100's fixed
   envelope and signal with retry, invocation-id idempotency, terminal
   failure, and continue-as-new coverage.
3. A promise-bearing call atomically creates one invocation and its full
   keyed promise set, returns immediately, and each promise resolves only
   from its exact stored producer.
4. Invalid replies, dead receivers, and terminal delivery failure cannot
   leave an unresolvable Promise.
5. A start-on-call tool records durable intent, starts one deterministic
   plugin workflow execution without plugin code in the session worker, and
   resolves or fails its keyed workflow Promises.
6. Cancellation distinguishes a shared bound receiver from a session-owned
   started execution, operates per key, and reuses P92 ownership semantics.
7. Plugin workflow and activity implementations run in plugin workers; no new
   HTTP, arbitrary-activity, dynamic-code, or plugin-specific session-worker
   protocol exists.
8. The engine remains deterministic and Temporal-neutral.
9. Existing P100 notify/pull tools, P101 Work, environment jobs, messaging,
   Fleet, timers, and MCP remain unchanged.
10. The keyed promise set demonstrably expresses the P86 env-job shape
    (one call, several keys, one group workflow resolving them over time)
    using only minimal generic test workflows — no production migration.
11. A lifecycle controller may also be a promise-bearing bound receiver when
    it declares a non-zero hard deadline and satisfies the self-receiver
    progress contract; a live proof resolves the Promise before the same
    workflow consumes run terminal.

## Follow-On Boundary

Later work may add:

- [P106](p106-joined-workflow-tools.md) pushed Accepted delivery and Joined
  single-result completion; ordered pushed notify streams (per-receiver
  log-order FIFO, head-of-line retry, per-producer high-water dedup) remain
  separate;
- fire-and-forget start-on-call workflows;
- early semantic replies from still-running started workflows;
- an authenticated external workflow SDK for trusted binding admission;
- plugin discovery, installation, and deployment control planes;
- operator projections for current workflow-backed tool bindings;
- P102 extraction of existing feature-specific core/worker paths after their
  equivalence gates pass.

Those follow-ons must preserve the central authority rule: the model chooses a
granted semantic tool, never its workflow destination, start recipe, completion
mode, completion keys, or transport.
