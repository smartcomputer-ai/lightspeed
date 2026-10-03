# P97: Direct Provider Model Discovery

**Status:** Implemented 2026-07-10.

Implemented: `models/list` is a direct, best-effort universe RPC over the
OpenAI Responses/Chat Completions and Anthropic Messages clients; provider
credentials reuse the existing API-key/OAuth resolver; Anthropic cursor
pagination remains internal; contracts and the TypeScript client are
regenerated. P128 Phase 2 also discovers every universe OpenAI-compatible
endpoint record independently. Anthropic's runtime reasoning vocabulary now
accepts the provider-reported `max` tier instead of the obsolete `ultra`
spelling.
The OpenAI discovery follow-up exposes the provider's creation timestamp and
enriches a deliberately small set of current GPT-5 families with reasoning
efforts from official OpenAI model documentation, because OpenAI's Models API
does not return capabilities.

## Goal

Expose a universe-scoped `models/list` RPC which asks Lightspeed's supported
model providers for the models available to the credentials in use, then
returns selectable Lightspeed model routes.

This is discovery, not a model registry. There is no database table, cache,
refresh RPC, user-managed catalog, or provider indirection layer. Every
`models/list` call makes the provider requests directly. The caller may call
it again whenever it needs a refresh.

Lightspeed still owns the request because it owns the provider clients and
resolves provider API keys/OAuth grants. No credential or provider response
body is returned to the caller.

## Boundary

The fixed, code-local discovery set is the entire routing policy:

| Provider id | Provider discovery request | Lightspeed API kind | Enabled in P97 |
| --- | --- | --- | --- |
| `openai` | `GET /v1/models` | `openai:responses` | Yes |
| `anthropic` | `GET /v1/models` | `anthropic:messages` | Yes |
| `openai` | `GET /v1/models` | `openai:completions` | Yes (P128 Phase 1) |
| Custom universe provider | Its endpoint's `GET /models` | Each explicitly admitted OpenAI API kind | Yes (P128 Phase 2) |

The set contains only API kinds which the runtime can execute. The runtime
registers `openai:responses`, `openai:completions`, and
`anthropic:messages`. Custom endpoints explicitly admit one or both OpenAI
API kinds; no compatibility is inferred from their model ids.

One provider result can expand into more than one route. Since P128, the
OpenAI entry expands the same provider-returned model id into both
`openai:responses` and `openai:completions` records. The tuple
`(providerId, apiKind, model)` is the record identity; identical `model`
strings under different API kinds are deliberately distinct choices.

P97 does **not** claim that a generic provider model-list response proves
every listed model supports every endpoint. The fixed expansion set is
Lightspeed's statement of which runtime route it is prepared to offer. We do
not infer per-model endpoint compatibility from model-id prefixes. A small
code-local reasoning-effort enrichment for documented built-in OpenAI model
families is selection metadata, not an endpoint compatibility catalog.

## RPC

```ts
// models/list { selectableOnly?: boolean }
{
  models: Array<{
    providerId: string,
    apiKind: "openai:responses" | "openai:completions" | "anthropic:messages",
    model: string,
    displayName: string,
    createdAtMs?: number,
    capabilities: {
      // Omitted means the provider did not report it and Lightspeed has no
      // documented built-in-family enrichment.
      reasoningEfforts?: string[],
      parallelToolUse?: boolean,
      maxOutputTokens?: number,
      maxInputTokens?: number
    },
    source: "provider",
    fetchedAtMs: number
  }>,
  providers: Array<{
    providerId: string,
    apiKinds: string[],
    fetchedAtMs?: number,
    error?: string
  }>
}
```

`selectableOnly` defaults to `false`. With `true`, Lightspeed removes OpenAI
model-id families that are clearly not general text-generation models
(embeddings, moderation, image/video, speech/transcription, realtime, search,
deep-research, and computer-use routes) and models older than the rolling
18-month normal-picker window. Older models remain available through the
unfiltered RPC and manual model entry. This does not assert that every
remaining model supports every Responses feature. Anthropic results are not
filtered: its Models API is already scoped to Anthropic models for Messages.

`models` contains every successful provider result. `providers` makes the
best-effort outcome explicit: a failed or unavailable provider does not hide
models returned by another provider, and does not turn a normal discovery
request into a total failure. Errors are sanitized Lightspeed messages; they
must not contain credentials, response bodies, or upstream request headers.

`source` is retained as the literal value `"provider"` for this API shape.
There is no `"catalog"` source in P97. `fetchedAtMs` is assigned after the
successful provider response is decoded; it is not a cache timestamp.

`displayName` is the provider's human-readable name when supplied. Otherwise
it is exactly `model`. Results are sorted by `providerId`, then `apiKind`,
then `displayName`/`model`, rather than exposing a provider-specific order.

## What the providers actually return

### OpenAI

OpenAI's `GET /v1/models` returns a list of model objects with only:

```json
{ "id": "…", "created": 0, "object": "model", "owned_by": "…" }
```

It provides an account-visible model id, owner, and creation time. It does
not provide a display name, input/output-token limits, reasoning-effort
levels, parallel-tool-use support, or a per-endpoint compatibility matrix.
P97 therefore maps `id` to both `model` and `displayName`, maps `created` to
`createdAtMs`, and otherwise leaves capabilities absent. Lightspeed enriches
only documented current GPT-5 families with reasoning-effort values from the
[official model catalog](https://developers.openai.com/api/docs/models); it
does not guess for unknown or compatible-provider model ids.

### Anthropic

Anthropic's paginated `GET /v1/models` (`after_id`, `before_id`, `limit`) is
explicitly a list of models available to the API credentials. Each
`ModelInfo` can contain `id`, `display_name`, `created_at`,
`max_input_tokens`, `max_tokens`, and nullable `capabilities`. The capability
object currently includes effort tiers, thinking modes, structured outputs,
image/PDF input, citations, code execution, batch support, and context
management. See [Anthropic's Models API reference](https://platform.claude.com/docs/en/api/models/list).

P97 follows every page (`limit=1000`) and normalizes only the facts it can
state precisely:

- `display_name` → `displayName` (fall back to `id` when absent);
- `max_tokens` → `maxOutputTokens` when present;
- `max_input_tokens` → `maxInputTokens` when present;
- supported `capabilities.effort.{low,medium,high,max,xhigh}` keys →
  `reasoningEfforts`.

Anthropic's model-list capability object does not report
`parallelToolUse`, so P97 leaves that field absent. P97 also leaves all
unrepresented native capability facts out of the normalized response rather
than introducing a generic capability blob.

The current gateway admits Anthropic reasoning effort values
`none`, `low`, `medium`, `high`, and `ultra`; the current Anthropic model-list
API reports `max` rather than `ultra`. P97 must reconcile that vocabulary in
the same implementation slice: return the provider's actual values and make
the Anthropic request-lowering/admission vocabulary agree. It must not relabel
`max` as `ultra` in discovery.

## Implementation

1. Add `ModelListParams`, `ModelListResponse`, `ModelView`,
   `ModelCapabilitiesView`, and `ModelProviderDiscoveryView` in `crates/api`;
   register `models/list` in the RPC manifest and add it to
   `AgentApiService`.
2. Add `list_models` to the existing OpenAI Responses client and Anthropic
   Messages client in `crates/llm-clients`. The methods use the existing base
   URL, auth override, timeout, error parsing, and provider-native headers.
   Anthropic implements cursor pagination; OpenAI needs a single request.
3. Extract or reuse the stored-provider credential resolver at the gateway
   boundary. It must resolve `model:<providerId>` credentials exactly as a
   generation request does, including OAuth token refresh; when no stored
   credential exists it preserves the existing client environment-key
   fallback.
4. Inject the existing deployment-shared OpenAI and Anthropic clients into
   `GatewayAgentApi` from `UniverseRuntime`, then implement a small
   `gateway/service/models_api.rs` that iterates the fixed set above and
   executes provider calls concurrently.
5. Map native results to wire views, record one timestamp per successful
   provider, sort deterministically, and return provider-local errors.
6. Add client tests for native JSON, pagination, auth override, and error
   redaction; gateway tests for partial success, route expansion, unknown
   capabilities, and no credentials in the serialized response. Regenerate
   `crates/api/contract/` with `cargo run -p api --bin export-schema`, then run
   `cargo test -p api`, `cargo test -p llm-clients`, and
   `cargo test -p temporal-runtime`.

## Explicit non-goals

- No model rows, migrations, provider registry documents, or refresh cache.
- No browser-to-provider requests or browser credential handling.
- No guessing OpenAI capabilities from names or a maintained model metadata
  table.
- No model-specific admission rule beyond the provider facts actually
  available through this endpoint. Provider request validation remains the
  execution boundary.
- No provider-managed cache or maintained per-model endpoint compatibility
  catalog; custom providers declare API kinds on their universe endpoint row.
