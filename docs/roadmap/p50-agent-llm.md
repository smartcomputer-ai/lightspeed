# P50: Agent LLM Effect Adapter

**Status**
- In progress

**Progress**
- Added `crates/forge-agent-llm` to the workspace.
- Added the thin adapter crate shape:
  `error`, `executor`, `receipt`, `blob_io`, `openai_responses`,
  `openai_completions`, `anthropic_messages`, and `testing`.
- Reworked the agent model boundary so provider requests carry
  `ResolvedContextWindow` snapshots. `ContextWindow` remains the compact
  retained-item id window in session state, while executable `LlmRequest`
  values carry the ordered `ContextItem` metadata they need.
- Narrowed `EffectExecutionRequest` back to `session_id` plus the effect intent.
  LLM/tool executors no longer receive full `SessionState` snapshots.
- Added `LlmGenerationAdapter`, `LlmAdapterRegistry`, and `LlmEffectExecutor`.
  The executor handles `LlmGenerate` inline through the registered adapter,
  returns failed token-count/compaction receipts for unsupported paths, and
  leaves non-LLM effects dispatched for another executor.
- Implemented the OpenAI Responses generation path:
  - materializes `forge-agent::OpenAiResponsesRequest` into
    `forge_llm::openai::responses::CreateResponseRequest`
  - loads instructions, context items, tool schemas, descriptions, and provider
    options from `BlobStore`
  - maps function tools, provider-native tools, tool choice, reasoning, text,
    metadata, truncation, context management, and extra request fields
  - calls a provider seam implemented by `forge_llm::openai::responses::Client`
  - stores provider request and raw provider response JSON blobs
  - extracts reducer facts: provider response id, finish reason, usage, token
    estimate, assistant context items, and observed tool calls
- Added deterministic tests for exact OpenAI Responses request JSON and
  end-to-end effect execution through a fake OpenAI Responses API.
- Added an ignored OpenAI Responses live smoke test for the agent adapter under
  `crates/forge-agent-llm/tests/`, matching the `forge-llm` env/root `.env`
  pattern and failing loudly when explicitly run without credentials.
- Added the local runtime crate with an inline effect router that composes
  LLM and host-tool executors for local SDK use.
- Added a local cross-crate runtime test that drives a full
  LLM -> host tool -> LLM loop using fake OpenAI Responses and real
  `forge-agent-tools` host filesystem tools.

## Goal

Create `forge-agent-llm`, the LLM effect adapter crate that connects
`forge-agent` LLM effect intents to the provider-native `forge-llm` clients.

This crate should make real LLM effects possible without making `forge-agent`
depend on provider clients, credentials, HTTP, or provider execution policy.

## Design Position

`forge-agent` plans provider-native `LlmRequest` values and records normalized
reducer-facing receipts. `forge-llm` owns provider API clients. The adapter
between them should stay thin:

```text
AgentEffectIntent::LlmGenerate
  -> load blob-backed provider input
  -> materialize provider-native forge-llm request
  -> call matching provider API
  -> store raw/native response blobs where needed
  -> extract reducer facts
  -> AgentEffectReceipt::LlmGeneration
```

The adapter parses only what the reducer must branch on:

- finish reason
- usage
- provider response id
- tool calls
- model-visible context items
- context-limit pressure
- token counts
- compaction result metadata

Raw or native provider payload retention stays blob-backed and adapter-owned.

## Agent Model Boundary

LLM effect execution should not depend on a full `SessionState` snapshot.
Provider requests therefore carry resolved context item metadata directly.

`ContextWindow` should remain a compact planning/state type inside
`ContextState`: it says which retained context item ids are currently selected,
in which order, for a provider API kind. It is useful for replay, compaction,
policy, and context-window planning.

Executable LLM requests contain a resolved snapshot of the selected context
items:

```rust
pub struct ResolvedContextWindow {
    pub api_kind: ProviderApiKind,
    pub items: Vec<ContextItem>,
    pub token_estimate: Option<TokenEstimate>,
}
```

The provider request model uses resolved windows:

```rust
pub struct OpenAiResponsesRequest {
    pub instructions_ref: Option<BlobRef>,
    pub input_window: ResolvedContextWindow,
    pub tools: Vec<ToolSpec>,
    // provider-native options...
}

pub struct AnthropicMessagesRequest {
    pub system_ref: Option<BlobRef>,
    pub messages_window: ResolvedContextWindow,
    pub tools: Vec<ToolSpec>,
    // provider-native options...
}

pub struct OpenAiCompletionsRequest {
    pub messages_window: ResolvedContextWindow,
    pub tools: Vec<ToolSpec>,
    // provider-native options...
}
```

This mirrors the existing tool behavior: `ToolSpec` values are copied into each
planned request, so the request is executable even if the registry changes
later. Context items should follow the same rule. The actual payloads still
stay behind `BlobRef`; only the compact metadata needed to load and materialize
provider input is copied.

The request planner in `forge-agent` now:

- keeps `ContextState.active_window: Option<ContextWindow>` as the current
  selected id window
- when building an `LlmRequest`, resolves every selected id against
  `ContextState.retained_items`
- preserves the window ordering exactly
- fails with an invariant/model error if a selected id is missing
- copies the selected `ContextItem` values into `ResolvedContextWindow`
- includes the resolved item metadata in the `request_fingerprint`

`forge-agent-llm` materializes provider input directly from the resolved items
in the request. It does not need `SessionState`, retained-item lookup, or a
side-channel context provider.

The effect execution boundary is execution metadata plus the intent:

```rust
pub struct EffectExecutionRequest {
    pub session_id: SessionId,
    pub intent: AgentEffectIntent,
}
```

P60 can add durable dispatch metadata separately without making every executor
depend on full replayed state.

## Target Crate Shape

Add:

```text
crates/forge-agent-llm/
  Cargo.toml
  src/lib.rs
  src/error.rs
  src/executor.rs
  src/openai_responses.rs
  src/openai_completions.rs
  src/anthropic_messages.rs
  src/receipt.rs
  src/blob_io.rs
  src/testing.rs
```

Candidate public entry points:

```rust
LlmEffectExecutor
LlmAdapterRegistry
OpenAiResponsesLlmAdapter
OpenAiCompletionsLlmAdapter
AnthropicMessagesLlmAdapter
```

The crate should implement the existing `EffectExecutor` boundary for simple
inline runners. Future process/Temporal runners should also be able to reuse the
provider-specific adapter functions inside separate activities.

## Initial Scope

Implement the first real provider path end to end, then add the others behind
the same structure.

Recommended order:

1. `openai:responses` generation
2. `anthropic:messages` generation
3. `openai:completions` generation
4. token counting where provider support exists or a conservative unsupported
   receipt otherwise
5. compaction as a specialized generation path once context compaction policy is
   ready

## Testing Strategy

- Use fake HTTP/transport fixtures for provider request/response mapping.
- Assert exact provider JSON for each API kind.
- Assert receipt extraction for text output, tool calls, usage, finish reasons,
  and provider response ids.
- Keep live provider tests ignored and explicit because they require API keys
  and cost money.
- Do not silently skip tests based on environment variables.

## Out Of Scope

- Host tools.
- Temporal workflow/activity code.
- Production dispatch/outbox enforcement.
- Provider-neutral message abstraction.
- CLI rendering.
- Automatic provider discovery from environment variables.

## Dependencies

- Depends on `forge-agent` and `forge-llm`.
- Consumed by local/process runners and future Temporal activities.
- Informs P60 outbox rules for non-idempotent or retry-sensitive provider calls.

## Done When

- `forge-agent-llm` builds as a workspace crate.
- At least one provider API kind can execute `LlmGenerate` from a committed
  `LlmRequest` and return a valid `AgentEffectReceipt`.
- Provider request materialization and receipt extraction are covered by
  deterministic tests.
- Raw/native provider outputs are retained through `BlobStore` where needed.
- `forge-agent` still does not depend on `forge-llm`.
- `cargo check -p forge-agent-llm` and `cargo test -p forge-agent-llm` pass.
