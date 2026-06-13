# P53: Async Agent Workflow

**Status**
- Completed

**Progress**
- Design decision recorded.
- Added `agent-core::io` with CoreAgent-specific `CoreAgentLlm` and
  `CoreAgentTools` traits plus LLM/tool request/result records.
- Removed `agent-core::model::effects`, `agent-core::runner::effects`,
  `EffectId`, effect joins, and effect id cursors from the active core model.
- Implemented `CoreAgentLlm` for `llm-runtime::LlmRuntime`.
- Implemented batch-oriented `CoreAgentTools` for `agent-tools`
  `InlineHostToolRuntime`.
- Switched local runtime tests, CLI setup, and eval setup to compose directly
  through the LLM/tool traits.

## Goal

Replace the current generic effect intent/dispatch/receipt lifecycle in
`agent-core` with an async agent workflow model.

Forge should keep the durable agent session log and deterministic reducer, but
it should stop acting as a substrate-neutral effect scheduler. The core agent
logic should be able to interleave ordinary Rust branching with awaited LLM and
tool calls, matching the programming model we want when running inside
Temporal.

Local mode is allowed to have weaker crash semantics. If a process crashes
after an LLM or tool call but before the corresponding Forge domain events are
recorded, local mode may repeat that work. Production users who need stronger
semantics should run the workflow in Temporal or another durable workflow
substrate.

This is a breaking refactor. Do not preserve compatibility with the current
effect event model.

## Decision

Keep:

- event-sourced Forge session log
- synchronous deterministic reducer
- replayable `SessionState`
- provider-native LLM request planning
- host tool target selection
- client-facing `agent-api` projection over session/run/item events

Remove as the central execution model:

- `AgentEffectIntent`
- `AgentEffectReceipt`
- `EffectEvent::{IntentCreated, Dispatched, ReceiptAccepted, ...}`
- `PendingEffectState`
- generic `EffectExecutor`
- runner quiescence based on pending effects
- `SessionCommand::RecordEffectReceipt`

Replace them with async workflow logic over two CoreAgent I/O traits:

```rust
#[async_trait::async_trait]
pub trait CoreAgentLlm: Send + Sync {
    async fn generate(
        &self,
        request: LlmGenerationRequest,
    ) -> Result<LlmGenerationResult, LlmGenerationError>;
}

#[async_trait::async_trait]
pub trait CoreAgentTools: Send + Sync {
    async fn invoke_batch(
        &self,
        request: ToolInvocationBatchRequest,
    ) -> Result<ToolInvocationBatchResult, ToolInvocationError>;
}
```

The async workflow can branch directly on returned values:

```rust
let generation = llm.generate(request).await?;

if generation.facts.tool_calls.is_empty() {
    append_apply(events_from_final_generation(generation)).await?;
} else {
    append_apply(events_from_tool_calls(&generation)).await?;
    let tool_batch = plan_tool_batch(session.state(), &generation)?;
    append_apply(tool_batch.events_before_call).await?;
    let results = tools.invoke_batch(tool_batch.request).await?;
    append_apply(events_from_tool_batch_result(results)).await?;
}
```

The important invariant is:

```text
Only committed Forge events mutate Forge session state.
```

The workflow may perform async work between committed event batches. The
substrate decides whether those async calls are durable and replay-safe.

## Terminology

Use "host" only for filesystem/process execution targets.

Host-related concepts:

- host filesystem
- host process execution
- host tools
- `ToolExecutionTarget { namespace: "host", id: ... }`
- `HostToolContext`

Do not call the wider runtime capability object `Host`. LLM calls are not host
calls.

Do not introduce a generic "port model" in the kernel. CoreAgent needs only two
runtime dependency traits:

- `CoreAgentLlm`
- `CoreAgentTools`

Custom agent compositions can use whatever I/O shape they want: direct function
calls, local traits, Temporal activity stubs, Python services, connector SDKs,
or application-specific dependency structs. They do not need to adopt a Forge
`AgentPorts` abstraction.

Temporal CoreAgent implementations may implement these two traits with activity
stubs, or call activities directly from Temporal-specific workflow code. Local
CoreAgent implementations can call provider clients and `agent-tools` directly.

## Why The Current Model Is Too Heavy

The current model makes `agent-core` implement a small workflow engine:

```text
policy creates effect intent
runner flushes causal events
runner dispatches or executes effect
executor returns receipt or dispatch status
runner records receipt
policy branches on settled receipt
```

That shape is useful only if Forge itself wants to own durable effect dispatch.
But the intended production substrate is Temporal, and Temporal already owns:

- activity scheduling
- activity result replay
- retries
- cancellation
- timers
- workflow crash recovery
- command/history matching

Keeping a second generic effect lifecycle in Forge duplicates those concerns and
makes extension expensive. Every new side effect needs a new core effect
variant, receipt variant, validation branch, pending-state rule, projection
case, and runner path.

The desired model is closer to Temporal workflow code:

```text
append domain event
await activity
branch on activity result
append next domain events
await next activity
...
```

On Temporal replay, the awaited activity returns the recorded result. In local
mode, the awaited function just runs again if the process has lost its in-memory
state before the result event was committed.

## Substrate Semantics

### Temporal Mode

Temporal mode should run the agent loop as workflow code.

LLM and tool calls are Temporal activities:

```text
agent workflow
  -> build provider-native LLM request
  -> execute llm_generate activity
  -> branch on recorded activity result
  -> execute tool_invoke_batch activity or fan out tool_invoke activities
  -> append Forge domain events
```

Activity implementations can reuse the same provider/tool adapters used by
local mode. The difference is where the call is made:

- workflow code calls a Temporal activity stub
- activity worker calls the LLM provider or host tool implementation
- tool batches may fan out to parallel activities when the Temporal SDK/runtime
  supports that shape
- Temporal records the result in workflow history
- replay returns the same result to workflow logic without repeating the call

The Forge session log remains the client-facing domain history. Temporal
history remains the workflow execution history. They do not need to use the
same schema.

P53 does not require choosing the final Temporal storage shape. Acceptable
future implementations include:

- the workflow appends Forge session events to a session store through a
  dedicated append activity
- the workflow owns session events in workflow state and exposes them through
  queries/signals plus an API gateway projection
- a hosted runtime writes projections from workflow progress to an external
  readable store

The key point is that Forge no longer models provider/tool execution as a
generic durable outbox inside `agent-core`.

### Local Mode

Local mode may use direct Tokio calls:

```text
append turn planned
call provider
append generation recorded
call host tool
append tool result recorded
```

If local mode crashes after a provider/tool call and before appending the
result event, it may repeat the call on resume. That is acceptable.

Local mode should still preserve useful local invariants:

- append admitted user input before acknowledging the command
- append planned request/domain facts before making a call when those facts are
  useful for debugging or retrying
- include request fingerprints and stable run/turn/tool-call ids
- keep provider raw responses and tool outputs blob-backed
- make tests deterministic with scripted CoreAgent I/O implementations

Do not add a production outbox to local mode as part of P53. That reintroduces
the old design.

### Test Mode

Tests should use scripted implementations:

```rust
pub struct ScriptedCoreAgentLlm {
    pub generations: VecDeque<LlmGenerationResult>,
}

pub struct ScriptedCoreAgentTools {
    pub invocations: VecDeque<ToolInvocationBatchResult>,
}
```

This gives deterministic end-to-end tests without effect receipts.

## Target Core Shape

`agent-core` should become a pure domain and workflow-planning crate. It may
contain async orchestration over traits, but it must not depend on Tokio,
Temporal SDK types, provider clients, host clients, filesystems, process
execution, or network I/O.

Target module shape:

```text
crates/agent-core/src/
  model/
    command.rs
    events.rs
    state.rs
    llm.rs
    tooling.rs
    ...
  admit.rs
  apply.rs
  planning.rs
  workflow.rs
  io.rs
  storage/
```

Responsibilities:

- `admit.rs`: command to initial domain events
- `apply.rs`: committed event to state
- `planning.rs`: pure helpers that inspect state and build next domain facts
- `workflow.rs`: async CoreAgent loop over `CoreAgentLlm` and
  `CoreAgentTools`
- `io.rs`: CoreAgent I/O traits and request/result records

If the Temporal Rust SDK makes a generic `workflow.rs` awkward, keep
`workflow.rs` small and push Temporal-specific orchestration to a future
`agent-temporal` crate. The pure planning helpers must still be reusable.

## New I/O Records

### LLM Generation

Replace `LlmGenerationIntent` and `LlmGenerationReceipt` with request/result
records that are not events by themselves:

```rust
pub struct LlmGenerationRequest {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub request: LlmRequest,
}

pub struct LlmGenerationResult {
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub status: LlmGenerationStatus,
    pub context_items: Vec<UncommittedContextItem>,
    pub facts: LlmGenerationFacts,
}
```

`LlmGenerationStatus`, `LlmGenerationFacts`, `LlmFinish`, and `LlmUsage` can
survive mostly unchanged, but they should move out of the effect model and into
an LLM/domain result module.

The request still carries provider-native `LlmRequest` values with resolved
context metadata. The LLM adapter still stores raw/native provider outputs in
the blob store and extracts only reducer-facing facts.

### Tool Invocation

Replace `ToolInvocationIntent` and `ToolInvocationReceipt` with request/result
records. Tool invocation is batch-oriented so `agent-core` does not need Tokio,
`futures::join_all`, or Temporal-specific fan-out APIs to express parallel tool
execution:

```rust
pub struct ToolInvocationBatchRequest {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub batch_id: ToolBatchId,
    pub calls: Vec<ToolInvocationRequest>,
}

pub struct ToolInvocationRequest {
    pub call_id: ToolCallId,
    pub tool_name: ToolName,
    pub arguments_ref: BlobRef,
    pub execution_target: Option<ToolExecutionTarget>,
}

pub struct ToolInvocationBatchResult {
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub batch_id: ToolBatchId,
    pub results: Vec<ToolInvocationResult>,
}

pub struct ToolInvocationResult {
    pub call_id: ToolCallId,
    pub status: ToolCallStatus,
    pub output_ref: Option<BlobRef>,
    pub model_visible_output_ref: Option<BlobRef>,
    pub error_ref: Option<BlobRef>,
}
```

`ToolExecutionTarget` remains a domain identity and stays copied onto the
request before invocation. Runtime/tool code resolves that target to a concrete
`HostToolContext` or other tool-specific capability.

Batch execution semantics:

- planning creates one logical tool batch from model-observed tool calls
- planning may split execution into waves when `ToolParallelism::Exclusive`
  tools are present
- each wave is sent through `CoreAgentTools::invoke_batch`
- the trait implementation decides how to execute calls in the batch
- local Tokio runtimes may spawn/join parallel-safe calls
- Temporal runtimes may fan out activities and await them
- simple tests or constrained runtimes may run calls serially
- returned results must preserve `call_id` identity; ordering should match the
  request when practical, but reducers should match by `call_id`

This keeps parallelism as a runtime/substrate concern while preserving a simple
agent workflow shape.

## New Domain Events

The event log should record domain facts, not generic effect lifecycle facts.

Recommended first cut:

```rust
pub enum SessionEventKind {
    Lifecycle(SessionLifecycleEvent),
    Run(RunEvent),
    Turn(TurnEvent),
    Context(ContextEvent),
    ToolConfig(ToolConfigEvent),
    Tool(ToolEvent),
    Llm(LlmEvent),
}
```

LLM events:

```rust
pub enum LlmEvent {
    GenerationStarted {
        run_id: RunId,
        turn_id: TurnId,
        request: LlmRequest,
    },
    GenerationCompleted {
        run_id: RunId,
        turn_id: TurnId,
        status: LlmGenerationStatus,
        facts: LlmGenerationFacts,
    },
}
```

Tool events:

```rust
pub enum ToolEvent {
    BatchStarted {
        run_id: RunId,
        turn_id: TurnId,
        batch_id: ToolBatchId,
        calls: Vec<ObservedToolCall>,
    },
    CallStarted {
        run_id: RunId,
        turn_id: TurnId,
        batch_id: ToolBatchId,
        call_id: ToolCallId,
        tool_name: ToolName,
        arguments_ref: BlobRef,
        execution_target: Option<ToolExecutionTarget>,
    },
    CallCompleted {
        run_id: RunId,
        turn_id: TurnId,
        batch_id: ToolBatchId,
        result: ToolInvocationResult,
    },
    BatchCompleted {
        run_id: RunId,
        turn_id: TurnId,
        batch_id: ToolBatchId,
    },
}
```

`ContextEvent::ItemsRecorded` remains the event that assigns durable context
item ids. LLM and tool result records should contain uncommitted/model-visible
items; workflow helpers turn those into `ContextEvent::ItemsRecorded` events
using the current id cursors.

Open question for implementation: `LlmEvent::GenerationStarted` may replace
`TurnEvent::Planned`, or `TurnEvent::Planned` may stay as the durable planned
request and `GenerationStarted` can be omitted. Prefer the smaller event set
when implementing.

The minimum required durable facts are:

- the planned request, if we want the exact provider request replayable in the
  Forge log
- generated context items and reducer facts
- tool calls observed from the generation
- tool invocation results
- turn/run completion

Started/progress events are useful for UI, but they are not the execution
contract.

## Workflow Shape

The async workflow owns the high-level loop.

Sketch:

```rust
pub async fn drive_session<P>(
    session: &mut SessionWorkflow<P>,
    llm: &impl CoreAgentLlm,
    tools: &impl CoreAgentTools,
    command: Option<SessionCommand>,
) -> Result<DriveOutcome, WorkflowError>
where
    P: SessionStore,
{
    if let Some(command) = command {
        let events = admit_command(session.state(), command)?;
        session.append_apply(events).await?;
    }

    loop {
        if maybe_start_next_run(session).await? {
            continue;
        }

        if maybe_run_active_turn(session, llm).await? {
            continue;
        }

        if maybe_run_tool_batch(session, tools).await? {
            continue;
        }

        if maybe_complete_run(session).await? {
            continue;
        }

        return Ok(session.outcome());
    }
}
```

The important difference from the current `CorePlanner` is that helpers such
as `maybe_run_active_turn` may await:

```rust
async fn maybe_run_active_turn<A>(
    session: &mut SessionWorkflow<impl SessionStore>,
    llm: &A,
) -> Result<bool, WorkflowError>
where
    A: CoreAgentLlm,
{
    let Some(plan) = plan_generation(session.state())? else {
        return Ok(false);
    };

    session.append_apply(plan.events_before_call).await?;

    let result = llm.generate(plan.request).await?;

    let events = events_from_generation_result(session.state(), result)?;
    session.append_apply(events).await?;

    Ok(true)
}
```

This restores ordinary control flow:

- call LLM
- inspect finish reason
- if final, complete the run
- if tool calls, invoke a tool batch through `CoreAgentTools`
- if context limit, compact or fail
- if cancelled/failed, record terminal outcome

No pending effect table is needed for the normal path.

## Session Store Boundary

The workflow should use the existing `SessionStore` as the append/read boundary.
Do not introduce a second appender trait in the first refactor.

`SessionStore::append` already has the right shape for workflow driving:

- it accepts a `session_id`
- it checks the expected head
- it commits a batch of `UncommittedSessionEvent`
- it returns assigned `SessionEntry` values

The workflow helper should wrap `SessionStore::append` and `CoreApplyEvent` so
local and Temporal implementations share the same domain rule:

```rust
pub struct SessionWorkflow<S> {
    session_id: SessionId,
    state: SessionState,
    store: Arc<S>,
    apply: CoreApplyEvent,
}

impl<S: SessionStore> SessionWorkflow<S> {
    pub async fn append_apply(
        &mut self,
        proposals: Vec<SessionEventProposal>,
    ) -> Result<(), WorkflowError> {
        let events = proposals_to_events(proposals);
        let result = self.store.append(AppendSessionEvents {
            session_id: self.session_id.clone(),
            expected_head: self.state.reduced_to.clone(),
            events,
        }).await?;

        for entry in result.entries {
            self.apply.apply(&mut self.state, &entry)?;
        }
        Ok(())
    }
}
```

Local mode uses the normal in-memory or persistent `SessionStore`.

Temporal mode should prefer the same `SessionStore` contract if practical. If a
future Temporal implementation cannot use `SessionStore` directly, add a small
adapter then. Do not add that abstraction preemptively in P53.

## Refactor Plan

## Current Implementation Status

First end-to-end slice is implemented:

- `agent-core::io` defines CoreAgent-specific `CoreAgentLlm` and
  batch-oriented `CoreAgentTools` traits.
- `SessionEventKind::Effect` is removed.
- `PendingEffectState`, `pending_effects`, turn `generation_effect_id`, and
  tool `pending_effect_id` are removed from replayed state.
- Turn generation now uses `TurnEvent::GenerationRequested`,
  `TurnEvent::GenerationCompleted`, and `TurnEvent::Completed`.
- Tool execution now uses `ToolEvent::CallStarted` and
  `ToolEvent::CallCompleted`; completed tool results are still reduced into
  context items before `ToolEvent::BatchCompleted`.
- `CoreAgentWorkflow` drives the workflow directly: deterministic planning
  events, awaited LLM/tool trait calls, then domain result events.
- `SessionRunner` remains the local runner facade for command admission, state
  loading, and `DriveOutcome` shaping.
- LLM/tool runtime errors are recorded as failed generation/tool result domain
  facts instead of aborting the drive. The planner can then retry, change the
  context window, change strategy, or eventually emit a terminal run failure.
- `RunnerQuiescence::WaitingOnEffects` and receipt admission are removed.
- `agent-api`, `agent-local`, CLI, and eval wiring no longer expose or depend
  on core effect lifecycle events.
- `model/effects.rs`, `runner/effects.rs`, `EffectId`, effect joins, and the
  temporary adapter bridges have been deleted.

### [x] G1: Introduce New Domain Result Types

- Move LLM result types out of `model/effects.rs`.
- Move tool invocation result types out of `model/effects.rs`.
- Add `LlmGenerationRequest`, `LlmGenerationResult`,
  `ToolInvocationBatchRequest`, `ToolInvocationRequest`,
  `ToolInvocationBatchResult`, and `ToolInvocationResult`.
- Add `io.rs` with the CoreAgent-specific `CoreAgentLlm` and
  `CoreAgentTools` traits.
- Delete the old effect intent/receipt model after call sites are moved.

### [x] G2: Replace Effect Events With Domain Events

- Add `LlmEvent` or fold generation events into `TurnEvent`.
- Add `ToolEvent::CallStarted` and `ToolEvent::CallCompleted`.
- Remove `SessionEventKind::Effect`.
- Remove `PendingEffectState` and `pending_effects` from `SessionState`.
- Remove `last_effect_id` from id cursors unless another domain id needs it.
- Remove effect receipt validation.
- Adjust `CoreApplyEvent` to mutate turn/tool state directly from domain
  events.

### [x] G3: Rewrite Policy As Planning Helpers

- Renamed `policy.rs` to `planning.rs`.
- Renamed `PolicyPipeline` to `CorePlanner`, `DecideNext` to `PlanNext`, and
  `PolicyError` to `PlanningError`.
- Renamed the core layers from `Core*Policy` to `Core*Planner`.
- Moved workflow request/result helpers into `workflow.rs`.
- Keep pure helpers for:
  - starting queued runs
  - planning context windows
  - building provider-native `LlmRequest`
  - deriving request fingerprints
  - selecting visible tools and provider tool choice
  - resolving tool execution targets
  - converting LLM results to context/turn events
  - converting tool results to context/tool events
  - deriving terminal run events
- Delete the "emit effect intent and wait for receipt" policy path.

### [x] G4: Add Async Workflow Driver

- Replace `SessionRunner`'s effect loop with async workflow driving.
- Move deterministic/async orchestration into `CoreAgentWorkflow` in
  `workflow.rs`.
- The driver:
  - loads/replays state
  - admits an optional command
  - appends/applies admitted events
  - calls planning helpers
  - awaits CoreAgent LLM/tool traits
  - appends/applies result domain events
  - repeats until idle/closed/iteration limit
- Keep an iteration limit for bugs and runaway loops.
- Remove `RunnerQuiescence::WaitingOnEffects`.
- Add quiescence/status variants based on domain state only:
  - idle
  - closed
  - active/running, if a drive call returns while work is still in progress
  - iteration limit reached

### [x] G5: Rework LLM Adapter Crate

- Repurpose the LLM adapter crate from an `EffectExecutor` implementation into a
  `CoreAgentLlm` implementation.
- Keep provider-native request materialization.
- Keep raw/native response blob retention.
- Keep reducer fact extraction.
- Return `LlmGenerationResult` instead of `AgentEffectReceipt`.
- Tests should assert provider JSON and returned generation results.

### [x] G6: Rework Tool Execution

- Replace tool `EffectExecutor` paths with `CoreAgentTools`.
- Make `CoreAgentTools` batch-oriented with `invoke_batch`.
- Reuse `agent-tools` host filesystem/process packages.
- Tool runtime maps `ToolExecutionTarget` to concrete capabilities such as the
  local host, a remote host protocol connection, or a future sandbox target.
- Return `ToolInvocationBatchResult` directly.
- Keep target selection in planning so each call request carries the resolved
  target.
- Keep actual parallel execution inside the runtime/substrate implementation,
  not inside `agent-core`.

### [x] G7: Update API Projection

- Remove effect views from `agent-api` unless there is a client-facing need for
  generic effect telemetry.
- Project LLM progress/results from `LlmEvent` or `TurnEvent`.
- Project tool progress/results from `ToolEvent`.
- Preserve session/run/item view shape for CLI and future frontends.

### G8: Delete Obsolete Outbox Roadmap

P53 supersedes P60's generic effect dispatch outbox direction.

After this refactor lands:

- mark P60 closed as superseded
- update P47/P50/P51 notes where they refer to effect intents/receipts
- update `README.md`, `AGENTS.md`, and `docs/spec/01-agent-idea.md`

## Tests To Rewrite

Remove tests that assert generic effect lifecycle behavior:

- effect intent creation
- dispatch-only executor behavior
- receipt admission
- pending-effect quiescence
- outbox replay behavior

Add tests for the new workflow behavior:

- local scripted LLM final answer completes a run
- local scripted LLM tool call invokes scripted tool and performs the next LLM
  turn
- provider request planning remains deterministic across replay
- tool target selection is copied into each `ToolInvocationRequest`
- parallel-safe tool calls can be delivered to one `CoreAgentTools` batch call
- local retry after a planned request with no result may repeat the provider
  call
- failed LLM result records a failed turn/run
- failed tool result records model-visible tool error context when available
- cancellation maps to domain cancellation events without generic effect state
- iteration limit catches runaway workflow loops

## Non-Goals

- exactly-once local execution
- generic durable effect outbox
- provider-neutral LLM message abstraction
- making host mean LLM/provider execution
- embedding Temporal SDK types in `agent-core`
- preserving old effect event compatibility

## Done When

- `agent-core` has no generic effect event/pending-effect lifecycle in session
  events, state, planning, workflow, or runner code.
- The agent loop can branch directly on awaited LLM/tool results.
- Local runtime uses direct CoreAgent I/O traits and accepts weak crash
  semantics.
- LLM adapters implement `CoreAgentLlm`.
- Tool adapters implement batch-oriented `CoreAgentTools`.
- API projection no longer exposes core effect lifecycle internals.
- Tests cover final-answer, tool-call, failure, and target-selection flows
  without effect receipts.
- Roadmap and top-level docs describe the async workflow architecture.
