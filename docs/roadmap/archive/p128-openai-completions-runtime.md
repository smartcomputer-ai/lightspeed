# P128: OpenAI Chat Completions Runtime And Configurable Provider Endpoints

**Status**

- Content-descriptor follow-up validated 2026-09-05: Chat Completions answers,
  refusals, and compaction summaries retain their full text in CAS with bounded
  previews. Shared content projection, full API message text, and run outputs use
  that same payload;
  JSON-formatted answers remain authored text rather than provider-native JSON.
  Seventeen OpenAI/DeepSeek provider live cases and a hosted tool-loop live case
  verify this path, including the run-completion descriptor and content read API.
- Proposed 2026-08-18. Phase 1 and Phase 2 completed 2026-08-18.
- Phase 1 shipped the native client auth/model-list additions, the generation
  and standalone-compaction adapter, hosted/eval/CLI registration, reasoning
  admission, dual-kind OpenAI model discovery, and live coverage for client
  transport, text/media/documents, tools, compaction, prompts, VFS, and skills.
- The implementation follows the current OpenAI API vocabulary: runtime-owned
  instruction/catalog entries use `developer` messages, and reasoning accepts
  `max` in addition to the tiers listed in the original draft. Compaction
  summaries use a Lightspeed-recognized user message so they remain valid Chat
  Completions history rather than an opaque pseudo-item.
- Phase 1 compatibility hardening completed 2026-08-18 before Phase 2:
  provider dialect selection now controls instruction roles and output-token
  fields; DeepSeek V4 thinking controls and exact `reasoning_content` replay,
  OpenRouter `reasoning`/`reasoning_details` replay, provider failure finish
  reasons, cache accounting, annotations, and provider capability admission
  are covered by fixtures. OpenAI `gpt-5.5` and DeepSeek V4 live suites cover
  reasoning, cache accounting, native dialect requests, and exact reasoning
  replay through real tool loops. The OpenAI lives also confirmed that hosted
  web search remains Responses-only and that `stop` is unsupported for
  `gpt-5.5`, so both are rejected before Chat generation rather than
  advertised.
- Phase 2 shipped universe-scoped OpenAI-compatible endpoints for API-key,
  OAuth, and credentialless providers; request-time Responses/Completions
  routing; per-provider model discovery; secure URL/header validation; strict
  no-fallback semantics for custom provider ids; Platform management; a schema
  migration for the new credentialless provider kind; and local-stub plus
  DeepSeek live coverage.
- A discovery follow-up exposes provider model creation timestamps, enriches
  current OpenAI GPT-5 families with their documented reasoning-effort tiers,
  and keeps the normal selectable catalog focused on recent generation models.
- A Fast-mode follow-up adds `generation.processingTier` (`standard`, `fast`,
  or `flex`) as a session/profile default with an optional per-run API
  override, maps it to typed OpenAI `service_tier` requests for both Responses
  and Chat Completions, rejects it for compatible custom providers, exposes it
  in the shared Platform session/profile config editor for built-in OpenAI,
  and covers all four client/runtime routes with live OpenAI tests.
- Builds on [P50](archive/p50-agent-llm.md) (llm-runtime crate shape; listed
  `openai:completions` as step 3 and left `llm-runtime/src/openai_completions.rs`
  as a placeholder), [P69](archive/p69-generic-auth-token-broker.md) (stored
  `model:<provider_id>` credentials, `ProviderKeyResolver`),
  [P97](archive/p97-model-discovery.md) (`models/list`; explicitly parked the
  `openai:completions` route "until the runtime registers an adapter"),
  [P116](archive/p116-llm-provider-transient-retry.md) (typed transient
  provider errors), and P95 (params validated at admission).
- Greenfield: no compatibility shims. `ProviderApiKind::OpenAiCompletions`,
  `OpenAiCompletionsParams`, the canonical string `openai:completions`, and
  the `llm-clients` Chat Completions client already exist; this plan wires
  them into a real runtime path and then makes the endpoint a per-provider
  setting.

## Goal

Two phases, shipped in order:

1. **Phase 1 — full `openai:completions` runtime on OpenAI's endpoint.** A
   session, profile, or CLI draft can select
   `{ apiKind: "openai:completions", providerId: "openai", model }` and get
   the same product behaviour a Responses session gets today: text and image
   input, documents, instructions, tool calls with the standard built-in
   toolset, VFS/skill/environment prompt entries, standalone compaction,
   usage/finish facts, transient retry, stored-key/OAuth credentials,
   `models/list` discovery, and per-capability live suites. Everything that
   the Chat Completions API cannot express (remote MCP tools, provider-side
   web search, provider-triggered compaction) is rejected at admission with a
   `ProviderCompatibility` error, never silently dropped.

2. **Phase 2 — configurable endpoint URL per model provider.** A model
   provider id other than the built-in `openai`/`anthropic` (for example
   `openrouter`, `vllm-lab`, `ollama-local`) resolves to its own base URL,
   optional extra headers, and credential at send time, so
   `openai:completions` (and, where the target speaks it, `openai:responses`)
   runs against any OpenAI-compatible server. The URL is universe-scoped
   runtime configuration, exactly like the credential it travels with; it
   never enters `ModelSelection`, `ProviderParams`, or the session log.

## Pre-implementation baseline

- `crates/llm-clients/src/openai/completions.rs` (703 lines) is a complete
  native Chat Completions client: `Config { api_key, base_url, organization,
  project, http }`, `Client::create` (non-streaming), `Client::stream`
  (SSE `CompletionStream`), typed `CreateCompletionRequest`,
  `CompletionMessage` (`content` as text or parts, `tool_calls`,
  `tool_call_id`), `CompletionTool`/`CompletionToolChoice`, `Completion`
  with `choices[].message`, `finish_reason`, `usage` (with
  `reasoning_tokens` / `cached_tokens` accessors), plus provider-error
  classification shared with the Responses client. It has unit tests and a
  live suite (`tests/openai_completions_live.rs`: text, stream, forced
  function call, invalid-model error). What it lacks relative to the
  Responses client: `create_with_auth` (per-request `RequestAuth` override),
  `from_env` / `from_env_allow_missing_key`, `list_models_with_auth`, and a
  request-time base-URL override.
- `crates/llm-runtime/src/openai_completions.rs` is a five-line placeholder.
  `params.rs` already defines `OpenAiCompletionsParams { response_format,
  temperature, top_p, stop, parallel_tool_calls, store, stream, metadata,
  extra }` and `validate_provider_params` accepts it, so a completions params
  body already passes admission — and then fails at generation with
  `no LLM generation adapter registered for OpenAiCompletions`.
- `harness` already treats `OpenAiCompletions` as a first-class
  `ProviderApiKind`: sessions pin it, `remote_mcp_supported_by_provider`
  returns `false` for it, `validate_context_config` rejects
  `ProviderTriggered` and `ProviderStandalone` compaction for it, and
  `tools/toolset.rs` maps it to the `CodexLike` built-in surface next to
  Responses. `api-projection` maps `"openai:completions"` both ways.
- `temporal-runtime/gateway/service/api_config.rs::validate_reasoning_effort`
  rejects any `reasoningEffort` for `openai:completions`; `models_api.rs`
  emits OpenAI models only as `openai:responses`; `worker/activities/state.rs
  ::llm_runtime_with_clients` registers Responses and Anthropic adapters
  only. `eval` and `test-support` mirror that registration.
- Transport configuration is deployment-wide and per API family:
  `oai::Config::from_env_allow_missing_key` reads `OPENAI_API_KEY`,
  `OPENAI_BASE_URL`, `OPENAI_ORG_ID`, `OPENAI_PROJECT_ID`; one `oai::Client`
  and one `am::Client` are built at worker start. Credentials can be
  overridden per `provider_id` at send time through `ProviderKeyResolver`
  (`model:<provider_id>` auth provider rows, API key or OAuth grant), but
  the URL cannot: every `provider_id` shares the one client's URL. Nothing
  today lets two provider ids of the same API kind reach two hosts.
- The Chat Completions wire shape differs from Responses in ways that
  matter for context materialization: instructions are a `system`/
  `developer` message rather than a top-level field; a model turn is one
  `assistant` message carrying `content` *and* `tool_calls[]` (Responses
  emits separate `message` and `function_call` items); tool results are
  `role: "tool"` messages keyed by `tool_call_id`; there are no reasoning
  items in OpenAI output, no server-side compaction, no
  `previous_response_id`, no current hosted tools; reasoning is requested
  with `reasoning_effort` on supported models. Compatible providers add
  assistant reasoning fields that must be replayed exactly. Images and PDFs
  are `image_url` / `file` content parts on user messages when the selected
  provider supports them.

## Design decisions

- **D1 — One adapter, one api kind, no shared "OpenAI message model".**
  `OpenAiCompletionsLlmAdapter` in `llm-runtime/src/openai_completions.rs`
  materializes `oai_c::CreateCompletionRequest` directly from `LlmRequest`
  and maps `Completion` back to `LlmGenerationResult`. It does not go through
  the Responses adapter or a neutral intermediate; per the architecture
  rules, provider structures stay native. Shared helpers (`blob_io`,
  `environment_prompts`, `skill_prompts`, `provider_keys`, `secrets`,
  `put_json`/`read_json`) are reused as-is.
- **D2 — Context entries are provider-native and round-trip byte-for-byte.**
  Assistant output is stored as:
  - one `Message { role: Assistant }` entry when `content` is non-empty
    (`application/json`, bounded preview), `content.provider_kind =
    "openai.completions.message"`; the payload preserves `role`, `content`,
    `refusal`, and `annotations` together and projects their visible text;
  - one `ToolCall { call_id, name }` entry **per** `tool_calls[]` element,
    `media_type = application/json`, body = the raw tool-call object,
    `content.provider_kind = "openai.completions.tool_call"`; native IDs stay in the payload; the `LlmToolCall` fact carries `arguments` parsed from
    `function.arguments` (invalid JSON → `arguments` kept as the raw string
    inside `{"__raw": …}` and the call still dispatches; the tool layer
    already reports schema errors back to the model).
  On materialization, a run of consecutive `[Message{Assistant}?, ToolCall*]`
  entries from the same turn folds back into **one** assistant message
  `{content, tool_calls}` — the same folding rule the Responses adapter
  applies to consecutive user parts, applied to assistant turns. A `ToolCall`
  entry that is not immediately preceded by that turn's assistant message
  still yields a valid assistant message with `content: null`, except
  DeepSeek thinking-mode tool messages use non-null empty content as required
  by that dialect.
  `ToolResult` entries become `role: "tool"` messages with `tool_call_id`.
  Compatible reasoning extensions are stored independently as
  `ReasoningState` with provider kind
  `openai.completions.reasoning_state`: the raw assistant fields
  `reasoning_content` (DeepSeek), `reasoning` and `reasoning_details`
  (OpenRouter) are merged into that same assistant message before its tool
  calls on replay. This is mandatory for DeepSeek thinking-mode tool loops
  and preserves OpenRouter signatures/details without interpreting them.
  Output `annotations` stay with their assistant content, project as citations,
  and are not replayed as assistant input.
  A `ProviderOpaque` / `ReasoningState` entry with `provider_kind` starting
  `openai.completions.` is passed through raw; any other provider-native
  entry (Responses items, Anthropic blocks) is a `RequestKindMismatch`
  error — sessions are pinned to one api kind, so this cannot happen without
  a bug.
- **D3 — Roles are dialect-specific.** `Instructions`, `VfsCatalog`,
  `SkillCatalog`, and `SkillActivation` entries materialize as `developer`
  messages for OpenAI/OpenRouter and `system` messages for DeepSeek and the
  conservative generic compatibility dialect. This is keyed by
  `ModelSelection.provider_id`, independent of endpoint routing.
- **D4 — Media.** Image entries → `{ "type": "image_url", "image_url": {
  "url": "data:<mime>;base64,…" } }` user parts; PDF documents → `{ "type":
  "file", "file": { "filename", "file_data": "data:application/pdf;base64,…"
  } }`; text documents inline with the same `[document: name]` header the
  Responses adapter uses. Consecutive user entries fold into one multi-part
  user message.
- **D5 — Params and neutral fields.** `LlmRequest.output_limit` →
  `max_completion_tokens` for OpenAI/OpenRouter and `max_tokens` for
  DeepSeek/generic compatible providers (including standalone compaction);
  `tool_choice` → `"none" | "auto" | "required" |
  {type:function,function:{name}}`; `parallel_tool_use` →
  `parallel_tool_calls` (params value wins only when the neutral field is
  unset, mirroring Responses); `reasoning_effort` → request
  `reasoning_effort` (**lift** the admission rejection: accept the OpenAI
  vocabulary `none|minimal|low|medium|high|xhigh|max`; the adapter re-validates
  and forwards supported values). DeepSeek additionally receives
  `thinking: {type: enabled|disabled}` derived from the neutral reasoning
  intent; `none` disables thinking and omits `reasoning_effort`. OpenAI
  `gpt-5.5` rejects `minimal`/`max`, while `stop` is rejected for current
  OpenAI reasoning model families before provider I/O. DeepSeek V4 thinking
  mode rejects the `tool_choice` field: neutral `Auto` is represented by
  omission, while choices that cannot be represented that way fail admission;
  explicit choices remain available with thinking disabled. `stream` in
  params is ignored: generation is always non-streaming (`stream=false`),
  matching Responses/Anthropic; the client's stream path stays for tests
  and future UI streaming. `store` and `metadata` forward as-is;
  `response_format`, `temperature`, `top_p`, supported `stop`, and `extra`
  forward as-is.
- **D6 — Facts.** `provider_response_id = completion.id`; finish:
  `tool_calls` → `ToolCalls`, `stop` → `Stop`, `length` → `Length`,
  `content_filter` → `ContentFilter` (all existing `LlmFinish` variants),
  compatible failure reasons `insufficient_system_resource` (DeepSeek) and
  `error` (OpenRouter) become typed retryable provider errors rather than
  successful unknown finishes; anything else → `Unknown`, missing →
  `ToolCalls` when tool calls exist
  else `Unknown` (same rule as the Anthropic adapter); usage: `prompt_tokens`,
  `completion_tokens`, `total_tokens`, cached/cache-miss tokens, and
  `reasoning_tokens` into `LlmUsage`; provider-specific raw usage (including
  OpenRouter cost) remains available on the retained raw response;
  `context_token_estimate = prompt_tokens`
  (`ProviderCounted`). Only `choices[0]` is read; `n` is never sent. A
  `refusal` on the message becomes a `Message{Assistant}` entry with the
  refusal text and finish `Stop` (it is model output, not a failure).
- **D7 — Compaction.** Enable `ProviderStandalone` for `OpenAiCompletions`
  in `engine::validate_context_config` and implement `LlmCompactionAdapter`
  as summarization-over-context, copying the Anthropic approach
  (`COMPACTION_INSTRUCTION`, target-token budget → the dialect's output-token
  field,
  summary stored as a recognized user `Message` entry so it can be replayed
  directly as valid Chat Completions history). `ProviderTriggered` stays
  rejected. DeepSeek compaction explicitly disables thinking because the
  result is a summary rather than an agent reasoning turn.
- **D8 — Unsupported capabilities fail at admission.** Remote MCP tools
  (already false in `remote_mcp_supported_by_provider`), `features.web.search`
  (already Responses-only in `gateway/service/mod.rs`; make the silent skip
  an admission error for completions), and hosted tools stay off. No
  emulation.
- **D9 — Credentials.** Add `Client::create_with_auth` and
  `list_models_with_auth` to the completions client (`RequestAuth::ApiKey`
  and `Bearer` both become `Authorization: Bearer`, as in Responses).
  Add `Config::from_env` / `from_env_allow_missing_key` reading
  `OPENAI_API_KEY`, `OPENAI_BASE_URL`, `OPENAI_ORG_ID`, `OPENAI_PROJECT_ID`
  — the completions path shares the OpenAI deployment variables in phase 1;
  `OPENAI_COMPLETIONS_*` remain test-only overrides.
- **D10 — Phase 2 places the endpoint on the model-provider record, not in
  env and not in the session.** The `model:<provider_id>` auth-provider row
  already is Lightspeed's universe-scoped "model provider" record and is
  already resolved immediately before provider I/O. `ModelApiKeyConfig`
  and `ModelOAuthConfig` gain an optional `endpoint`:

  ```rust
  pub struct ModelEndpointConfig {
      /// OpenAI-compatible base URL, e.g. https://openrouter.ai/api/v1
      pub base_url: String,
      /// Non-secret extra request headers (e.g. HTTP-Referer, X-Title).
      #[serde(default)] pub headers: BTreeMap<String, String>,
      /// Explicit API kinds this endpoint may serve. Empty is invalid.
      pub api_kinds: Vec<String>,
  }
  ```

  `ModelEndpointConfig` is also used by a credentialless `ModelEndpoint`
  provider-row variant for local Ollama/vLLM servers. Rows without `endpoint`
  keep today's meaning only for a built-in provider. A row with `endpoint`
  for a *new* provider id is a complete provider definition; a custom row
  without one fails before network I/O. Built-in `openai` keeps its
  env-configured URL unless the row overrides it; Anthropic endpoint overrides
  remain deferred. Public endpoints require HTTPS; HTTP is accepted only for
  loopback hosts. URLs may not contain credentials, query strings, or
  fragments, and transport-owned headers such as Authorization, Host,
  Content-Type, and Cookie are rejected. Rationale:
  universe operators, not deployment operators, add OpenRouter/vLLM/Ollama
  targets; the credential and the URL belong together; there is no second
  registry to keep in sync; and `provider_id` continues to be the only
  routing key the session log knows. Secret headers (a non-standard auth
  header) are out of scope — the credential is the row's secret and goes
  out as `Authorization: Bearer`.
- **D11 — Runtime resolves `(auth, endpoint)` in one call.**
  `ProviderKeyResolver::resolve_provider_key` is generalized to
  `ModelProviderResolver::resolve_model_provider(provider_id) ->
  Option<ResolvedModelProvider { auth: Option<ResolvedProviderAuth>,
  endpoint: Option<ResolvedEndpoint> }>`; `StoredProviderKeyResolver`
  becomes `StoredModelProviderResolver`. Adapters pass the resolved
  endpoint to the client per request (`create_with_transport(request,
  auth, Option<&EndpointOverride>)`): the client keeps its `HttpClient`
  and default URL, and builds the request URL from the override when
  present. No per-provider client cache is needed; connection pooling is
  per `reqwest::Client`, which is shared. Endpoint values are redacted
  from the stored `provider_request_ref` blob only insofar as they never
  appear in it (the URL is transport, not body); headers from the row are
  non-secret by definition.
- **D12 — `models/list` follows the same table.** Phase 1 expands each
  OpenAI-returned model into both `openai:responses` and
  `openai:completions` records (as P97 anticipated). Phase 2 additionally
  lists every universe model-provider row that carries an `endpoint`,
  calling `GET {base_url}/models` with the row's credential and emitting
  records for the row's `api_kinds`; failures are per-provider discovery
  results, not errors.
- **D13 — No new session/profile vocabulary.** `ModelSelection { api_kind,
  provider_id, model }` is sufficient in both phases. Nothing about
  endpoints is added to `SessionConfig`, profiles, or the CLI beyond
  accepting a free-form `providerId`.

## Wire changes

Phase 1:

- `api`: none to the session/profile schema. `models/list` output gains
  `openai:completions` records; `ModelProviderDiscoveryView.apiKinds` for
  `openai` lists both kinds. Regenerate `crates/api/contract/` and the
  TypeScript client (`npm run check`).

Phase 2:

- `api::auth`: `AuthProviderConfig::ModelApiKey {}` / `ModelOAuth {…}` gain
  `endpoint?: { baseUrl, headers?, apiKinds }` (camelCase on the wire), and
  `ModelEndpoint { endpoint }` represents a credentialless target.
  `auth/providers/create` validates `baseUrl` using HTTPS except loopback
  HTTP, header names/values and bounded sizes, and `apiKinds` as a non-empty
  subset of
  `{openai:responses, openai:completions}` (Anthropic-compatible endpoints
  are a follow-up: allow `anthropic:messages` only once the Anthropic
  client gets the same override).
- `store-pg`: the endpoint fields remain additive JSON; the pre-release
  `004_auth` baseline also admits the `model_endpoint` provider kind.
- `models/list`: `ModelProviderDiscoveryView.providerId` may now be any
  universe provider id; `source` says `universe`.

## Runtime changes

Phase 1 (in dependency order):

1. `llm-clients/openai/completions.rs`: `Config::from_env*`,
   `Client::create_with_auth`, `list_models_with_auth`, `auth_header`
   (copy of the Responses pattern), constants `OPENAI_COMPLETIONS_API_KIND`
   re-export; unit tests for header/auth selection. Keep `create` as the
   `None`-auth wrapper.
2. `harness`: allow `ProviderStandalone` compaction for `OpenAiCompletions`
   in `validate_context_config`; unit tests.
3. `llm-runtime/src/openai_completions.rs`: `OpenAiCompletionsApi` trait
   (`create`, with `auth`), impl for `oai_c::Client`,
   `OpenAiCompletionsLlmAdapter { client, blobs, provider_keys }` with
   `with_provider_key_resolver`, `materialize_create_request`,
   `materialize_compact_request`, `result_from_response`,
   `result_from_compact_response`, `openai_completions_params(...)` in
   `params.rs`, `LlmGenerationAdapter` + `LlmCompactionAdapter` impls.
   Provider kind constants `openai.completions.message` /
   `openai.completions.tool_call`. Fake-API unit tests asserting exact
   request JSON for: instructions + user text; multi-part user (image +
   caption); PDF; assistant+tool_calls fold; tool result; skill/VFS system
   messages; tool_choice variants; params passthrough; refusal; usage and
   finish mapping; kind mismatch errors.
4. Registration: `temporal-runtime/worker/activities/state.rs`
   (`default_llm_runtime` builds a third client from
   `oai_c::Config::from_env_allow_missing_key`; `llm_runtime_with_clients`
   registers generation + compaction for `OpenAiCompletions`), `eval/main.rs`,
   `test-support` where it builds registries, `docs/variables.md` note that
   `OPENAI_*` variables now serve both OpenAI api kinds.
5. Admission: `gateway/service/api_config.rs::validate_reasoning_effort`
   accepts the OpenAI vocabulary for completions; `gateway/service/mod.rs`
   turns the `features.web.search` skip into `invalid_request` for
   completions; `models_api.rs` emits both OpenAI kinds and reports
   `apiKinds: [responses, completions]` in `provider_success/failure`;
   remove P97's "not registered" filter. Gateway unit tests.
6. CLI: `LIGHTSPEED_CHAT_API_KIND=openai:completions` already parses via
   `api-projection`; verify the CLI draft round-trips and add a test.
7. Live suites under `crates/llm-runtime/tests/`:
   `openai_completions_live.rs` (text, image, PDF, tool round-trip,
   parallel tool calls, refusal/finish, invalid model → typed transient vs
   terminal), `openai_completions_compaction_live.rs`,
   `openai_completions_prompts_live.rs`, `openai_completions_skills_live.rs`
   — mirror the Responses files; `support/mod.rs` gains a completions
   model/client helper honouring `OPENAI_COMPLETIONS_MODEL/API_KEY/BASE_URL`.
   `temporal-runtime/tests/sessions_live.rs` gets one end-to-end managed
   session on `openai:completions` (tool call + reply). All `#[ignore]`,
   fail loudly without keys.

Phase 2:

1. `auth`: `ModelEndpointConfig`, validation, tests; `api::auth` DTOs and
   projection; contract regen; Platform `SecretsPage`/Integrations
   "Model provider" form gains base URL / headers / api kinds fields
   (`platform/web/src/pages/SecretsPage.tsx`, `use-integrations.ts`).
2. `llm-runtime/provider_keys.rs` (kept to avoid a noisy module rename):
   `ModelProviderResolver`, `ResolvedModelProvider`, `ResolvedEndpoint`,
   `Static*` test resolver; keep `ProviderKeyResolver` name only if the
   diff is otherwise unwieldy — prefer the rename (greenfield).
   `temporal-runtime/worker/secrets.rs::StoredProviderKeyResolver` reads
   the row's `endpoint` and returns it alongside auth; validates the
   request's `api_kind` against `endpoint.api_kinds` (mismatch →
   `ProviderKeyError::NotUsable`).
3. `llm-clients`: `EndpointOverride { base_url: Url, headers: HeaderMap }`
   in `transport`; `oai_c::Client::create_with_transport` and
   `oai::Client::create_with_transport` (Responses; `compact` too) resolve
   the URL via `join_url(&override.base_url, "chat/completions" |
   "responses")` and merge headers per request; `list_models_with_transport`
   for discovery. Anthropic left unchanged (documented).
4. Adapters: pass `resolved.endpoint` through on every provider call
   (generation, compaction). Unit tests with a fake API asserting the
   override reaches the client and default is used when absent.
5. `models_api.rs`: enumerate universe model-provider rows with endpoints
   (`AuthProviderStore::list` filtered by kind + `endpoint.is_some()`), one
   discovery task per row bounded by the existing timeout, records for
   each declared api kind. Tests with the fake store.
6. `docs/variables.md`: `OPENAI_BASE_URL` documented as the built-in
   `openai` provider default only. `docs/design.md`/README: one paragraph on model
   providers and endpoints. Mark P97 table row as done.
7. Live and transport coverage: the client and runtime each have an ignored
   DeepSeek test whose shared OpenAI client deliberately has neither a key nor
   a DeepSeek URL, proving request-time resolution drove both destination and
   authentication. A small Tokio TCP stub covers exact Completions, Responses,
   anonymous-auth, custom-header, model-discovery, and header-isolation paths
   without adding a mock dependency.

## Verification

Phase 1 done when:

- `cargo test -p llm-clients -p llm-runtime -p harness -p temporal-runtime
  -p api -p eval` green; contract artifacts regenerated; `npm run check`
  green.
- `cargo test -p llm-runtime --test openai_completions_live -- --ignored`
  (+ compaction/prompts/skills) pass against OpenAI with `OPENAI_API_KEY`.
- `source scripts/dev/env.sh && cargo test -p temporal-runtime --test
  temporal_live openai_completions -- --ignored --test-threads=1` passes.
- `models/list` returns both OpenAI kinds; a Platform session created with
  `apiKind: openai:completions` runs a tool round-trip end to end.
- Selecting `features.mcp` servers or `features.web.search` on a
  completions session is rejected at `session/config/put` / profile put
  with `ProviderCompatibility` / `invalid_request`.

Phase 2 done when:

- A universe with a `model:openrouter` row (`endpoint.baseUrl =
  https://openrouter.ai/api/v1`, key stored) can start a session with
  `providerId: openrouter, apiKind: openai:completions` and complete a tool
  round-trip; `models/list` shows its models under `providerId: openrouter`.
- The same with a credentialless loopback vLLM/Ollama URL over plain HTTP.
- Removing the row makes the next generation fail with a typed
  `ProviderKeyResolution` error, not a fall-through to `api.openai.com`.
- `openai` sessions without a row still use `OPENAI_BASE_URL` (regression
  test in `temporal-runtime` unit tests with a fake resolver).

## Resolved questions

- Insecure HTTP is accepted only for `localhost` or loopback IP addresses;
  RFC1918 and public hosts require HTTPS.
- Endpoint headers are non-secret and transport-owned headers are reserved.
  Azure-style secret headers and query-versioned endpoints remain deferred.
- Streaming for UI latency is not part of this plan; the engine's per-turn
  result model is non-streaming for every provider today.

## Deferred

- Anthropic-compatible custom endpoints (`anthropic:messages` on a
  non-Anthropic host) — needs the same `create_with_transport` in the
  Anthropic client; trivial once phase 2 lands but not required.
- Provider-side prompt caching controls (`prompt_cache_key`) and
  `logprobs`; both are `extra` passthrough today.
- Legacy `/v1/completions` (non-chat) — out of scope; `openai:completions`
  means Chat Completions everywhere in Lightspeed.
