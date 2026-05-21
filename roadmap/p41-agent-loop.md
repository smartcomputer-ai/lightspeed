# P41: Agent Reducer, Decider, and Local Stepper

**Status**
- Complete (2026-05-05)

**Goal**
Implement the first executable Forge-native agent loop on top of the p40 core
model. This phase turns scoped journal events plus bounded `SessionState` into
next effect intents, and settles receipts back into state and journal events.

P41 is still not the production runtime. It should prove the deterministic
domain loop with fake adapters and an in-memory/local test harness. Real host
tools, production artifact stores, Temporal workflows, CXDB/Postgres
persistence, and CLI UI are follow-on phases.

**Source**
- Spec of record: `spec/04-new-agent-spec.md`
- Model foundation: `roadmap/p40-new-agent-core.md`
- Tool execution follow-on: `roadmap/p42-agent-tools.md`

## Design Position

Forge uses a journaled, ref-backed, snapshot-driven loop:

```text
input -> journal event -> reduce bounded state -> decide effect intents
effect receipt -> journal event -> reduce bounded state -> decide next work
```

The reducer/decider must be deterministic and side-effect free. Runners execute
effects outside the core and append receipts. Large payload bytes stay in the
artifact/CAS layer and are referenced by events, transcript items, context
items, and receipts.

## Prerequisites From P40

- Agent definition/version primitives exist.
- Session state is explicitly bounded and contains only control data needed for
  next-step decisions.
- Journal events have session-local sequence and causality joins.
- Transcript/projection items are separate from active state.
- Artifact put/get is not an agent effect; adapters use artifact storage
  infrastructure directly.
- Fake artifact/effect helpers are introduced in this phase for deterministic
  tests.

## Scope

### In scope
- Pure reducer APIs for applying journal events to `SessionState`.
- Pure decider APIs for producing effect intents from `SessionState`.
- Local in-process stepper that:
  - appends input events
  - reduces state
  - records emitted effect intents
  - calls fake effect executors
  - appends receipts
  - reduces state again until quiescent
- First turn planner behavior sufficient for deterministic tests.
- First LLM loop behavior using fake LLM receipts.
- Tool-call observation/planning and generic `ToolInvoke` intent emission.
- Tool-result turn continuation with fake tool receipts.
- Context pressure/count/compaction control flow with fake receipts if needed.
- Transcript/projection event emission from authoritative journal events.
- Deterministic tests for complete fake runs.

### Out of scope
- Real LLM provider calls.
- Real tool execution, including host shell/filesystem tools.
- Real MCP calls.
- Temporal workflow/activity implementation.
- Postgres/SQLite/CXDB/S3/filesystem production persistence.
- CLI/TUI rendering.
- Hooks, approval, permissions, sandbox policy, and dynamic tool loading.

## Target Module Shape

The exact file split can change, but p41 should add or clarify:

- `loop/reducer.rs`
  - `apply_event(state, event) -> state/events or result`
  - journal event validation and state transition helpers
- `loop/decider.rs`
  - `decide_next(state) -> Vec<AgentEffectIntent>`
  - run/turn/tool/context decision rules
- `testing/stepper.rs`
  - local deterministic stepper over fake stores/executors
- `loop/planner.rs`
  - first turn/context planning implementation
- `loop/journal.rs`
  - append-only in-memory journal for tests
- `testing/`
  - fake LLM/tool/artifact helpers
- `model/`
  - existing p40 serializable domain contracts, re-exported from the crate root
    for stable public paths

## Priority 0: Reducer Foundation

### [x] G1. Journal append contract and sequencing
- Work:
  - Define an append result that assigns or validates session-local journal
    sequence.
  - Ensure every appended event carries session id and causal ids where
    applicable.
  - Preserve runner-supplied observed timestamps on events; append/reduce must
    not sample wall-clock time.
  - Reject duplicate or out-of-order event ids/sequences in deterministic tests.
- DoD:
  - Journal sequence is monotonic per session.
  - Reducer receives already-ordered events with stable `observed_at_ms` values
    and never samples time.
- Completed:
  - Added `loop/journal.rs` with an append-only `InMemoryJournal` and
    `JournalAppendResult`.
  - Appends assign missing journal sequences, validate preassigned sequences
    against the next session-local sequence, and preserve runner-supplied
    `observed_at_ms` values.
  - Appends reject duplicate event ids, wrong-session events, duplicate or
    out-of-order sequences, mismatched join session ids, and malformed effect
    event joins.
  - Added deterministic unit tests for journal sequencing, duplicate detection,
    timestamp preservation, session isolation, and effect join validation.

### [x] G2. Input and lifecycle reduction
- Work:
  - Apply session open/pause/resume/close events.
  - Apply run requested/follow-up/steer/interrupt/confirmation events.
  - Apply configuration and planning boundary inputs:
    `SessionConfigUpdated`, `TurnContextOverrideRequested`, `ToolRegistrySet`,
    `ToolProfileSelected`, and `ToolOverridesSet`.
  - Apply history rewrite/rollback requests as compact history-control state
    changes plus lifecycle/observation events.
  - Apply lifecycle events for run/turn transitions.
- DoD:
  - Invalid lifecycle transitions fail with typed model errors.
  - Pending input queues and current run state update deterministically.
  - Every run and resolved turn continues to reference the effective agent
    version/config revision it used.
- Completed:
  - Added `loop/reducer.rs` with `apply_event` and `apply_events`.
  - Reducer requires ordered, session-matching journal events with assigned
    `JournalSeq` values and updates `SessionState.latest_journal_seq`.
  - Input reduction now handles session open/pause/resume/close, run request,
    follow-up, steering, interrupt, confirmation responses, config updates,
    tool registry/profile/override boundaries, and history rewrite/rollback
    requests.
  - Lifecycle reduction now handles session status changes, run lifecycle
    changes, turn started/completed/failed/lifecycle changes, context operation
    state, context pressure, and history rewrite completion.
  - Added deterministic tests for run queueing, follow-up/steering queues,
    config revisions, tool profile selection, history control, lifecycle
    transitions, turn tracking, and invalid event ordering/transitions.

### [x] G3. Effect reduction
- Work:
  - Apply `EffectIntentRecorded` by inserting pending effect records.
  - Apply stream frames as non-authoritative observations only.
  - Apply `EffectReceiptRecorded` by settling pending effects.
  - Classify receipt failures into model-visible tool results vs runner/system
    failures where the data is already explicit.
- DoD:
  - No pending effect disappears without settled/abandoned state.
  - Receipt application is idempotent for the same `EffectId`.
- Completed:
  - Effect reduction records pending effect intents in session state and the
    matching active run state.
  - Stream frames mark pending effects as streaming without treating stream data
    as authoritative replay state.
  - Receipts settle pending effects in both session and run state, and duplicate
    receipts for already-settled effects are idempotent no-ops.
  - LLM receipts update compact usage/latest-output control refs where present;
    token-count receipts update bounded context state.
  - Non-retryable failed receipts are reflected as current-run failure outcome
    data for later decider/stepper handling.
  - Added tests for pending effect recording, streaming status, receipt
    settlement/idempotency, duplicate intent rejection, and non-retryable
    failure classification.

### [x] G4. Tool batch reduction
- Work:
  - Create active tool batches from LLM receipts that contain observed tool
    calls.
  - Plan accepted/unavailable calls from the selected tool registry/profile.
  - Update per-call status on generic tool receipts.
  - Complete the batch when all calls are terminal.
- DoD:
  - Parallel grouping is data-only and deterministic.
  - Unavailable tools become model-visible failed tool results.
- Completed:
  - LLM generation receipts with observed tool calls now create deterministic
    `ActiveToolBatch` records using the selected tool registry/profile.
  - Tool calls are planned as accepted or unavailable from model-visible tools;
    profile/override-disabled calls become unavailable.
  - Unavailable calls are converted into terminal failed call statuses plus
    deterministic model-visible result refs.
  - `ToolInvoke` intents mark matching active-batch calls as pending and record
    per-call pending effect ids.
  - Generic tool receipts update per-call status, store model-visible results,
    clear pending tool effects, and move fully settled batches into completed
    batch history.
  - Added tests for active batch creation, unavailable tool failures,
    pending-call intent tracking, tool receipt settlement, and terminal batch
    completion.

## Priority 1: Decider and Planner

### [x] G5. Run/turn decider
- Work:
  - Start a queued run when the session can accept foreground work.
  - Allocate turn ids deterministically.
  - Decide whether the next action is planning, LLM generation, tool execution,
    compaction/counting, waiting, completion, or failure.
- DoD:
  - `decide_next` emits no duplicate effect intents for already-pending work.
  - Loop limits are enforced from state/config.
- Completed:
  - Added the first deterministic decider API and continuation rules:
    queued runs start, follow-up inputs can be promoted after a terminal run,
    turn ids/effect ids are allocated from snapshot state, ready turns emit LLM
    intents, queued accepted tool calls emit generic tool intents, and pending
    effects suppress duplicate work.
  - `max_turns` is checked before planning the next LLM turn.
  - Context count/compaction prerequisites emit fake-supported
    `LlmCountTokens`/`LlmCompact` intents before generation.
  - Final outputs, non-retryable failures, and interrupted runs now drive
    terminal lifecycle decisions in the pure loop.

### [x] G6. First context planner
- Work:
  - Select required prompt refs, run input refs, recent context refs,
    tool-result refs, summaries, and selected tool definitions.
  - Produce `ResolvedTurnContext`.
  - Emit count/compaction prerequisites only as fake-supported control paths in
    this phase.
- DoD:
  - Planner output is deterministic for the same state.
  - Large content remains referenced by `ArtifactRef`.
- Completed:
  - Added `loop/planner.rs` with a clean SDK extension API:
    `TurnPlanner`, `TurnPlanningRequest`, `TurnPlanningOutcome`,
    `ToolCandidate`, `ToolCandidateSource`, and `DefaultTurnPlanner`.
  - Default request construction derives planner inputs from run prompt/input
    refs, active context items, pending steering inputs, completed tool results,
    and tool registry/profile/config selection.
  - Default planner deterministically orders inputs, deduplicates content refs,
    applies message/tool ref and token budgets, preserves required inputs/tools,
    selects response format/provider option refs, and produces a structured
    `TurnReport`.
  - Dynamic tool selection is represented through explicit `ToolCandidate`s so
    SDK users can add, disable, force, reorder, or replace tools before
    planning without changing session state.
  - Pending context operations become `CountTokens` or `CompactContext`
    prerequisites; generation can check `TurnPlan::is_ready_for_generation`.
  - Planner returns both durable `TurnPlan` and immutable
    `ResolvedTurnContext`, with all large content kept behind `ArtifactRef`.
  - Added deterministic tests for input ordering/budget behavior, dynamic tool
    candidates, disabled/forced tools, tool-result inclusion, and context
    operation prerequisites.

### [x] G7. LLM and tool continuation loop
- Work:
  - Emit `LlmComplete`/`LlmStream` intent for a ready turn.
  - On LLM receipt with no tool calls, record authoritative receipt/lifecycle
    events, derive assistant transcript/projection items from those events, and
    complete the run.
  - On LLM receipt with tool calls, create/execute a tool batch.
  - On completed tool batch, emit the next LLM turn with tool result refs.
- DoD:
  - Fake run can complete: user input -> LLM -> final answer.
  - Fake run can complete: user input -> LLM tool calls -> tool receipts ->
    LLM final answer.
- Completed:
  - Added `loop/decider.rs` with deterministic `decide_next` /
    `decide_next_with` APIs that emit journalable lifecycle events plus
    `AgentEffectIntent`s without executing effects.
  - Decider starts queued runs, plans ready turns through the configured
    `TurnPlanner`, emits `LlmComplete`/`LlmStream` intents, and avoids duplicate
    work while run-scoped effects are pending.
  - LLM receipts with final assistant refs now lead to `TurnCompleted` and
    terminal run completion, preserving the final output ref in completed run
    history.
  - LLM receipts with tool calls continue through reducer-created active tool
    batches; the decider emits queued generic `ToolInvoke` intents and, after
    tool receipts settle the batch, emits the next LLM turn with model-visible
    tool result refs in context.
  - Full transcript/projection item materialization remains in G9; G7 keeps the
    authoritative continuation semantics in journal events and bounded run
    state.
  - Added deterministic journal/reducer/decider tests for fake final-answer runs
    and fake LLM tool-call -> tool receipt -> final-answer runs.

## Priority 2: Local Stepper and Projections

### [x] G8. Local stepper with fake executors
- Work:
  - Implement an in-process stepper that drives state to quiescence using fake
    LLM/tool/confirmation/subagent executors.
  - Keep artifact reads/writes in fake adapter infrastructure, not core
    effects.
- DoD:
  - Stepper tests require no live services or CLI binaries.
  - The stepper emits journal events and bounded state snapshots.
- Completed:
  - Added `testing/stepper.rs` with `LocalStepper`, `LocalEffectExecutor`,
    `FakeEffectExecutor`, and `StepperQuiescence`.
  - The local stepper appends events through `InMemoryJournal`, reduces them
    into bounded `SessionState`, drives `decide_next`, executes fake effects,
    appends receipts, and stops at a classified quiescent state.
  - Fake LLM, tool, token-count, and compaction receipts require no live
    provider, CLI binary, MCP server, Temporal worker, or artifact service.
  - Added deterministic local-stepper tests for direct final answers, tool
    round trips, unavailable-tool recovery, follow-up promotion, steering
    inclusion, context compaction prerequisites, and pending-effect
    interruption.

### [x] G9. Transcript/projection emission
- Work:
  - Derive transcript/projection items from journal/effect/lifecycle events.
  - Include joins for session/run/turn/effect/tool ids.
  - Store only previews plus artifact refs.
- DoD:
  - A fake run yields user, assistant, reasoning, tool-call, tool-output, and
    status projection items as applicable.
- Completed:
  - Added `loop/projection.rs` with `ProjectionBuilder` and `ProjectionOutput`
    for deriving projection and transcript items from authoritative journal
    events.
  - Derived items include stable joins for session, run, turn, effect, tool
    batch, and tool call ids where available.
  - Projection/transcript records store previews and artifact refs only; large
    content remains behind `ArtifactRef`.
  - Stepper tests assert user, assistant, reasoning, tool-call, tool-output,
    compaction, status, and transcript tool-result projection paths.

### [x] G10. Quiescence and interruption semantics
- Work:
  - Define quiescent states: waiting for input, waiting for confirmation response,
    waiting on pending effects, completed, failed, cancelled, interrupted.
  - Apply interrupt/cancel events to active runs and pending effects.
- DoD:
  - No stepper loop spins without new input/effect receipts.
  - Cancellation settles or abandons pending effects explicitly.
- Completed:
  - Added explicit stepper quiescence classification for waiting for input,
    confirmation, pending effects, context prerequisites, completed, failed,
    cancelled, and interrupted states.
  - The stepper stops when `decide_next` emits no work, so it does not spin
    without new input or effect receipts.
  - `RunInterruptRequested` now abandons pending session/run effects, clears the
    active LLM effect, cancels pending tool calls, and records the interrupted
    run in history.

## Testing

- Unit tests live beside reducer/decider modules.
- Integration-style local stepper tests use fake adapters and in-memory
  journal/artifact stores.
- Tests must fail loudly; no runtime env-var gating.

Required test flows:

- open session -> request run -> fake final LLM answer -> completed run
- fake tool call round trip -> final answer
- unavailable tool -> model-visible tool error -> recovery answer
- follow-up queued while run active
- steering input applied before next turn
- interrupt active run with pending effect
- context pressure triggers fake compaction path
- journal sequence and id allocation remain deterministic

## Acceptance

- `cargo test -p forge-agent` passes with deterministic tests only.
- The loop is executable with fake LLM/tool executors.
- No real provider, host tool, MCP server, Temporal worker, CXDB, Postgres, S3,
  or CLI UI is required.
- Active `SessionState` remains bounded; transcript history is exposed through
  journal/projection records plus artifact refs.
- p42 can add real generic tool dispatch without changing p41 core loop
  concepts.
