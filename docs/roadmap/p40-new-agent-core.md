# P40: New Agent Core Model Foundation

**Status**
- Complete (2026-05-05)

**Goal**
Implement the new `lightspeed-agent` core model layer described by
`docs/spec/04-new-agent-spec.md`.

This phase is intentionally not a full agent. It should create the durable,
serializable domain vocabulary that later phases use for the loop, tools, CLI,
Temporal, and persistence surfaces:

- ids
- lifecycle enums
- agent definition/version records
- config and resolved context structs
- transcript/artifact references
- scoped journal event records
- turn/context planning records
- bounded session/run state snapshots
- event and effect intent/receipt enums
- pending effect state
- tool registry/profile/batch model records
- projection item records
- deterministic helper functions and invariants

The output should be a compiling `lightspeed-agent` crate with focused unit tests for
the model layer. It should not call an LLM, execute tools, spawn processes, run
Temporal workflows, or implement hooks/policy.

**Source**
- Spec of record: `docs/spec/04-new-agent-spec.md`
- Scratch/context: `docs/spec/04-new-agent-idea.md`
- Primary conceptual reference: `refs/aos-agent/`
- Old Lightspeed agent reference: `refs/lightspeed-agent/`
- Codex reference checkout: `/Users/lukas/dev/tmp/codex/codex-rs/`

**Design Position**
Lightspeed should lift concepts from AOS and other agents, not copy their runtime
stack:

- Do not import AOS AIR, WASM, world, governance, or `AirSchema` machinery.
- Do not rebuild the old Lightspeed session loop.
- Do not vendor Codex protocol types directly.
- Keep the core deterministic, serializable, and Temporal-agnostic.
- Keep `lightspeed-agent` as an agent core SDK. Host shell/filesystem/process
  execution is tool-package/runner-specific and must not be hardcoded into the
  core crate.
- Model Lightspeed as journaled, ref-backed, and snapshot-driven: journal + CAS
  artifacts + bounded session state. The journal is scoped to agent/session
  events; it is not a fully generic event store.
- Use explicit ids for persisted identity. `SubmissionId` is the external
  queue/idempotency id; `CorrelationId` is correlation metadata.
- Keep large payloads behind artifact refs.
- Treat hooks, approval, permission, sandbox, and policy review as deferred SDK
  extension surfaces.

## Reference Lift Map

### AOS concepts to lift now
- `refs/aos-agent/src/contracts/ids.rs`
  - `SessionId`, `RunId`, `ToolBatchId` shape.
  - Sequence-backed child ids.
  - Lightspeed additions: `TurnId`, `EffectId`, `SubmissionId`, `ToolCallId`,
    `ProjectionItemId`, and `ArtifactRef`.
- `refs/aos-agent/src/helpers/ids.rs`
  - Deterministic allocation from state counters.
  - Adapt into pure helpers that do not depend on AOS state layout.
- `refs/aos-agent/src/contracts/lifecycle.rs`
  - Separate session status, session lifecycle, and run lifecycle.
  - Terminal-state helpers and valid transition checks.
- `refs/aos-agent/src/helpers/lifecycle.rs`
  - Lifecycle transition validation.
  - Host command applicability should not be lifted into core model rules.
- `refs/aos-agent/src/contracts/config.rs`
  - Provider/model/reasoning/run override model.
  - Lightspeed additions: context/token settings, loop limits, profile ids,
    persistence mode references where needed.
- `refs/aos-agent/src/contracts/state.rs`
  - `SessionState`, `RunState`, `RunRecord`, `RunCause`, `RunOutcome`.
  - Queued follow-ups, steering refs, pending effects, active run/tool batch.
  - Staged tool follow-up turn shape.
- `refs/aos-agent/src/contracts/events.rs`
  - Input events, lifecycle events, effect receipt/stream frame separation.
  - Lightspeed should split these into its own `AgentEvent` families.
- `refs/aos-agent/src/contracts/turn.rs`
  - `TurnInputLane`, `TurnInput`, priorities, budget, prerequisites,
    `TurnPlan`, and `TurnReport`.
  - This phase defines the structs; p41 builds the planner behavior.
- `refs/aos-agent/src/contracts/context.rs`
  - Transcript ranges, active window items, provider compatibility metadata,
    context pressure, token count quality, compaction operation state, and
    compaction records. Lightspeed keeps full transcript/compaction history out of
    active session state.
- `refs/aos-agent/src/contracts/tooling.rs`
  - Tool specs, tool mappers/executors as data, parallelism hints, runtime
    context, observed/planned tool calls, and profile builders.
  - Lightspeed should use open mapper/executor ids and handler bindings instead of
    AOS host-specific mapper names.
- `refs/aos-agent/src/contracts/batch.rs`
  - Per-call `ToolCallStatus`, active tool batch, execution groups, pending
    effect set, result refs, and settlement state.
- `refs/aos-agent/src/contracts/host.rs`
  - Defer host command/status vocabulary to runner/tool packages outside
    `lightspeed-agent`.

### AOS helpers to study but not implement fully in p40
- `refs/aos-agent/src/helpers/turn.rs`
  - Use as planner contract reference. Only model types and simple invariants
    belong in p40.
- `refs/aos-agent/src/helpers/workflow/mod.rs`
  - Use as event/effect state-machine reference. The reducer/decider loop is
    p41.
- `refs/aos-agent/src/helpers/workflow/tool_batch.rs`
  - Use for batch state invariants and execution group terminology. Actual tool
    effect emission and receipt settlement are p42.

### Old Lightspeed agent concepts to lift now
- `refs/lightspeed-agent/src/turn.rs`
  - User, assistant, tool-results, system, and steering transcript categories.
  - Convert to artifact-backed records instead of embedding full bodies in all
    state.
- `refs/lightspeed-agent/src/events.rs`
  - Human-facing event projection ergonomics and stable event helper style.
  - Do not keep the old `EventKind` as authoritative core state.
- `refs/lightspeed-agent/src/config.rs`
  - Turn/tool loop limits, command timeouts, subagent depth, thread key, and
    CXDB persistence mode vocabulary.
  - Remove `tool_hook_strict` from first-cut core. Hooks are deferred.
- `refs/lightspeed-agent/src/session/types.rs`
  - `SubmitOptions`, `SubmitResult`, `SessionCheckpoint`,
    `SessionPersistenceSnapshot`, and subagent handle/result shapes.
  - Recast as run input/config overrides, transcript lineage, snapshot refs,
    and subagent status records.
- `refs/lightspeed-agent/src/session/persistence.rs`
  - Typed persistence family names and idempotency-key thinking.
  - P40 should define refs and record payload shapes, not CXDB write paths.
- `refs/lightspeed-agent/src/tools/registry.rs`
  - Tool registry API ergonomics and output limit config.
  - Hook types in this file should not be lifted into first-cut core.
- `refs/lightspeed-agent/src/patch/`
  - Patch data model is useful for future host tools, but p40 only needs
    artifact/projection refs for patches.

### Codex concepts to lift now
- Protocol queue identity and event correlation:
  - Lightspeed `SubmissionId` plus optional `CorrelationId` on submissions,
    effects, and projection items.
- Resolved turn context snapshot:
  - provider/model/reasoning
  - current date/timezone
  - tool profile and model-visible tool specs
  - model/provider options
  - context refs
  - runtime/extension refs for runner-provided context
- Stable item lifecycle projection:
  - item ids for user/assistant/reasoning/tool/patch/compaction entries.
  - started/updated/completed state is projection data, not authoritative
    domain state.
- Fork, rollback, and history rewrite:
  - define lineage and replacement records now, implementation behavior later.
- Subagent routing model:
  - parent/child ids, role metadata, final status mapping, cancellation intent.

### Codex concepts to defer
- Hooks
- Approval flows
- Permission grants
- Sandbox policy review
- Dynamic tools beyond static model structs
- App-server/realtime/product telemetry surfaces

## Target Module Layout

The exact files can change, but p40 should leave a coherent public module
layout in `crates/lightspeed-agent/src/`:

- `lib.rs`
  - public module exports and high-level crate docs.
- `agent.rs`
  - `AgentDefinition`, immutable `AgentVersion`, default prompts/config/tools.
- `ids.rs`
  - id newtypes and deterministic allocation helpers.
- `error.rs`
  - model/reducer error taxonomy. No adapter-specific errors yet.
- `lifecycle.rs`
  - session/run lifecycle enums and transition checks.
- `config.rs`
  - `SessionConfig`, `RunConfig`, `TurnConfig`, context/token/tool limits,
    opaque extension refs, and persistence mode references.
- `refs.rs`
  - `ArtifactRef`, previews, and payload storage metadata.
- `transcript.rs`
  - transcript item/entry records, source ranges, and transcript boundaries.
- `context.rs`
  - active window items, provider compatibility, context state, token count,
    pressure, and latest compaction summary.
- `turn.rs`
  - turn input lanes, priorities, budgets, prerequisites, plans, reports, and
    resolved turn context snapshots.
- `tooling.rs`
  - tool specs, tool profiles, observed/planned calls, tool runtime context.
- `batch.rs`
  - active tool batch, call status, execution groups, per-call refs.
- `effects.rs`
  - core effect intent envelope, receipt enum, stream frame enum, failure
    classification.
- `events.rs`
  - scoped journal event envelope plus input/lifecycle/effect/observation event
    families.
- `state.rs`
  - bounded `SessionState`, `RunState`, compact `RunRecord`, pending effects,
    queues, fork lineage, and compact history boundary state.
- `projection.rs`
  - stable projection item records for CLI/JSONL/web clients.
- `subagent.rs`
  - parent/child metadata and subagent effect/status model.

## Scope

### In scope
- Replace the reset crate shell with the new core model modules.
- Define serializable Rust structs/enums that match `docs/spec/04-new-agent-spec.md`.
- Derive or implement `Clone`, `Debug`, `PartialEq`, `Eq` where practical.
- Derive `Serialize` and `Deserialize` for persisted or projected records.
- Use `BTreeMap`/`BTreeSet` for deterministic persisted ordering where order is
  semantically irrelevant.
- Add simple constructors only when they encode stable invariants.
- Add lifecycle validity helpers and terminal-state helpers.
- Add id allocation helpers based on state counters.
- Add agent definition/version records but do not model owner or tenant in p40.
- Add scoped journal event sequence and ref-backed payload conventions.
- Add serde round-trip and invariant unit tests.
- Update `crates/lightspeed-agent/README.md` to point at spec/04 and the new module
  map if it still references the legacy spec/02 implementation.

### Out of scope
- LLM request execution.
- Token counting implementation.
- Context planner algorithm beyond type definitions and invariants.
- Shell/filesystem/MCP/subagent execution.
- Tool argument validation or dispatch.
- Temporal workflow/activity code.
- CXDB write/read implementation.
- CLI chat UI.
- Attractor integration.
- User/project hooks.
- Approval, permission, sandbox, or policy review framework.

## Priority 0: Contract Freeze

### [x] G1. Public module skeleton and crate baseline
- Work:
  - Replace the current reset shell with the module layout above.
  - Keep public exports narrow but usable for later phases.
  - Ensure the crate still builds with no live services.
- Files:
  - `crates/lightspeed-agent/src/lib.rs`
  - `crates/lightspeed-agent/README.md`
- DoD:
  - `cargo test -p lightspeed-agent` compiles the new module skeleton.
  - README no longer describes the removed legacy implementation as current.
- Completed:
  - Added the public `lightspeed-agent` module skeleton for the spec/04 core model.
  - Updated the crate README to describe the new module map and deferred
    extension surfaces.

### [x] G2. Identifiers and refs
- Work:
  - Define id newtypes for `SessionId`, `RunId`, `TurnId`, `EffectId`,
    `SubmissionId`, `ToolBatchId`, `ToolCallId`, `ProjectionItemId`.
  - Define `CorrelationId` as optional metadata, not a replacement for
    `SubmissionId`.
  - Define `ArtifactRef`; artifact refs do not carry semantic payload kind and
    transcript prefixes/snapshots are represented by source session ids,
    boundaries, and artifact refs where needed.
  - Add allocation helpers for run, turn, tool batch, effect, and projection
    item ids.
- Files:
  - `crates/lightspeed-agent/src/ids.rs`
  - `crates/lightspeed-agent/src/refs.rs`
- DoD:
  - IDs are explicit in events/state/effects.
  - No durable identity depends on vector position.
  - Unit tests cover deterministic sequence allocation.
- Completed:
  - Added explicit id newtypes for sessions, runs, turns, submissions,
    correlations, effects, tool batches, tool calls, and projection items.
  - Added deterministic session-local id cursors for run, turn, batch, effect,
    and projection item ids.
  - Added artifact refs with previews/storage metadata and serde coverage.
  - Added transcript boundaries in the transcript model with msgpack coverage.

### [x] G2a. Agent definition/version primitives
- Work:
  - Add `AgentId` and `AgentVersionId` newtypes.
  - Add reusable `AgentDefinition` and immutable `AgentVersion` records.
  - Agent definitions carry a stable string handle; prompt-facing name and
    description live on immutable agent versions.
  - Agent versions carry prompt refs, default run config, tool registry/profile
    defaults, skill/plugin/app refs where applicable, and opaque extension refs.
  - Sessions reference an initial/effective `AgentVersionId`; config changes
    create explicit revision/version boundaries.
  - Do not model tenant or owner in p40.
- Files:
  - `crates/lightspeed-agent/src/agent.rs`
  - `crates/lightspeed-agent/src/ids.rs`
  - `crates/lightspeed-agent/src/config.rs`
  - `crates/lightspeed-agent/src/state.rs`
- DoD:
  - Agent definitions and versions serde round-trip.
  - Agent versions are usable without live providers or tools.
  - Session/run/turn records can reference the effective agent version/config
    revision they used.
- Completed:
  - Added `AgentId`, `AgentVersionId`, reusable `AgentDefinition`, and
    immutable `AgentVersion` records in `agent.rs`.
  - Agent definitions carry stable handles; agent versions carry display name,
    description, prompt refs, default run config, tool registry/profile
    defaults, skill/plugin/app refs, opaque config refs, and metadata.
  - Session state, run state, run records, and resolved turn context can carry
    the effective agent version/config revision; `RunConfig` remains execution
    knobs rather than provenance.
  - Added serde round-trip coverage for agent versions.

### [x] G3. Lifecycle and configuration model
- Work:
  - Define session status, session lifecycle, run lifecycle, turn lifecycle
    where needed.
  - Define transition validation helpers.
  - Define `SessionConfig`, `RunConfig`, `TurnConfig`, and config override
    records.
  - Include tool/profile/context/token/loop/subagent limits as data.
  - Exclude hook/policy config from first-cut core except optional opaque future
    extension refs if the spec requires placeholders.
- Files:
  - `crates/lightspeed-agent/src/lifecycle.rs`
  - `crates/lightspeed-agent/src/config.rs`
  - `crates/lightspeed-agent/src/error.rs`
- DoD:
  - Invalid lifecycle transitions fail with typed model errors.
  - Config structs are serde round-trippable.
  - No hook/policy behavior appears in first-cut config.
- Completed:
  - Added session status, run lifecycle, and turn lifecycle enums with
    terminal-state helpers and transition validation.
  - Added the first core `ModelError` taxonomy for deterministic model helper
    failures.
  - Added session/run/turn config records, context budgets, loop limits, tool
    output limits, opaque extension refs, and CXDB persistence mode.
  - Kept hook/policy/approval/sandbox behavior out of config; only future
    extension refs remain as opaque data.

### [x] G4. Transcript, artifact, and context records
- Work:
  - Define transcript entries for user, assistant, reasoning, tool result,
    system, developer, steering, summary, and custom records.
  - Define transcript ledger entries with sequence numbers and source ranges.
  - Define active window items, provider compatibility, token count quality,
    context pressure records, context operation state, and bounded compaction
    summary state.
  - Model provider-native artifacts explicitly with compatibility metadata.
- Files:
  - `crates/lightspeed-agent/src/transcript.rs`
  - `crates/lightspeed-agent/src/context.rs`
  - `crates/lightspeed-agent/src/refs.rs`
- DoD:
  - Large text/output bodies can be represented by refs with optional previews.
  - Compaction records preserve source range and replacement refs outside the
    active session snapshot; context state retains only the latest summary.
  - Unit tests cover ledger append/source-range behavior.
- Completed:
  - Added artifact-backed transcript records for user/assistant/reasoning/tool
    result/system/developer/steering/summary/custom content.
  - Added transcript ledger records for projection/storage plus bounded context
    sequence/range tracking for active state.
  - Added active window items, context lanes, provider compatibility reuse,
    token usage/count records, context pressure records, context operation
    state, compaction records, latest compaction summaries, and context state
    helpers.
  - Added focused unit tests for ledger append/source ranges, msgpack
    round-trips, provider-native artifacts, and compaction/context invariants.

### [x] G5. Turn planning and resolved context types
- Work:
  - Define turn input lanes, kinds, priorities, budgets, tool choice, reports,
    prerequisites, and turn plans.
  - Define `ResolvedTurnContext` as an immutable snapshot of effective provider,
    model, current date/timezone, context refs, selected tool profile,
    model-visible tool specs, response format, provider options,
    correlation metadata, and runtime/extension refs.
  - Include extension refs only as future placeholders, not executable hooks or
    policies.
- Files:
  - `crates/lightspeed-agent/src/turn.rs`
  - `crates/lightspeed-agent/src/context.rs`
- DoD:
  - Later effect adapters can execute from a resolved context without reading
    mutable process-global state.
  - Unit tests verify overrides resolve deterministically from session/run/turn
    inputs once helper functions are introduced.
- Completed:
  - Added turn input lanes, input kinds, priorities, budgets, tool choice,
    token estimates, prerequisites, state updates, plans, and reports.
  - Added `ResolvedTurnContext` with provider/model/reasoning settings,
    date/timezone fields, context refs, selected tool profile,
    model-visible tools, active window items, provider options, response format
    refs, budget, correlation metadata, and future runtime/extension refs.
  - Added deterministic resolution from run provenance, `RunConfig`, optional
    `TurnConfig`, and `TurnPlan`.

### [x] G6. Tool registry/profile/batch model
- Work:
  - Define `ToolSpec`, open `ToolExecutorKind`/`ToolMapperKind` records,
    `ToolParallelismHint`, `ToolProfile`, and `ToolRuntimeContext`.
  - Define observed tool calls, planned tool calls, execution groups, per-call
    status, and active tool batch state.
  - Preserve provider call ids and normalized Lightspeed call ids separately.
  - Represent unknown/unavailable tools as planned ignored/failed calls that can
    become model-visible tool results later.
- Files:
  - `crates/lightspeed-agent/src/tooling.rs`
  - `crates/lightspeed-agent/src/batch.rs`
- DoD:
  - The model can represent observed, accepted, ignored, pending, succeeded,
    failed, and cancelled calls.
  - Execution groups encode parallel-safe/resource-key constraints as data.
  - No actual tool dispatch exists in p40.
- Completed:
  - Added `ToolSpec`, `ToolExecutorKind`, `ToolMapperKind`,
    `ToolParallelismHint`, `ToolProfile`, `ToolRegistry`, and
    `ToolRuntimeContext`.
  - Added observed and planned tool-call records that preserve normalized Lightspeed
    call ids separately from provider call ids.
  - Added unavailable planned-call representation for unknown/unavailable tools
    without dispatching them.
  - Added `ToolExecutionPlan` grouping by parallel-safety and resource-key
    conflicts.
  - Added `ActiveToolBatch`, per-call `ToolCallStatus`, pending tool effects,
    and model-visible result refs.

### [x] G7. Event and effect model
- Work:
  - Define input events from spec section 6.1.
  - Define lifecycle events from spec section 6.2.
  - Define effect intent, receipt, and stream-frame event records from spec
    section 6.3.
  - Define observation/projection events from spec section 6.4.
  - Define `AgentEffectIntent` variants for LLM, confirmation, MCP, generic tool
    invocation, and subagent effects as data only.
  - Define receipts with success/failure/metadata fields sufficient for later
    state reduction.
  - Do not model artifact store put/get as agent effects. Artifact bytes are
    runner/adapter infrastructure; effects and receipts carry refs.
- Files:
  - `crates/lightspeed-agent/src/events.rs`
  - `crates/lightspeed-agent/src/effects.rs`
- DoD:
  - Every effect has an `EffectId` idempotency key.
  - Receipt records can settle an effect without requiring runner-specific
    error types.
  - Hook/policy/approval events are absent or clearly deferred placeholders.
- Completed:
  - Added `AgentEffectIntent`, `AgentEffectKind`, effect metadata, LLM, MCP,
    confirmation, generic tool invocation, and subagent request records.
  - Added `AgentEffectReceipt`, receipt variants, effect failures,
    cancellations, retry metadata, and stream-frame records.
  - Added `AgentEvent` with input, lifecycle, effect, and observation families
    covering the first-cut spec event set.
  - Added helper accessors/tests proving effect events carry the `EffectId`
    idempotency key through intent, stream, and receipt phases.
  - Kept hook, approval, permission, sandbox, and policy-review events out of
    the first-cut event model.
  - Correction: artifact store put/get effect records are not part of the
    core effect model and were removed in G7a.

### [x] G7a. Journal envelope and artifact-effect correction
- Work:
  - Add an explicit scoped journal envelope/sequence for `AgentEvent`.
  - Record causality joins: run, turn, effect, tool batch, tool call,
    submission, correlation, and optional parent event/effect.
  - Remove `ArtifactPut`/`ArtifactGet` effect and receipt variants from the
    core effect model.
  - Keep `ArtifactRef` records and artifact/CAS storage contracts.
- Files:
  - `crates/lightspeed-agent/src/events.rs`
  - `crates/lightspeed-agent/src/effects.rs`
  - `crates/lightspeed-agent/src/refs.rs`
- DoD:
  - Journal events are small and ref-backed.
  - Effects do not represent artifact store CRUD.
  - Unit tests cover journal sequencing and effect id causality.
- Completed:
  - Added `JournalSeq` allocation to `AgentEvent`.
  - Added `AgentEventJoins` for optional event causality and lookup ids:
    run, turn, effect, tool batch, tool call, submission, correlation,
    parent event, and parent effect.
  - Removed artifact store put/get effect and receipt variants from the core
    effect model while keeping `ArtifactRef`.
  - Added unit coverage for journal sequence and effect causality.

### [x] G8. Session, run, pending effect, fork, and rewrite state
- Work:
  - Define `SessionState`, `RunState`, `RunRecord`, `RunCause`, `RunOutcome`,
    pending input queues, pending effects, and current active run/tool batch.
  - Define session lineage/fork records and history rewrite/rollback records.
  - Define subagent parent/child metadata and status records.
  - Keep old filesystem-revert semantics out of rollback records. Rollback is
    model-context only unless a later external tool effect says otherwise.
- Files:
  - `crates/lightspeed-agent/src/state.rs`
  - `crates/lightspeed-agent/src/subagent.rs`
- DoD:
  - State can represent a new session, active run, waiting run, completed run,
    interrupted run, forked session, and rewritten transcript.
  - Unit tests cover core state construction and lifecycle invariants.
- Completed:
  - Added `SessionState`, `RunState`, `RunRecord`, `RunCause`, `RunOutcome`,
    pending run/steering/confirmation queues, pending effect records, and current
    active run/tool-batch fields.
  - Added deterministic helpers for session status transitions, starting and
    finishing foreground runs, queueing inputs, recording pending effects,
    settling effects by removing them from active pending state, and applying
    compact history rewrite/rollback boundary updates.
  - Added session lineage/fork source records plus compact history control
    state that keeps rollback model-context-only.
  - Added subagent parent/child relationship, status, routing, cancellation,
    and final-output/failure metadata records.
  - Added unit tests for new session construction, active/completed/interrupted
    runs, fork/history-boundary representation, pending effect settlement, and
    subagent status invariants.

### [x] G8a. Bounded active session state
- Work:
  - Make `SessionState` explicitly the compact control snapshot managed by
    local/Temporal runners.
  - Keep current run, pending queues, pending effects, active tool batch,
    context state, selected profile, subagent handles, id allocator, effective
    agent version/config revision, and latest journal sequence.
  - Move unbounded full history to journal/transcript/projection records.
  - Replace completed run/tool-batch history in active state with compact
    summaries and refs needed for next-step decisions.
- Files:
  - `crates/lightspeed-agent/src/state.rs`
  - `crates/lightspeed-agent/src/batch.rs`
  - `crates/lightspeed-agent/src/transcript.rs`
  - `crates/lightspeed-agent/src/projection.rs`
- DoD:
  - Active state remains bounded across long sessions.
  - UIs can reconstruct history from journal/projection records plus artifacts.
  - The reducer/decider can make the next decision without fetching full
    historical payload bytes.
- Completed:
  - Added effective agent version/config revision and latest journal sequence
    to active session state.
  - Added compact `RunRecord` summaries for completed runs; detailed completed
    tool batches and usage records no longer live in completed run history.
  - Added `StateRetentionPolicy` with bounded completed run history and dropped
    history accounting.
  - Added tests proving completed run history retention is bounded.

### [x] G9. Projection item model
- Work:
  - Define projection items with stable item ids and item lifecycle states.
  - Include user, assistant, reasoning, tool, patch, compaction, warning, and
    status item kinds.
- Files:
  - `crates/lightspeed-agent/src/projection.rs`
- DoD:
  - Projection records carry enough ids to join back to session/run/turn/effect
    or tool-call state.
- Completed:
  - Added projection item lifecycle states, join ids, projection item kinds for
    user/assistant/reasoning/tool/patch/compaction/warning/status/file
    change/token/cost/custom entries, and item update/complete/fail helpers.
  - Added tests for projection item joins/lifecycle/serde round-trips.

### [x] G9a. Transcript item projection contract
- Work:
  - Make transcript items a product/UI-facing projection surface separate from
    active `SessionState`.
  - Each transcript item carries session id, journal sequence, join ids,
    lifecycle/status, kind, content ref, preview, source event id, and
    timestamps.
  - Keep full bodies in artifacts/CAS.
- Files:
  - `crates/lightspeed-agent/src/transcript.rs`
  - `crates/lightspeed-agent/src/projection.rs`
  - `crates/lightspeed-agent/src/events.rs`
- DoD:
  - A CLI/web UI can page transcript items and fetch artifact bodies without
    reading Temporal workflow state.
  - Transcript items can represent user, assistant, reasoning, tool result,
    summary, patch, status, and custom records.
- Completed:
  - Added ref-backed `TranscriptItem` plus `TranscriptItemJoins` carrying
    session id, journal sequence, run/turn/effect/tool joins, source event id,
    content ref, preview, source range, metadata, and timestamps.
  - Added serde coverage for transcript items.

### [x] G10. Serialization and invariant tests
- Work:
  - Add focused unit tests beside each model module.
  - Round-trip representative events, effects, state, context records, and tool
    batches through JSON and msgpack where relevant.
  - Assert transition errors, id allocation, and tool-batch terminal status
    helpers.
- Files:
  - `crates/lightspeed-agent/src/**/*.rs`
- DoD:
  - `cargo test -p lightspeed-agent` passes.
  - Tests do not require CXDB, Temporal, CLI binaries, or live LLM keys.
  - Tests fail loudly on broken invariants instead of skipping.
- Completed:
  - Added focused unit coverage across agent, ids, refs, lifecycle, config,
    transcript, context, turn, tooling, batch, effects, events, state,
    subagent, and projection modules.
  - Verified `cargo test -p lightspeed-agent` passes with deterministic model tests.

## Priority 1: Shape for Later Phases

### [x] G11. Reducer/decider boundary types only
- Work:
  - Define type aliases or traits for the future pure boundary:
    `apply(event, state) -> state/events` and `decide(state) -> intents`.
  - Add no full loop behavior in p40.
- Files:
  - `crates/lightspeed-agent/src/state.rs`
  - `crates/lightspeed-agent/src/events.rs`
  - `crates/lightspeed-agent/src/effects.rs`
- DoD:
  - p41 can implement the loop without renaming core model concepts.
- Completed:
  - Added first-cut `ReducerOutcome`, `DeciderOutcome`, `ReduceResult`, and
    `DecideResult` boundary types without implementing loop behavior.

### [x] G12. Persistence schema naming placeholders
- Work:
  - Keep typed family constants for future CXDB records if useful.
  - Do not implement CXDB writes.
  - Align names with `lightspeed.agent.runtime.v2` where still relevant, but do not
    duplicate CXDB DAG fields inside payload structs.
- Files:
  - `crates/lightspeed-agent/src/refs.rs`
  - `crates/lightspeed-agent/src/events.rs`
  - `crates/lightspeed-agent/src/transcript.rs`
- DoD:
  - Future persistence can map records to CXDB without changing event/state
    identity fields.
- Completed:
  - Added schema-family and record-kind constants for agent runtime refs,
    journal events, transcript ledgers, and transcript items.

### [x] G13. Documentation of deferred extension seams
- Work:
  - Add crate-level docs or README notes that hooks/policy/approval/sandbox are
    future SDK extension surfaces.
  - Mention likely future extension points without adding executable APIs.
- Files:
  - `crates/lightspeed-agent/src/lib.rs`
  - `crates/lightspeed-agent/README.md`
- DoD:
  - The codebase does not accidentally imply hooks/policy are supported in the
    first cut.
- Completed:
  - Updated crate docs and README to describe the core SDK scope, journaled
    ref-backed bounded-state model, host-tool exclusion, and deferred
    hook/policy/approval/sandbox extension surfaces.

## Acceptance
- `crates/lightspeed-agent` exposes the new core model layer from spec/04.
- The model can represent:
  - agent definitions and immutable agent versions
  - session/run/turn lifecycle
  - transcript records/projections and artifact refs
  - scoped journal events with sequence and causality refs
  - resolved turn context
  - context pressure and bounded compaction summary state
  - LLM/confirmation/MCP/generic tool/subagent effect intents and receipts
  - tool registry/profile/batch state
  - projection items
  - fork/rollback/history rewrite metadata
- Artifact refs are core model primitives, but artifact store put/get is
  adapter/storage infrastructure, not an agent effect family.
- No LLM, tool, MCP, Temporal, CXDB, or CLI execution is implemented.
- Host shell/filesystem/process support is not part of `lightspeed-agent`; it belongs
  in runner/tool-package follow-on work.
- Hook/policy/approval/sandbox concepts are documented as deferred.
- `cargo test -p lightspeed-agent` passes with deterministic model tests only.

## Follow-on Work
- `docs/roadmap/p41-agent-loop.md`: implement the pure reducer/decider and local
  stepper over these model types.
- `docs/roadmap/p42-agent-tools.md`: implement tool registry execution, generic
  tool-batch dispatch, and standard host filesystem/shell tools outside the
  core crate.
- `docs/roadmap/p45-new-cli.md`: implement the CLI projection/control surface.
