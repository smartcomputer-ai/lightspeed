# P100: Workflow Emissions And Workflow Tools — Typed Session-to-Workflow Facts

**Status**
- Proposed 2026-07-23.
- Revised 2026-07-24 after design review: reframed from a standalone port
  transport into the general **emission** substrate that also carries the
  existing run-terminal notifications; port consumption is pull-first with
  push delivery deferred to the first mid-run receiver; the `Delivered`
  session event was dropped for P92 transport-philosophy consistency; the
  messaging migration was demoted from committed second topology to a
  candidate with an explicit burden of proof.
- Slice 1 transport refactor implemented 2026-07-24: the neutral,
  universe-scoped emission envelope and deterministic ids live in `engine`;
  run-terminal and environment-job source resolutions now use the single
  `deliver_emission` signal; both promise-specific signal/DTO families and
  the promise-named outbound queue are deleted.
- Slice 2 plugin-admission milestone implemented 2026-07-24: validated
  opaque workflow endpoints, port/invocation ids, source-universe-scoped
  managed-session declarations and fingerprints, durable
  `WorkflowToolConfigEvent` / `WorkflowToolState`, atomic managed-session
  opening, trusted gateway start/retry checks, and bounded event projection
  are in place. Each declaration carries its own opaque receiver, so new
  workflow plugins do not require a registry entry or session-worker change.
- Slice 3 tool/event integration implemented 2026-07-24: admitted managed
  ports materialize into the derived provider toolset and a distinct
  `WorkflowTool` runtime binding; the runtime validates CAS-backed schemas
  and arguments, returns a stable acknowledgement, and enforces a
  32-emission per-run/per-tool cap; the engine validates the typed internal
  effect and atomically records the ordinary successful tool result plus the
  bounded `WorkflowTool::Emitted` fact. Public event projection exposes
  declaration, invocation metadata/refs, and delivery failures without
  inlining arguments or leaking the internal effect. Runtime routing now uses
  `ToolDispatchMode::{Local, WorkflowTool}`; the unused per-tool `Activity`
  mode and `activity_type` metadata were removed because hosted tools already
  execute inside the enclosing `tool_invoke_batch` Temporal activity.
- Slice 4 pull reconciliation reads implemented 2026-07-24: the engine
  replays the complete log through the ordinary reducer, then projects one
  run's invocations in log order, authorizes by exact durable receiver
  binding, rejects malformed or unbound reads, and keeps inherited
  source-session emissions out of fork reads; the Temporal worker wraps that
  projection with its existing universe-scoped, paginated session store.
  Protocol coverage drives two differently addressed ports through one run,
  proves each receiver reads only its own emission across store pages after
  the durable run-terminal boundary, and rejects an unrelated endpoint. P101
  can now build `read_work_cycle_result` on this internal operation. The
  projection takes explicit domain arguments rather than defining a wire DTO;
  P101 owns the actual reconciliation activity request/result in
  `temporal-workflow`.
- Follow-on split 2026-07-25: **P100b (Workflow-Backed Tools)** owns the
  interaction modes that require a live receiver or a newly started workflow:
  push delivery, bound-workflow request/reply, and start-on-call workflows.
  P100 remains the implemented notify/pull substrate. This document retains
  the shared transport laws but no longer owns a second implementation phase
  for those capabilities.
- Implementation review and hardening 2026-07-24 (slices 1-4 verified
  against this spec; engine, API/projection, and Temporal suites green).
  Confirmed: both
  promise-specific signal families are grep-clean with no transitional dual
  funnel; envelope/ids are deterministic, domain-separated, and
  substrate-neutral (`engine` gained no transport or Temporal dependency);
  binding and creation fingerprints now use an explicit canonical v1 field
  encoding rather than serde output; pull reads replay the ordinary reducer,
  so durable binding, canonical id, joins, arguments ref, and successful
  call invariants are rechecked before projection; duplicate receiver
  delivery is proven to resolve once and become an end-to-end no-op. The
  managed-session admission now supports multiple independent opaque
  receivers in one immutable declaration; the earlier built-in endpoint
  registry proposal was removed because extending a compiled registry for
  every workflow plugin would violate the stable session-worker goal.
  The spec's named projection summary views were superseded by bounded
  event-stream variants — recorded in the API section with the port-summary
  read kept as an open item. The worker-level pull wrapper is
  `#[allow(dead_code)]` until P101 lands its reconciliation activity —
  intentional under "refactor fully, extend lazily", noted so it is not
  mistaken for wired-through coverage. Temporal signal calls correctly stay
  in the Temporal workflow adapter; a runtime-neutral async transport trait
  is not a slice-1 requirement.
- Design review 2026-07-25 (settled, folded into this document and P100b):
  the "port" noun is retired — one **workflow tool** family named
  `WorkflowTool*` (`WorkflowToolDefinition`/`Binding`/`Declaration`/`Id`,
  `WorkflowToolEvent`, `WorkflowToolConfigEvent`/`WorkflowToolState`,
  `ToolDispatchMode::WorkflowTool`, `EmissionBody::ToolInvocation`,
  invocation ids `wti:sha256:`). Code rename executed 2026-07-26 across
  engine/tools/temporal-workflow/temporal-server/api/api-projection/cli:
  modules renamed to `workflow_tool.rs`, binding fingerprints now
  `wtb:sha256:`, fingerprint hash domains renamed to
  `lightspeed.workflow-tool.{binding,invocation}.v1` with golden digests
  repinned (identity change, greenfield), contract artifacts and both
  TypeScript consumers regenerated and verified. Earlier status entries
  above keep the historical names. The same review settled P100b:
  `BoundDeliveryMode` is deleted (completion determines delivery),
  completion becomes a keyed promise set (request/reply is the single-key
  case), the two proposed promise sources merge into one
  `PromiseSource::Workflow`, and v1 push is slimmed to independent
  per-emission delivery — the FIFO/head-of-line/high-water ordered-stream
  laws formerly in this document's push section move behind a deferred
  ordered-notify-stream mode with no current consumer.
- Completed 2026-07-28 as the independent notify/pull substrate. Final
  hardening proves an exact retry of the atomic tool-result append returns the
  already committed entries and leaves one emission per call; a newly
  constructed worker storage boundary reads receiver-isolated results from the
  complete paginated log after run terminal; public `SessionConfig` rejects
  workflow-tool binding fields and ordinary config replacement preserves the
  immutable managed declaration. Existing continue-as-new live coverage proves
  that the same durable session log remains authoritative across executions.
  P101's `work_report` schema and Work reconciliation handler are now an
  explicit first-consumer proof, not a completion dependency of this generic
  primitive.
- Greenfield: internal workflow protocols and engine event vocabulary change
  without compatibility aliases. Replaced surfaces (the `resolve_promise` /
  `resolve_promise_source` signal split, promise-specific notification DTO
  names) are deleted in the same change that lands their replacement.
- Staging principle (settled 2026-07-24): **refactor fully, extend lazily.**
  Everything that unifies *existing* transports lands in one push — slice 1
  deletes both promise-specific signals, including the env-job fold, so
  there is exactly one signal era, never a transitional third. Everything
  that is *new capability* — push port delivery, request/reply,
  workflow-as-tool — belongs to the design-gated P100b primitive milestone,
  separate from later production-feature migrations. Its seams (envelope
  fields, generic trusted binding admission, and workflow-substrate adapter
  boundary) are fixed here and now.
- First consumer: **P101 (Durable Work)**, which declares `work_report` when
  it creates its managed session and consumes emissions by **pull** at run
  reconciliation.
- Builds on **P92 (Unified Suspension)**, **P95 (Config Redesign)**, the
  existing tool-result/effect path, CAS-backed tool arguments, and the
  run-terminal notify-intent machinery it generalizes.

## Decision

Lightspeed's cross-workflow communication reduces to three primitives. Two
already exist; P100 names the third and builds it:

- **Admission** — a command entering a session (`RequestRun`,
  `DeliverMessage`, `ResolvePromise`). One inbound funnel, unified by P92.
- **Emission** — a typed fact leaving a durable workflow for one admitted
  receiver: deterministic identity, durably recorded at the source,
  delivered at least once, deduplicated by the receiver. The session log is
  the source of truth for session-produced emissions.
- **Promise** — the awaitable join (P92): created by tool effects, resolved
  by admissions, suspended on by `await`.

P100 owns the emission primitive and adds its first tool-triggered producer:
**workflow-bound tools** — schema-defined function tools a trusted
workflow plugin or control-plane caller declares when creating a managed
session, with one fixed opaque receiver per binding:

```text
workflow plugin / lifecycle controller
  -> declares tool {
       tool: "work_report",
       input_schema: WorkReportV1,
       schema_revision: 1,
       receiver: admitted AgentWorkWorkflow endpoint
     }

model
  -> calls work_report({...})

session
  -> validates the call against the declared schema
  -> appends a typed WorkflowTool::Emitted event
  -> returns a small accepted result to the model

receiver workflow
  -> consumes the emission (pull at a boundary, or push delivery later)
  -> deduplicates by emission identity
  -> interprets WorkReportV1
  -> updates its own state machine
```

The model chooses a registered tool and supplies only that tool's declared
arguments. It cannot choose:

- a workflow id;
- a transport signal name;
- a universe;
- a tool id or schema revision;
- a delivery mode.

Those are fixed by the admitted tool binding.

The critical reframing versus the first draft: **the run-terminal
notification is already an emission.** `RunTerminalNotifyIntent` is a
log-backed intent admitted atomically with the run, carrying a fixed holder
workflow id and a deterministic token, delivered at least once by a
workflow-state pump, deduplicated by the receiver. That is this substrate
with `semantic_type = lightspeed.run.terminal.v1` and a lifecycle transition
instead of a tool call as the producer. P100 therefore does not add a third
delivery mechanism beside the promise-notification pump and the env-job
`resolve_promise_source` push; it defines the one envelope and one fixed
signal they all converge on.

P100 implements **notify-only** workflow tools. Calling one records a fact;
it does not synchronously execute the receiver's handler or return a
semantic response. P100b adds promise-bearing completion by creating a keyed
set of promises per invocation (request/reply is the single-key case) and
reusing `await` — never a second waiting primitive.

### Target lifecycle and completion are independent

“Existing workflow” in earlier drafts means a **bound workflow execution**: a
specific durable workflow instance that was created before the tool call and
whose opaque endpoint was fixed at managed-session admission. It does not mean
an already-compiled workflow type.

Workflow-backed tools have two independent axes:

| Target lifecycle | Accepted completion | Promise-bearing completion (keyed set) |
|---|---|---|
| Bound workflow execution | P100 notify tool | P100b request/reply and keyed-set tools |
| Start a new workflow execution | possible fire-and-forget start; deferred | P100b workflow-as-tool |

The primitives underneath the table stay small:

- an **emission** sends a fact to a bound workflow execution;
- a **Promise** optionally represents an eventual semantic reply;
- a **workflow start** optionally creates the execution on demand.

P100 implements only the upper-left cell. P100b specifies the two
promise-bearing cells and keeps the lower-left cell deferred until a real
consumer needs fire-and-forget workflow creation.

## The Unified Communication Model

| Primitive | Direction | Existing instances | P100 instances |
|---|---|---|---|
| Admission | into a session | `RequestRun`, `DeliverMessage`, `ResolvePromise` | unchanged |
| Emission | out of a workflow, one fixed receiver | run-terminal notify (Fleet/Work), env-job terminal push | tool invocations |
| Promise | awaitable join | `Run`, `EnvJob`, `Timer` sources | P100b: keyed `Workflow` source |

Consequences P100 commits to:

1. **One envelope, one fixed inbound signal** (`deliver_emission`) for every
   cross-workflow fact. `AgentSessionWorkflow` is itself just another
   receiver: it maps `lightspeed.run.terminal.v1` tokens to promises and
   admits `ResolvePromise`; `AgentWorkWorkflow` maps the same body to its
   execution cycle. The promise-specific `resolve_promise` signal name and
   DTOs are deleted, not aliased.
2. **The env-job path folds in — in the same push.** `EnvironmentJobWorkflow`'s
   `resolve_promise_source` push is the same primitive produced by a
   non-session workflow; slice 1 migrates it onto the shared envelope/signal
   in the same change that deletes `resolve_promise`, so the session
   workflow never carries two inbound funnels. The emission spine is
   producer-neutral: sessions back their emissions with the session log;
   other producers keep their own durable source of truth.
3. **Fleet stays admission-based.** `agent_spawn`/`agent_request`/
   `agent_send` are commands into peer sessions and do not become workflow
   tools.
   Fleet participates in this unification only through its terminal
   notifications riding the shared spine.
4. **The P92 concurrency surface is untouched.** `await`, `cancel`, and
   `detach` remain the only suspension vocabulary; workflow tools never grow a
   wait
   mode of their own.

## Why This Is The Right Abstraction

Lightspeed's communication mechanisms each have a specific destination and
lifecycle:

| Mechanism | Meaning |
|---|---|
| `message_send` | deliver content to an external messaging channel via the durable outbox |
| `agent_send` | place content in another session's mailbox |
| `agent_request` + Promise | ask another session to run and await its result |
| run-terminal emission | tell a holder workflow that one admitted run terminated |
| workflow-bound tool | let the agent emit a schema-validated semantic fact to one admitted receiver workflow |

The missing primitive is not another general mailbox. It is a safe way for a
workflow to lend an agent a small part of its command vocabulary.

Without workflow tools, every workflow-backed product feature faces two bad
choices:

1. add a bespoke tool, effect kind, signal DTO, delivery loop, and retry
   policy;
2. expose a raw tool such as
   `signal(workflow_id, signal_name, arbitrary_json)`.

The first duplicates infrastructure. The second gives model output authority
over routing and protocol selection, is difficult to authorize, and turns
every receiving workflow into an untyped public endpoint.

Workflow-bound tools separate the stable transport from product meaning:

```text
generic, owned by P100              domain-specific, owned by consumer
---------------------------------   -----------------------------------
tool declaration                    tool name and description
JSON Schema validation              payload DTO
fixed receiver destination          interpretation
emission identity                   state transition
session-log emission                business invariants
shared at-least-once delivery       duplicate handling result
```

This is a singular extension point without becoming arbitrary pub/sub.

## Product Invariants

1. **Every workflow tool is model-visible as an ordinary typed function
   tool.**
   Providers need no workflow-specific vocabulary.
2. **The destination is capability-bound, never argument-bound.**
3. **The session log is authoritative for what the agent emitted.**
   Delivery evidence is transport state, never the sole copy of the fact.
4. **The receiver workflow is authoritative for what the emission means.**
   The session does not interpret `work_report`, `request_approval`, or
   future domain payloads.
5. **Delivery to a live admitted receiver is at least once.** Existing
   run/source receivers remain semantically idempotent by promise/cycle
   token. Pushed tool-invocation receivers (P100b promise-bearing
   completions) deduplicate by invocation id — the key their domain state
   is keyed by anyway — and the reply side is idempotent because each
   promise is first-terminal-writer-wins. Ordered pushed notify streams,
   with per-producer log-order FIFO and a log-sequence high-water mark,
   are a deferred mode with no current consumer; pull reconciliation never
   uses push-delivery state.
6. **A successful tool result means "durably recorded," not "the receiver
   completed handling it."**
7. **Workflow tools do not wake or steer arbitrary sessions.**
   Session-to-session
   communication remains Fleet/mailbox behavior.
8. **Workflow tools do not create opaque reducer branches.** The engine
   understands
   the generic emission lifecycle; only the receiver understands the payload.
9. **Tool emissions are capped deterministically at admission.** Pull-mode
   v1 uses a per-run/per-tool emission count. A future push queue may add a
   separately defined pending-delivery cap. Exceeding a cap is an ordinary
   failed tool call that emits nothing.
10. **Receivers must never require new work in the emitting session to
    process an emission** (the re-entrancy law below).

## Vocabulary

- **Emission** — one durable typed fact from a producer (a session or
  another durable workflow) to one admitted receiver, identified by a
  deterministic emission id and, for session producers, the emitting
  event's log sequence.
- **Emission envelope** — the bounded wire shape delivered to receivers; the
  payload body is a closed enum, not open JSON.
- **Delivery spine** — the shared per-workflow pump plus the single fixed
  `deliver_emission` signal all emissions travel on.
- **Lifecycle controller** — the optional durable workflow that owns the
  session's higher-level objective or lifecycle, such as `AgentWorkWorkflow`.
- **Receiver workflow** — the durable workflow named by one admitted tool
  binding. It may be the lifecycle controller or an independent workflow
  plugin.
- **Workflow plugin** — workflow-owned code that supplies its endpoint and
  tool schemas through the generic trusted managed-session creation boundary;
  the session worker does not contain plugin-specific routing or semantics.
- **Workflow tool** — one model-visible function tool plus an immutable
  receiver binding. ("Port" is the retired earlier name for a bound-target
  workflow tool; it survives only in dated status entries above and in the
  not-yet-renamed implementation.)
- **Tool definition** — tool name, description, JSON Schema refs, semantic
  type, and schema revision.
- **Tool binding** — the admitted association between a tool definition, one
  session, and one receiver workflow.
- **Invocation** — one observed model tool call of a declared workflow
  tool; its emission id is the invocation id.
- **Handler** — receiver-owned deterministic logic that interprets a
  consumed emission.

"Signal" in this document means the fixed transport signal. A workflow tool
is not a dynamically named signal and is not a general subscription.

## Ownership And Authority

### One lifecycle controller, multiple tool receivers

P100 fixes the topology to:

```text
Session
  lifecycle controller: AgentWorkWorkflow?       // zero or one

  tools:
    work_report      -> AgentWorkWorkflow
    request_approval -> ApprovalWorkflow          // later
```

The lifecycle controller answers "who owns this session's outer loop?" Each
tool receiver answers "which workflow owns this semantic operation?" They are
independent relationships.

A Work-managed session has one `AgentWorkWorkflow` controller and binds
`work_report` to it. The same session may independently bind other ports to
other workflow plugins. A trusted creator may also admit plugin tools with no
lifecycle controller; ordinary public session creation admits neither.

The session's source universe and each supplied receiver are recorded
durably. The receiver may itself be universe-scoped or deployment-scoped;
the trusted creation boundary is responsible for admitting it. No receiver
identity is accepted from model arguments or ordinary session/profile
configuration.

### Workflow endpoint identity and plugin admission

Use a small, opaque, durable reference:

```rust
pub struct WorkflowEndpointRef {
    pub workflow_id: String,
    pub workflow_kind: String,
}
```

`workflow_kind` is diagnostic and admission metadata, not a dynamic signal
name. Every push-capable receiver implements the same fixed P100 signal.
Routing policy, sharding, protocol versioning, start arguments, and
ensure-start behavior are **plugin concerns**, not fields on the durable ref
— the first draft's `WorkflowEndpointClass`/`routing_key`/
`protocol_version` fields froze deployment policy into an identity type and
are dropped.

`workflow_id` is an opaque identifier chosen by the owning workflow. P100
requires only that it is non-empty and bounded; it does not infer tenancy,
kind, routing, or structure from a prefix. The workflow id remains stable
across continue-as-new. The binding never contains a substrate run id.

P100 deliberately has **no built-in endpoint registry**. A compiled mapping
from feature or service kinds to workflow ids would require changing the
session deployment for every new workflow, defeating the purpose of workflow
tools.
Instead, a trusted workflow plugin or control-plane caller supplies already
resolved per-tool endpoints to the generic managed-session start operation.
The session boundary validates the bounded declarations, schemas, collisions,
and fingerprints, then copies the optional controller, endpoints, and
bindings into the session log atomically with creation.

The core does not start receivers, compose their ids, or know whether they
are universe-scoped, deployment-scoped, sharded, or already running. The
plugin owns those choices before admission. If product-level plugin discovery
is later required, it belongs in a generic data-driven plugin catalog outside
the session worker; P100 neither requires nor defines one.

### Who may declare workflow tools

P100 admits bindings only through the trusted internal managed-session
creation operation:

- an authorized workflow plugin or control-plane caller supplies one
  optional lifecycle controller plus a bounded list of tool
  definition/receiver declarations;
- each receiver is independent and need not equal the lifecycle controller;
- this trusted operation is not part of the ordinary public session API;
- public/profile config and `session/config/put` cannot contain a workflow
  endpoint, invent a workflow tool, or retarget an admitted binding;
- session read projections expose bounded tool summaries separately from the
  public config document;
- a model cannot create or mutate a workflow tool.

A future workflow SDK may expose this same operation behind an authenticated
workflow capability. That authentication belongs at the server/control-plane
boundary, before the substrate-neutral `CoreAgentCommand`; it does not add
plugin kinds to the session reducer. Dynamic tool replacement may be added
only when a real plugin needs it.

Known limitation, accepted deliberately: immutable creation bindings mean a
long-lived managed session cannot take a tool schema upgrade in place — a new
schema revision or receiver requires a new managed session. This is fine for
per-Work sessions and revisited only if a controller with an indefinite
session lifetime appears.

Lifecycle ownership also makes the session non-branchable. A session with a
lifecycle controller cannot be cloned or history-forked because doing so would
duplicate or partially inherit one controller's ownership. Workflow-tool-only
declarations without a lifecycle controller remain cloneable and forkable.

## Workflow Tool Definition

Illustrative types:

```rust
pub struct WorkflowToolDefinition {
    pub tool_id: WorkflowToolId,
    pub revision: u32,
    pub semantic_type: String,
    pub tool: ToolSpec,
}

pub struct WorkflowToolBinding {
    pub session_universe_id: Uuid,
    pub definition: WorkflowToolDefinition,
    pub receiver: WorkflowEndpointRef,
    pub binding_fingerprint: String,
}
```

The complete `ToolSpec` is retained because the provider-visible tool name,
parallelism, and target policy live outside `FunctionToolSpec`. Binding
validation requires `ToolKind::Function`, forbids execution targets, and
then retains the function payload fields:

- the model-visible name;
- description ref;
- input JSON Schema ref;
- optional output JSON Schema ref;
- strictness and provider options.

For notify-only tools, the runtime owns the output. The model-visible result
is a stable acknowledgement such as:

```json
{
  "accepted": true,
  "invocationId": "wti:sha256:..."
}
```

Consumers should not use the optional function output schema to imply that
the receiver has processed the emission.

### Semantic type and revision

`semantic_type` is a reverse-DNS-style identifier such as:

```text
lightspeed.work.report.v1
lightspeed.approval.request.v1
acme.invoice.triage.v3
```

Reserved by the substrate itself:

```text
lightspeed.run.terminal.v1
```

It lets the receiver select a typed decoder and makes traces intelligible. It
does not select a destination or handler in the session.

The definition revision and the schema/document fingerprints are copied into
every emission. Replacing a workflow tool creates a new immutable binding
fingerprint.
An in-flight call continues to use the toolset revision against which the
turn was planned.

### Validation

At binding admission:

- tool and tool ids obey existing identifier limits;
- tool names do not collide with standard, Fleet, messaging, MCP, or other
  declared tools;
- description and schemas exist in CAS;
- input and optional output schemas are supported JSON Schema documents;
- semantic type and revision are non-empty and versioned;
- the managed session's source universe is recorded in the binding;
- the receiver came through the trusted managed-session creation boundary,
  never model arguments or ordinary public/profile configuration;
- binding size and total tool count stay below deployment limits.

The managed-session creation fingerprint includes the session's source
universe, its optional lifecycle controller, and every definition/receiver
binding.
Retrying with the same session id and fingerprint reopens it; retrying with a
different source universe, controller, tool, or receiver is a conflict. The
fingerprint is a durable creation fact in the session log; it is not inferred
only from Temporal start args.

Fingerprint-input stability hardened 2026-07-24: binding and creation
fingerprints use an explicit versioned, length-prefixed field encoding.
Serde field order, JSON formatting, and unrelated DTO representation changes
do not participate in identity. Golden digest tests pin the v1 encoding;
adding a new identity-bearing field requires an explicit encoding-version
decision.

At invocation:

- the call resolves to the exact binding from its planned toolset revision;
- arguments validate against that binding's input schema;
- the deterministic per-run/per-tool emission cap is enforced;
- the runtime, not the model, supplies receiver and binding metadata;
- oversized arguments remain CAS-backed through the existing observed tool
  call.

A schema-invalid or cap-exceeding invocation is converted to an ordinary
failed tool result before `Tool::CallCompleted` is proposed. It creates no
emission and never turns a model error into a workflow invariant failure.

## Config And Toolset Integration

P95 made the installed toolset derived state. P100 preserves that invariant:

```text
effective tool declaration
  = ordinary tools derived from public/profile SessionConfig
  + immutable workflow tools admitted at managed-session creation
```

Illustrative internal declarations:

```rust
pub struct ManagedSessionWorkflowTools {
    pub version: u32,
    pub lifecycle_controller: Option<WorkflowEndpointRef>,
    pub tools: Vec<WorkflowToolDeclaration>,
}

pub struct WorkflowToolDeclaration {
    pub definition: WorkflowToolDefinition,
    pub receiver: WorkflowEndpointRef,
}
```

The distinction is authority:

- public/profile feature blocks continue through the existing config path and
  cannot express workflow endpoints;
- workflow tools enter only through trusted managed-session creation args and
  are immutable in P100;
- the creator, not the session worker, owns receiver resolution and plugin
  deployment policy.

Toolset reconciliation consumes both sections and still produces one
`ToolPatch` and one toolset revision. It installs each workflow tool as:

- a normal `ToolKind::Function(FunctionToolSpec)` for provider presentation;
- a runtime `ToolBinding` whose dispatch mode is
  `ToolDispatchMode::WorkflowTool { tool_id, binding_fingerprint }`.

This is not a return to an externally writable `session/tools/update` API.
The trusted creator declares capabilities; the runtime still materializes
tools. Later public config changes reconcile ordinary feature tools while
preserving immutable workflow-tool bindings. There is no second toolset
writer and no model-controlled endpoint.

## Session Event Vocabulary

P100 adds a closed semantic invocation family — deliberately smaller than
the first draft — plus the durable binding-configuration facts described
below:

```rust
pub enum WorkflowToolEvent {
    Emitted {
        invocation: WorkflowToolInvocation,
    },
    DeliveryFailed {
        invocation_id: WorkflowToolInvocationId,
        error_ref: BlobRef,
    },
}
```

`Emitted` is semantic: the agent stated this fact. Terminal `DeliveryFailed`
is semantic in the operator sense: this fact is permanently undeliverable
and someone should know. There is **no `Delivered` event**. P92 §6 settled
the split this substrate must follow: semantic state lives in the session
log; transport state is transient and recomputable in the workflow. The
run-terminal path already works exactly this way — notifications enqueue on
the terminal *transition*, the flush queue is rebuilt by replay, and
quiescence gates continue-as-new — and it carries no delivered-fact events.
The first draft's mandatory `Emitted`/`Delivered`/`DeliveryFailed` outbox
took the opposite position for the same problem; this revision resolves the
contradiction in P92's favor. A substrate whose replay cannot rebuild the
flush queue may keep a durable delivery cursor in its own transport state;
the session log stays free of transport bookkeeping either way, and receiver
dedup makes conservative re-sends harmless.

Tool bindings need their own durable configuration facts because existing
`ToolConfigEvent` values contain only provider-facing `ToolSpec` data, not a
controller, receiver endpoint, tool id, or binding fingerprint:

```rust
pub enum WorkflowToolConfigEvent {
    ManagedBindingsAdmitted {
        session_universe_id: Uuid,
        declaration_version: u32,
        lifecycle_controller: Option<WorkflowEndpointRef>,
        creation_fingerprint: String,
        bindings: Vec<WorkflowToolBinding>,
    },
}
```

The exact command/event batching may follow existing config reconciliation,
but these facts and their corresponding `WorkflowToolState` are in the
session log. All managed bindings are appended atomically with session open.
Replay reads only those durable facts and never performs endpoint discovery.

The invocation envelope is bounded:

```rust
pub struct WorkflowToolInvocation {
    pub invocation_id: WorkflowToolInvocationId,
    pub tool_id: WorkflowToolId,
    pub semantic_type: String,
    pub schema_revision: u32,
    pub binding_fingerprint: String,
    pub session_universe_id: Uuid,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub tool_batch_id: ToolBatchId,
    pub tool_call_id: ToolCallId,
    pub arguments_ref: BlobRef,
    /// Reserved for promise-bearing completions (P100b keyed promise
    /// sets); always None in notify mode. Request/reply uses the single
    /// reserved key `reply`.
    pub completion_promises: Option<BTreeMap<String, PromiseId>>,
}
```

The receiver endpoint is copied into durable binding state but need not be
repeated in the receiver-facing envelope because the delivery target is
already fixed.

`invocation_id` is deterministically derived from:

```text
session source universe id
+ session id
+ run id
+ turn id
+ tool batch id
+ tool call id
+ binding fingerprint
```

It is stable across activity retry, worker restart, and session
continue-as-new, and cannot collide with the same session-local ids in
another universe.

### Payload access

Emission envelopes carry the bounded invocation and `arguments_ref`, not an
unbounded copy of model arguments. A receiving workflow:

1. validates envelope metadata and deduplicates by emission identity;
2. records the bounded envelope in its own durable state;
3. uses a consumer-owned activity to load the CAS blob and decode its typed
   payload;
4. records that bounded activity result durably before branching.

P100 may provide a generic CAS-load/schema-check activity helper, but it
does not dynamically interpret the consumer's semantic type. This keeps
workflow code deterministic and avoids duplicating large arguments into both
session and receiver histories.

### Relationship to `ToolEffect`

Existing tools may return generic `ToolEffect` values, and Promise tools
decode recognized effects into typed Promise events/state because the
session must branch on them.

Workflow tools follow the same convergence:

```text
tool runtime recognizes WorkflowTool binding
  -> creates a typed internal tool-emission effect
  -> engine validates it against the active binding and call joins
  -> same command append records Tool::CallCompleted
     and WorkflowTool::Emitted
```

The generic `ToolEffect` carrier may be reused internally to avoid changing
the tool runtime trait, but the durable contract is `WorkflowToolEvent`, not
a magic string that every product independently scans.

The model supplies only arguments. It cannot forge the internal effect,
invocation joins, receiver, or binding fingerprint.

## The Emission Envelope

One envelope, one fixed signal, a closed body enum, a producer-neutral
source:

```rust
pub struct EmissionEnvelope {
    pub emission_id: EmissionId,
    pub producer: EmissionProducer,
    pub body: EmissionBody,
}

pub enum EmissionProducer {
    /// Session-log-backed producer. `log_seq` is the sequence of the
    /// emitting event. Universe scope is explicit because SessionId is only
    /// universe-local.
    Session {
        universe_id: Uuid,
        session_id: SessionId,
        log_seq: EventSeq,
    },
    /// Non-session durable workflow (EnvironmentJobWorkflow today). Dedup
    /// is by emission id; for source resolutions the first-writer-wins
    /// ResolvePromise funnel already makes duplicates no-ops. The universe
    /// scopes this communication edge and does not imply that the workflow
    /// endpoint itself is universe-owned.
    Workflow {
        universe_id: Uuid,
        workflow_id: String,
    },
}

pub enum EmissionBody {
    RunTerminal {
        token: String,
        run_id: RunId,
        status: RunStatus,
        output_ref: Option<BlobRef>,
        failure_message_ref: Option<BlobRef>,
    },
    /// Non-run promise-source resolutions (environment jobs today);
    /// replaces PromiseSourceResolutionSignal / resolve_promise_source.
    SourceResolution {
        promise_id: PromiseId,
        resolution: PromiseResolution,
    },
    ToolInvocation {
        invocation: WorkflowToolInvocation,
    },
}
```

- For tool invocations, `emission_id` equals the invocation id. For
  run-terminal emissions it derives from universe, session, run, and intent
  token; for source resolutions from universe, source identity, and promise
  id. The emission-id format therefore admits both the `emission:` and
  `wti:` canonical digest prefixes deliberately: the durable invocation and
  its delivery share one identity so receiver dedup and session-log
  identity use a single key.
- The body is a closed internal enum, not open JSON: receivers are trusted
  internal workflows, new emission kinds extend the enum, and the substrate
  stays deterministic. Domain openness lives inside `ToolInvocation` via
  `semantic_type` + CAS payload, exactly one level down.
- `AgentSessionWorkflow` receives the same envelope as everyone else: a
  `RunTerminal` body maps token → `PromiseId` → `ResolvePromise` admission;
  a `SourceResolution` body admits `ResolvePromise` directly. This deletes
  the promise-specific signal vocabulary rather than generalizing around
  it.

Considered and rejected (design review 2026-07-25): a fully uniform body —
every emission as `{semantic_type, payload_ref}` with
`lightspeed.run.terminal.v1` as just another reserved type. That shape would
force the session workflow to load and decode CAS payloads on its own
promise-resolution path and would lose compile-time exhaustiveness over the
two bodies the substrate itself must branch on. Two-level closure — a closed
transport enum with the open semantic type one level down inside
`ToolInvocation` — is the deliberate amount of openness.

## Consumption: Pull First, Push When Needed

The first draft assumed push delivery from day one. Its own first consumer
disproves the need: **P101 Work buffers reports and acts only at the
run-terminal boundary, and its reconciliation activity reads the session log
anyway.** The run-terminal emission already wakes Work; a tool-delivery pump
would hand it envelopes it must ignore until reconciliation reads the log
regardless.

Consumption therefore has two modes: P100 ships pull first and P100b adds
push for the first mid-run consumer.

### Pull (P100 v1 — sufficient for P101)

A boundary-subscribed receiver (one that already receives the run-terminal
emission for the runs it cares about) reads tool emissions through one
internal read operation:

```text
read_tool_emissions(receiver_endpoint, session_id, run_id | after_log_seq)
  -> bounded Vec<WorkflowToolInvocation>
```

The operation authorizes the supplied receiver endpoint and returns only
invocations whose durable binding targets that receiver. A lifecycle
controller cannot read another plugin receiver's payloads merely because both
tools belong to the same session, and vice versa.

The read **fails closed** and first replays every entry through the ordinary
engine reducer. An unknown binding, a non-canonical or duplicate invocation
id, a join mismatch, a tampered durable binding, a mismatched arguments ref,
or an emission without its successful tool call fails the entire read rather
than silently skipping the bad entry — a poisoned log is an operational
error, never an empty result. Receivers must treat a read failure as a
cycle-level fault (P101: the reconciliation activity errors and the Work
state machine surfaces it), not as "no reports".

The run-terminal boundary guarantees every prior emission for that run is
durable, so a pull at the boundary is complete by construction. No pump, no
delivery events, no receiver dedup set, no continue-as-new outbox concerns.
Emissions consumed by pull never enter transport state at all.

### Push (P100b — promise-bearing completions only)

Revised 2026-07-25: delivery mode is not a declared knob — **completion
determines delivery**. `Accepted` tools are pull-only; push exists solely
because promise-bearing completion requires it (the agent parks mid-run on a
reply the receiver can only produce if it sees the invocation mid-run). Push
joins the **existing** delivery spine (the generalized run-terminal pump),
not a new one:

1. the `Emitted` transition enqueues the envelope into the workflow's
   transient flush queue (replay-rebuilt, exactly like promise
   notifications today);
2. the pump delivers each envelope independently via the fixed
   `deliver_emission` signal with deterministic bounded retry — no
   cross-invocation ordering, because promise-bearing invocations are
   independent of one another;
3. bounded retries exhausted or a non-retryable determination appends
   terminal `DeliveryFailed`, drops the entry, fails the invocation's
   still-pending promises in the same append, and surfaces the failure in
   projections — so a dead receiver cannot block continue-as-new or leave
   an unresolvable promise;
4. flush-queue quiescence gates continue-as-new, exactly as P92 §6 already
   specifies for promise notifications.

Push receiver dedup needs no dedicated transport state: a receiver keys its
domain state by invocation id (the emission id), so a duplicate delivery is
a no-op, and the reply side is already idempotent because each promise is
first-terminal-writer-wins.

Deferred with the ordered-notify-stream mode (no current consumer):
per-receiver FIFO in session-log order, head-of-line retry with
order-preserving backoff, and the per-producer `log_seq` high-water dedup
mark. Those laws bind that future mode, not v1 push.

### Tool completion versus delivery

In both modes, the workflow tool call completes when `Emitted` is durable. It
does not wait for consumption.

This keeps tool execution from coupling model latency to an arbitrarily
long-lived receiver and avoids a deadlock where:

```text
session waits for receiver handler
receiver waits for run terminal or another external event
run cannot terminate because tool call is waiting
```

Receivers must tolerate either ordering between emission consumption and the
run-terminal emission. P101 Work reconciles only at the matching
run-terminal boundary.

## Delivery Protocol Is Substrate-Neutral

The first draft specified Temporal signals normatively. The contract is
narrower than Temporal and must be stated that way, because the engine and
this substrate are meant to outlive any single durable-workflow engine:

- **Engine (substrate-neutral, `crates/engine`):** the emission envelope and
  deterministic ids, tool/binding DTOs, the `WorkflowToolEvent` and
  `WorkflowToolConfigEvent` families, deterministic reducer state, and the
  invariants above. The engine holds no transport state and performs no
  delivery I/O.
- **Workflow-substrate adapter:** each durable-workflow substrate owns the
  deterministic command that delivers one envelope to one resolved endpoint.
  This boundary does not require a shared async trait: workflow SDK contexts
  are themselves substrate-specific deterministic APIs, not ordinary runtime
  I/O adapters. Receiver-side envelope helpers remain plain library code with
  no workflow-substrate dependency; the ordered-stream high-water helper is
  deferred with that mode.
- **Temporal adapter (`temporal-workflow` / `temporal-server`):** the fixed
  `deliver_emission` signal, endpoint resolution/ensure-start policy,
  replay-rebuilt flush queues, and continue-as-new gating. Signals target the
  stable workflow id, never a run id. A shared Temporal helper or pump may
  centralize producer behavior when P100b generalizes delivery, but inline
  `WorkflowContext::external_workflow().signal()` calls inside
  `temporal-workflow` do not compromise engine neutrality.
- **Another substrate:** implements its own deterministic delivery command
  and admission funnel over the same neutral envelope, endpoint, identity,
  and receiver semantics.

## Re-entrancy And Edge Direction

The combined topology the product needs — a controller managing a session,
that session emitting tool invocations upward, the same session holding
promises on
sub-workflows and child sessions — is safe only under explicit
edge-direction rules. They are law, not guidance:

1. **Notify emissions are non-blocking by construction.** The emitting run
   never waits on them, so notify edges can never form a cycle.
2. **A receiver's handler must never require new work in the emitting
   session in order to process that session's emission.** Handlers branch on
   their own state plus the delivered fact. A handler that schedules a run
   in the emitting session must do so as an independent consequence, never
   as a precondition of consuming the emission.
3. **Request/reply tools (P100b) create an upward wait edge** — the session
   parks awaiting a promise its receiver resolves. The dangerous shape:
   the receiver's handler needs a new run in the emitting session to
   compute the reply, while that session's active run is parked on the
   request — deadlock. Rule 2 forbids it. P100b initially rejected
   request-mode tools bound to the session's own lifecycle controller, then
   Channels supplied the concrete safe pattern: the same workflow may be
   controller and receiver when it processes pushed invocations independently
   of run-terminal handling, never re-enters the emitting session to compute
   the reply, and the binding carries a non-zero hard Promise deadline. See
   P100b's self-receiver progress contract. `await { mailbox: true }` softens
   head-of-line blocking but does not repeal the rule.
4. **Workflow-as-tool promises (P100b) create downward wait edges** — the
   session waits on a sub-workflow it started, mirroring Fleet spawn edges.
   Downward wait edges plus upward notify edges keep the graph a DAG; the P92
   cycle residual stays unconstructible.

## Notify In P100, Promise Modes In P100b

P100 deliberately distinguishes two interaction shapes:

```text
notify:
  agent calls tool
  -> emission is durable
  -> model receives accepted
  -> receiver reacts independently

promise-bearing (P100b, keyed promise set):
  agent calls workflow-backed tool
  -> same append: WorkflowTool::Emitted + Promise::Created x N
     (one promise per completion key derived from validated arguments;
      request/reply is the single reserved key "reply";
      PromiseSource::Workflow { producer, invocation_id, key })
  -> envelope carries completion_promises
  -> receiver resolves each keyed promise through the ordinary
     SourceResolution emission -> ResolvePromise admission path
  -> agent uses await/cancel/detach per promise
```

The P100b mode adds no second waiting machinery: an emission plus promise
effects in the same append, resolved through the same spine and funnel
everything else uses. It must not keep a tool activity open, add a
`workflow_tool_wait`, or overload notify acknowledgements with domain results.
The `completion_promises` field exists in the envelope from day one so the
shape cannot ossify notify-only.

The sibling P100b seam, **workflow-as-tool**: a tool call that *starts* an
authorized workflow plugin (deterministic workflow id derived from
session/run/turn/call identity, exactly like Fleet and job ids today) and
returns keyed `PromiseSource::Workflow` promises resolved by the workflow's
terminal emissions. The start recipe and authorization belong to the plugin
control plane, while completion uses the same envelope and spine; it is P86's
job pattern with the provider replaced by a durable workflow.

P101 Work needs notify only: `work_report` declares the agent's disposition;
the Work workflow does not return information through that call.

## First Consumer: P101 Work

P101 declares:

```text
tool name:        work_report
semantic type:    lightspeed.work.report.v1
schema revision:  1
receiver:         the AgentWorkWorkflow that created the session
consumption:      pull at run reconciliation
```

Illustrative payload:

```rust
pub struct WorkReportV1 {
    pub outcome: WorkDispositionKind,
    pub summary: Option<String>,
    pub requested_input: Option<String>,
}
```

P100 guarantees only that a valid invocation is durably emitted and readable
at the run boundary. P101 owns:

- whether `complete` or `blocked` is valid in its current state;
- conflicting-report policy;
- reconciliation at the matching run-terminal boundary;
- result construction;
- whether another execution cycle is scheduled.

This division is the proof that the abstraction is useful: a later approval,
triage, escalation, or workflow-specific control tool reuses P100 without
adding another session delivery protocol.

## Messaging: A P102 Candidate, Not A P100 Commitment

The first draft committed to `message_send`/`message_edit`/`message_react`
migrating onto tools bound to a `MessagingWorkflow`. Demoted on review because
P100 did not yet distinguish domain-state ownership from plugin-execution
ownership:

- **What state would MessagingWorkflow authoritatively own?** Retry
  attempts, status, ack results, and rate accounting already live on
  durable outbox rows; the bridge acks and re-pends; `message_send` is
  already a synchronous, idempotent, durable enqueue through a worker
  activity. A workflow between tool and outbox adds a hop and a dedup layer
  and, per the first draft's own constraint ("must not become an
  independent second business-state authority"), owns nothing the rows do
  not.
- **Hot-workflow hazard.** A canonical per-universe Messaging endpoint
  funnels every message in a universe through one workflow's history —
  continue-as-new churn and per-workflow signal-throughput limits. Sharding
  policy is exactly the speculative generality the endpoint type no longer
  carries.
- **The genuine future need doesn't require a workflow receiver.** "Agent
  waits for actual channel delivery" is a bridge ack resolving a
  `Promise` through the existing spine — no MessagingWorkflow involved.

Messaging tools therefore stay on their current inline/outbox path in P100.
P102 may reopen the architectural question for a different reason: a
`MessagingWorkflow` may be useful as the separately deployed plugin execution
boundary even while the outbox row remains the sole delivery-lifecycle
authority. That migration still must prove equivalent enqueue, rate-limit,
retry, and result semantics and must avoid one hot per-universe workflow; it
is not an acceptance requirement for P100 or P100b.

The multi-receiver proof in the implementation plan uses a second minimal
workflow-plugin declaration instead.

## Scope

P100 includes:

1. The unified emission envelope and fixed `deliver_emission` signal
   carrying run-terminal notifications and env-job source resolutions —
   replacing the entire promise-specific signal vocabulary
   (`resolve_promise` and `resolve_promise_source`) in one push.
2. Zero or one immutable lifecycle-controller reference on a session.
3. Any bounded number of fixed receiver endpoints authorized for the
   session's source universe; receivers may be universe- or
   deployment-scoped.
4. A generic internal managed-session start operation usable by any trusted
   workflow plugin, not only Work.
5. Schema-defined, model-visible function tools derived into the normal
   session toolset.
6. Immutable per-tool receivers admitted together at managed-session
   creation without plugin-specific session-worker code.
7. Notify-only invocation semantics with deterministic per-run/per-tool
   emission caps.
8. A typed `WorkflowToolEvent` family (`Emitted`, terminal
   `DeliveryFailed`).
9. Pull-based emission reads for boundary-subscribed receivers.
10. Deterministic, universe-scoped emission identity shared by the durable
    invocation and its delivery.
11. The substrate laws P100b's push delivery must preserve; P100 v1 does not
    implement push. Log-order delivery and the high-water dedup rule are
    recorded for the deferred ordered-notify-stream mode only.
12. Session/API projections sufficient to inspect declared workflow tools,
    emissions,
    and terminal delivery failure.
13. Protocol, reducer, runtime, and live Temporal tests.
14. P101's `work_report` plus a second receiver binding in the same session
    as the proof that routing is not controller-specific.

P100 v1 completion is pull-first. P100b owns push when the first receiver must
react mid-run.

## Explicit Non-Goals

P100 does not add:

- `signal(workflow_id, signal_name, json)` or any model-selectable routing;
- global topics, broadcast, wildcard subscriptions, or a pub/sub registry —
  observers read projections and the session log; they do not receive
  deliveries;
- multiple lifecycle controllers per session;
- external webhook, schedule, email, or bridge ingress;
- session-to-session messaging or replacement of Fleet;
- synchronous handler completion;
- request/reply tools or a second waiting primitive (the envelope seam
  exists; the mode does not);
- a `Delivered` session event or any transport bookkeeping in the session
  log;
- the messaging migration;
- arbitrary receiver-defined reducer events;
- model-authored tool schemas or runtime workflow-tool creation;
- adding or retargeting creation-time workflow tools after managed-session
  creation;
- a public endpoint that lets ordinary session callers target workflows;
- a durable product database for tool invocations;
- Work status, goal-loop, approval, or other consumer semantics;
- dynamic tool registration while a turn or tool batch is active.

## API And Projection Surface

P100 is primarily an internal workflow protocol. It adds no public mutation
RPCs.

Implemented shape (2026-07-24, supersedes this section's earlier named
summary structs): tool declarations, emissions, and delivery failures are
projected as bounded **session event-stream variants**
(`WorkflowToolsConfigured`, `WorkflowToolEmitted`,
`WorkflowToolDeliveryFailed`) carrying ids, refs, and metadata only; the
internal emit effect is stripped from tool-call effect views so the trusted
carrier never leaks. Contract artifacts and the TypeScript client are
regenerated.

Still open from the original intent: a **bounded current-tool summary**
on session reads (the earlier `WorkflowToolView` idea) — "which workflow tools
does this session have right now, bound to which receiver kind" — answerable
today only by replaying config events. Land it when a concrete operator or
plugin SDK consumer needs it. Session reads are already universe-scoped, so
the source universe need not be repeated unless a future cross-universe
operator view requires it.

Full arguments remain behind the existing CAS/event authorization boundary.
Do not copy payloads into summary views.

If a future workflow SDK exposes managed-session creation with workflow tools,
it
should use an authenticated server operation bound to the calling workflow's
endpoint capability. Dynamic custom changes, if later needed, must not be
implemented as a general `session/tools/update` or accept raw unverified
workflow ids.

## Crate And Module Shape

Expected changes:

```text
crates/engine/
  EmissionEnvelope / EmissionProducer / EmissionBody and deterministic ids
  workflow-tool ids, endpoint/binding DTOs
  WorkflowToolConfigEvent + WorkflowToolEvent and deterministic reducer state
  validation of typed internal emission effects
  session-log projections and emission reads

crates/tools/
  WorkflowTool ToolDispatchMode/binding
  generic inline invocation adapter
  schema validation and stable acknowledgement

crates/temporal-workflow/
  optional lifecycle-controller and resolved receiver bindings
  fixed deliver_emission signal over the neutral engine DTO
  shared delivery-spine pump (generalized from promise notifications)
  receiver workflow integration

crates/temporal-server/
  trusted workflow-plugin managed-session creation path
  effective config/toolset materialization
  delivery retry/failure projection

crates/api/ and api-projection/
  read-only tool and emission views if exposed through session reads
```

P100 does not add domain handlers to `engine`. A receiver workflow imports
the envelope type and owns its typed payload DTO.

## Implementation Plan

### Slice 1: Emission envelope and delivery signal — the whole transport
refactor, one push

Generalizes and unifies the existing transports; prerequisite for P101
slice 1. This is a rename-and-fold of working mechanisms, not new
machinery, and it deletes **both** promise-specific signals in the same
change so no transitional dual-funnel state ever ships.

- [x] Add the neutral, universe-scoped `EmissionEnvelope`,
      `EmissionProducer`, existing `EmissionBody` variants, and deterministic
      `EmissionId` derivation in `engine`.
- [x] Replace the `resolve_promise` signal with the fixed generic
      `deliver_emission` signal carrying `RunTerminal` bodies; delete the
      promise-specific DTO/signal names.
- [x] Fold the env-job push in the same change: `EnvironmentJobWorkflow`
      emits `SourceResolution` bodies through `deliver_emission`; delete
      `resolve_promise_source` and `PromiseSourceResolutionSignal`; preserve
      the P86/P92 env-job live coverage.
- [x] Include the observed `run_id` and producer `log_seq` in the
      run-terminal body/envelope.
- [x] `AgentSessionWorkflow` maps `RunTerminal` tokens and
      `SourceResolution` bodies into its existing `ResolvePromise`
      admission.
- [x] Preserve the existing semantic idempotency boundaries: promise/cycle
      tokens for run-terminal delivery and promise ids for source resolution.
      The pushed-tool high-water helper belongs to the deferred
      ordered-notify-stream mode.
- [x] Prove in a protocol test that a non-session workflow receives the same
      envelope through the same signal — proven live 2026-07-26 by the P100b
      plugin suite (`workflow_tool_plugins_live.rs`), whose plugin workflows
      consume `deliver_emission` envelopes on a separate worker; P101's Work
      receiver remains the first production consumer.
- [x] Preserve all Fleet Promise, duplicate delivery, cancellation, and
      continue-as-new tests.

### Slice 2: Tool, endpoint, and plugin-admission contracts

- [x] Add validated workflow endpoint, tool, and invocation ids.
- [x] Add durable `WorkflowToolConfigEvent` facts and `WorkflowToolState` for
      controller, resolved receiver, binding fingerprint, and managed-session
      creation fingerprint.
- [x] Add the optional immutable lifecycle controller plus the managed
      session's durable source universe to the
      trusted managed-session start path.
- [x] Add `WorkflowToolDefinition`, semantic type/revision, and binding
      fingerprint validation.
- [x] Bind every workflow tool to one admitted `WorkflowEndpointRef`.
- [x] Admit immutable per-tool receiver declarations atomically with
      managed-session creation; receivers need not equal the lifecycle
      controller.
- [x] Keep plugin discovery, workflow-id composition, start policy, and
      plugin semantics out of the session core and worker. No built-in
      endpoint registry is part of P100.
- [x] Reject raw receiver creation or retargeting from ordinary public
      config writes.

### Slice 3: Toolset and event-log integration

- [x] Materialize tool definitions as ordinary `FunctionToolSpec` entries
      plus `WorkflowTool` runtime bindings.
- [x] Validate call arguments against the admitted schema; enforce the
      deterministic per-run/per-tool cap before proposing a successful tool
      result.
- [x] Add typed internal emission effect handling.
- [x] Atomically append tool completion and `WorkflowTool::Emitted`.
- [x] Project declarations, emissions, and failure without inlining
      payloads.

### Slice 4: Pull reconciliation reads

- [x] Add the receiver-authorized internal
      `read_tool_emissions(receiver_endpoint, session_id, run_id)` operation
      over the session log/projection; return only bindings targeting that
      receiver.
- [x] Guarantee boundary completeness: every emission of a run is readable
      once that run's terminal emission is delivered.
- [x] This is the operation P101's `read_work_cycle_result` builds on.

### Slice 5: Moved to P100b

Push delivery is the first implementation slice of P100b. The transport laws
remain specified above because push must reuse P100's envelope, source log,
fixed `deliver_emission` signal, and terminal-failure semantics; the
ordering laws bind only the deferred ordered-notify-stream mode. P100 has no
unimplemented push slice of its own.

### Slice 6: Prove generic plugin receivers

- [x] Admit controller and independent plugin receivers in the same generic
      managed-session declaration without changing session-worker routing.
- [x] Exercise two differently addressed plugin tools in one session and
      prove each receiver can read only its own emissions.
- [x] Keep the P101 production-consumer proof at the follow-on boundary: P100
      exposes only the generic declaration, emission, and pull primitives and
      contains no Work-specific effect, signal, or delivery loop.

### Slice 7: Failure and compatibility coverage

- [x] Test schema-invalid and cap-exceeding calls create no emission.
- [x] Test duplicate tool-result admission creates one emission: retrying the
      exact atomic append returns its existing entries and does not grow the
      durable emission set.
- [x] Test duplicate `deliver_emission` receiver delivery is an end-to-end
      no-op: both copies enter the ordinary admission funnel, but
      `ResolvePromise` first-writer-wins appends one resolution.
- [x] Test pull reads are complete at the run-terminal boundary across
      worker restart and session continue-as-new: pull rebuilds solely from
      the paginated durable log through fresh worker dependencies, while the
      Temporal continue-as-new suite proves that log spans executions.
- [x] Test public config cannot retarget immutable managed-session bindings:
      the wire document rejects binding fields and config replacement leaves
      durable workflow-tool state unchanged.
- [x] Confirm Fleet, Promises, messaging bridges, MCP, and standalone
      sessions are unchanged.

Live Temporal tests must source `dev/env.sh` and run serially with
`--test-threads=1`.

## Acceptance Criteria

P100 is complete when:

1. One envelope and one fixed signal carry every inbound cross-workflow
   fact — run-terminal notifications to session and non-session receivers
   alike, and env-job source resolutions; both promise-specific signals are
   gone and the session workflow has exactly one inbound funnel.
2. A trusted workflow plugin or control-plane caller can provision a managed
   session with typed tools bound to one or more opaque receivers, with or
   without a lifecycle controller, without adding plugin-specific code to
   the session core or worker.
3. The model sees only the declared tool name, description, and schema; it
   cannot choose the receiver or transport.
4. A valid call atomically produces an ordinary successful tool result and
   one typed, CAS-backed `WorkflowTool::Emitted` fact linked to the exact
   run/turn/batch/call.
5. A boundary-subscribed receiver can read only its own run emissions,
   complete at the run-terminal boundary across retry, restart, and
   continue-as-new.
6. Invalid arguments, cap violations, and unauthorized tool
   declaration/mutation fail without emitting.
7. Toolset derivation remains capability-driven, managed-session workflow
   tools remain
   immutable, and no public `session/tools/update` surface returns.
8. One session can bind workflow tools to two different fixed workflow-plugin
   receivers without changing the session engine or worker.
9. The session log contains no successful-delivery bookkeeping events;
    `Emitted` is the v1 semantic invocation event. When P100b push lands,
    terminal `DeliveryFailed` is the only additional tool event.
10. Existing Fleet, Promise, external messaging, MCP, and run-terminal
    behavior remains semantically unchanged (renamed, not re-implemented).

## Follow-On Boundary

P100 intentionally leaves these seams to
[P100b](p100b-workflow-backed-tools.md):

- push delivery for promise-bearing completions to bound workflow
  executions;
- promise-bearing tools as `Emitted + Promise::Created × N` over a keyed
  promise set (request/reply is the single-key case);
- start-on-call workflow tools with deterministic execution identity and
  the same keyed `PromiseSource::Workflow`.

Other follow-ons remain outside both P100 and P100b:

- P101's first production-consumer proof: declare the `work_report` schema at
  managed-session creation, read `WorkReportV1` at the run-terminal boundary,
  and reconcile Work without adding a Work-specific transport.
- Push-notify and ordered pushed notify streams (per-receiver log-order
  FIFO, head-of-line retry, per-producer high-water dedup) — deferred until
  a mid-run notify consumer exists.
- An authenticated workflow SDK exposing the generic trusted managed-session
  creation capability.
- Production migrations of existing tool families, including messaging and
  environment jobs; the deferred
  [P102](p102-workflow-plugin-extractions.md) owns their compatibility gates.
- Tool emission projections feeding mission control or evals.
- External systems starting workflow plugins through a separate ingress
  plane.

Those extensions must reuse the fixed envelope, emission identity, event
family, and authority model. They must not weaken the central rule: the
agent chooses a granted semantic operation, never an arbitrary destination.
