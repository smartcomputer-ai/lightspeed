# P64: Provider-Native Context Compaction

**Status**
- In progress.
- Depends on P50/P53 provider-native request materialization and P62 CAS-backed
  blob retention.
- Follows P63's context-entry direction: canonical Forge context such as
  instructions, skill catalog entries, and active skill activations lives as
  explicit context entries and must be preserved independently from compacted
  conversation history.
- As of 2026-06-05, the OpenAI Responses provider-triggered first cut is
  implemented through CoreAgent policy/config validation, request lowering,
  runtime capture of returned native compaction items, deterministic active
  context pruning, API config mapping, and ignored live contract tests.
- Also as of 2026-06-05, explicit OpenAI Responses provider-standalone
  compaction is implemented for `ProviderStandalone`: core can request
  compaction manually or from an idle high-watermark, runtime/worker/test
  runners call `/responses/compact`, gateway exposes `context/compact`, and
  API projection emits compaction requested/finished events plus debug
  provider context items.
- Remaining work is broader integration coverage, live verification against
  OpenAI, and the deferred Forge-managed fallback mode for providers without
  native compaction.

## Goal

Make compaction a first-class context-management capability while keeping Forge
provider-native and deterministic.

The default path should be provider API triggered compaction. For OpenAI
Responses, ordinary `/responses` calls should opt into server-side compaction
through provider-native `context_management` configuration. Forge should record
the opaque compaction item returned by the provider and use it to shrink the
next active context window.

Standalone provider-native compaction and Forge-managed summarization are
separate explicit policy modes, not the primary design and not implicit
runtime fallbacks from provider-triggered compaction.

Priority order:

1. Provider-triggered compaction during normal generation.
2. Explicit standalone provider-native compaction.
3. Forge-managed summarization or deterministic pruning for providers without
   native compaction support.

Policy selection should be pinned after configuration resolution. If a session
or run selects `ProviderTriggered`, Forge should either execute
provider-triggered compaction or reject clearly when the selected provider API
cannot support it. It should not silently fall back to standalone compaction,
Forge-managed summarization, or deterministic pruning. `None` in configuration
means "inherit/default", not "try multiple strategies".

## Context

OpenAI Responses now supports native compaction in two useful modes:

- Server-side compaction on normal `responses.create` requests via
  `context_management` entries with `type: "compaction"` and
  `compact_threshold`.
- Standalone compaction through `POST /responses/compact`.

The server-side mode is the important default for Forge. The model request
crosses a rendered-token threshold, OpenAI runs compaction inside the same
provider operation, and the response includes an encrypted opaque compaction
item. That item is provider-native state, not a human summary. For stateless
input-array chaining, clients can append the output items and drop earlier items
before the most recent compaction item. For `previous_response_id` chaining,
the provider owns more of the continuation state and clients should not
manually prune the provider-side chain.

Reference:
https://developers.openai.com/api/docs/guides/compaction

Forge already has several pieces this needs:

- `ContextEntryKind::ProviderOpaque`
- blob-backed raw/native context items
- context revision checks
- `ContextEvent::EntriesRemoved`
- `ContextEvent::StateReplaced`
- provider-native OpenAI Responses request materialization
- raw OpenAI output item retention for message, reasoning, and function-call
  items

The provider-triggered policy and lifecycle now exist for OpenAI Responses:
native compaction configuration is lowered into normal generation requests,
returned compaction output items become `ProviderOpaque` context entries, and
eligible superseded entries are pruned with a `ProviderCompacted` reason.
The provider-standalone policy now exists for OpenAI Responses as an explicit
manual/API or idle high-watermark path. Remaining work is to broaden
integration coverage, run live verification, and implement Forge-managed
fallback only when there is a concrete use case.

## Non-Goals

- Do not implement compaction by default as a text-summary turn.
- Do not ask the model to reinterpret provider-native opaque compaction items.
- Do not make `engine` call OpenAI, Anthropic, tokenizers, or any other
  provider service.
- Do not put provider-specific JSON parsing into reducer logic beyond compact
  metadata required for deterministic branching.
- Do not rely on compaction output to preserve canonical Forge context such as
  instructions, skill catalogs, or active skill bodies.
- Do not use compaction to rewrite the event log. Compaction changes active
  context state; the durable session log remains the audit history.
- Do not support cross-provider migration of native compaction artifacts in the
  first cut. Native compaction items are bound to their provider API family.

## Design Position

Compaction is a context-window operation, not a generic agent subroutine.

`engine` should own deterministic facts:

- the active compaction policy,
- whether a provider request is allowed or expected to trigger native
  compaction,
- which active context entries are eligible to prune after a recorded
  compaction item,
- which context entries must be retained because they are canonical Forge
  context or unconsumed inputs.

Runtime adapters should own side effects and provider-specific materialization:

- render OpenAI `context_management`,
- detect returned OpenAI compaction output items,
- store exact native item JSON in CAS,
- emit `ProviderOpaque` context inputs that preserve provider item identity,
- optionally call standalone `/responses/compact`.

The reducer should not inspect or summarize native compaction payloads. It
should only validate and arrange active context entries.

## Provider Modes

### Server-Side Provider Compaction

This is the normal mode for OpenAI Responses.

Forge sends a normal generation request with provider-native compaction
configuration:

```json
{
  "context_management": [
    {
      "type": "compaction",
      "compact_threshold": 120000
    }
  ]
}
```

OpenAI decides whether the rendered request crosses the threshold. If it does,
the provider runs compaction as part of the response operation and returns an
opaque compaction output item.

Forge then:

- stores the native output item JSON in CAS,
- records it as `ContextEntryKind::ProviderOpaque`,
- sets `provider_kind` to a stable value such as
  `openai.responses.compaction`,
- sets `provider_item_id` from the provider item id when present,
- keeps the raw response blob as usual,
- prunes eligible active context entries before the next request once the
  compaction item is committed.

This path should work for both non-streaming and streaming responses. The
runtime must treat compaction output items as durable context outputs, not as
transient stream metadata.

### Standalone Provider-Native Compaction

This is the controlled fallback for providers that expose a native compaction
endpoint but do not trigger compaction inside normal generation, or when Forge
wants to compact while idle.

For OpenAI Responses, this means `POST /responses/compact`.

The standalone path should still be provider-native:

- materialize the current provider-native input window,
- call the provider's compact endpoint,
- store returned opaque native items,
- replace or prune active context around the returned compaction item.

This path is useful for manual compaction commands, idle-time maintenance, or
recovering from context pressure before starting the next turn. It should not
be the default path for normal OpenAI Responses generations.

Forge implements this for OpenAI Responses behind the explicit
`ProviderStandalone` policy. Core records a pending compaction request with a
trigger (`manual` or `highWatermark`), emits substrate-neutral
`CompactContext`, and only runtime/worker adapters perform the provider call.
The result appends provider-opaque compaction context and the existing
deterministic provider-compaction prune step removes superseded active entries.

### Forge-Managed Fallback Compaction

This is the last resort.

For providers without native compaction, Forge may run a specialized summary
generation or deterministic pruning policy. The output should be recorded as
Forge semantic context, not provider-native opaque state.

This fallback must be explicitly marked as lower fidelity than native
compaction because it cannot preserve encrypted reasoning or other provider
continuation state.

## Core Model

Add a provider-aware compaction policy to session/run configuration. A possible
shape:

```rust
pub struct ContextConfig {
    pub compaction: Option<CompactionPolicy>,
}

pub enum CompactionPolicy {
    Disabled,
    ProviderTriggered {
        compact_threshold_tokens: Option<u32>,
    },
    ProviderStandalone {
        compact_threshold_tokens: Option<u32>,
        target_tokens: Option<u32>,
    },
    ForgeManaged {
        compact_threshold_tokens: Option<u32>,
        target_tokens: Option<u32>,
    },
}
```

The exact shape can differ, but it should keep the distinction between
provider-triggered, provider-standalone, and Forge-managed fallback explicit.
`compact_threshold_tokens` is intentionally optional: for `ProviderTriggered`,
`None` means use the provider's default server-side threshold and omit
`compact_threshold` from the provider request; for `ProviderStandalone`,
`None` means no idle high-watermark auto-compaction and manual/API compaction
can still be requested. `target_tokens` is a provider compact-endpoint target
hint for how small the compacted output should be.

OpenAI Responses request planning should lower `ProviderTriggered` into the
provider-native request record:

```rust
let mut compaction = json!({
    "type": "compaction"
});
if let Some(compact_threshold_tokens) = compact_threshold_tokens {
    compaction["compact_threshold"] = json!(compact_threshold_tokens);
}

OpenAiResponsesRequest {
    context_management: Some(json!([compaction])),
    ..
}
```

Provider-triggered compaction does not require a new CoreAgent action before
generation. It is part of the ordinary `GenerateLlm` action.

Standalone provider compaction does require a new substrate-neutral action if
Forge supports it as an idle/pre-turn operation:

```rust
CoreAgentAction::CompactContext {
    request: ContextCompactionRequest,
}
```

That action now exists for the explicit `ProviderStandalone` path. Server-side
OpenAI compaction still rides on ordinary `GenerateLlm` and does not use this
action.

## Context Entries

Native compaction items should be represented as provider-opaque active context:

```rust
ContextEntryInput {
    kind: ContextEntryKind::ProviderOpaque,
    content_ref: native_item_ref,
    media_type: Some("application/json".to_owned()),
    provider_kind: Some("openai.responses.compaction".to_owned()),
    provider_item_id: Some(provider_item_id),
    preview: Some("OpenAI Responses compaction item".to_owned()),
    token_estimate: None,
}
```

The OpenAI Responses adapter already passes JSON `ProviderOpaque` entries
through as raw input items. That is the right rendering behavior for compaction
items too.

Add stable provider-kind constants for:

- `openai.responses.compaction`
- future provider-native compaction item families
- Forge-managed summary entries if needed

Do not add a semantic `ContextEntryKind::CompactionSummary` for opaque native
items. A native compaction item is not a summary.

## Context Pruning

Recording a compaction item is not enough. Active context must shrink.

After a native compaction item is committed, Forge should prune eligible
entries that the provider compaction item supersedes. For OpenAI stateless
input-array chaining, this means the next provider request can keep:

- stable keyed instructions,
- the active skill catalog,
- active session-scoped skill activations,
- unconsumed active run input or steering entries,
- the latest native compaction item,
- entries after the latest native compaction item,
- any active provider-native item that is required by an in-flight tool or turn
  invariant.

Eligible entries before the latest compaction item can be removed from active
context:

- old user messages,
- old assistant messages,
- old reasoning state,
- old tool call items,
- old tool result items,
- older provider-native conversation items that are superseded by the latest
  compaction item.

Use context events to make this deterministic:

- `ContextEvent::EntriesRemoved` for simple pruning,
- or `ContextEvent::StateReplaced` when the full active window is easier to
  validate.

Add a more specific reason if useful:

```rust
pub enum ContextRemovalReason {
    Pruned,
    ProviderCompacted,
}

pub enum ContextRewriteReason {
    Pruned,
    PolicyChanged,
    ProviderCompacted,
}
```

The reducer must continue to reject removal of unconsumed run input and
steering entries.

### Next-Turn Input Shape

Provider-native compaction is not a replacement for current Forge canonical
context. A follow-up request after provider-triggered compaction should not
send "just the compaction item".

For OpenAI stateless input-array chaining, the next rendered request should
contain:

1. current canonical Forge context:
   - stable keyed instructions rendered through the top-level OpenAI
     `instructions` field,
   - current skill catalog entries rendered as developer messages,
   - active skill activation entries rendered as developer messages,
   - unconsumed run input or steering entries,
2. the latest `openai.responses.compaction` provider-opaque item, rendered
   back as the raw provider item,
3. any entries after that compaction item,
4. the new user input for the next turn.

Entries before the latest compaction item are only retained when they are
canonical Forge context or protected by a run/tool invariant. The compaction
item supersedes old provider conversation state; it does not supersede
current instructions, skill catalog, active skill bodies, environment/context
updates, or other pinned runtime context.

The useful Codex precedent is: process the compacted transcript, keep the
provider compaction item and selected real user messages, drop stale
developer/context messages and old assistant/tool/reasoning artifacts, then
reinject the current canonical context from live session state. Forge should
use the same principle, adapted to `ContextEntryKind`:

- keep `Instructions`, `SkillCatalog`, active `SkillActivation`, unconsumed
  run input/steering, the latest compaction `ProviderOpaque`, and entries after
  it;
- remove old `Message`, `ReasoningState`, `ToolCall`, `ToolResult`, and older
  provider-native conversation items before the latest compaction item when no
  invariant protects them;
- never trust opaque provider compaction output to preserve Forge-owned
  canonical context.

## OpenAI Responses Runtime Work

Update `llm-runtime` OpenAI Responses materialization:

- materialize `OpenAiResponsesRequest.context_management` as a native
  `context_management` request field instead of only stuffing it into `extra`
  if the client type supports a typed field,
- otherwise pass it through in `extra` with the exact provider field name,
- keep `store=false` compatibility where configured.

Update OpenAI response result parsing:

- detect output items whose provider `type` represents compaction,
- store the exact raw output item JSON in CAS,
- emit a `ProviderOpaque` context input with provider kind
  `openai.responses.compaction`,
- preserve item id/status metadata where available,
- do not treat the item as visible assistant output,
- do not ignore unknown output item types when they are provider state that
  must round-trip.

The current adapter intentionally records message, function-call, and reasoning
items. P64 should extend that to provider-native compaction items.

## Workflow And Worker Work

Server-side provider compaction needs minimal workflow changes because it rides
inside the existing `GenerateLlm` action:

```text
drive emits GenerateLlm
  -> worker calls OpenAI Responses with context_management
  -> runtime captures compaction item as ProviderOpaque context input
  -> drive commits context entries and turn completion
  -> context planner/pruner removes superseded active entries
```

The context pruning step should be deterministic and planned by core after the
generation result is committed. It should not require the worker to decide
which Forge context entries to remove.

Standalone compaction now uses a dedicated activity:

```text
drive emits CompactContext
  -> worker calls provider compact endpoint
  -> drive commits provider opaque compact item
  -> core prunes active context
```

The same action is handled by the in-process runner for tests/evals. Worker
errors remain activity failures so Temporal retry policy owns transient
provider/runtime failures instead of encoding retry suppression in context
state.

## API And Client Surface

Public API should expose compaction as context state/progress, not provider
implementation detail.

First cut:

- project `ProviderCompacted` context removals/replacements as ordinary context
  delta events,
- expose `context/compact` as an explicit "compact according to policy"
  command for manual/API use,
- project `ContextCompactionRequested` and `ContextCompactionFinished` so
  clients can show that compaction happened even when the opaque provider item
  itself is not human-readable,
- add a compact status item only if the UI needs a dedicated activity row,
- show token estimates before/after when available,
- expose active context entries enough for debugging provider-native compact
  items without dumping encrypted payloads.

Avoid a public "summarize context" API as the primary control. If a manual
command exists, it should mean "compact context according to policy", with the
provider-native path preferred.

## Tests

Unit tests:

- OpenAI request planning includes native `context_management` for
  provider-triggered compaction.
- OpenAI adapter materializes `context_management` exactly.
- OpenAI adapter captures a synthetic compaction output item as
  `ProviderOpaque`.
- OpenAI adapter replays a compaction `ProviderOpaque` item as a raw input
  item.
- Core prunes entries before the latest compaction item while preserving
  instructions, skill catalog, active skill activations, unconsumed run input,
  and entries after compaction.
- Core does not prune while a turn/tool invariant requires an entry.
- Core emits standalone compaction requests from manual command admission and
  idle high-watermark pressure.
- OpenAI runtime materializes `/responses/compact` requests and captures
  compaction output items as provider-opaque context.

Integration tests:

- In-process runner completes a long synthetic run where the fake LLM returns a
  compaction item and old context is pruned before the next turn.
- Gateway/session projection surfaces context replacement/removal events with
  the compaction reason.
- Gateway maps `ProviderStandalone` API config and routes `context/compact`.

Live tests:

- OpenAI Responses live test with low `compact_threshold` verifies that a
  provider compaction item is returned and retained.
- A follow-up turn verifies that earlier input items can be dropped while the
  model still retains the prior task state.
- Existing standalone `/responses/compact` live test remains as a fallback
  contract test, not the primary Forge behavior test.

## Milestones

### G1: Policy And Request Lowering

- [x] Add compaction policy to session/run config.
- [x] Lower OpenAI Responses provider-triggered policy into
  `context_management`.
- [x] Add unit tests for request planning and adapter materialization.

### G2: Capture Native Compaction Items

- [x] Extend OpenAI Responses result parsing to detect compaction output items.
- [x] Store exact native item JSON in CAS.
- [x] Emit `ProviderOpaque` context input with stable provider kind.
- [x] Add adapter tests.

### G3: Deterministic Active-Context Pruning

- [x] Add context prune/rewrite reason for provider compaction.
- [x] Plan pruning after a committed compaction item.
- [x] Preserve canonical keyed context and unconsumed run inputs.
- [ ] Add broader core tests for prune eligibility and active tool invariants.

### G4: Runtime And Projection Integration

- [x] Wire Temporal workflow, worker activity registration, and in-process
  runner handling for `CompactContext`.
- [ ] Add end-to-end workflow/runner tests that commit standalone compaction
  context entries and pruning events in the right order.
- [x] Project context prune/rewrite events clearly through `api-projection`.
- [x] Expose active context entries enough for debugging provider-native
  compaction items without dumping encrypted payloads.
- [x] Project compaction requested/finished events for UI/debug visibility.
- [ ] Add CLI/TUI display support only if current context events are too opaque.

### G5: Live OpenAI Contract

- [x] Add ignored OpenAI Responses live test for server-side compaction.
- [x] Keep standalone `/responses/compact` live test as a provider-client contract.
- [ ] Run the ignored OpenAI Responses compaction tests against a
  compaction-capable model.
- [ ] Verify ZDR-friendly configuration with `store=false` where relevant.

### G6: Standalone And Fallback Paths

- [x] Add `CompactContext` action for explicit `ProviderStandalone`
  compaction.
- [x] Trigger standalone compaction from manual command/API admission.
- [x] Trigger standalone compaction from idle context high-watermark pressure.
- [x] Wire OpenAI `/responses/compact` through runtime/workflow/worker and
  in-process runner.
- [ ] Add Forge-managed summary fallback for providers without native compaction.

## Open Questions

- What default `compact_threshold_tokens` should Forge use per model family?
- Should compaction policy live in `ContextConfig`, provider request defaults,
  or both?
- Should server-side native compaction pruning happen immediately after the
  turn or at the next turn planning boundary?
- How should provider-native compaction interact with `previous_response_id`
  chaining if Forge enables it later?
- Should active context retain a compact debug marker for removed ranges, or is
  the event log enough?
- Do we need a separate public compaction activity view, or are context rewrite
  events sufficient for clients?
