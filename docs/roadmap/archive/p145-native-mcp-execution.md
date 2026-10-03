# P145 — Native MCP Execution

**Status**

- Implemented 2026-08-31. Lightspeed becomes an MCP client at model time: for
  servers configured with native execution, the runtime presents the server's
  tools to the model — injected as ordinary function tools, or discoverable
  through search meta-tools for large inventories — and executes the model's
  calls over `tools/call`.
- Two record-owned axes: `execution: provider | native` and, for native
  servers, `exposure: inject | search`. Search exposure is v1 scope, not a
  follow-up: many real MCP servers are far too large to inject (Lightspeed's
  own Configurator MCP advertises the better part of a hundred generated
  tools), and Configurator in search mode is the flagship live acceptance.
- Builds on P143 (bounded `rmcp` Streamable HTTP client, SSRF policy, limits,
  server-owned policy), [P143b](p143b-rmcp-oauth.md) (`rmcp` OAuth behind the
  durable Lightspeed broker), P110 (universe-owned auth, broker resolution
  immediately before I/O), P114 (per-call tool activities and execution
  classes), P138 (model-facing names, never digests), P137 (prompt-cache
  discipline), and [P144](p144-mcp-approvals.md) (the approval gate).
- Replaces the roadmap idea "Support MCP tunnels to model providers": when
  Lightspeed is the client, private servers never need to be reachable by a
  model provider at all.
- Provider-hosted execution (P67) remains supported and remains the default;
  execution mode is authored per MCP server record.
- Delivered end to end: revision-keyed worker-local inventory caching,
  injection in all three adapters, search meta-tools, `rmcp` `tools/call`,
  dispatch-time P144 approvals, CAS-backed rich-result placeholders, the
  deployment plus record private-egress gate, Platform/CLI/generated clients,
  an ignored Temporal live acceptance against the Configurator MCP, and a
  standalone `temporal-runtime/tests/mcp_live.rs` matrix over local Stateless
  Streamable HTTP servers.

## Why

Provider-hosted MCP delegates listing and execution to OpenAI or Anthropic.
That is the right default for public servers on providers that support it,
but it cannot cover:

1. **Private and internal servers.** Company-internal MCP servers and client
   servers that must not be publicly reachable cannot be contacted by a model
   provider. Only Lightspeed's own runtime can sit on the right network.
2. **Providers without MCP.** `openai:completions` — the API kind behind
   OpenAI-compatible endpoints such as Deepseek and local servers (P128) —
   has no MCP concept, and `remote_mcp_supported_by_provider` rejects MCP
   toolsets on it today. Native execution makes MCP available to every model
   Lightspeed can drive.
3. **Provider asymmetry.** Anthropic's connector has no approval flow and
   drops `server_description`; P146 added its provider-specific deferred tool
   search. Diagnostics for provider-side MCP failures remain opaque (the P143
   problem, at model time). Native execution behaves identically on every API
   kind.
4. **Data path.** In provider mode the model provider holds the MCP bearer
   token and the tool traffic. Native mode keeps both between Lightspeed and
   the MCP server.

## Decision

Add two modes to the MCP server record, owned by the record like every other
MCP policy (P143):

```text
McpServerRecord.execution: provider | native     # default: provider
McpServerRecord.exposure:  inject | search       # default: inject; native only
```

Sessions and profiles keep selecting only `serverId`. The same endpoint may
be registered more than once with different modes (P110/P143 precedent for
split policies) — which is also the recommended hybrid for a big server with
a known hot path: a Selected-allowlist `inject` record for the hot tools
beside a `search` record for the long tail, sharing the URL and a compatible
credential. No dedicated hybrid mode is added.

**Native execution means:**

- the model sees the server's tools either **injected** — at LLM request
  materialization the adapter resolves the current inventory live (through a
  bounded, cached runtime client) and lowers each allowlisted tool into an
  ordinary provider **function tool** with a namespaced name — or through
  **search** — two session-global meta-tools (`mcp_find_tools`, `mcp_call`)
  with the record's authored description as the index hook, and per-tool
  schemas entering context only on demand;
- the model's calls come back as ordinary function calls, the engine plans an
  ordinary tool batch, and a per-call activity executes `tools/call` against
  the MCP server with the broker-resolved credential;
- results enter the session log as ordinary tool results.

No provider MCP entry (`type: "mcp"` / `mcp_servers`) is sent for a native
server. P145 itself does not touch provider-mode lowering; large public
servers there keep OpenAI `defer_loading`, and
[P146](p146-anthropic-tool-search.md) improves that path separately.

Exposure is authored, never inferred. The runtime must not auto-switch on the
live tool count: the count can change between turns, and flipping a session's
tool surface mid-conversation is hostile to both the provider prefix cache
and the model's coherence. Crossing the injection cap is a typed failure
naming both fixes — author a Selected allowlist, or switch the record to
`search`.

## Where the Inventory Lives (the core design choice)

Three placements were considered for the dynamic tool list; the first is
rejected, and the other two ship as the two exposures:

**A — Materialize discovered tools into the engine toolset.** Discovery at
config admission (and on every refresh) would write one function `ToolSpec`
per remote tool via `PatchTools`. Rejected:

- MCP inventories are dynamic by design (`tools/list` may change per call,
  servers add tools at runtime); syncing them means nondeterministic network
  I/O feeding event-sourced state, plus a refresh policy nobody can get right.
- Every drift churns the session log and bumps the toolset revision, killing
  the provider prefix cache and bloating replay.
- It persists advertised tool metadata, which P143 deliberately forbids for
  the control plane; the session log would become a second, stale inventory
  store with its own authority.
- It diverges from provider mode, where the engine already holds one spec per
  server and the live list is the provider's business.

**B — Inject at the activity boundary (chosen: the `inject` exposure).** The engine keeps exactly
what it keeps today: one `RemoteMcpToolSpec` per server as authored
capability state, frozen into the planned-turn fingerprint. The live
inventory is resolved when the provider request is materialized — the same
boundary where bearer tokens are injected — and the persisted redacted
`provider_request_ref` blob is the audit record of exactly which tools the
model saw, just as it is for every other request byte. This preserves P67's
split: configured capability is engine state; what the server advertised is
an observation.

**C — Meta-tools with on-demand schemas (chosen: the `search` exposure).**
A generic invocation channel plus a search tool, with per-tool schemas
entering context only when the model asks for them. It forfeits provider-side
schema enforcement for the inner arguments and steers models somewhat worse
than named tools — which is why it is not the default — but it is the only
shape that scales to servers with dozens or hundreds of tools, and it is the
now-established ecosystem pattern (provider-side tool search and deferred
loading, search+execute gateway proxies, Claude Code's own deferred tools).
The v1 refinement over the classic catalog design: **no upfront inventory
fetch at all** — the authored record `description` is the index hook inside
the meta-tool description (deterministic engine state, no admission I/O),
and live truth arrives as append-only tool results when the model searches.

## Model-Facing Names (`inject` exposure)

The derived toolset entry for a server is already named `mcp_<server_label>`
(the record's model-facing label, normalized).
Native tools extend it:

```text
mcp_<server_label>__<remote_tool_name>   e.g. mcp_github_read__search_issues
```

- Names, never digests (P138). The remote name must match
  `^[A-Za-z0-9_-]+$` and the combined name must fit the strictest provider
  bound (64 chars). In Selected mode, record validation enforces
  representability of every authored name at `mcp/servers/put`; in All mode,
  tools whose names cannot round-trip are omitted at lowering and counted in
  the activity's diagnostics — they would be uncallable anyway.
- Resolution is deterministic: a call name is matched against active native
  RemoteMcp specs by `spec_tool_name + "__"` prefix; toolset admission
  rejects a native spec whose derived name plus `__` prefixes another's, so
  at most one spec can match. The remainder is the remote tool name.
- The allowlist check on the resolved name uses the authored
  `allowed_tools` materialized into the spec — deterministic engine state. A
  call that matches no spec or fails the allowlist follows the existing
  unknown-tool path.

## Search Exposure

A native server with `exposure: search` contributes no injected tools and no
namespaced names. While at least one search-exposure spec is active, the
derived toolset contains two **session-global** function tools (precedent:
the concurrency tools pulled in when a workflow binding exposes model-owned
promises):

```text
mcp_find_tools { server?, query?, names?, cursor? }
mcp_call       { server, tool, arguments }
```

- `mcp_find_tools` browses, searches, or loads selected full definitions from
  one server's live inventory through the same worker-local cached resolver
  that injection uses. The byte-bounded hit shape, pagination rules, detail
  mode, and argument-name indexing are specified by
  [P150](p150-scalable-mcp-discovery-and-tool-search.md). Matching remains
  deterministic and lexical; only allowlisted tools are ever returned, so
  search never widens Selected policy.
- `mcp_call` executes one tool through the same pinned per-call path as an
  injected call: audience check, broker resolution, `tools/call`, result
  mapping, bounds. The runtime validates that `server` names an active
  search-exposure spec and applies the spec's allowlist; pre-wire validation
  of `arguments` against the cached schema turns obvious mistakes into cheap
  typed tool errors before any network I/O. A schema-invalid or
  non-allowlisted call is a model-visible tool error, never a run failure.
- **The index is the authored record `description`**, rendered with the
  server id into the meta-tool description ("`configurator`: manage
  Lightspeed universes, sessions, bots…"). That is deterministic engine
  state: no discovery at session admission, no catalog entry to refresh, no
  stored inventory. Discoverability comes from the authored sentence; ground
  truth from `mcp_find_tools` at the moment the model cares.
- Both meta-tools are byte-stable across turns and sessions regardless of
  inventory size or churn, so a search-exposure server is prompt-cache
  neutral until the model actually uses it — the property injection cannot
  have for large or churn-y servers.
- Approvals and transcripts always render the resolved `server` + `tool` —
  the P144 approval fact carries both — never a generic `mcp_call` row.

## Engine Changes

- `RemoteMcpToolSpec` gains `execution: RemoteMcpExecution` (`Provider |
  Native`) and `exposure: RemoteMcpExposure` (`Inject | Search`),
  materialized from the record at config admission like every other
  server-owned field; both participate in the request fingerprint
  automatically.
- `remote_mcp_supported_by_provider` becomes mode-aware: native specs are
  compatible with **every** `ProviderApiKind` (they lower to plain function
  tools); provider specs keep today's Responses/Anthropic-only rule with the
  same typed admission failure naming the fix (switch the record, or register
  a native twin).
- Calls resolved to a native spec are client-effect calls: the planner emits
  `ObservedToolCall` → tool batch → dispatch, exactly like function tools.
  `invokes_client_effect` semantics move from the spec to the resolved call.
- To the engine, `mcp_find_tools` and `mcp_call` are ordinary derived
  function tools. An `mcp_call`'s server and tool live in its arguments, and
  reducers do not parse argument blobs — so allowlist and approval
  evaluation for search-exposure calls happen in the runtime at the dispatch
  boundary, surfacing as typed tool errors or needs-approval reports, never
  as planning failures.
- Dispatch pins the resolution onto the request, following the
  `WorkflowToolCallRuntime` precedent, so executors and retries never
  re-derive it:

```rust
pub struct RemoteMcpCallRuntime {
    pub server_id: String,
    pub server_url: String,        // admitted audience
    pub remote_tool_name: String,
    pub auth_required: bool,
}
```

- Execution spec for native MCP calls: class `RemoteInteractive`,
  `retry_safe: false`. `idempotentHint` is untrusted and never enables
  retries; an ambiguous transport failure surfaces as a tool error the model
  can reason about, not an automatic replay of a possibly-executed call.
- The engine still performs no I/O. It never sees an inventory; it sees a
  spec, a call name, and a result.

## Runtime: Inventory Injection

`llm-runtime` gains a narrow resolver trait beside `SecretResolver`, so the
crate stays free of MCP/store/network dependencies:

```rust
#[async_trait]
pub trait McpInventoryResolver: Send + Sync {
    async fn list_tools(
        &self,
        spec: &RemoteMcpToolSpec,
    ) -> Result<Vec<NativeMcpTool>, McpInventoryError>;
}

pub struct NativeMcpTool {
    pub remote_name: String,
    pub description: Option<String>,   // bounded
    pub input_schema: Value,           // bounded bytes/depth
}
```

Each adapter (Responses, Completions, Anthropic) lowers a native spec by
calling the resolver and expanding it, **in place at the spec's position and
sorted by name**, into function tool entries — canonical JSON so the rendered
prefix is byte-stable across turns while the inventory is unchanged (P137;
the Anthropic breakpoint sits on the last non-deferred tool, so inventory churn
invalidates from the tools array onward — inherent, and the reason the cache
below has a TTL rather than none). The injected tools appear identically in
the send request and the redacted persisted blob; there is nothing secret in
them.

The `temporal-runtime` implementation adapts the P143 transport — the same
bounded `rmcp` Streamable HTTP client, SSRF policy, and
`McpToolDiscoveryLimits` (which already bound schema bytes/depth for exactly
this moment) — plus:

- broker resolution immediately before I/O with the server URL as audience
  (`BrokerSecretResolver` semantics, including the admitted-audience check);
- a **worker-local, in-memory, single-flight cache** keyed by
  `(server_id, record_revision)` with a short TTL (initial default 300 s,
  a deployment constant). Never persisted, never shared, never served across
  a record edit — this is a runtime materialization detail, not the stored
  inventory P143 forbids;
- aggregate bounds: per-server tool cap from the limits, plus a per-request
  cap across servers. Exceeding a bound fails the turn with a typed error
  naming both fixes — author a Selected allowlist, or switch the record to
  `search` — with no silent truncation;
- a cache-miss fetch failure fails the turn with a typed, bounded MCP
  transport error (P116-style transient classification where retryable). It
  does not silently drop the server's tools from the request, and it never
  serves an expired inventory as fallback.

When `allowed_tools` is authored, only those names are injected (and a
selected-but-not-advertised name is simply absent — policy is never rewritten
by an outage, per P143). `defer_loading` applies to provider mode only;
native ignores it — `exposure: search` is the native answer to large
inventories. The same resolver and cache serve `mcp_find_tools`; search adds
only the bounded deterministic matching step at the activity.

## Runtime: Call Execution

A native MCP call rides the existing per-call activity path
(`tool_invoke_call`), branching on the pinned `RemoteMcpCallRuntime`:

1. Load the current server record; refuse if disabled or if its URL no longer
   matches the admitted audience (same rule the secret resolver enforces).
2. Resolve the credential through the broker; `CredentialAbsent` is tolerated
   only for optional-auth policies.
3. `initialize` + `tools/call` through the shared `rmcp` transport with the
   P143 SSRF policy and response budget; per-call wall clock from
   `tool_call_operation_timeout(RemoteInteractive)`.
4. Map the result:
   - `structuredContent` when present, else concatenated text content, as the
     bounded tool result payload;
   - `isError: true` becomes a failed tool result the model sees — never a
     run failure;
   - binary/image/resource content is stored to CAS with a typed placeholder
     in the text result in v1; provider-native rich tool-result rendering is
     a follow-up;
   - transport/auth failures become bounded typed tool errors (no raw bodies,
     no tokens, ever).

Discovery's "no `tools/call` reachable from the discovery service" boundary
stands: the control-plane discoverer and the worker executor are separate
narrow traits over the shared transport module, and only the worker's has a
call method. Unlike discovery, execution persists exactly what every tool
call persists — the call and its result in the session log; that *is* the
audit trail.

Connections are per-activity in v1 (initialize, call, shutdown; worker-local
reuse is an optimization). Servers whose tools depend on long-lived MCP
session state get no affinity guarantee; document the limitation.

## Approvals

Native calls whose effective policy requires approval park before dispatch
per P144, and the gate is evaluated **at the dispatch boundary for both
exposures**: the runtime resolves the spec's policy and reports
needs-approval instead of executing; the engine records the fact and parks.
An injected call's server is knowable statically from its namespaced name,
but an `mcp_call`'s is not without parsing arguments — one gate location
keeps the two exposures identical. Approval policy is the record's
`always | never` (P144 removes `providerDefault`); rejection produces a
model-visible tool error. Nothing about approval policy moves — the record
owns it (P143).

## Private Network Egress

Native execution exists substantially *for* internal servers, so the SSRF
default (public targets only) needs a deliberate, deployment-owned exception:

- a deployment-level egress allowlist (e.g.
  `LIGHTSPEED_MCP_PRIVATE_NETWORKS`, a CIDR/host list in `docs/variables.md`'s
  core-runtime section) names the private ranges MCP traffic may reach;
- a per-record `allowPrivateNetwork: true` opt-in is honored only inside that
  deployment allowlist. A record flag alone must never open the runtime's
  network position to any universe manager;
- discovery (P143) and execution share the one policy — the existing
  `allow_private_networks` knob on the discovery client generalizes into it;
- everything else in the SSRF posture (DNS pinning, no redirects, no bearer
  off-origin, response budgets) applies to private targets unchanged.

## Platform UI

- The two-step Add MCP server flow uses the shared fixed progress header also
  used by bot creation: numbered circles, completed/current states, and a
  separately scrolling form body. Connection discovery still gates forward
  navigation.
- The server editor gains the execution and exposure choices, with plain
  wording for the data-path difference ("the model provider connects
  directly" vs "Lightspeed connects") and for exposure ("tools shown to the
  model up front" vs "the model searches on demand — recommended for large
  servers"). During creation, the server starts with every advertised tool
  allowed and the form explains that access can be refined after connection
  and authentication. During editing, tool selection and live inventory sit
  directly below exposure rather than inside Advanced options. Changing either
  is an ordinary put; sessions pick it up at the next config
  admission/reconciliation.
- Native calls need no new transcript surface: they render as the ordinary
  tool-call rows clients already have — an improvement over the opaque
  provider items of provider mode.
- Discovery/allowlist UX from P143 is identical in both modes.

## Protocol Scope (v1)

Tools only. The client declares no sampling, elicitation, or roots
capabilities at `initialize`, so conforming servers will not request them;
requests from non-conforming servers are refused as unsupported. No
resources, no prompts, no `notifications/tools/list_changed` handling (the
TTL bounds staleness), no stdio or legacy SSE transports, no MCP server
hosting. Sessions are per-activity; Streamable HTTP only.

## Implementation Slices

### Slice 1 — Modes and engine resolution

- `execution` and `exposure` on the record/API/store with validation
  (Selected-mode name representability), materialization into
  `RemoteMcpToolSpec`, mode-aware provider compatibility, namespaced call
  resolution + admission ambiguity invariant, `RemoteMcpCallRuntime`
  pinning, execution-class assignment.

### Slice 2 — Inventory injection

- `McpInventoryResolver` in `llm-runtime`, lowering in all three adapters
  (sorted, canonical, in-place expansion), redacted-blob parity, typed
  injection failures, aggregate bounds.
- `temporal-runtime` resolver over the shared transport with broker + TTL
  single-flight cache.

### Slice 3 — Call execution

- Worker executor branch on the pinned runtime, `tools/call` through the
  shared client, result mapping, timeout, no-retry semantics, token
  non-exposure proofs.

### Slice 4 — Search exposure

- `mcp_find_tools` / `mcp_call` derived meta-tools (present exactly when a
  native search spec is active), description-index rendering from the
  authored record description, the find/browse activity over the shared
  resolver with bounded top-K schema results, `mcp_call` runtime resolution
  + allowlist + pre-wire schema validation, and the dispatch-boundary
  needs-approval report (the P144 slice 5 hook).

### Slice 5 — Egress policy and Platform

- Deployment private-network allowlist shared with discovery, per-record
  opt-in, `docs/variables.md`.
- Server editor mode/exposure selection; demo route/fixture updates;
  generated consumers.

### Slice 6 — Live acceptance

- **Standalone native matrix**: `temporal-runtime/tests/mcp_live.rs` runs
  through the real registry/API, Postgres, Temporal worker, approval
  continuation, `rmcp` client, and local Stateless Streamable HTTP servers.
  It covers a small Selected/injected server; a paginated 45-tool All/search
  server; a second Selected/search registration for that large server;
  search-result pagination; text, structured, and binary results; and an
  approval-gated call.
- **Live inventory, not snapshot**: the matrix discovers 45 tools, changes
  only the server to 46 tools, then discovers 46 after the admission
  cooldown, without updating the registry record.
- **Configurator MCP native search smoke**: the generated Configurator
  registry is discovered and a read-only generated tool is found and called
  through the two meta-tools.
- **Provider-hosted MCP**: the separate OpenAI Responses and Anthropic
  Messages `*_mcp_live.rs` suites call a public playground MCP server end to
  end.

## Tests

- Engine: namespace resolution and ambiguity rejection; allowlist
  enforcement; fingerprint changes on mode/allowlist edits and on nothing
  else; native spec accepted on `openai:completions`; provider spec still
  rejected there.
- Adapters: byte-stable injection across turns with an unchanged inventory;
  in-place ordering; oversized inventory → typed turn failure; unresolvable
  server → typed turn failure; redacted blob contains the injected tools and
  no token.
- Cache: TTL expiry, single-flight, record-revision invalidation, no
  persistence.
- Executor: result mapping (text, structuredContent, isError, binary
  placeholder), audience-mismatch refusal, disabled-record refusal,
  timeout → failed result, no retry of ambiguous failures, no raw
  bodies/tokens in errors or logs.
- Egress: private target refused without the deployment allowlist, allowed
  with it plus the record opt-in; DNS-rebind and redirect rules hold for
  private targets.
- Search exposure: meta-tools present exactly when a native search spec is
  active and byte-stable across turns and sessions; browse pagination and
  deterministic query matching; Selected allowlist filters results; top-K
  schema bounds hold; unknown server, non-search server, and non-allowlisted
  tool are typed tool errors; pre-wire schema validation catches malformed
  arguments before network I/O; `mcp_call` execution (audience, broker,
  timeout, result mapping) is path-identical to injected calls;
  needs-approval is reported at dispatch with the resolved server and tool
  on the fact.
- Mixed sessions: provider-mode, native-inject, and native-search servers
  side by side; provider-mode lowering is unaffected.

## Acceptance

1. A universe manager flips one record to native and, without touching any
   session or profile, models on all three API kinds see and execute that
   server's tools in either exposure.
2. A server the size of Configurator is usable through search exposure with
   only two static meta-tools in the request; per-tool schemas enter context
   only on demand, and approvals/transcripts show the resolved tool, never
   a generic `mcp_call` row.
3. An internal-network MCP server (deployment-allowlisted) is usable by
   sessions while remaining unreachable by any model provider.
4. A Deepseek-shaped `openai:completions` model uses MCP tools end to end.
5. Native tool calls appear as ordinary tool calls in transcripts, results
   land in the session log, and no inventory is persisted anywhere.
6. Tokens appear only on the MCP server wire; provider requests for native
   servers carry no MCP entries and no credentials.

## Non-Goals

- Changing the provider-hosted path or its default.
- Materializing discovered tools into the engine toolset, or any stored/
  shared inventory (worker-local TTL cache excepted, as bounded above).
- Automatic fallback between modes, and automatic exposure switching on the
  live tool count; both are authored, and a mismatch is a typed failure.
- Embedding/semantic search inside `mcp_find_tools`; v1 matching is simple
  and deterministic.
- Code mode (model-written orchestration scripts over MCP); it composes
  later from native execution plus an environment sandbox.
- MCP resources, prompts, sampling, elicitation, roots, list_changed,
  stdio/SSE transports, or hosting MCP servers.
- Trusting tool annotations for retries, approval, or authorization.
- Per-session or per-profile execution/exposure overrides.

## Follow-ups

- A P136 catalog index for search exposure if authored record descriptions
  prove too thin a discovery hook.
- Code mode: rendering search-exposure servers as a code API executed in an
  environment; `mcp_find_tools`/`mcp_call` are exactly the surface such a
  script would consume.
- Anthropic's Tool Search Tool in provider-mode lowering:
  [P146](p146-anthropic-tool-search.md).
- Provider-native rich tool results (images/resources) instead of CAS
  placeholders.
- `notifications/tools/list_changed`-driven cache invalidation.
- MCP session affinity for stateful servers if demand appears.
