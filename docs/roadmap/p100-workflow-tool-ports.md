# P100: Workflow Emissions And Tool Ports — Typed Session-to-Workflow Facts

**Status**
- Proposed 2026-07-23.
- Revised 2026-07-24 after design review: reframed from a standalone port
  transport into the general **emission** substrate that also carries the
  existing run-terminal notifications; port consumption is pull-first with
  push delivery deferred to the first mid-run receiver; the `Delivered`
  session event was dropped for P92 transport-philosophy consistency; the
  messaging migration was demoted from committed second topology to a
  candidate with an explicit burden of proof.
- Greenfield: internal workflow protocols and engine event vocabulary change
  without compatibility aliases. Replaced surfaces (the `resolve_promise` /
  `resolve_promise_source` signal split, promise-specific notification DTO
  names) are deleted in the same change that lands their replacement.
- Staging principle (settled 2026-07-24): **refactor fully, extend lazily.**
  Everything that unifies *existing* transports lands in one push — slice 1
  deletes both promise-specific signals, including the env-job fold, so
  there is exactly one signal era, never a transitional third. Everything
  that is *new capability* — push port delivery, request/reply,
  workflow-as-tool — waits for its first consumer, but its seams (envelope
  fields, registry shape, transport trait) are fixed here and now.
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
**workflow-bound tool ports** — schema-defined function tools the trusted
runtime declares on behalf of a lifecycle controller or registered workflow
service, with one fixed receiver per binding:

```text
lifecycle controller / workflow service
  -> declares port {
       tool: "work_report",
       input_schema: WorkReportV1,
       schema_revision: 1,
       receiver: admitted AgentWorkWorkflow endpoint
     }

model
  -> calls work_report({...})

session
  -> validates the call against the declared schema
  -> appends a typed WorkflowPort::Emitted event
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
- a port id or schema revision;
- a delivery mode.

Those are fixed by the admitted port binding.

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

P100 implements **notify-only** ports. Calling a port records a fact; it does
not synchronously execute the receiver's handler or return a semantic
response. A later request/reply mode creates a Promise per invocation and
reuses `await` — never a second waiting primitive.

## The Unified Communication Model

| Primitive | Direction | Existing instances | P100 instances |
|---|---|---|---|
| Admission | into a session | `RequestRun`, `DeliverMessage`, `ResolvePromise` | unchanged |
| Emission | out of a workflow, one fixed receiver | run-terminal notify (Fleet/Work), env-job terminal push | port invocations |
| Promise | awaitable join | `Run`, `EnvJob`, `Timer` sources | later: `WorkflowToolInvocation`, `Workflow` sources |

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
   `agent_send` are commands into peer sessions and do not become ports.
   Fleet participates in this unification only through its terminal
   notifications riding the shared spine.
4. **The P92 concurrency surface is untouched.** `await`, `cancel`, and
   `detach` remain the only suspension vocabulary; ports never grow a wait
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
| workflow-bound tool port | let the agent emit a schema-validated semantic fact to one admitted receiver workflow |

The missing primitive is not another general mailbox. It is a safe way for a
workflow to lend an agent a small part of its command vocabulary.

Without ports, every workflow-backed product feature faces two bad choices:

1. add a bespoke tool, effect kind, signal DTO, delivery loop, and retry
   policy;
2. expose a raw tool such as
   `signal(workflow_id, signal_name, arbitrary_json)`.

The first duplicates infrastructure. The second gives model output authority
over routing and protocol selection, is difficult to authorize, and turns
every receiving workflow into an untyped public endpoint.

Workflow-bound tool ports separate the stable transport from product meaning:

```text
generic, owned by P100              domain-specific, owned by consumer
---------------------------------   -----------------------------------
port declaration                    tool name and description
JSON Schema validation              payload DTO
fixed receiver destination          interpretation
emission identity                   state transition
session-log emission                business invariants
shared at-least-once delivery       duplicate handling result
```

This is a singular extension point without becoming arbitrary pub/sub.

## Product Invariants

1. **Every model-visible port is an ordinary typed function tool.**
   Providers need no workflow-specific vocabulary.
2. **The destination is capability-bound, never argument-bound.**
3. **The session log is authoritative for what the agent emitted.**
   Delivery evidence is transport state, never the sole copy of the fact.
4. **The receiver workflow is authoritative for what the emission means.**
   The session does not interpret `work_report`, `request_approval`, or
   future domain payloads.
5. **Delivery is at least once and, per session producer, in log order.**
   Receivers deduplicate session-produced emissions with a per-session
   high-water mark over the emission's log sequence, not an unbounded id
   set; non-session producers rely on emission-id idempotency.
6. **A successful tool result means "durably recorded," not "the receiver
   completed handling it."**
7. **Ports do not wake or steer arbitrary sessions.** Session-to-session
   communication remains Fleet/mailbox behavior.
8. **Ports do not create opaque reducer branches.** The engine understands
   the generic emission lifecycle; only the receiver understands the payload.
9. **Emissions are rate-capped at admission.** A looping model cannot flood a
   receiver; exceeding a cap is an ordinary failed tool call that emits
   nothing.
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
- **Receiver workflow** — the durable workflow named by one admitted port
  binding. It may be the lifecycle controller or a shared workflow service.
- **Workflow service** — a registered receiver such as a future approval
  service whose endpoint is resolved by trusted runtime policy rather than
  supplied by the model or raw profile config.
- **Port** — one model-visible function tool plus an immutable receiver
  binding.
- **Port definition** — tool name, description, JSON Schema refs, semantic
  type, and schema revision.
- **Port binding** — the admitted association between a port definition, one
  session, and one receiver workflow.
- **Invocation** — one observed model tool call of a declared port; its
  emission id is the invocation id.
- **Handler** — receiver-owned deterministic logic that interprets a
  consumed emission.

"Signal" in this document means the fixed transport signal. A port is not a
dynamically named signal and is not a general subscription.

## Ownership And Authority

### One lifecycle controller, multiple port receivers

P100 fixes the topology to:

```text
Session
  lifecycle controller: AgentWorkWorkflow?       // zero or one

  ports:
    work_report      -> AgentWorkWorkflow
    request_approval -> ApprovalWorkflow          // later
```

The lifecycle controller answers "who owns this session's outer loop?" Each
port receiver answers "which workflow owns this semantic operation?" They are
independent relationships.

A Work-managed session has one `AgentWorkWorkflow` controller and binds
`work_report` to it. The same session may independently bind other ports to
registered service workflows. A standalone session has no lifecycle
controller and no controller ports.

Every receiver is validated to belong to the session's universe and recorded
durably. No receiver identity is accepted from model arguments.

### Workflow endpoint identity and the endpoint registry

Use a small, opaque, durable reference:

```rust
pub struct WorkflowEndpointRef {
    pub universe_id: Uuid,
    pub workflow_id: String,
    pub workflow_kind: String,
}
```

`workflow_kind` is diagnostic and admission metadata, not a dynamic signal
name. Every receiver implements the same fixed P100 signal. Routing policy,
sharding, protocol versioning, start arguments, and ensure-start behavior
are **resolver concerns**, not fields on the durable ref — the first draft's
`WorkflowEndpointClass`/`routing_key`/`protocol_version` fields froze
transport policy into an identity type and are dropped.

The workflow id remains stable across continue-as-new. The binding never
contains a substrate run id.

Endpoint resolution lives in one **workflow endpoint registry** with two
consumers by design, even though P100 implements only the first:

1. **Port receivers** — mapping a lifecycle-controller capability or a known
   feature grant to a same-universe receiver endpoint.
2. **Startable workflow kinds** (follow-on) — the catalog a session tool
   consults to start an admitted workflow as durable work and hold a
   `PromiseSource::Workflow` promise on its completion. This is P86's job
   pattern generalized; building the registry port-receiver-only would force
   a second registry later.

For lifecycle controllers, the receiver must already exist because it created
or owns the managed session. For built-in services, the registry owns
workflow-id composition, routing policy, start args, and ensure-start
behavior. A profile selects a feature; it never selects a workflow shard.

### Who may declare ports

P100 admits bindings only through trusted runtime materialization:

- a managed-session controller may declare controller-bound ports when it
  creates the session; in P100, those bindings are immutable for that
  session's lifetime;
- a built-in feature resolver may materialize ports to a registered workflow
  service endpoint;
- public/profile config may grant a known feature but never contain a raw
  workflow id;
- ordinary public `session/config/put` callers cannot invent, retarget, or
  widen a resolved receiver binding;
- session read projections expose bounded port summaries separately from the
  public config document;
- a model cannot create or mutate a port.

P100 needs only a small built-in endpoint registry, not a public one. A
future workflow SDK may expose custom endpoint registration behind an
authenticated capability. Dynamic controller-port replacement may be added
only when a real controller needs it.

Known limitation, accepted deliberately: immutable controller bindings mean a
long-lived managed session cannot take a port schema upgrade in place — a new
schema revision requires a new managed session. This is fine for per-Work
sessions and revisited only if a controller with an indefinite session
lifetime appears.

## Port Definition

Illustrative types:

```rust
pub struct WorkflowToolPortDefinition {
    pub port_id: WorkflowToolPortId,
    pub revision: u32,
    pub semantic_type: String,
    pub function: FunctionToolSpec,
}

pub struct WorkflowToolPortBinding {
    pub definition: WorkflowToolPortDefinition,
    pub receiver: WorkflowEndpointRef,
    pub binding_fingerprint: String,
}
```

`FunctionToolSpec` already carries:

- the model-visible name;
- description ref;
- input JSON Schema ref;
- optional output JSON Schema ref;
- strictness and provider options.

For notify-only ports, the runtime owns the output. The model-visible result
is a stable acknowledgement such as:

```json
{
  "accepted": true,
  "invocationId": "wpi_..."
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
every emission. Replacing a port creates a new immutable binding fingerprint.
An in-flight call continues to use the toolset revision against which the
turn was planned.

### Validation

At binding admission:

- tool and port ids obey existing identifier limits;
- tool names do not collide with standard, Fleet, messaging, MCP, or other
  declared tools;
- description and schemas exist in CAS;
- input and optional output schemas are supported JSON Schema documents;
- semantic type and revision are non-empty and versioned;
- receiver universe equals session universe;
- the receiver came from the lifecycle-controller capability or the endpoint
  registry, never an untrusted raw workflow id;
- binding size and total port count stay below deployment limits.

The managed-session creation fingerprint includes its optional lifecycle
controller and controller-bound port definitions. Retrying with the same
session id and fingerprint reopens it; retrying with a different controller
or controller-port set is a conflict. Service-bound port fingerprints derive
from the admitted feature/config revision and the registered service
endpoint.

At invocation:

- the call resolves to the exact binding from its planned toolset revision;
- arguments validate against that binding's input schema;
- per-port rate and per-session pending-emission caps are enforced;
- the runtime, not the model, supplies receiver and binding metadata;
- oversized arguments remain CAS-backed through the existing observed tool
  call.

A schema-invalid or cap-exceeding invocation is an ordinary failed tool
call. It creates no emission.

## Config And Toolset Integration

P95 made the installed toolset derived state. P100 preserves that invariant
by resolving workflow-bound tools from two trusted declaration sources:

```text
effective tool declaration
  = public/profile SessionConfig features
      -> built-in workflow-service ports
  + immutable lifecycle-controller port declarations
```

Illustrative internal declarations:

```rust
pub struct ControllerWorkflowPorts {
    pub version: u32,
    pub controller: WorkflowEndpointRef,
    pub ports: Vec<WorkflowToolPortDefinition>,
}

pub struct ResolvedWorkflowServicePort {
    pub service_id: String,
    pub binding: WorkflowToolPortBinding,
}
```

The distinction is authority:

- public/profile feature blocks are changed through the existing config path;
- the feature resolver maps known capabilities to registered service
  endpoints;
- lifecycle-controller ports are admitted only from trusted managed-session
  creation args and are immutable in P100.

Toolset reconciliation consumes both sections and still produces one
`ToolPatch` and one toolset revision. It installs each port as:

- a normal `ToolKind::Function(FunctionToolSpec)` for provider presentation;
- a runtime `ToolBinding` whose execution mode is
  `WorkflowPort { port_id, binding_fingerprint }`.

This is not a return to an externally writable `session/tools/update` API.
Callers declare capabilities; the runtime still materializes tools.

Later public config changes may add or remove built-in service ports through
their normal feature blocks and existing idle/toolset-revision rules, while
preserving immutable lifecycle-controller ports. There is no second toolset
writer and no model-controlled endpoint.

## Session Event Vocabulary

P100 adds a closed generic event family — deliberately smaller than the
first draft:

```rust
pub enum WorkflowPortEvent {
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

Controller-port declarations are durable managed-session creation facts.
Service-port declarations are reproducibly derived from the admitted session
config and registered same-universe service endpoint. Both reuse existing
config/tool events rather than adding a second event family.

The invocation envelope is bounded:

```rust
pub struct WorkflowToolInvocation {
    pub invocation_id: WorkflowToolInvocationId,
    pub port_id: WorkflowToolPortId,
    pub semantic_type: String,
    pub schema_revision: u32,
    pub binding_fingerprint: String,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub tool_batch_id: ToolBatchId,
    pub tool_call_id: ToolCallId,
    pub arguments_ref: BlobRef,
    /// Reserved for request/reply ports; always None in notify mode.
    pub reply_promise_id: Option<PromiseId>,
}
```

The receiver endpoint is copied into durable binding state but need not be
repeated in the receiver-facing envelope because the delivery target is
already fixed.

`invocation_id` is deterministically derived from:

```text
session id
+ run id
+ turn id
+ tool batch id
+ tool call id
+ binding fingerprint
```

It is stable across activity retry, worker restart, and session
continue-as-new.

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

Workflow ports follow the same convergence:

```text
tool runtime recognizes WorkflowPort binding
  -> creates a typed internal port-emission effect
  -> engine validates it against the active binding and call joins
  -> same command append records Tool::CallCompleted
     and WorkflowPort::Emitted
```

The generic `ToolEffect` carrier may be reused internally to avoid changing
the tool runtime trait, but the durable contract is `WorkflowPortEvent`, not
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
    /// emitting event; receivers dedup with a per-session high-water mark
    /// because per-receiver delivery preserves log order.
    Session { session_id: SessionId, log_seq: u64 },
    /// Non-session durable workflow (EnvironmentJobWorkflow today). Dedup
    /// is by emission id; for source resolutions the first-writer-wins
    /// ResolvePromise funnel already makes duplicates no-ops.
    Workflow { workflow_id: String },
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
    PortInvocation {
        invocation: WorkflowToolInvocation,
    },
}
```

- For port invocations, `emission_id` equals the invocation id. For
  run-terminal emissions it derives from session, run, and intent token; for
  source resolutions from the source identity and promise id.
- The body is a closed internal enum, not open JSON: receivers are trusted
  internal workflows, new emission kinds extend the enum, and the substrate
  stays deterministic. Domain openness lives inside `PortInvocation` via
  `semantic_type` + CAS payload, exactly one level down.
- `AgentSessionWorkflow` receives the same envelope as everyone else: a
  `RunTerminal` body maps token → `PromiseId` → `ResolvePromise` admission;
  a `SourceResolution` body admits `ResolvePromise` directly. This deletes
  the promise-specific signal vocabulary rather than generalizing around
  it.

## Consumption: Pull First, Push When Needed

The first draft assumed push delivery from day one. Its own first consumer
disproves the need: **P101 Work buffers reports and acts only at the
run-terminal boundary, and its reconciliation activity reads the session log
anyway.** The run-terminal emission already wakes Work; a port-delivery pump
would hand it envelopes it must ignore until reconciliation reads the log
regardless.

So consumption has two modes, and P100 ships them in order:

### Pull (P100 v1 — sufficient for P101)

A boundary-subscribed receiver (one that already receives the run-terminal
emission for the runs it cares about) reads port emissions through one
internal read operation:

```text
read_port_emissions(session_id, run_id | after_log_seq)
  -> bounded Vec<WorkflowToolInvocation>
```

The run-terminal boundary guarantees every prior emission for that run is
durable, so a pull at the boundary is complete by construction. No pump, no
delivery events, no receiver dedup set, no continue-as-new outbox concerns.
Emissions consumed by pull never enter transport state at all.

### Push (deferred slice — first mid-run receiver)

Push earns its complexity only for receivers with no boundary subscription
that must react mid-run — an approval service, request/reply ports. When the
first such consumer exists, port emissions join the **existing** delivery
spine (the generalized run-terminal pump), not a new one:

1. the `Emitted` transition enqueues the envelope into the workflow's
   transient flush queue (replay-rebuilt, exactly like promise
   notifications today);
2. the pump delivers per receiver in session-log order via the fixed
   `deliver_emission` signal; head-of-line retry with deterministic bounded
   backoff preserves order;
3. bounded retries exhausted or a non-retryable determination appends
   terminal `DeliveryFailed`, drops the entry, and surfaces the failure in
   projections — so a dead receiver cannot block the queue or
   continue-as-new indefinitely;
4. flush-queue quiescence gates continue-as-new, exactly as P92 §6 already
   specifies for promise notifications.

Receiver dedup rule, stated precisely because "bounded dedup set" is not a
spec: per producer session, the receiver stores the highest `log_seq` it has
applied. Because per-receiver delivery is FIFO in log order, any envelope
with `log_seq` at or below the mark is a duplicate. This is O(1) per
producer and never evicts too early.

### Tool completion versus delivery

In both modes, the port tool call completes when `Emitted` is durable. It
does not wait for consumption.

This keeps tool execution from coupling model latency to an arbitrarily
long-lived receiver and avoids a deadlock where:

```text
session waits for receiver handler
receiver waits for run terminal or another external event
run cannot terminate because tool call is waiting
```

Receivers must tolerate either ordering between port consumption and the
run-terminal emission. P101 Work reconciles only at the matching
run-terminal boundary.

## Delivery Protocol Is Substrate-Neutral

The first draft specified Temporal signals normatively. The contract is
narrower than Temporal and must be stated that way, because the engine and
this substrate are meant to outlive any single durable-workflow engine:

- **Engine (substrate-neutral, `crates/engine`):** port/binding DTOs, the
  `WorkflowPortEvent` family, deterministic emission identity, the pending
  invariants above. The engine holds no transport state.
- **Transport contract (substrate-neutral trait):**

```rust
pub trait EmissionTransport {
    /// Deliver one envelope to one admitted endpoint.
    fn deliver(
        &self,
        endpoint: &WorkflowEndpointRef,
        envelope: &EmissionEnvelope,
    ) -> DeliveryOutcome;
}

pub enum DeliveryOutcome {
    Accepted,
    Retryable(TransportError),
    Terminal(TransportError),
}
```

  plus an endpoint-resolution trait behind the registry. Receiver-side
  helpers (envelope decode, high-water dedup) are plain library code with no
  workflow-substrate dependency.
- **Temporal adapter (reference implementation, `temporal-workflow` /
  `temporal-server`):** the fixed `deliver_emission` signal, ensure-start
  policy for service endpoints, replay-rebuilt flush queues, continue-as-new
  gating. Signals target the stable workflow id, never a run id.

A different durable engine implements `EmissionTransport` and the admission
funnel; nothing in the engine crate or the port contract changes.

## Re-entrancy And Edge Direction

The combined topology the product needs — a controller managing a session,
that session emitting ports upward, the same session holding promises on
sub-workflows and child sessions — is safe only under explicit
edge-direction rules. They are law, not guidance:

1. **Notify emissions are non-blocking by construction.** The emitting run
   never waits on them, so notify edges can never form a cycle.
2. **A receiver's handler must never require new work in the emitting
   session in order to process that session's emission.** Handlers branch on
   their own state plus the delivered fact. A handler that schedules a run
   in the emitting session must do so as an independent consequence, never
   as a precondition of consuming the emission.
3. **Request/reply ports (future) create an upward wait edge** — the session
   parks awaiting a promise its receiver resolves. The dangerous shape:
   the receiver's handler needs a new run in the emitting session to
   compute the reply, while that session's active run is parked on the
   request — deadlock. Rule 2 forbids it, and binding admission should
   conservatively reject request-mode ports bound to the session's own
   lifecycle controller until a concrete use case demonstrates a safe
   pattern. `await { mailbox: true }` softens head-of-line blocking but does
   not repeal the rule.
4. **Workflow-as-tool promises create downward wait edges** (session waits
   on a sub-workflow it started), mirroring Fleet spawn edges. Downward wait
   edges plus upward notify edges keep the graph a DAG; the P92 cycle
   residual stays unconstructible.

## Notify Now, Promise Later

P100 deliberately distinguishes two interaction shapes:

```text
notify:
  agent calls port
  -> emission is durable
  -> model receives accepted
  -> receiver reacts independently

request/reply (later):
  agent calls workflow-backed request port
  -> same append: WorkflowPort::Emitted + Promise::Created
     (promise_create_effect, PromiseSource::WorkflowToolInvocation)
  -> envelope carries reply_promise_id
  -> receiver eventually resolves the promise through the ordinary
     RunTerminal-style emission -> ResolvePromise admission path
  -> agent uses await/cancel/detach
```

The later mode adds no new machinery: an emission plus a promise effect in
the same append, resolved through the same spine and funnel everything else
uses. It must not keep a tool activity open, add a `workflow_port_wait`, or
overload notify acknowledgements with domain results. The
`reply_promise_id` field exists in the envelope from day one so the shape
cannot ossify notify-only.

The sibling seam, **workflow-as-tool**: a tool call that *starts* an
admitted workflow kind (deterministic workflow id derived from
session/run/turn/call identity, exactly like Fleet and job ids today) and
returns a `PromiseSource::Workflow` promise resolved by the workflow's
terminal emission. It consumes the same endpoint registry and the same
spine; it is P86's job pattern with the provider replaced by a durable
workflow. Deferred, but the registry and envelope are designed for it now.

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

## Messaging: A Candidate, Not A Commitment

The first draft committed to `message_send`/`message_edit`/`message_react`
migrating onto ports bound to a `MessagingWorkflow`. Demoted on review; the
migration must first meet a burden of proof it currently fails:

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

Messaging tools therefore stay on their current inline/outbox path, on the
same side of the line as `message_noop`: pure or already-durable bounded
tools do not become workflows merely because ports exist. If a real
messaging orchestration responsibility emerges that outbox rows cannot own
(cross-message conversation policy, channel-level delivery orchestration
with timers), the migration reopens with that responsibility named first.

The multi-receiver proof in the implementation plan uses a minimal synthetic
service receiver instead.

## Scope

P100 includes:

1. The unified emission envelope and fixed `deliver_emission` signal
   carrying run-terminal notifications and env-job source resolutions —
   replacing the entire promise-specific signal vocabulary
   (`resolve_promise` and `resolve_promise_source`) in one push.
2. Zero or one immutable lifecycle-controller reference on a session.
3. Any bounded number of fixed, same-universe receiver endpoints across that
   session's ports.
4. A generic internal managed-session start operation usable by any
   registered lifecycle controller, not only Work.
5. Schema-defined, model-visible function tools derived into the normal
   session toolset.
6. Immutable controller-bound ports admitted at managed-session creation and
   built-in service ports resolved from known feature grants.
7. Notify-only invocation semantics with per-port rate caps.
8. A typed `WorkflowPortEvent` family (`Emitted`, terminal
   `DeliveryFailed`).
9. Pull-based emission reads for boundary-subscribed receivers.
10. Deterministic emission identity, log-order delivery, and the high-water
    dedup rule.
11. Push delivery on the shared spine as a defined, deferrable slice gated
    on the first mid-run receiver.
12. Session/API projections sufficient to inspect declared ports, emissions,
    and terminal delivery failure.
13. Protocol, reducer, runtime, and live Temporal tests.
14. P101's `work_report` plus a second receiver binding in the same session
    as the proof that routing is not controller-specific.

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
- request/reply ports or a second waiting primitive (the envelope seam
  exists; the mode does not);
- a `Delivered` session event or any transport bookkeeping in the session
  log;
- the messaging migration;
- arbitrary receiver-defined reducer events;
- model-authored tool schemas or runtime port creation;
- adding or retargeting controller-declared ports after managed-session
  creation;
- a public endpoint that lets ordinary session callers target workflows;
- a durable product database for port invocations;
- Work status, goal-loop, approval, or other consumer semantics;
- dynamic tool registration while a turn or tool batch is active.

## API And Projection Surface

P100 is primarily an internal workflow protocol. It adds no public mutation
RPCs.

Existing session reads should project bounded diagnostics:

```rust
pub struct WorkflowToolPortView {
    pub port_id: WorkflowToolPortId,
    pub tool_name: ToolName,
    pub semantic_type: String,
    pub revision: u32,
    pub receiver_workflow_kind: String,
    pub binding_fingerprint: String,
}

pub struct WorkflowPortEmissionView {
    pub invocation_id: WorkflowToolInvocationId,
    pub port_id: WorkflowToolPortId,
    pub run_id: RunId,
    pub tool_call_id: ToolCallId,
    pub delivery_failed: bool,
    pub error_ref: Option<BlobRef>,
}
```

Full arguments remain behind the existing CAS/event authorization boundary.
Do not copy payloads into summary views.

If a future workflow SDK exposes managed-session creation with ports, it
should use an authenticated server operation bound to the calling workflow's
endpoint capability. Dynamic custom changes, if later needed, must not be
implemented as a general `session/tools/update` or accept raw unverified
workflow ids.

## Crate And Module Shape

Expected changes:

```text
crates/engine/
  workflow-port ids, endpoint/binding DTOs
  WorkflowPortEvent (Emitted, DeliveryFailed) and deterministic reducer state
  validation of typed internal emission effects
  session-log projections and emission reads

crates/tools/
  WorkflowPort ToolExecutionMode/binding
  generic inline invocation adapter
  schema validation, rate caps, and stable acknowledgement

crates/temporal-workflow/
  EmissionEnvelope / EmissionProducer / EmissionBody / deliver_emission DTOs
  optional lifecycle-controller and resolved receiver bindings
  shared delivery-spine pump (generalized from promise notifications)
  receiver-side envelope/dedup helper (substrate-neutral module)

crates/temporal-server/
  controller-authorized managed-session creation path
  workflow endpoint registry (built-in resolver)
  effective config/toolset materialization
  EmissionTransport Temporal adapter
  delivery retry/failure projection

crates/api/ and api-projection/
  read-only port and emission views if exposed through session reads
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

- [ ] Add `EmissionEnvelope`, `EmissionProducer`, `EmissionBody`, and
      deterministic `EmissionId` derivation.
- [ ] Replace the `resolve_promise` signal with the fixed generic
      `deliver_emission` signal carrying `RunTerminal` bodies; delete the
      promise-specific DTO/signal names.
- [ ] Fold the env-job push in the same change: `EnvironmentJobWorkflow`
      emits `SourceResolution` bodies through `deliver_emission`; delete
      `resolve_promise_source` and `PromiseSourceResolutionSignal`; preserve
      the P86/P92 env-job live coverage.
- [ ] Include the observed `run_id` and producer `log_seq` in the
      run-terminal body/envelope.
- [ ] `AgentSessionWorkflow` maps `RunTerminal` tokens and
      `SourceResolution` bodies into its existing `ResolvePromise`
      admission.
- [ ] Provide the substrate-neutral receiver helper (envelope decode,
      per-session high-water dedup, emission-id idempotency for workflow
      producers).
- [ ] Prove in a protocol test that a non-session workflow receives the same
      envelope through the same signal.
- [ ] Preserve all Fleet Promise, duplicate delivery, cancellation, and
      continue-as-new tests.

### Slice 2: Port, endpoint, and controller contracts

- [ ] Add validated workflow endpoint, port, and invocation ids.
- [ ] Add the workflow endpoint registry (built-in resolver) with the
      documented two-consumer shape.
- [ ] Add the optional immutable same-universe lifecycle controller to the
      trusted managed-session start path.
- [ ] Add `WorkflowToolPortDefinition`, semantic type/revision, and binding
      fingerprint validation.
- [ ] Bind every port to one admitted `WorkflowEndpointRef`.
- [ ] Admit immutable controller ports atomically with managed-session
      creation.
- [ ] Reject raw receiver creation or retargeting from ordinary public
      config writes.

### Slice 3: Toolset and event-log integration

- [ ] Materialize port definitions as ordinary `FunctionToolSpec` entries
      plus `WorkflowPort` runtime bindings.
- [ ] Validate call arguments against the admitted schema; enforce per-port
      rate and pending caps.
- [ ] Add typed internal emission effect handling.
- [ ] Atomically append tool completion and `WorkflowPort::Emitted`.
- [ ] Project declarations, emissions, and failure without inlining
      payloads.

### Slice 4: Pull reconciliation reads

- [ ] Add the internal `read_port_emissions(session_id, run_id)` operation
      over the session log/projection.
- [ ] Guarantee boundary completeness: every emission of a run is readable
      once that run's terminal emission is delivered.
- [ ] This is the operation P101's `read_work_cycle_result` builds on.

### Slice 5: Push delivery on the shared spine (deferred)

Gated on the first mid-run receiver (approval service or request/reply
ports). Not required for P101.

- [ ] Enqueue `PortInvocation` envelopes on the `Emitted` transition into
      the same flush queue as run-terminal emissions.
- [ ] Per-receiver FIFO in log order; deterministic bounded retry;
      head-of-line semantics.
- [ ] Terminal `DeliveryFailed` append on exhausted/non-retryable delivery;
      queue entry dropped; projection updated.
- [ ] Flush-queue quiescence gates continue-as-new (existing P92 rule, no
      new gate).
- [ ] Receiver-side high-water dedup exercised under duplicate and restart
      conditions.

### Slice 6: Prove multiple receivers and Work

- [ ] Register a minimal synthetic service receiver with a different port
      schema (this replaces the first draft's messaging topology as the
      second-receiver proof).
- [ ] Bind controller and service ports to the same session and prove each
      emission resolves to only its fixed receiver.
- [ ] Have P101 declare `work_report` when it creates its managed session.
- [ ] Prove Work consumes `WorkReportV1` by pull at the run-terminal
      boundary with no port-specific transport.
- [ ] Prove no Work-specific tool effect, signal, or delivery loop is
      required.

### Slice 7: Failure and compatibility coverage

- [ ] Test schema-invalid and cap-exceeding calls create no emission.
- [ ] Test duplicate tool-result admission creates one emission.
- [ ] Test pull reads are complete at the run-terminal boundary across
      worker restart and session continue-as-new.
- [ ] Test public config cannot retarget controller or service bindings.
- [ ] Test ordinary feature reconciliation can add/remove its own built-in
      service ports at an idle boundary.
- [ ] With slice 5: crash before signal, crash after signal/before receiver
      apply, duplicate delivery, receiver absent, terminal delivery
      failure, both sides' continue-as-new independently.
- [ ] Confirm Fleet, Promises, messaging bridges, MCP, and standalone
      sessions are unchanged.

Live Temporal tests must source `local/env.sh` and run serially with
`--test-threads=1`.

## Acceptance Criteria

P100 is complete when:

1. One envelope and one fixed signal carry every inbound cross-workflow
   fact — run-terminal notifications to session and non-session receivers
   alike, and env-job source resolutions; both promise-specific signals are
   gone and the session workflow has exactly one inbound funnel.
2. A trusted controller workflow can create/own a session with a typed
   port, while a known feature can independently add a port bound to a
   registered service workflow.
3. The model sees only the declared tool name, description, and schema; it
   cannot choose the receiver or transport.
4. A valid call atomically produces an ordinary successful tool result and
   one typed, CAS-backed `WorkflowPort::Emitted` fact linked to the exact
   run/turn/batch/call.
5. A boundary-subscribed receiver can read a run's complete emissions at
   the run-terminal boundary across retry, restart, and continue-as-new.
6. Invalid arguments, cap violations, and unauthorized port
   declaration/mutation fail without emitting.
7. Toolset derivation remains capability-driven and no public
   `session/tools/update` surface returns.
8. P101 implements `work_report` only as a schema and a pull-consuming Work
   handler over this substrate, with no Work-specific transport.
9. One session can bind controller and service ports to two different fixed
   receiver workflows without changing the session engine.
10. The session log contains no transport bookkeeping events; `Emitted` and
    terminal `DeliveryFailed` are the only port events.
11. Existing Fleet, Promise, external messaging, MCP, and run-terminal
    behavior remains semantically unchanged (renamed, not re-implemented).

## Follow-On Boundary

P100 intentionally leaves these seams, designed-for but unbuilt:

- **Request/reply ports**: `Promise::Created` in the same append as
  `Emitted`, `reply_promise_id` in the envelope, resolution through the
  ordinary spine; subject to the re-entrancy law.
- **Workflow-as-tool**: start an admitted workflow kind from the endpoint
  registry with a deterministic id and a `PromiseSource::Workflow` promise
  resolved by its terminal emission.
- **Push delivery** for mid-run receivers (slice 5).
- An authenticated workflow SDK exposing custom endpoint registration and
  controller-owned managed-session creation.
- The messaging migration, if a MessagingWorkflow responsibility that outbox
  rows cannot own is ever named.
- Port emission projections feeding mission control or evals.
- Hardened deterministic workflows exposing request ports as agent tools.
- External systems starting controller workflows through a separate ingress
  plane.

Those extensions must reuse the fixed envelope, emission identity, event
family, and authority model. They must not weaken the central rule: the
agent chooses a granted semantic operation, never an arbitrary destination.
