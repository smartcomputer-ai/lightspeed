# P56: Substrate-Neutral CoreAgent Drive

**Status**
- Implemented

**Progress**
- Design pressure identified from P55 Temporal Claw.
- Added `CoreAgentDrive` and `CoreAgentAction` in `agent-core`.
- Removed the local runner facade from `agent-core`.
- Moved `SessionRunner`, `RunnerStores`, `DriveCommand`, `DriveSession`,
  `DriveOutcome`, `RunnerQuiescence`, and `RunnerError` to `agent-local`.
- Rebuilt the local runner as an action fulfiller over `CoreAgentDrive`.
- Rebuilt `agents/claw` workflow driving on `CoreAgentDrive` while keeping
  Temporal signals, queries, and activities in the Claw crate.
- Documented `CoreAgentLlm` and `CoreAgentTools` as execution adapter traits,
  not workflow-side substrate traits.
- Verified `cargo test -p agent-core`, `cargo test -p agent-local`,
  `cargo test -p claw`, and the ignored Temporal fake live test.

## Goal

Refactor the CoreAgent drive loop so `agent-core` contains only the parts that
both local runtime and Temporal workflow code can use.

P53 correctly removed the old durable effect lifecycle, but its replacement
kept a local-runtime-shaped async workflow in `agent-core`. P55 showed that
Temporal Rust workflows cannot cleanly reuse that shape because workflow code
must not own direct `SessionStore`, `BlobStore`, `CoreAgentLlm`, or
`CoreAgentTools` dependencies.

The target shape is:

```text
agent-core:
  deterministic CoreAgent state/admit/apply/plan/codec
  serializable CoreAgent request/result records
  substrate-neutral drive machine that emits actions

agent-local:
  local substrate that fulfills actions with SessionStore, BlobStore,
  CoreAgentLlm, and CoreAgentTools
  local runner facade and DriveCommand/DriveOutcome API

agents/claw:
  Temporal substrate that fulfills actions with activities
```

Do not add a general `agent-temporal` crate in P56. Extracting a Temporal helper
crate should wait until more than one Temporal agent or workflow needs shared
Temporal-specific glue.

## Problem

`crates/agent-core/src/core_agent/workflow.rs` currently mixes reusable domain
logic with local execution mechanics:

- direct `SessionStore::append`
- direct `BlobStore` writes for failure conversion
- direct awaited `CoreAgentLlm::generate`
- direct awaited `CoreAgentTools::invoke_batch`
- `WorkflowEventBuffer` staging before durable append
- `RunnerQuiescence` and `DriveOutcome` shaping for local command execution

That is useful for local mode, but it is not a neutral substrate boundary.
`crates/agent-core/src/runner` is part of the same problem: it is a local
runtime facade, not core substrate. It should move to `agent-local` as part
of P56 rather than remain in `agent-core` behind compatibility wrappers.

Temporal workflow code needs to:

- call activities for Postgres/CAS writes
- call activities for LLM/tool execution
- use non-`Send` workflow futures and `WorkflowContext`
- mutate workflow state only through Temporal workflow state APIs
- stay deterministic on replay

The current `CoreAgentLlm` and `CoreAgentTools` traits are still useful runtime
adapter traits, especially inside local runtime and Temporal activities. They
are not the right abstraction for workflow-side Temporal dispatch because they
require `Send + Sync` implementers and `async_trait` defaults to `Send` futures.

## Decision

Introduce a substrate-neutral CoreAgent drive machine in `agent-core`.

The drive machine should own only deterministic CoreAgent state and durable head
metadata. It should not perform async I/O. Instead it should emit explicit
actions that the embedding substrate fulfills.

Sketch:

```rust
pub struct CoreAgentDrive {
    session_id: SessionId,
    state: CoreAgentState,
    head: Option<SessionPosition>,
}

pub enum CoreAgentAction {
    AppendEvents {
        expected_head: Option<SessionPosition>,
        events: Vec<DynamicUncommittedSessionEvent>,
    },
    GenerateLlm {
        request: LlmGenerationRequest,
    },
    InvokeTools {
        request: ToolInvocationBatchRequest,
    },
    Idle,
    Closed,
    StepLimitReached,
}
```

The exact names are open, but the semantics are not:

- `agent-core` decides what action is needed next.
- The substrate executes the action.
- The substrate returns committed entries or I/O results.
- The drive machine converts those results into proposals/actions and applies
  only committed entries.

The shared invariant becomes:

```text
Only committed Lightspeed session entries mutate CoreAgentState.
```

This is stricter than the current local `WorkflowEventBuffer` behavior and
matches the Temporal implementation.

## Proposed API Shape

The drive machine should support:

```rust
impl CoreAgentDrive {
    pub fn from_replayed(
        session_id: SessionId,
        state: CoreAgentState,
        head: Option<SessionPosition>,
    ) -> Self;

    pub fn admit_command(
        &mut self,
        command: CoreAgentCommand,
        observed_at_ms: u64,
    ) -> Result<CoreAgentAction, DriveError>;

    pub fn next_action(
        &mut self,
        observed_at_ms: u64,
        max_steps: usize,
    ) -> Result<CoreAgentAction, DriveError>;

    pub fn resume_appended(
        &mut self,
        entries: Vec<DynamicSessionEntry>,
    ) -> Result<(), DriveError>;

    pub fn resume_generation(
        &mut self,
        result: LlmGenerationResult,
        observed_at_ms: u64,
    ) -> Result<CoreAgentAction, DriveError>;

    pub fn resume_tool_batch(
        &mut self,
        result: ToolInvocationBatchResult,
        observed_at_ms: u64,
    ) -> Result<CoreAgentAction, DriveError>;

    pub fn state(&self) -> &CoreAgentState;
    pub fn head(&self) -> Option<&SessionPosition>;
}
```

This sketch is intentionally not final. The important point is that the machine
must be expressible without `async`, Tokio, Temporal SDK types, provider
clients, filesystem/process code, or Postgres code.

## Local Runtime After P56

`agent-local` should own the local substrate loop:

```text
load/replay session state
create CoreAgentDrive
admit command
while action is not idle/closed/limit:
  if AppendEvents:
    call SessionStore::append
    drive.resume_appended(committed_entries)
  if GenerateLlm:
    call CoreAgentLlm::generate
    on error, convert to failed generation result
    drive.resume_generation(result)
  if InvokeTools:
    call CoreAgentTools::invoke_batch
    on error, convert to failed tool result
    drive.resume_tool_batch(result)
return DriveOutcome
```

`SessionRunner`, `RunnerStores`, `DriveCommand`, `DriveSession`,
`DriveOutcome`, and `RunnerQuiescence` should live in `agent-local`, not
`agent-core`. P56 can be aggressive here: these APIs are still internal to the
current workspace direction, and preserving old import paths is less important
than removing the misleading boundary.

Avoid a long deprecation/re-export phase unless an immediate workspace caller
needs it. Update call sites directly.

## Temporal Claw After P56

`agents/claw` should own the Temporal substrate loop:

```text
workflow state owns CoreAgentDrive or its serializable parts
signals add pending admissions
for each admission:
  if TextRun: call put_blob activity, build RequestRun
  if CoreCommand: decode DynamicCommand
  drive.admit_command(...)
  fulfill emitted actions

while drive emits work:
  if AppendEvents: call append_events activity
  if GenerateLlm: call llm_generate activity
  if InvokeTools: call tool_invoke_batch activity
  resume drive with committed entries/results
wait for next signal
```

The workflow should not duplicate CoreAgent planning/request/result logic. It
should only translate drive actions into Temporal activities.

## I/O Traits

Keep `LlmGenerationRequest`, `LlmGenerationResult`,
`ToolInvocationBatchRequest`, and `ToolInvocationBatchResult` in `agent-core`.
They are the right shared request/result records.

`CoreAgentLlm` and `CoreAgentTools` can stay in `agent-core` for now, but they
should be documented as runtime adapter traits, not workflow substrate traits.
They are appropriate for:

- local runtime execution
- fake/scripted tests
- Temporal activity implementations

They are not required for Temporal workflow code.

If this remains confusing after P56, consider moving the traits to
`agent-local` or splitting `io.rs` into:

```text
io_types.rs      shared serializable request/result records
runtime_io.rs    local/activity adapter traits
```

## Failure Semantics

P56 should preserve local behavior where LLM/tool runtime errors become
reducer-visible failed generation/tool results instead of aborting the drive.

Temporal Claw should use the same conversion, either:

- inside activities, returning failed result records instead of failing the
  activity for expected provider/tool errors; or
- in the Temporal substrate loop, converting activity failures to failed result
  records when appropriate.

Unexpected infrastructure failures may still fail the workflow. P56 should make
the distinction explicit.

## Refactor Plan

### G1: Extract Drive Action Types

- Add CoreAgent drive action/result types in `agent-core`.
- Keep action payloads serializable and dynamic-store-compatible.
- Avoid Tokio, Temporal SDK, provider, host, filesystem, process, or Postgres
  dependencies.

### G2: Extract Drive Machine

- Move the deterministic loop logic out of `CoreAgentWorkflow` into a
  substrate-neutral machine.
- Reuse existing `CoreAdmitCommand`, `CoreApplyEvent`, `CorePlanner`,
  `CoreAgentCodec`, `next_generation_request`, `next_tool_batch_request`,
  `generation_result_proposals`, and `tool_batch_result_proposals`.
- Apply only committed entries returned through `resume_appended`.

### G3: Rebuild Local SessionRunner On The Drive Machine

- Move `crates/agent-core/src/runner` into `crates/agent-local` rather than
  preserving it in `agent-core`.
- Keep local runner behavior stable where practical, but do not preserve old
  `agent_core::runner` import paths unless required by a live call site.
- Preserve `DriveOutcome` and `RunnerQuiescence` as local facade outputs inside
  `agent-local`.
- Preserve local tests for final-answer, tool-call, failure, and iteration
  limit behavior.
- Remove or shrink `WorkflowEventBuffer`.

### G4: Rebuild Claw Workflow On The Drive Machine

- Replace duplicated planning/request/result loop in `agents/claw`.
- Keep Temporal-specific signals, queries, activities, and workflow state in
  `agents/claw`.
- Keep workflow id equal to session id.
- Keep Signal-With-Start behavior.

### G5: Clarify I/O Trait Documentation

- Document that `CoreAgentLlm` and `CoreAgentTools` are execution adapter
  traits for local runtimes and activities.
- Document that workflow substrates should fulfill drive actions directly.

## Tests

Add or preserve tests for:

- drive machine emits append action after command admission
- drive machine applies only committed appended entries
- drive machine emits LLM action after planned generation events are committed
- drive machine resumes LLM result into domain event append action
- drive machine emits tool action after model-observed tool calls
- local `SessionRunner` still completes final-answer and tool-call loops
- local LLM/tool errors still record failed domain facts
- Temporal Claw fake live test still completes two Signal-With-Start inputs
- Temporal Claw OpenAI ignored live test still compiles

## Non-Goals

- Reintroducing durable effect intent/receipt events
- Generic effect outbox
- `agent-temporal` crate extraction
- Maintaining `agent-core::runner` as a compatibility facade
- Provider-neutral LLM message abstraction
- Python bridge work
- Continue-As-New implementation
- Changing Claw's public signal/query API beyond what the refactor requires

## Done When

- `agent-core` exposes a substrate-neutral CoreAgent drive machine.
- Local runtime and Temporal Claw use the same drive machine.
- `agent-core` CoreAgent driving no longer requires direct async store, blob,
  LLM, or tool trait objects.
- `agent-core` no longer owns the local runner facade; runner types live in
  `agent-local`.
- `cargo test -p agent-core`, `cargo test -p agent-local`, and
  `cargo test -p claw` pass.
- The ignored Temporal fake live test passes against `local`.
