# P66: Minimal Web Access

**Status**
- In progress.
- First cut is deliberately two pieces:
  1. OpenAI Responses provider-native `web_search`.
  2. A Forge-owned, guarded, recorded `web_fetch` function tool.
- Start with provider-native OpenAI Responses web search and stop there before
  adding fetch. Provider-neutral search providers, arbitrary HTTP requests,
  browser automation, authenticated fetches, and MCP-based provider plugins are
  deferred.
- Builds on P49 tool packages, P50 provider-native request materialization, and
  P64's pattern for recording opaque provider-native output items.
- As of 2026-06-06, G1 is implemented at the standard tool/runtime layer:
  `tools::web::search` builds OpenAI Responses hosted `web_search` provider-native
  tools, request defaults can opt into `web_search_call.action.sources`, the
  OpenAI Responses runtime preserves `web_search_call` output items as
  provider-opaque context, and an ignored live test documents the OpenAI
  prerequisites.
- As of 2026-06-06, product-level session config can compose provider-rendered
  host filesystem tools and OpenAI Responses `web_search` into one selected
  tool profile. OpenAI Responses sessions default to cached hosted web search
  and editable host filesystem tools, with explicit API/CLI controls to disable
  web search or choose host tool mode `edit`, `readOnly`, or `none`.
- VFS mounts and sandbox backing are now independent of tool selection. Mount
  changes update VFS state only; configured tools remain a static session
  toolset and execution fails clearly if the required backing is absent.
- G2 `web_fetch` remains pending.

## Goal

Give the agent enough web access to answer current-information questions and
inspect cited pages without turning Forge into a generic HTTP client.

The minimum useful cut is:

- let OpenAI Responses models use their hosted `web_search` tool;
- preserve OpenAI web-search output items and sources for auditability;
- add a narrow `web_fetch(url)` tool later for fetching a specific page through
  Forge-owned network guardrails.

The engine remains deterministic. It plans tool/request data and records
results, but it does not perform network I/O, call providers, resolve DNS, or
parse fetched pages.

## Reference Points

Codex is the closest model for the first piece. Its normal path is a hosted
OpenAI Responses tool with `type: "web_search"`, cached/live mode controlled by
`external_web_access`, optional domain/location/context-size settings, and
`web_search_call` output preservation. Codex also has a separate standalone
search endpoint path, but that is not the right first cut for Forge.

OpenClaw is the better reference for the second piece. It splits provider
search from guarded page fetch, with provider plugins for search and a separate
`web_fetch` tool that owns URL validation, SSRF policy, redirects, byte limits,
content extraction, caching, and untrusted-content wrapping. Forge should copy
the split, not the full provider/plugin system yet.

OpenAI's current Responses docs recommend the GA `web_search` tool for new
Responses integrations. They also document:

- `web_search_call` output items;
- `filters.allowed_domains` and `filters.blocked_domains`;
- `search_context_size` values of `low`, `medium`, and `high`;
- `user_location` with `type: "approximate"`;
- `external_web_access: false` for cached/offline-index mode;
- `include: ["web_search_call.action.sources"]` for complete consulted URLs.

Reference:
https://developers.openai.com/api/docs/guides/tools-web-search

## Non-Goals

- Do not implement Tavily, Brave, Exa, SerpAPI, Perplexity-style APIs, or other
  provider search integrations in P66.
- Do not add an MCP search provider framework in P66.
- Do not expose arbitrary HTTP methods, request bodies, custom headers, cookies,
  credentials, or internal-network access.
- Do not implement browser automation or page interaction.
- Do not make OpenAI Chat Completions search models part of the first cut.
- Do not make `engine` perform provider calls, network I/O, DNS resolution, or
  page extraction.
- Do not parse web-search result content in reducers for branching decisions.

## Design Position

Provider-native search and Forge-managed fetch are different capabilities.

Provider-native search is a hosted model/provider tool. The model sees and
chooses the tool inside the OpenAI Responses request. Forge should pass through
the exact provider-native tool JSON and record the provider's output items.

Forge-managed fetch is a local/runtime side effect. It should be a normal
function tool with a client effect, dispatched through the existing tool
runtime path and recorded as a tool result. It is useful when the model already
has a URL and needs the page body.

MCP belongs later at provider boundaries, not in the first safety substrate. The
first guarded fetch implementation should be owned by Forge so network policy,
trace shape, result recording, and hosted safety behavior are consistent.

## G1: OpenAI Responses Native Web Search

Add an OpenAI Responses hosted web-search tool as a provider-native tool.

Use the existing `ToolKind::ProviderNative` path:

```rust
ToolKind::ProviderNative(ProviderNativeToolSpec {
    api_kind: ProviderApiKind::OpenAiResponses,
    native_tool_ref,
    execution: ProviderNativeToolExecution::ProviderHosted,
})
```

The `native_tool_ref` blob should contain the OpenAI tool JSON, for example:

```json
{
  "type": "web_search",
  "external_web_access": false,
  "search_context_size": "medium",
  "filters": {
    "allowed_domains": ["docs.rs"],
    "blocked_domains": ["reddit.com"]
  },
  "user_location": {
    "type": "approximate",
    "country": "US"
  }
}
```

Mode semantics:

| Forge mode | OpenAI lowering |
|---|---|
| `disabled` | omit the tool |
| `cached` | include `{ "type": "web_search", "external_web_access": false }` |
| `live` | include `{ "type": "web_search", "external_web_access": true }` |

Even though OpenAI defaults live access to true when the field is omitted, Forge
should send an explicit value when a mode is configured. That makes the planned
request self-explanatory in CAS and traces.

Recommended G1 configuration shape:

```rust
pub struct OpenAiResponsesWebSearchConfig {
    pub mode: WebSearchMode,
    pub search_context_size: Option<WebSearchContextSize>,
    pub allowed_domains: Vec<String>,
    pub blocked_domains: Vec<String>,
    pub user_location: Option<OpenAiApproximateUserLocation>,
    pub include_sources: bool,
}

pub enum WebSearchMode {
    Disabled,
    Cached,
    Live,
}

pub enum WebSearchContextSize {
    Low,
    Medium,
    High,
}
```

OpenAI Responses sessions default to `cached` at the Forge API/config layer,
matching Codex's conservative baseline. Explicit `tools.webSearch = false`
disables the provider-native search tool for a session.

### G1 Implementation Shape

Keep the provider-native tool builder outside `engine`.

Candidate crate shape:

```text
crates/tools/src/web/
  mod.rs
  search.rs
```

The builder should produce a `ToolSpecBundle` with:

- stable Forge tool key such as `web_search`;
- `ProviderNativeToolExecution::ProviderHosted`;
- `ToolParallelism::ParallelSafe`;
- `ToolTargetRequirement::None`;
- no invocation catalog binding, because OpenAI hosts execution.

The active tool profile then controls whether the hosted tool is visible on a
turn. This keeps search in the same model-facing tool surface as the rest of
Forge's tool planning and avoids adding a hidden provider-specific side channel.

OpenAI Responses request materialization already accepts `oai::Tool::Raw`
through provider-native tool specs. G1 should preserve that path and add only
the small builder/config needed to create the raw `web_search` tool.

When `include_sources` is true, the OpenAI Responses request should include
`web_search_call.action.sources` in the request `include` list. The current
default include list already preserves encrypted reasoning content; web-search
sources should be appended without dropping existing includes.

### G1 Output Recording

OpenAI Responses can return output items with:

```json
{
  "type": "web_search_call",
  "id": "...",
  "status": "...",
  "action": { "...": "..." }
}
```

The OpenAI runtime should stop ignoring this item type. Store the exact raw item
as a `ContextEntryKind::ProviderOpaque` entry:

```text
provider_kind = "openai.responses.web_search_call"
media_type = "application/json"
preview = "OpenAI Responses web search call"
provider_item_id = item.id
content_ref = raw output item JSON
```

Do not turn it into a Forge tool call and do not schedule a client effect. The
provider already executed it.

The runtime already stores the full raw OpenAI response separately. The
provider-opaque context item exists so continuation context, session views, and
API projections do not silently lose the fact that a hosted search happened.

### G1 Validation

Validate early and fail clearly:

- `web_search` provider-native specs are valid only for
  `ProviderApiKind::OpenAiResponses`.
- hosted web search should use `ProviderNativeToolExecution::ProviderHosted`;
  `ClientEffect` is not meaningful for this first cut.
- domain filters should be hostnames without scheme, matching OpenAI's API
  expectation.
- `search_context_size` must be `low`, `medium`, or `high`.
- `user_location.country`, when present, should be a two-letter country code.
- specific function-tool choices should not be generated for the hosted search
  tool in G1; use ordinary `tool_choice: "auto"` unless a later API-specific
  hosted-tool choice is added.

### G1 Tests

Add focused tests:

- tool builder emits the exact OpenAI `web_search` JSON for cached and live
  modes;
- active profile with `web_search` lowers to an OpenAI Responses request with
  a raw hosted tool;
- `include_sources` appends `web_search_call.action.sources` without removing
  the existing reasoning include;
- unsupported provider API rejects the provider-native search tool;
- OpenAI runtime captures `web_search_call` output as provider-opaque context;
- ignored live test verifies a search-capable OpenAI Responses model can call
  the hosted tool and returns either citations or a `web_search_call`.

## G2: Guarded Recorded Web Fetch

After G1 lands, add a narrow `web_fetch` function tool in the standard tools
crate.

The first fetch tool should do one thing:

```json
{
  "url": "https://example.com/page",
  "max_chars": 20000
}
```

First-cut behavior:

- `GET` only;
- `http` and `https` only;
- no custom headers, bodies, cookies, credentials, or browser/session state;
- redirect limit of 5, with policy checks on every hop;
- timeout and total byte cap;
- strict SSRF guard for localhost, loopback, link-local, private, multicast,
  and other non-public targets;
- content-type allowlist for text, HTML, JSON, and markdown-like responses;
- simple extraction to text or markdown;
- return final URL, status, content type, byte count, SHA-256, and extracted
  text;
- mark the result as untrusted web content in the model-visible output;
- record the exact tool output in the normal tool result path.

"Recorded" means replay does not refetch. The engine records the tool result it
received. Future deterministic replay uses the recorded content and metadata;
only a new tool invocation performs network I/O.

No cache is required in the first fetch implementation. A short runtime cache
can be added later if repeated fetches become noisy, but it should not replace
the durable recorded result.

Candidate crate shape:

```text
crates/tools/src/web/
  fetch.rs
  guard.rs
  extract.rs
```

Fetch execution belongs in runtime adapters, inline runners, or Temporal
activities. The deterministic core sees only `ToolInvoke` and
`ToolInvocationReceipt`.

## Deferred Work

- MCP/plugin search providers.
- Tavily, Brave, Exa, SerpAPI, Ollama, enterprise search, or customer crawler
  integrations.
- Arbitrary HTTP request tool.
- Authenticated fetches and site-specific connectors.
- Browser automation.
- Search result normalization across providers.
- Fetch caching and freshness policy.
- Rich citation projection from OpenAI message annotations.
- UI controls for always-search versus optional-search behavior.

## Acceptance Criteria

G1 is done when:

- a session/tool profile can expose OpenAI Responses hosted `web_search`;
- cached and live mode lower to explicit OpenAI tool JSON;
- OpenAI `web_search_call` output is preserved as provider-opaque context;
- complete source inclusion is available through request defaults/config;
- unsupported provider combinations fail before provider execution;
- unit tests cover request lowering and output capture;
- an ignored live test documents the required OpenAI model/API prerequisites.

G2 is done when:

- `web_fetch(url)` exists as a standard function tool;
- fetch has strict URL/network guardrails and bounded output;
- fetched content is marked untrusted;
- no arbitrary HTTP method/header/body surface exists;
- replay uses the recorded tool result and does not refetch;
- tests cover SSRF rejects, redirects, limits, extraction, and successful fetch.
