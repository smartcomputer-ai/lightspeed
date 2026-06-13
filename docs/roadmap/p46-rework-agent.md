# P46: Agent Core Primitive Rework

**Complete:** 2026-05-11

**Status**
- Complete

Implemented so far:

- First model contract slice is implemented under `crates/forge-agent/src/model/`.
- Added model files for ids, blobs, config, provider compatibility, session log
  envelopes, commands, fact events, effects/receipts, bounded state, context,
  turns, and tooling.
- Restored logical storage contracts under `crates/forge-agent/src/storage/`
  for blobs and session event logs, backed by in-memory helpers.
- Added `crates/forge-agent/src/transition.rs` with pure command admission,
  projection, shared event proposals, and the raw policy decision contract.
- Added `crates/forge-agent/src/admit.rs` with first-cut `CoreAdmitCommand`
  behavior for open/config-change/request-run/steer/cancel/close and effect
  receipt recording.
- Added `crates/forge-agent/src/apply.rs` with first-cut `CoreApplyEvent`
  replay behavior for lifecycle, run queue/start/steer/cancel/terminal events,
  turn-start events, context item recording, active window planning, and
  compaction state updates, turn request planning, LLM generation intent
  recording, effect receipt settlement, generation-pending turn state, and turn
  completion from settled generation receipts.
- Added `crates/forge-agent/src/policy.rs` with first-cut `CoreRunPolicy`
  queue-to-start, cancel-finalization, and run terminalization behavior, plus
  `CoreTurnPolicy` turn-start, provider-native request planning, and LLM
  generation intent batch behavior, plus generation receipt settlement into
  context items and completed turn outcomes.
- Added first-cut `CoreToolPolicy` behavior to start an active run-level tool
  batch from a completed generation turn with tool-call facts.
- Split tool configuration facts from runtime tool-batch facts as
  `ToolConfigEvent` and `ToolEvent`.
- Added first-cut client-effect tool invocation behavior: accepted function
  tools and provider-native `ClientEffect` tools become `ToolInvoke` effect
  intents, and `ToolInvocationReceipt` settles the run-level tool call state.
- Added first-cut tool batch completion behavior: terminal tool calls become
  model-visible `ToolResult` context items, then `ToolEvent::BatchCompleted`
  moves the active batch into completed batch state.
- Added first-cut `CoreContextPolicy` behavior to record the active run input
  as a retained context item after the run is started, then plan an active
  context window after a turn frame exists.
- Added `PolicyPipeline` as the pure first-non-empty policy coordinator for
  `CoreRunPolicy -> CoreToolPolicy -> CoreContextPolicy -> CoreTurnPolicy`.
- String id newtypes now validate construction and serde deserialization;
  general ids use a portable ASCII token shape, and provider-facing `ToolName`
  uses the stricter provider-safe shape.
- Pruned stale `forge-agent` dependencies left over from runner/provider-era
  code; the crate now depends only on current core-contract libraries, with
  `tokio` scoped to async unit tests.
- Added `crates/forge-agent/README.md` pointing to
  `docs/spec/04-new-agent-idea.md` and
  `docs/roadmap/p45-forge-llm-provider-native-rewrite.md` as the current direction.
- Superseded the earlier `raw_provider_response_ref` receipt sketch for P46:
  raw response retention remains an adapter/runner responsibility for the next
  phase, while P46 receipts carry reducer facts and blob-backed context items.
- No runner, adapter, full policy loop, or dedicated test-harness module was
  introduced.
- `cargo check -p forge-agent` and `cargo test -p forge-agent` pass.

## Goal

Rebuild `forge-agent` as a first-principles core SDK for a real Forge-native
agent.

This is no longer a refactor of the old agent loop. The old Forge agent and the
AOS agent contain useful ideas, but P46 should reset the crate around the
primitives Forge now needs after the provider-native `forge-llm` rewrite:

- durable session identity and a linear event log
- command admission separated from committed facts
- bounded replay state
- first-class context-window and compaction records
- provider API kind compatibility
- exact provider-native LLM request contracts
- typed effect intents and receipts
- tool-call and tool-result data contracts
- deterministic kernel APIs that can later run under Temporal

The first cut should be serious enough that later phases can build the real
agent loop on top of it. It should leave a compiling `forge-agent` crate with
stable, serializable core types and pure state-transition contracts. It should
not try to ship production storage backends, test harnesses, a production
runner, CLI, Temporal workflow, or real provider/tool adapter.

## Source Material

Authoritative direction:

- `docs/spec/04-new-agent-idea.md`
- `docs/roadmap/p45-forge-llm-provider-native-rewrite.md`

Deferred implementation references:

- `docs/roadmap/p43-cas.md`
- `docs/roadmap/p44-agent-session-store.md`

P43/P44 remain good implementation references, but P46 should not implement
production blob storage, production session storage, or durable store backends.

Conceptual references only:

- `refs/aos-agent/`
- `refs/aos-cli/src/chat/`
- `refs/forge-agent-old/`
- `/Users/lukas/dev/tmp/codex/codex-rs/`

Do not copy any of these architectures wholesale. Use them to check vocabulary,
edge cases, and implementation experience, then build the Forge core directly.

## Design Position

### Core SDK, not runner

`forge-agent` should define the deterministic domain core:

- model records
- command and event contracts
- replay/projection state
- effect intent and receipt contracts
- context-window planning inputs and outputs
- pure state-transition APIs for admission, projection, and policy/decide

It should not own host shell/filesystem execution, Tokio task orchestration,
Temporal workflow APIs, CLI UI, production persistence backends, test harnesses,
or provider credentials. Those belong in later runner, adapter, storage, or
host crates.

### Event-sourced session first

A session is the durable stream identity. Session state is the bounded replay
result of that stream.

```text
SessionCommand
  external request from API, CLI, Temporal signal/update, local runner,
  activity result, or another workflow

SessionEvent
  committed fact accepted by the aggregate

SessionEntry
  log entry envelope with session-local position, timestamp, joins,
  and one SessionEvent payload

SessionState
  bounded state rebuilt by applying SessionEntry values in order
```

Commands are not persisted as aggregate events. A command is admitted against
current state and becomes zero or more committed facts. Rejected commands return
typed errors. If rejected-command audit is needed later, use an inbox or audit
log separate from the aggregate event log.

### Replay stays inert

Replay must only rebuild state. It must not:

- validate external commands
- emit new work
- execute effects
- schedule activities
- call providers
- load wall-clock time
- depend on runner behavior

The central invariant remains:

```text
Only committed EffectEvent::IntentCreated entries may cause external execution.
```

### Provider-native, not common-message native

P45 made `forge-llm` a provider API client crate. P46 must not rebuild a fake
unified message model in `forge-agent`.

The agent owns:

- session API kind compatibility
- context-window planning
- compaction policy data
- exact provider-native request contracts over blob-backed items
- raw/native provider output retention
- small generation receipt facts extracted from provider output

The agent does not pretend that `openai:responses`, `anthropic:messages`, and
`openai:completions` share one durable message tree. Durable records may carry
provider-neutral control facts when the reducer must decide from them, but native
request and response payloads stay provider-shaped and blob-backed.

### Context management is core

The active context window is not just recent transcript text. It is a planned,
bounded, provider-compatible view assembled from prompts, inputs, assistant
outputs, tool results, summaries, pinned items, and compaction records.

Compaction is a first-class state transition because it rewrites the active
window. It must be represented by committed context events, not hidden inside a
provider adapter.

### Large payloads are blobs

Events and state carry `BlobRef` values for large or provider-native payloads:

- prompts
- user inputs
- assistant outputs
- raw provider requests
- raw provider responses in later adapter/runner records
- tool arguments
- tool outputs
- reasoning summaries
- compaction summaries
- transcript/projection details

`BlobRef` identifies bytes only. Media type, preview text, provider role, and
semantic meaning belong to the owning record.

### The model is single-active-run first

For the first real agent core, keep the control model simple:

- a session may have at most one active foreground run
- follow-up runs can be queued
- steering and cancellation can be admitted while a run is active
- a model generation turn is exclusive while in flight
- tool calls may be batched after a model output
- the next model turn waits for terminal model-visible tool results

This matches the useful LLM invariant: the model is either generating output or
the agent is deterministically planning the next committed step.

### One public transition layer

Do not introduce nested public `RunCommand`/`RunEvent`/`RunPolicy` or
`TurnCommand`/`TurnEvent`/`TurnPolicy` aggregates in P46.

The public core stays session-scoped:

```text
SessionCommand -> SessionEventProposal values
SessionEntry   -> SessionState
SessionState   -> SessionEventProposal values
```

Runs and turns are child control records inside `SessionState`, not independent
event streams. The implementation may use private helpers such as
`apply_run_event`, `apply_turn_event`, `decide_run_progress`, or
`decide_turn_generation`, but those helpers should not become separate public
kernels until a real boundary forces that split.

### Turn state is first-class child state

Turn state remains important because a turn is the durable control frame for one
model call. It should record planning, request construction, in-flight
generation facts, and terminal outcome without becoming its own
aggregate.

First-cut shape:

```rust
pub struct TurnState {
    pub turn_id: TurnId,
    pub run_id: RunId,
    pub status: TurnStatus,
    pub request: Option<LlmRequest>,
    pub generation_effect_id: Option<EffectId>,
    pub facts: Option<LlmGenerationFacts>,
    pub outcome: Option<TurnOutcome>,
}
```

### Policy as middleware

The policy layer should be pipeline-shaped. Core agent behavior is implemented
through the same policy interface that later extension policies use.

```text
SessionState
  -> CoreRunPolicy
  -> CoreToolPolicy
  -> CoreContextPolicy
  -> CoreTurnPolicy
  -> extension policies later
  -> coordinator returns the first non-empty proposal batch
```

Policies are pure ordered proposal producers. The coordinator runs layers in
fixed order and returns the first non-empty proposal batch. Every policy layer
reads the same committed `SessionState`; no layer observes uncommitted
proposals from an earlier layer in the same decide pass. Policies do not mutate
`SessionState`, execute effects, write blobs, call providers, stamp append
metadata, or schedule activities. Any proposal that depends on another proposal
being applied must wait for the next decide pass.

Turn planning belongs inside this policy pipeline. The durable planning artifact
is `LlmRequest`: a ref-backed request contract, not a fully materialized
provider request blob. "Plan the next turn and request generation" is a policy
decision, not a separate planner subsystem.

## Scope

### In scope

- Recreate the `forge-agent` crate source around a pure-contract module layout.
- Define serializable domain primitives and public SDK contracts.
- Add id newtypes and replay-derived cursor records.
- Add `BlobRef` as a model-facing reference type and a logical blob-store
  contract.
- Add session log envelope contracts such as `SessionEntry` and
  `UncommittedSessionEvent`, plus a logical session-store contract.
- Define `SessionCommand` separately from committed `SessionEvent` values.
- Define fact-oriented event families for session, run, turn, context, tooling,
  and effects.
- Define bounded `SessionState`, `RunState`, `TurnState`, context state,
  pending effect state, and tool batch state.
- Define provider API kind compatibility and provider-native request contracts.
- Define LLM effect intent and receipt records that preserve raw provider data
  and expose only generation facts.
- Define tool registry/profile/call/result data contracts without host tool
  execution.
- Define pure command admission, projection/apply, and policy/decide traits
  and function signatures.
- Define typed input/output/error contracts for those transition APIs.

### Out of scope

- Production blob stores, CAS backends, or object-store/filesystem persistence.
- Production session stores, durable append/read backends, or database
  integrations.
- Test harnesses, fake stores, fake adapters, and dedicated contract tests.
- Real OpenAI/Anthropic calls.
- Real tool execution.
- Shell/filesystem/process/MCP tool packages.
- Temporal workflow/activity implementation.
- CLI/TUI/chat UI.
- Attractor integration.
- Production filesystem/object-store CAS.
- Production CXDB/Postgres/SQLite session store.
- Durable session-state snapshots.
- Fork, rollback, or history rewrite.
- General hook/plugin/approval/permission/sandbox framework.
- Provider fallback/routing.
- Implemented autonomous policy loop.
- Full reducer/decider behavior beyond the first lifecycle and run queue/start
  slices.
- Event migration from earlier experimental logs.

## Target Module Layout

Keep the serializable model contracts under `src/model/`. Later transition and
policy modules can sit beside `model/` at the crate root unless a clearer
boundary emerges.

```text
crates/forge-agent/src/
  lib.rs
  transition.rs
  admit.rs
  apply.rs
  policy.rs
  model/
    mod.rs
    error.rs
    ids.rs
    blobs.rs
    config.rs
    provider.rs
    log.rs
    command.rs
    events.rs
    effects.rs
    state.rs
    context.rs
    turn.rs
    tooling.rs
  // later P46 slices, root-level unless pressure says otherwise:
  policies.rs
```

Rules:

- Root modules are data-only or pure transition contracts.
- No Tokio, Temporal, store, or test-harness types appear in the public core
  contracts.
- `forge-llm` native client types may be used by later adapters, but durable
  event/state records should not embed provider client structs directly.

## Core Primitive Set

### Identity

Use typed newtypes for all persisted identity.

```rust
pub struct SessionId(String);
pub struct AgentHandle(String);
pub struct RunId(u64);
pub struct TurnId(u64);
pub struct EffectId(u64);
pub struct ToolBatchId(u64);
pub struct ToolCallId(String);
pub struct SubmissionId(String);
pub struct CorrelationId(String);
```

Child ids should be session-local unless there is a concrete need for global
identity. Compose `SessionId + child id` at external boundaries.

State should track replay-derived counters so id allocation is deterministic:

```rust
pub struct IdCursors {
    pub last_run_id: u64,
    pub last_turn_id: u64,
    pub last_effect_id: u64,
    pub last_tool_batch_id: u64,
}
```

Contract requirements:

- ids serialize predictably
- invalid string ids fail loudly
- replay-derived cursor records make next-id allocation deterministic for later
  implementations

### Blob References

P46 should define the model-facing reference type only. Store APIs and hash
writing behavior belong to a later storage phase.

```rust
#[serde(transparent)]
pub struct BlobRef(String);
```

Contract requirements:

- serialized `BlobRef` is a plain `sha256:<64hex>` string
- canonical format is `sha256:<64 lowercase hex chars>`
- semantic metadata is not stored on `BlobRef`
- production byte storage, GC, and durable blob backends are out of scope for
  P46

### Session Log Envelope

P46 should define the log envelope types consumed by pure transition functions.
The restored storage layer may define logical store traits and an in-memory
helper, but durable append/read backends remain out of scope.

```rust
pub struct SessionPosition {
    pub seq: EventSeq,
}

pub struct SessionEntry {
    pub position: SessionPosition,
    pub observed_at_ms: u64,
    pub joins: SessionEventJoins,
    pub event: SessionEvent,
}

pub struct UncommittedSessionEvent {
    pub observed_at_ms: u64,
    pub joins: SessionEventJoins,
    pub event: SessionEvent,
}
```

Contract requirements:

- event sequence is monotonic per session
- store-assigned sequence and optimistic append behavior are deferred
- event payloads do not carry session id, sequence, or timestamp
- transition APIs consume already-ordered `SessionEntry` values

### Commands

Commands represent external requests. They are not reducer events.

```rust
pub enum SessionCommand {
    OpenSession {
        config: SessionConfig,
    },
    UpdateSessionConfig {
        config: SessionConfig,
    },
    SetToolRegistry {
        registry: ToolRegistry,
    },
    SelectToolProfile {
        profile_id: ToolProfileId,
    },
    RequestRun {
        submission_id: Option<SubmissionId>,
        input_ref: BlobRef,
        run_config: RunConfig,
    },
    RequestRunSteering {
        input_ref: BlobRef,
    },
    RequestRunCancellation,
    CloseSession,
    RecordEffectReceipt {
        receipt: AgentEffectReceipt,
    },
}
```

Admission trait:

```rust
pub trait AdmitCommand {
    fn admit(
        &self,
        state: &SessionState,
        command: SessionCommand,
    ) -> Result<Vec<SessionEventProposal>, CommandError>;
}
```

Contract requirements:

- rejected commands do not mutate state
- accepted commands produce fact-oriented events
- command admission never executes effects

### Events

Committed events are facts.

```rust
pub struct SessionEvent {
    pub kind: SessionEventKind,
}

pub enum SessionEventKind {
    Lifecycle(SessionLifecycleEvent),
    Run(RunEvent),
    Turn(TurnEvent),
    Context(ContextEvent),
    ToolConfig(ToolConfigEvent),
    Tool(ToolEvent),
    Effect(EffectEvent),
}
```

Session lifecycle events:

```rust
pub enum SessionLifecycleEvent {
    Opened { config: SessionConfig },
    ConfigChanged { config: SessionConfig, revision: u64 },
    Closed,
}
```

Run events:

```rust
pub enum RunEvent {
    Started {
        run_id: RunId,
        submission_id: Option<SubmissionId>,
        input_ref: BlobRef,
        run_config: RunConfig,
        config_revision: u64,
    },
    Queued {
        submission_id: Option<SubmissionId>,
        input_ref: BlobRef,
        run_config: RunConfig,
    },
    SteeringAdded {
        run_id: RunId,
        input_ref: BlobRef,
    },
    CancellationRequested {
        run_id: RunId,
    },
    Completed {
        run_id: RunId,
        output_ref: Option<BlobRef>,
    },
    Failed {
        run_id: RunId,
        failure: RunFailure,
    },
    Cancelled {
        run_id: RunId,
    },
}
```

Turn events:

```rust
pub enum TurnEvent {
    Started {
        turn_id: TurnId,
        run_id: RunId,
    },
    Planned {
        turn_id: TurnId,
        run_id: RunId,
        request: LlmRequest,
    },
    GenerationRequested {
        turn_id: TurnId,
        run_id: RunId,
        effect_id: EffectId,
    },
    Completed {
        turn_id: TurnId,
        outcome: TurnOutcome,
    },
}
```

Context events:

```rust
pub enum ContextEvent {
    ItemsRecorded {
        items: Vec<ContextItem>,
    },
    WindowPlanned {
        run_id: RunId,
        turn_id: TurnId,
        window: ContextWindow,
    },
    CompactionRecorded {
        run_id: RunId,
        turn_id: Option<TurnId>,
        record: CompactionRecord,
    },
}
```

Tool config and tool runtime events:

```rust
pub enum ToolConfigEvent {
    RegistryChanged { registry: ToolRegistry },
    ProfileSelected { profile_id: ToolProfileId },
}

pub enum ToolEvent {
    BatchStarted {
        run_id: RunId,
        turn_id: TurnId,
        batch_id: ToolBatchId,
        calls: Vec<ObservedToolCall>,
    },
    BatchCompleted {
        run_id: RunId,
        turn_id: TurnId,
        batch_id: ToolBatchId,
    },
}
```

Observed tool calls are generation facts on `LlmGenerationReceipt`. Tool results are
authoritative through `ToolInvocationReceipt`. `ToolConfigEvent` owns
model-visible tool configuration changes. `ToolEvent` owns runtime tool-batch
lifecycle facts after a completed model turn has queued tool calls. Tool-result
context items are committed before a batch can complete, so the next model turn
never starts without the terminal tool outputs in context.

Effect events:

```rust
pub enum EffectEvent {
    IntentCreated {
        intent: AgentEffectIntent,
    },
    ReceiptAccepted {
        receipt: AgentEffectReceipt,
    },
    CancellationRequested {
        effect_id: EffectId,
    },
    Abandoned {
        effect_id: EffectId,
        reason: EffectAbandonReason,
    },
}
```

Contract requirements:

- event names read as facts, not requests
- events are small and blob-backed
- serde names are explicit
- effect receipts do not resurrect terminal runs

### Provider API Kind

API kind is part of session compatibility.

```rust
pub enum ProviderApiKind {
    OpenAiResponses,
    AnthropicMessages,
    OpenAiCompletions,
}

pub struct ModelSelection {
    pub api_kind: ProviderApiKind,
    pub provider_id: String,
    pub model: String,
    pub options: ModelProviderOptions,
}

pub enum ModelProviderOptions {
    None,
    OpenAiResponses(OpenAiModelOptions),
    AnthropicMessages(AnthropicModelOptions),
    OpenAiCompletions(OpenAiModelOptions),
}

pub struct TurnConfig {
    pub max_output_tokens: Option<u32>,
    pub provider_request_defaults: ProviderRequestDefaults,
}

pub enum ProviderRequestDefaults {
    None,
    OpenAiResponses(OpenAiResponsesRequestDefaults),
    AnthropicMessages(AnthropicMessagesRequestDefaults),
    OpenAiCompletions(OpenAiCompletionsRequestDefaults),
}

pub struct ProviderCompatibility {
    pub api_kind: ProviderApiKind,
    pub model: String,
    pub native_context_family: String,
}
```

First-cut rule:

- a session pins one `ProviderApiKind`
- model changes inside the same API kind are allowed only when the policy can
  prove the current active context remains valid
- switching API kinds is out of scope until explicit rewrite/compaction support
  exists

Contract requirements:

- incompatible API kind changes while context exists are represented as typed
  validation/admission failures
- no durable model record uses a fake shared provider message enum

### Provider-Native Request Plan

The committed LLM generation intent should contain the exact provider execution
plan. The plan is ref-based so large context windows can reuse per-item blobs
instead of materializing a new mega request blob for every turn.

```rust
pub struct LlmRequest {
    pub model: ModelSelection,
    pub request_fingerprint: String,
    pub kind: LlmRequestKind,
}

pub enum LlmRequestKind {
    OpenAiResponses(OpenAiResponsesRequest),
    AnthropicMessages(AnthropicMessagesRequest),
    OpenAiCompletions(OpenAiCompletionsRequest),
}

pub struct OpenAiResponsesRequest {
    pub instructions_ref: Option<BlobRef>,
    pub input_window: ContextWindow,
    pub previous_response_id: Option<String>,
    pub tools: Vec<ToolSpec>,
    pub tool_choice: Option<OpenAiResponsesToolChoice>,
    pub reasoning: Option<OpenAiReasoningConfig>,
    pub text: Option<serde_json::Value>,
    pub include: Vec<String>,
    pub max_output_tokens: Option<u32>,
    pub max_tool_calls: Option<u32>,
    pub metadata: BTreeMap<String, String>,
    pub context_management: Option<serde_json::Value>,
    pub extra: BTreeMap<String, serde_json::Value>,
}

pub struct AnthropicMessagesRequest {
    pub system_ref: Option<BlobRef>,
    pub messages_window: ContextWindow,
    pub tools: Vec<ToolSpec>,
    pub tool_choice: Option<AnthropicToolChoice>,
    pub thinking: Option<AnthropicThinkingConfig>,
    pub max_tokens: u32,
    pub metadata: Option<serde_json::Value>,
    pub context_management: Option<serde_json::Value>,
    pub extra: BTreeMap<String, serde_json::Value>,
}

pub struct OpenAiCompletionsRequest {
    pub messages_window: ContextWindow,
    pub tools: Vec<ToolSpec>,
    pub tool_choice: Option<OpenAiCompletionsToolChoice>,
    pub response_format: Option<serde_json::Value>,
    pub max_tokens: Option<u32>,
    pub max_completion_tokens: Option<u32>,
    pub metadata: BTreeMap<String, String>,
    pub extra: BTreeMap<String, serde_json::Value>,
}
```

The executor loads immutable item and tool content blobs referenced by the
provider-specific plan, materializes the matching `forge-llm` native request in
memory, and calls the matching provider client. The executor may materialize a
request blob for audit or debugging, but that is not the primary execution
contract.

Contract requirements:

- committed effects can be executed without re-planning the request
- large context windows reuse immutable per-item blobs
- small provider options are structured inline instead of hidden behind refs
- provider API kind implies the native response family
- context planning remains a pure data contract and does not call a provider

### LLM Effects and Receipts

Effect intent:

```rust
pub enum AgentEffectIntent {
    LlmGenerate(LlmGenerationIntent),
    LlmCountTokens(LlmCountTokensIntent),
    LlmCompact(LlmCompactionIntent),
    ToolInvoke(ToolInvocationIntent),
}

pub struct LlmGenerationIntent {
    pub effect_id: EffectId,
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub request: LlmRequest,
}

pub struct LlmCountTokensIntent {
    pub effect_id: EffectId,
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub request: LlmRequest,
}
```

Receipt:

```rust
pub enum AgentEffectReceipt {
    LlmGeneration(LlmGenerationReceipt),
    LlmTokenCount(LlmTokenCountReceipt),
    LlmCompaction(LlmCompactionReceipt),
    ToolInvocation(ToolInvocationReceipt),
}

pub struct LlmGenerationReceipt {
    pub effect_id: EffectId,
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub status: LlmGenerationStatus,
    pub context_items: Vec<UncommittedContextItem>,
    pub facts: LlmGenerationFacts,
}
```

Generation facts are the small normalized facts the reducer needs:

```rust
pub struct LlmGenerationFacts {
    pub provider_response_id: Option<String>,
    pub finish: LlmFinish,
    pub usage: Option<LlmUsage>,
    pub tool_calls: Vec<ObservedToolCall>,
    pub context_token_estimate: Option<TokenEstimate>,
    pub compaction: Option<CompactionRecord>,
}
```

These facts are not a common provider response model. They are reducer inputs
extracted by the adapter. P46 deliberately does not add a whole raw provider
response field to the receipt; raw response retention belongs to the
adapter/runner phase once provider execution exists. Provider-native response
output is split by the adapter into uncommitted `context_items`, so one raw
response can produce multiple assistant, reasoning, tool-call, or compaction
context items without replay parsing opaque provider JSON. Admission assigns
`ContextItemId` values when accepting those outputs into committed
`ContextEvent::ItemsRecorded` facts.

Contract requirements:

- effect ids are unique and pending until settled
- duplicate terminal receipts are idempotent or typed errors by policy
- provider-native output is available through committed context item refs;
  whole raw provider response retention is deferred to the adapter/runner phase
- reducer can decide from finish/tool_calls/context updates without reparsing
  provider JSON

### Tooling Data Contracts

`ToolSpec` describes a model-visible logical capability. It does not bind to a
local handler in the core crate. If a model calls a client/function tool, the
call becomes a `ToolInvoke` effect and the runner/tool package decides how to
execute it.

```rust
pub struct ToolRegistry {
    pub tools: BTreeMap<ToolName, ToolSpec>,
    pub profiles: BTreeMap<ToolProfileId, ToolProfile>,
}

pub struct ToolProfile {
    pub profile_id: ToolProfileId,
    pub visible_tools: Vec<ToolName>,
    pub tool_choice: Option<ToolChoice>,
}

pub struct ToolSpec {
    pub name: ToolName,
    pub kind: ToolKind,
    pub parallelism: ToolParallelism,
}

pub enum ToolKind {
    Function(FunctionToolSpec),
    ProviderNative(ProviderNativeToolSpec),
}

pub struct FunctionToolSpec {
    pub model_name: Option<ToolName>,
    pub description_ref: Option<BlobRef>,
    pub input_schema_ref: BlobRef,
    pub output_schema_ref: Option<BlobRef>,
    pub strict: Option<bool>,
    pub provider_options_ref: Option<BlobRef>, // Deferred: tool lowering may move this inline later.
}

pub struct ProviderNativeToolSpec {
    pub api_kind: ProviderApiKind,
    pub native_tool_ref: BlobRef,
    pub execution: ProviderNativeToolExecution,
}

pub struct ObservedToolCall {
    pub call_id: ToolCallId,
    pub tool_name: ToolName,
    pub provider_kind: Option<String>,
    pub arguments_ref: BlobRef,
    pub native_call_ref: Option<BlobRef>,
}
```

Tool invocation intents and receipts should be model-visible where appropriate:

```rust
pub struct ToolInvocationIntent {
    pub effect_id: EffectId,
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub batch_id: ToolBatchId,
    pub call_id: ToolCallId,
    pub tool_name: ToolName,
    pub arguments_ref: BlobRef,
}

pub struct ToolInvocationReceipt {
    pub effect_id: EffectId,
    pub batch_id: ToolBatchId,
    pub call_id: ToolCallId,
    pub status: ToolCallStatus,
    pub output_ref: Option<BlobRef>,
    pub model_visible_output_ref: Option<BlobRef>,
    pub error_ref: Option<BlobRef>,
}
```

Contract requirements:

- missing/unavailable tools can become deterministic model-visible failures
- provider-native hosted tools can be passed through without pretending they
  have local handlers
- model-visible result ordering is stable even if runner completions arrive out
  of order
- no host filesystem/process assumptions appear in `ToolSpec`

### Context Window and Compaction

Separate transcript-like facts from the active context window used for the next turn.

```rust
pub struct ContextState {
    pub retained_items: Vec<ContextItem>,
    pub active_window: Option<ContextWindow>,
    pub latest_compaction: Option<CompactionRecord>,
}

pub struct ContextWindow {
    pub api_kind: ProviderApiKind,
    pub item_ids: Vec<ContextItemId>,
    pub token_estimate: Option<TokenEstimate>,
}

pub struct ContextItem {
    pub item_id: ContextItemId,
    pub kind: ContextItemKind,
    pub source: ContextItemSource,
    pub provider_kind: Option<String>,
    pub native_item_ref: BlobRef,
    pub media_type: Option<String>,
    pub preview: Option<String>,
    pub provider_item_id: Option<String>,
    pub token_estimate: Option<TokenEstimate>,
}

pub struct UncommittedContextItem {
    pub kind: ContextItemKind,
    pub source: ContextItemSource,
    pub provider_kind: Option<String>,
    pub native_item_ref: BlobRef,
    pub media_type: Option<String>,
    pub preview: Option<String>,
    pub provider_item_id: Option<String>,
    pub token_estimate: Option<TokenEstimate>,
}

pub enum ContextItemKind {
    Message { role: ContextMessageRole },
    ToolCall { call_id: ToolCallId, name: ToolName },
    ToolResult { call_id: ToolCallId, is_error: bool },
    ReasoningState,
    CompactionState,
    ProviderOpaque,
}
```

Each context item is one immutable provider-renderable item or block by blob
reference. The active context window is an ordered list of item ids, not a
rewritten blob containing the whole prompt.

Compaction records must describe the window change without needing to understand
the provider's opaque compaction payload:

```rust
pub struct CompactionRecord {
    pub mode: CompactionMode,
    pub source_item_ids: Vec<ContextItemId>,
    pub output_item_ids: Vec<ContextItemId>,
    pub result_window: ContextWindow,
    pub summary_ref: Option<BlobRef>,
}
```

Contract requirements:

- compaction changes active state only through committed events
- provider-native message/item payloads remain blob-backed and reusable
- active state remains bounded

## Kernel APIs

P46 should define the deterministic kernel surface even if later phases fill in
the full policy loop.

### Projection

```rust
pub trait ApplyEvent {
    fn apply(
        &self,
        state: &mut SessionState,
        entry: &SessionEntry,
    ) -> Result<(), ModelError>;
}
```

Responsibilities:

- validate session lifecycle event ordering
- update bounded `SessionState`
- track active and queued runs
- update config revision and provider compatibility
- track pending effects and terminal receipts
- apply context window and compaction facts
- track tool batches and terminal tool results
- update id cursors from committed ids

Non-responsibilities:

- validating external commands
- planning turns
- compiling provider requests
- executing effects
- scheduling activities
- invoking hooks

### Admission

```rust
pub trait AdmitCommand {
    fn admit(
        &self,
        state: &SessionState,
        command: SessionCommand,
    ) -> Result<Vec<SessionEventProposal>, CommandError>;
}
```

First-cut behavior can be intentionally small:

- open session
- update config
- submit run or queue follow-up
- add steering to active run
- request cancellation of active run
- record effect receipt
- close session when allowed

### Policy Pipeline Contract

Define the policy boundary and ordering without implementing the full
autonomous loop.

```rust
pub trait DecideNext {
    fn decide(
        &self,
        state: &SessionState,
    ) -> Result<Vec<SessionEventProposal>, PolicyError>;
}

pub struct PolicyPipeline {
    layers: Vec<Box<dyn DecideNext>>,
}
```

Core behavior should use the same trait as extension behavior. The first policy
layers are:

- `CoreRunPolicy`
- `CoreToolPolicy`
- `CoreContextPolicy`
- `CoreTurnPolicy`

The coordinator returns the first non-empty policy output:

- policies read the same committed state and the pipeline short-circuits on the
  first non-empty proposal batch
- no policy sees another policy's uncommitted proposals in the same pass
- dependent proposals wait for a later decide pass after apply/replay
- no duplicate pending effect ids
- no generation while another generation is pending
- no tool invocation for unknown or disabled tools
- no incompatible context-window replacement while a compact effect is pending
- no events that mutate terminal runs except allowed late receipt handling

Full autonomous policy behavior is a later phase. P46 should define the
contract surface that the next roadmap slices can implement layer by layer.

## Implementation Plan

This plan is intentionally rough. Each policy slice is expected to discover
model, event, effect, and state refinements that should be folded back into the
contracts before moving on.

### P46.1 Core contract skeleton

- Add root-level `src/lib.rs` and the first `src/model/` contract modules.
- Re-export the intended public SDK types from stable paths.
- Establish `SessionCommand`, `SessionEvent`, `SessionEntry`,
  `SessionState`, ids, blob refs, provider envelopes, and effect envelopes as
  the top-level vocabulary.

Contract requirements:

- crate docs state that `forge-agent` is the deterministic core SDK
- the public transition layer is session-scoped
- no `kernel/`, `testing/`, runner, adapter, or tool-execution module is
  introduced in the first slice
- `storage/` remains limited to logical storage contracts and in-memory helpers

### P46.2 Shared model baseline

- Add id newtypes and replay-derived cursor records.
- Add `BlobRef` as a model reference type.
- Add `EventSeq`, `SessionPosition`, `SessionEntry`,
  `UncommittedSessionEvent`, and `SessionEventJoins`.
- Add first-cut `SessionConfig`, `RunConfig`, `TurnConfig`, model selection,
  provider API kind, and provider compatibility records.

Contract requirements:

- event envelopes carry position, timestamp, joins, and payload
- event payloads do not carry persistence metadata
- production store append/read backends are deferred
- incompatible API kind changes are represented as typed validation/admission
  failures
- config revision fields are explicit and monotonic

### P46.3 Commands, events, and effects baseline

- Add `SessionCommand`, command errors, and fact-oriented event families.
- Keep command-shaped concepts out of `SessionEvent`.
- Add event joins for run/turn/effect/tool/submission/correlation references.
- Add first-cut `AgentEffectIntent` and `AgentEffectReceipt` envelopes.

Contract requirements:

- command admission can represent accepted and rejected open/submit/steer/cancel
- event payloads do not carry session id, event sequence, or observed timestamp
- effect intent events are the only executable-work authority

### P46.4 Bounded state baseline

- Add `SessionState`, `RunState`, `TurnState`, `ContextState`,
  `PendingEffectState`, and tool batch state.
- Add state invariants and transition-result structs needed by projection and
  policy contracts.

Contract requirements:

- state contains only bounded control data needed for next-step decisions
- state can represent active run, queued follow-ups, pending effects, context
  items/windows, compaction state, and run-level tool batches
- invalid lifecycle transitions are representable as typed model errors

### P46.5 Session transition traits

- Implemented in `crates/forge-agent/src/transition.rs`.
- First `CoreAdmitCommand` implementation added in
  `crates/forge-agent/src/admit.rs` for opening sessions, updating config,
  queuing run requests, steering/cancelling active runs, and closing idle
  sessions. It also admits runner effect receipts when they match an existing
  pending effect intent.
- First `CoreApplyEvent` implementation added in `crates/forge-agent/src/apply.rs`
  for lifecycle events, run queue/start/steer/cancel/terminal events, turn-start
  events, turn request planning, LLM generation intent recording,
  generation-requested turn markers, effect receipt settlement, turn completion
  from settled generation receipts, context item/window events, replay position
  tracking, and deterministic id cursor updates.
- Add pure session-level command admission and projection traits/signatures:

```rust
pub trait AdmitCommand {
    fn admit(
        &self,
        state: &SessionState,
        command: SessionCommand,
    ) -> Result<Vec<SessionEventProposal>, CommandError>;
}

pub trait ApplyEvent {
    fn apply(
        &self,
        state: &mut SessionState,
        entry: &SessionEntry,
    ) -> Result<(), ModelError>;
}
```

Contract requirements:

- traits are runtime-neutral and side-effect-free
- `admit` returns event proposals only
- `apply` cannot emit effects or schedule work

### P46.6 Policy pipeline contract

- Raw policy contract implemented in `crates/forge-agent/src/transition.rs`.
- First coordinator behavior implemented as `PolicyPipeline` in
  `crates/forge-agent/src/policy.rs`.
- Add the raw policy trait and a shared `SessionEventProposal` type.
- Define policy layer ordering as:
  `CoreRunPolicy -> CoreToolPolicy -> CoreContextPolicy -> CoreTurnPolicy`.
- State explicitly that each layer may force refinement of model/event/effect
  records discovered while designing that layer.

```rust
pub trait DecideNext {
    fn decide(
        &self,
        state: &SessionState,
    ) -> Result<Vec<SessionEventProposal>, PolicyError>;
}
```

Contract requirements:

- core and extension policies use the same interface
- policy layers are ordered but independently understandable
- `decide` can propose session events but cannot mutate state directly
- policies all read the same committed state
- `PolicyPipeline` short-circuits on the first non-empty proposal batch
- appending, timestamping, and durable storage remain outside the policy
  coordinator
- proposals that depend on newly proposed events wait for the next decide pass

### P46.7 Core run policy contract

- First queue-to-start behavior implemented in
  `crates/forge-agent/src/policy.rs`.
- It owns run-level progress decisions at the session layer:
  start queued work, promote follow-ups, route steering/cancellation, mark runs
  terminal from committed turn/effect state, and enforce single-active-run
  constraints.
- Refine run events, run state, and run-related command admission as needed.
- Admission records `RunEvent::Queued`; `CoreRunPolicy` proposes
  `RunEvent::Started` when the session is open, no run is active, and queued
  work exists.
- Admission records `RunEvent::SteeringAdded` and
  `RunEvent::CancellationRequested` for active run commands.
- `CoreRunPolicy` proposes `RunEvent::Cancelled` when a cancelling run has no
  active turn, active tool batch, or pending effects.
- `CoreRunPolicy` proposes `RunEvent::Completed` when the active run is idle
  and the latest committed turn has `TurnOutcome::FinalOutput`.
- `CoreRunPolicy` proposes `RunEvent::Failed` when the active run is idle and
  the latest committed turn has a failed or cancelled terminal outcome.
- `CoreRunPolicy` allocates `RunId` deterministically from
  `state.id_cursors.last_run_id + 1`; projection updates the cursor from
  committed events.
- `CoreTurnPolicy` does not start another turn after a run-terminal turn
  outcome; the run policy owns closing that run first.

Contract requirements:

- no separate public run aggregate is introduced
- run policy proposes session events through `SessionEventProposal`
- active/queued/terminal run state remains bounded

### P46.8 Core turn policy contract

- First turn-frame behavior implemented in `crates/forge-agent/src/policy.rs`
  and `crates/forge-agent/src/apply.rs`.
- It owns turn planning as policy behavior:
  start a durable turn frame, then later build `LlmRequest`, record
  turn-planned facts, and propose LLM generation intents.
- Fold the old "turn planner" concept into policy helpers instead of a separate
  subsystem.
- Refine `TurnState`, turn events, active context inputs, provider-native
  request contracts, LLM generation intents, and LLM receipts as needed.
- `CoreTurnPolicy` proposes `TurnEvent::Started` when the session is open, a run
  is active, no turn/tool batch is active for that run, and the run is not
  cancelling.
- `CoreTurnPolicy` allocates `TurnId` deterministically from
  `state.id_cursors.last_turn_id + 1`; projection updates the cursor from
  committed events.
- When the active turn is `Started` and an active context window exists,
  `CoreTurnPolicy` builds the provider-native `LlmRequest` from committed
  session config, active run model override, context instructions, selected
  tool profile, and the active `ContextWindow`, then proposes
  `TurnEvent::Planned`.
- When the active turn is `Planned`, `CoreTurnPolicy` proposes one coherent
  ordered batch: first `EffectEvent::IntentCreated` with
  `AgentEffectIntent::LlmGenerate`, then `TurnEvent::GenerationRequested`
  pointing at the same `EffectId`.
- `CoreApplyEvent` records the pending LLM effect, advances
  `state.id_cursors.last_effect_id`, and only lets a turn enter
  `GenerationPending` after the matching generation intent has already been
  committed.
- `CoreAdmitCommand` accepts `SessionCommand::RecordEffectReceipt` only when
  the receipt matches an existing pending intent, then proposes
  `EffectEvent::ReceiptAccepted`.
- `CoreApplyEvent` marks receipt-recorded effects as settled, keeps the receipt
  on the pending effect until turn settlement consumes it, and removes the
  effect when the active turn completes.
- When the active turn is `GenerationPending` and its LLM generation effect has
  a settled receipt, `CoreTurnPolicy` proposes context item records from the
  receipt, a compaction record when provided, and `TurnEvent::Completed` with a
  normalized `TurnOutcome`.

Contract requirements:

- `TurnState` is the child state for one model-call frame
- exactly one generation may be pending for a turn
- committed LLM intent contains the exact provider-native request ref needed by
  a later runner
- old unified provider concepts do not appear in the turn model
- effect intent recording is the executable-work authority; the turn marker is
  the child-state link to that already-recorded intent
- generation receipts are the adapter boundary back into committed facts; replay
  never reparses raw provider output to discover context items or facts

### P46.9 Core tool policy contract

- First tool-batch handoff behavior implemented in
  `crates/forge-agent/src/policy.rs`, `crates/forge-agent/src/apply.rs`, and
  the model event/tool records.
- It owns tool-call follow-up decisions:
  turn LLM generation facts into active run-level tool batch state, represent
  unavailable tools as model-visible failures, and propose tool invocation
  intents for accepted calls.
- Refine tool registry/profile/call/result records, tool batch state, and tool
  invocation effect contracts as needed.
- `TurnEvent::Completed { outcome: ToolCallsQueued }` remains the durable turn
  fact. Tool batch startup is recorded separately as `ToolEvent::BatchStarted`
  because the batch has its own run-level lifecycle after the model turn is
  complete.
- `CoreToolPolicy` reads completed turns with `ToolCallsQueued`, allocates
  `ToolBatchId` from `state.id_cursors.last_tool_batch_id + 1`, and proposes
  `ToolEvent::BatchStarted`.
- `CoreApplyEvent` validates that the batch calls match the completed turn's
  generation facts, inserts `ActiveToolBatch`, sets
  `active_run.active_tool_batch_id`, advances the tool-batch cursor, and marks
  each call as accepted only when it is visible in the selected tool profile.
  Unavailable calls receive a deterministic model-visible failure
  `ToolCallResult` immediately.
- Once a tool batch is active, `CoreToolPolicy` turns accepted client-effect
  calls without pending effects into `EffectEvent::IntentCreated` proposals
  containing `AgentEffectIntent::ToolInvoke`. Function tools are client-effect
  tools by default; provider-native tools only use this path when their
  execution mode is `ClientEffect`.
- `CoreApplyEvent` validates `ToolInvoke` intents against the active tool
  batch, links the pending `EffectId` back to the matching `ToolCallState`, and
  marks that call `Pending`.
- `CoreApplyEvent` applies `ToolInvocationReceipt` by writing a
  `ToolCallResult`, moving the call to a terminal status, clearing the pending
  call effect link, and removing the effect from active pending state.
- Once all calls in an active batch are terminal, `CoreToolPolicy` proposes
  `ContextEvent::ItemsRecorded` for missing `ContextItemKind::ToolResult`
  items in original call order, followed by `ToolEvent::BatchCompleted`.
- `CoreApplyEvent` only applies `ToolEvent::BatchCompleted` after all terminal
  result context items are present; it then moves the batch into
  completed-batch state and clears `active_run.active_tool_batch_id`.

Contract requirements:

- tool execution remains out of scope
- out-of-order tool receipt settlement can still produce stable model-visible
  result order
- tool policy proposes session-level actions only
- a completed turn is not mutated to start tools; it is the source fact for a
  separate tool-batch lifecycle

### P46.10 Core context policy contract

- First context behavior implemented in `crates/forge-agent/src/policy.rs`
  and `crates/forge-agent/src/apply.rs`.
- It owns context item/window and compaction decisions:
  token-count needs, active-window replacement records, provider-managed
  compaction observations, client-managed compaction effects, and provider API
  kind compatibility checks.
- Refine `ContextItem`, `ContextWindow`, token count, compaction records, and
  provider compatibility records as needed.
- `CoreContextPolicy` records the active run input as a retained context item
  after the run has started. This stays out of command admission because the
  run-scoped `ContextItemSource::RunInput` needs the committed `RunId`, which
  only exists after `RunEvent::Started` has been appended and applied.
- `CoreContextPolicy` plans a `ContextEvent::WindowPlanned` only after the
  active run has an active turn and retained context items exist. It reads the
  session model API kind from committed config and builds a `ContextWindow`
  over item ids rather than materializing a whole-context blob.
- `CoreApplyEvent` validates deterministic `ContextItemId` allocation from
  `state.id_cursors.last_context_item_id + 1`, rejects windows that reference
  unknown context items, and applies compaction records by updating
  `latest_compaction` plus the active window.
- Recording new context items invalidates the active context window; the next
  model turn must receive a freshly planned window that includes the new
  retained items.

Contract requirements:

- compaction changes active state only through committed events
- provider-native message/item payloads remain blob-backed and reusable
- active context state remains bounded
- run input context appears as committed facts, not hidden runner state

### P46.11 Final contract pass and documentation

- Do a final naming and module-boundary pass across models, events, effects,
  state, and policy contracts.
- Update `crates/forge-agent/README.md` if it exists or add one if useful.
- Mark earlier roadmap assumptions that conflict with P46 as historical context
  or superseded where needed.
- Do not update old archived specs unless the change is directly relevant.

Contract requirements:

- docs point to `docs/spec/04-new-agent-idea.md` and P45 as current direction
- roadmap language reflects the single public session transition layer
- no active roadmap for the next agent phase depends on the old unified
  `forge-llm` abstraction

## Designs Rejected

### Continue the old P40-P44 implementation path

Rejected because the crate has been reset and P45 changed the LLM boundary
fundamentally. The old path is still useful context, but the next implementation
should be direct and provider-native from the start.

### Copy AOS agent contracts wholesale

Rejected because AOS was trying to solve a broader world/workflow/runtime
problem. Forge should lift event-sourcing lessons, id discipline, and context
management ideas without importing AIR, world governance, host schemas, or AOS
runtime assumptions.

### Recreate a unified provider message model in `forge-agent`

Rejected because it would undo the main P45 design decision. The agent can
normalize a few generation facts, but native requests/responses must stay
provider-shaped.

### Implement a full autonomous runner in P46

Rejected because the first cut needs durable primitives and invariants more
than another partial loop. The full runner should come after the event/state
contracts are stable enough to support it.

### Implement production stores or test harnesses in P46

Rejected for scope. The old work gives strong references for CAS, durable
session storage, and fake runners. Those pieces can be reintroduced quickly
after the pure contracts settle.

## Non-Goals

- No production `BlobStore` or `SessionStore` backend.
- No dedicated test harness, fake adapters, or fake runner.
- No production Temporal runner.
- No local process runner.
- No real provider adapter implementation.
- No host tool packages.
- No CLI rewrite.
- No Attractor rebuild.
- No durable snapshots.
- No fork/rollback/rewrite support.
- No compatibility layer for old agent event logs.

## Done When

- `forge-agent` has a coherent `model/` contract layout.
- Core records are serializable and use `BlobRef` for large payloads.
- Commands are separate from committed events.
- The committed event model is fact-oriented.
- Session log envelope contracts and logical storage interfaces are defined
  around the same model types.
- The public transition layer is session-scoped; run and turn state are child
  control records, not separate public aggregates.
- `TurnState` is modeled as the control frame for one model call.
- Pure admission, projection, and policy/decide traits are defined.
- The policy pipeline is middleware-shaped and ordered around
  `CoreRunPolicy`, `CoreToolPolicy`, `CoreContextPolicy`, and
  `CoreTurnPolicy`.
- `apply` is specified as replay-only and cannot emit work.
- turn planning is represented as policy behavior, not a separate planner
  subsystem.
- Provider API kind compatibility is explicit in the contracts.
- LLM effect intents carry exact provider-native request refs.
- LLM receipts expose generation facts and provider-native output context refs
  only; whole raw provider response retention is deferred to the adapter/runner
  phase.
- Tooling records model registry/profile/call/result data without host
  execution.
- No production store, runner, adapter, or test harness module is introduced.
- `cargo check -p forge-agent` and `cargo test -p forge-agent` pass once the
  contracts are implemented.
- `rg "AgentEvent|ArtifactRef|JournalStore|InputEvent|DeciderOutcome|ProviderAdapter|forge_llm::Client|ContentPart|ToolDefinition" crates/forge-agent`
  finds no old public model surface.
