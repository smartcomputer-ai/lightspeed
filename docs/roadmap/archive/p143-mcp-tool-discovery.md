# P143 — MCP Tool Discovery and Catalog Diagnostics

**Status**

- Proposed 2026-08-31 to make every configured MCP connection independently
  observable before it is given to a model. Today the Platform can show only
  the authored server and credential records, so an operator cannot
  distinguish "configured" from "advertises tools" without inspecting
  provider response blobs.
- The first end-to-end target is Lightspeed's own Configurator MCP. It gives us
  a deterministic generated tool inventory, Streamable HTTP, controllable
  no-auth and API-key modes, and hostile/failure fixtures without involving a
  third-party workspace or a privileged human OAuth account.
- Builds on P67 (provider-native remote MCP),
  [P68](archive/p68-remote-mcp-registry-linking.md) (universe server catalog
  and session links), [P69](archive/p69-generic-auth-token-broker.md) (OAuth,
  encrypted credentials, refresh, and broker resolution), P95 (session config
  as the authority), P110 (the universe server owns its credential), and P124
  (Platform management UI and passthrough API).
- Deliberately reverses one narrow P68 non-goal: Lightspeed gains a bounded
  MCP client for control-plane `initialize` and `tools/list` only. Model-time
  MCP execution remains provider-hosted and provider-native.
- This is a greenfield breaking change. P143 also removes the P95/P110
  per-session MCP behavior overrides: a configured MCP server owns its tool
  allowlist, approval policy, and deferred-loading policy, while sessions and
  profiles select only its `serverId`. Do not add compatibility aliases,
  fallback merging, or a feature-version transition for the removed fields.
- Implemented 2026-08-31 through slices 1–3: the breaking server-id-only link,
  live core/Platform API, broker-backed Streamable HTTP client built on the
  official `rmcp` SDK, bounded/SSRF-safe transport adapter, request-local
  diagnostics, generated consumers, demo path, and server-owned All/Selected
  editor are in place. Synthetic tests cover pagination, bearer injection,
  live repeat calls, duplicate names, schema bounds, and admission cooldown.
  Slice 4 now has serialized live Configurator `single`-mode acceptance through
  the real Gateway HTTP and Configurator processes, with the discovered names
  checked exactly against the generated registry. Authenticated `api-key`
  acceptance and a later dedicated low-privilege external OAuth acceptance
  remain; no external account is required by the implementation or routine
  tests.
- Hardened 2026-09-03 after an external Streamable HTTP server rejected the
  sessionless `server/discover` compatibility probe with HTTP 400 instead of a
  JSON-RPC legacy signal. Discovery and native execution now retry once with a
  fresh `initialize` lifecycle for compatibility-shaped probe rejections;
  authentication, rate-limit, and ordinary server failures remain terminal.

## Decision

A configured MCP server is not proven usable merely because its catalog row is
`active`, its OAuth flow completed, or a session materialized an
`mcp_<server>` tool. Add an authenticated, provider-independent discovery
operation that performs exactly this protocol exchange:

```text
validated universe MCP server record
  -> resolve its current credential through the broker
  -> initialize MCP transport
  -> send notifications/initialized
  -> call tools/list, following bounded pagination
  -> sanitize and return the live tool inventory
  -> render that inventory and its diagnostics in the Platform for this request
```

Discovery never invokes an advertised tool. It is an explicit operator action
on create/edit and refresh, not part of session replay, session startup, or
every model turn.

The session contract becomes a reference to one fully configured connection:

- `features.mcp.servers` selects universe server records;
- each link contains only `serverId`;
- the MCP server record exclusively owns `allowedTools`, `approvalDefault`, and
  `deferLoadingDefault`;
- the session still materializes one provider-native `RemoteMcp` tool, not one
  Lightspeed function tool per discovered remote tool;
- OpenAI/Anthropic still list and execute remote tools at provider send time.

The discovered inventory is request-scoped control-plane evidence and UI
assistance. It is never a database record, cache, execution authority, or
session input. Every display or refresh obtains it directly from the MCP server.

The catalog retains an internal transport field, defaulted to
`streamable_http`, so the runtime has an extension point if the current MCP
spec gains another transport. It is not part of the public API, CLI, or UI
while Streamable HTTP is the only supported value. Legacy HTTP+SSE and
transport auto-detection are deliberately unsupported in this greenfield
product.

## Why

There are currently four different states collapsed into "configured":

1. the endpoint and auth policy are syntactically valid;
2. a credential is bound and locally considered active;
3. the current credential can initialize an MCP session;
4. the server advertises at least one tool to that credential.

P68 and P69 establish the first two. Provider-hosted MCP attempts the latter
two only inside an LLM request, where diagnostics are provider-specific and
may be lossy. A provider can return an empty tool list without a top-level
error, leaving the model to say that no tools exist while the management UI
still shows a green server and OAuth grant.

Tool discovery belongs in Lightspeed because the Platform needs to:

- show what a connection actually exposes before it is linked to an agent;
- replace the free-form comma-separated `allowedTools` input with choices;
- distinguish no tools, authentication failure, transport failure, and
  malformed protocol responses;
- diagnose OAuth scope, workspace governance, and credential lifecycle issues
  without asking a model to probe a privileged account;
- preserve one consistent experience across OpenAI and Anthropic;
- validate the MCP control plane without sending user prompts or workspace
  content to an LLM provider.

P95 and P110 left `allowedTools`, `approval`, and `deferLoading` as optional
session-link overrides of fields already present on the universe MCP server.
That layering is unnecessary and creates competing authorities. In the current
materializer a session allowlist replaces the server allowlist rather than
intersecting it, and a session approval value replaces the server default, so
the supposed "narrowing" can broaden tools or weaken approval behavior.

An MCP server id instead names one configured connection and policy. When the
same endpoint needs different exposure, register it more than once under clear
ids such as `github_read` and `github_write`; `serverUrl` is intentionally
non-unique, and records may bind the same compatible universe credential. This
keeps least-privilege choices in the MCP catalog and makes session/profile
configuration a simple capability grant by id.

## Safety Position

MCP connections can carry the full authority of a human account. Discovery is
read-only in the MCP protocol, but it still uses that authority against an
untrusted remote endpoint. Treat it as a privileged configuration operation.

The implementation must preserve these boundaries:

1. **No tool execution.** The client implements only protocol initialization,
   `tools/list`, and transport shutdown. It has no generic `tools/call` method
   reachable from the discovery service.
2. **No token exposure.** The broker resolves the token immediately before the
   outbound request. Tokens never enter API responses, database records,
   Temporal history, error text, traces, or request logs.
3. **Exact destination.** Reuse and strengthen remote MCP URL validation for
   server-side access: public HTTPS by default, DNS/IP checks at connection
   time, no credentials in URLs, redirects disabled, and no bearer forwarding
   to a different origin or path.
4. **Bounded untrusted data.** Cap time, response bytes, page count, tool
   count, individual names/descriptions, and JSON schema depth/size. Reject
   duplicate or invalid tool names. Treat every description, title, schema,
   and annotation as untrusted display data.
5. **No implicit policy change.** Discovery does not enable a server, bind a
   grant, change OAuth scopes, select tools, or rewrite session configs.
6. **Restricted callers.** Require the same universe management permission as
   creating or editing an MCP server. Ordinary session users cannot cause
   credentialed probes.
7. **Rate limited and auditable.** Rate-limit by universe and server. Record
   who requested discovery when caller identity is available, plus outcome,
   duration, and counts—never tool metadata, headers, bodies, tokens, or raw
   remote errors.

The UI must repeat that the connection uses the authenticating person's
permissions. It must not present `tools/list` as proof that every advertised
operation is safe or correctly annotated.

## First Proof: Configurator MCP

Implement and accept the generic path against
`platform/configurator-mcp` before testing any external OAuth provider. This
is not a Configurator-specific implementation: Configurator is the first
controlled server exercising the same catalog, transport, broker, live API,
and UI paths that every remote MCP server uses.

The fixture is unusually useful:

- it already serves stateless JSON-response Streamable HTTP at `/mcp`;
- its tool set is generated from the committed Lightspeed API contract and
  `tool-filter.json`, so expected names and visible metadata are deterministic;
- `single` mode provides a credentialless first pass;
- `api-key` mode accepts a universe-scoped `lsk_` bearer, exercising P69's
  static-bearer resolution without an external account;
- invalid, revoked, wrong-universe, and missing credentials can be constructed
  safely;
- listing tools authenticates the connection but does not dispatch any
  generated Configurator tool upstream.

Acceptance proceeds in this order:

1. **Local no-auth.** Start Runtime plus Configurator in `single` mode,
   register the development loopback `/mcp` endpoint, discover tools, and show
   the inventory in the Platform. The inventory must match the generated
   Configurator registry rather than a hard-coded count.
2. **Local authenticated.** Start both in `api-key` mode, bind a dedicated
   development-universe static bearer grant, and prove the bearer is present
   only on the Configurator request wire. Missing/revoked/wrong-universe keys
   produce the typed diagnostics defined below.
3. **Selection.** Choose a small explicit subset, link the Configurator server
   to a disposable session, and verify the unchanged provider-native MCP spec
   carries the server record's `allowedTools` subset while the session link
   carries only `serverId`. Invoking a tool is a separate, explicitly approved
   test and is not part of discovery.
4. **Contract drift.** Add or remove a generated fixture tool, refresh the
   inventory, and prove All/Selected and selected-but-missing behavior without
   silently rewriting policy.

Loopback HTTP is a development/test exception only. Production discovery keeps
the public-HTTPS and SSRF rules in this document. Synthetic MCP servers remain
useful for pagination and malicious-response cases that Configurator does not
naturally exercise.

Only after the Configurator path passes should the project validate OAuth with
a dedicated low-privilege external test account. No particular provider is
part of the P143 architecture, schema, or diagnostic taxonomy.

## Product Flow

### Create a server

1. The user enters an MCP URL.
2. Existing protected-resource discovery suggests the auth policy.
3. The user saves the server and completes OAuth or binds a bearer grant.
4. After the credential is bound, the Platform offers **Discover tools**.
5. A successful probe displays the live inventory without storing it.
6. The user chooses **All advertised tools** or **Selected tools** and saves
   that policy on the MCP server record.
7. Sessions and profiles link the server through the existing config path.

Do not probe automatically before OAuth consent. After consent, one automatic
probe is acceptable only if the login completion screen says it will list
capabilities and the same management permission gates it. The initial slice
should use an explicit button so behavior is unambiguous.

### Inspect an existing server

The server detail/edit view can request and show:

- request-local connection state: loading, ready, empty, or failed;
- the live tool count;
- tool name, title, bounded description, and read/write/destructive hints when
  supplied;
- whether all tools or an explicit subset is allowed;
- a **Refresh tools** action;
- a sanitized failure with a stable error code and useful next action.

Before the first request in the current view, show no observed connection
state. Do not render a last-known-good inventory, "stale" inventory, or last
successful discovery timestamp from storage. A failed refresh replaces the
display with its failure rather than falling back to old tool metadata.

An empty successful list is not green success. Show it as **No tools
advertised** with guidance appropriate to the auth policy:

- confirm the OAuth/bearer identity has access;
- check requested scopes;
- check provider workspace/admin governance;
- refresh or reauthorize only this connection if required.

Do not hard-code provider behavior into the protocol layer. Provider-specific
help links or hints may be added in the Platform by matching the endpoint host,
but the live outcome remains generic.

### Select allowed tools

On the MCP server create/edit page, replace the comma-separated field with an
explicit mode backed by the current live discovery response:

```text
(*) All advertised tools
( ) Selected tools
    [x] search
    [x] fetch
    [ ] create_page
    [ ] update_page
```

Semantics remain precise and server-owned:

- **All advertised tools** stores `allowedTools: null`/absent on the MCP server.
  Newly added remote tools become available without a catalog edit.
- **Selected tools** stores an explicit non-empty list of names on the MCP
  server.
- A discovered tool disappearing does not silently remove it from the authored
  allowlist. Show it under **Selected but not currently advertised** so an
  outage or temporary permission change cannot rewrite policy.
- A newly discovered tool is shown but remains unselected in selected mode.
- An empty selected set is invalid; unlink or disable the server instead of
  encoding an ambiguous empty filter.
- Session and profile editors select configured server ids only. They do not
  expose tool, approval, or deferred-loading overrides.
- Different policies for one endpoint are separate MCP server records, which
  may share the URL and a compatible universe credential.

If live discovery fails, the server editor must preserve and display the
authored allowlist without claiming that any selected name is currently
missing. The create/put API continues to validate authored names
syntactically; it does not require or repeat discovery when saving.

Tool annotations inform badges and recommended approval settings only. They
never override the MCP server's approval policy, because a malicious server can
lie about `readOnlyHint` or `destructiveHint`.

## API

Add one live command to the core API. Names are illustrative; the exported
contract decides the final generated spelling.

```rust
pub struct McpServerToolsDiscoverParams {
    pub server_id: String,
}

#[serde(tag = "status", rename_all = "camelCase")]
pub enum McpServerToolsDiscoverResponse {
    Success {
        tools: Vec<McpAdvertisedToolView>,
    },
    Failure {
        code: McpToolDiscoveryFailureCode,
        message: String,
    },
}

pub struct McpAdvertisedToolView {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub annotations: Option<McpToolAnnotationsView>,
}

pub struct McpToolAnnotationsView {
    pub read_only_hint: Option<bool>,
    pub destructive_hint: Option<bool>,
    pub idempotent_hint: Option<bool>,
    pub open_world_hint: Option<bool>,
}
```

The JSON shape is correspondingly small:

```json
{
  "status": "success",
  "tools": [
    {
      "name": "search",
      "title": "Search",
      "description": "Search the connected workspace",
      "annotations": { "readOnlyHint": true }
    }
  ]
}
```

An empty `tools` array is a successful empty discovery; the Platform derives
the **empty** state from it. A bounds violation or malformed page returns a
`failure` outcome and no partial tools. The API therefore has no independent
state enum, nullable diagnostic, `truncated` flag, tool count, observation
timestamp, duration, or echoed server id/revision. The caller already knows
the id, while timestamps, duration, and counts belong in internal metrics/audit
metadata.

Return only tool metadata used by the management UI: name, effective title,
bounded description, and a narrow typed projection of the standard annotation
hints. Validate and bound input/output schemas while parsing the untrusted MCP
response, but do not expose them in v1. They are not needed to select tools and
would dominate the response and its attack surface.

Candidate methods:

```text
mcp/servers/tools/discover   # performs bounded external I/O and returns result
```

Discovery deliberately has no `expectedRevision`. It is a read-like live probe
that persists nothing, so a concurrent catalog edit creates no write-integrity
risk. Resolve one server record at request start and use it consistently for
that probe. The Platform must discard an in-flight result if the user changes
the selected server or edits/saves its connection settings before the response
arrives. Optimistic concurrency remains on `mcp/servers/put`, where it protects
authored configuration.

The Platform adds one matching route:

```text
POST /api/v1/universes/:id/mcp-servers/:serverId/tools/discover
```

Do not return the raw MCP response. Stable typed failures keep provider
payloads, proxy pages, reflected credentials, and oversized error bodies out
of the public API.

## Discovery Failure Taxonomy

The RPC succeeds when the configured probe ran and produced either a live tool
list or an expected operational failure. Use the `failure` outcome for the
latter so the Platform has stable, actionable codes. Continue to use ordinary
API errors for invalid params, permission denial, missing server, rate-limit
admission, store failure, and internal bugs.

At minimum distinguish these operational failures:

| Code | Meaning | Suggested UI action |
|---|---|---|
| `credentialAbsent` | Auth is required but no grant is bound | Connect account |
| `grantNeedsReauth` | Broker says the grant is expired/revoked/dead | Reconnect this server |
| `grantAudienceMismatch` | Bound grant cannot cover this URL | Fix server or credential |
| `unauthorized` | Remote server returned 401/invalid token | Refresh/reconnect |
| `forbidden` | Credential is known but provider policy denies listing | Check scopes/admin governance |
| `remoteRateLimited` | Remote server refused the probe due to rate limiting | Retry later |
| `remoteFailure` | Remote endpoint returned another bounded non-success response | Check endpoint/provider status |
| `unreachable` | DNS/connect/TLS/timeout failure | Retry/check endpoint |
| `invalidResponse` | Not valid MCP/JSON-RPC for the negotiated transport | Check server compatibility |
| `unsupportedProtocol` | Initialization/version negotiation failed | Upgrade or choose another endpoint |
| `paginationLimit` | Server exceeded discovery bounds | Narrow/fix server; partial list is not authoritative |
| `responseTooLarge` | Untrusted inventory exceeded byte/schema limits | Fix server |

Sanitize remote messages into bounded operator-safe text. Detailed internal
errors may include host, HTTP status, MCP error code, and request correlation
id, but never authorization material or raw response bodies.

## No Tool Inventory Storage

The MCP server is the only source of advertised tool metadata. Do not add a
tool-inventory table or place inventories on `mcp_servers`. Lightspeed never
persists or caches discovered tool names, titles, descriptions, schemas,
annotations, hashes, cursors, or raw responses. The discovery response exists
only for the API request and the Platform view that requested it.

The authored `allowedTools` list is different: it is universe configuration
and remains on the MCP server record. It records operator policy, not an
assertion about what the remote server currently advertises.

An existing audit facility may retain the requesting principal, server id and
revision, timestamps, duration, outcome code, and bounded counts. It must not
retain advertised tool metadata or sanitized/raw response bodies. Credential
refresh and reauthorization need no inventory invalidation behavior because
there is no stored inventory.

## Session Contract Refactor

Make the greenfield breaking change in both `api` and `harness`:

```rust
pub struct McpServerLink {
    pub server_id: String,
}
```

Delete `allowed_tools`, `approval`, and `defer_loading` from the public and
engine link DTOs. Delete the corresponding CLI flags, profile/session editor
fields, generated client fields, demo fixture values, conversion code, and
override tests. Reject those properties through the existing
`deny_unknown_fields` behavior; do not accept and ignore them.

At session config admission and toolset reconciliation, the gateway loads the
selected MCP server record and materializes its current non-secret authored
configuration into `RemoteMcpToolSpec`:

```text
McpServerRecord.allowed_tools          -> RemoteMcpToolSpec.allowed_tools
McpServerRecord.approval_default       -> RemoteMcpToolSpec.approval
McpServerRecord.defer_loading_default  -> RemoteMcpToolSpec.defer_loading
```

That deterministic, event-sourced materialization is not a discovered tool
inventory and does not violate the live-discovery rule. Existing session
reconciliation semantics remain: catalog values are resolved when the session
configuration is admitted or reconciled, never fetched nondeterministically by
the engine. Credential resolution remains current immediately before provider
I/O under P110.

## Runtime Placement

Keep protocol types and validation in `crates/mcp`. Put network I/O in a new
non-deterministic control-plane adapter owned by `temporal-runtime`, alongside
the gateway's OAuth metadata client and broker-backed secret resolver. Do not
add network dependencies to `harness` or run discovery in workflow code.

The adapter should expose a deliberately narrow trait:

```rust
#[async_trait]
pub trait McpToolDiscoverer: Send + Sync {
    async fn discover_tools(
        &self,
        server_url: &str,
        bearer: Option<&SecretValue>,
        limits: McpDiscoveryLimits,
    ) -> Result<Vec<DiscoveredMcpTool>, McpToolDiscoveryFailure>;
}
```

The production implementation supports current Streamable HTTP only. The
official `rmcp` SDK owns MCP wire types, initialization, protocol-version and
session handling, JSON-RPC correlation, and `tools/list`. A narrow custom HTTP
backend supplies the Lightspeed-specific DNS pinning, redirect policy, bearer
injection, timeout, and aggregate response-byte bound before `rmcp` parses the
response. Tests use controlled local servers or an in-memory discoverer; no
unit test needs a real token.

This review found no other P143 protocol code that should be hand-maintained.
The main adjacent candidate is `crates/auth/src/mcp_oauth.rs`: `rmcp` now owns
current MCP protected-resource and authorization-server discovery, challenge
parsing, client registration, PKCE, token exchange, and refresh machinery.
Migrating that module is specified by
[P143b](p143b-rmcp-oauth.md), which adapts `rmcp`'s HTTP and protocol seams to
Lightspeed's SSRF policy and durable `OAuthClientStore` / `SecretStore` / grant
broker; P143 must not replace those authorities or start a second credential
lifecycle. Provider-proxied MCP
request/response handling remains in the provider-native OpenAI and Anthropic
adapters because those providers, not Lightspeed, are the MCP client. The
Configurator MCP already uses the official TypeScript SDK. Internal MCP
execution, elicitation, resources, prompts, and approval orchestration are
explicitly later work; they should use the same `rmcp` foundation when
designed.

Do not reuse the LLM provider as the discovery client. Doing so would preserve
the current opaque failure mode, couple inventory to a selected model API, and
send a management operation through a third party unnecessarily.

## OAuth and Credential Lifecycle

Discovery resolves the server's current `mcp_server:<server_id>` reference
through the existing broker. This exercises the same audience checks and
single-flight refresh path as model-time MCP without revealing the token.

The existing catalog status remains authored/configuration state. Discovery
must not mark a server `active` merely because OAuth completed, and a transient
probe failure must not disable a server. The live response owns the current
view's observed health and is discarded with that view.

As a small adjacent improvement, protected-resource/authorization-server
discovery should retain advertised `scopes_supported` and present it beside
the manual requested-scopes field. It may suggest scopes but must not silently
expand consent or rewrite an existing OAuth policy. Some providers use admin
governance independent of OAuth scopes, so a successful token exchange is not
proof that `tools/list` is authorized.

## Limits

Production bounds remain hard constants, but their defaults must accommodate
broad enterprise catalogs rather than only small development fixtures:

- 30 seconds total wall time;
- 64 pages;
- 2,048 tools;
- 16 MiB total decoded response data;
- 128 bytes per tool name and 16 KiB per title/description;
- 512 KiB per input/output schema and JSON depth 64;
- no more than one active discovery per universe/server;
- a short per-server cooldown after completion.

Every dimension remains bounded. Crossing an inventory-completeness bound still
fails discovery without returning a partial inventory. Oversized titles and
descriptions are the exception: they are untrusted, non-authoritative hints and
are truncated on a UTF-8 boundary instead of taking down the entire server.
The unpaginated management projection returns the retained discovery text
unchanged. Native search keeps those bounded descriptions internally and
returns only one byte-bounded result page at a time.

## Implementation Slices

### Slice 1 — Protocol client and API

- Add sanitized advertised-tool DTOs, error taxonomy, and bounds to `mcp`.
- Implement Streamable HTTP initialization and paginated `tools/list` through
  `rmcp` in a narrow `temporal-runtime` adapter; support no legacy transport.
- Resolve optional/required credentials through the P69 broker immediately
  before I/O.
- Add the live-only `mcp/servers/tools/discover` command; add no revision
  parameter, inventory read API, or store trait.
- Prove that tokens and raw remote payloads never enter logs or API errors.

### Slice 2 — Server-owned MCP policy

- Reduce `api::McpServerLink` and `engine::McpServerLink` to `serverId` only.
- Materialize `allowedTools`, approval, and deferred loading exclusively from
  the current MCP server record during config admission/reconciliation.
- Remove session/profile link overrides from the gateway, projections, CLI,
  Platform editor, generated clients, demos, fixtures, and tests.
- Make the removal breaking: no legacy aliases, ignored fields, fallback merge,
  or feature-version compatibility path.

### Slice 3 — Platform UI

- Add Discover/Refresh actions and request-local connection-state presentation.
- Render searchable tool cards/table with safe metadata and annotation badges.
- Replace the MCP server's comma-separated allowlist editor with All/Selected
  mode backed by the live response.
- Make session and profile MCP selection a server picker only.
- Preserve selected-but-missing tools and never auto-enable newly discovered
  tools in Selected mode.
- Add provider-neutral troubleshooting plus optional host-specific help links.

### Slice 4 — Hardening and live acceptance

- Add SSRF, redirect, DNS rebinding, oversized response, pagination loop,
  duplicate name, malformed schema, and hostile text tests.
- Make Configurator MCP the first live fixture: `single` mode proves no-auth
  discovery and `api-key` mode proves brokered bearer discovery, typed auth
  failures, live refresh, and server-level UI selection against the generated
  registry.
- Add synthetic paginated and fake-OAuth servers only for protocol branches the
  Configurator cannot exercise deterministically.
- After the first-party path is green, validate one external OAuth server only
  with a dedicated low-privilege test workspace/account; never use a
  production human account for automated CI.
- Confirm OpenAI and Anthropic sessions still receive the unchanged
  provider-native remote MCP spec and honor the selected allowlist.

## Tests

Required coverage includes:

- Configurator MCP in `single` mode, with discovered names and visible metadata
  equal to its generated registry and no generated tool dispatched upstream;
- Configurator MCP in `api-key` mode with a dedicated development-universe key,
  plus missing, invalid, revoked, and wrong-universe credential cases;
- no-auth server, one page, non-empty tools;
- paginated listing and cursor termination;
- successful empty list returns `{ status: "success", tools: [] }` and the UI
  renders **empty**;
- required auth with no grant;
- active OAuth grant injected as a bearer token only on the wire;
- expired access token refreshed once with rotated refresh persistence;
- 401, 403, broker `needs_reauth`, audience mismatch, timeout, TLS failure,
  remote 429/5xx, invalid JSON-RPC, and unsupported protocol;
- redirects never receive the bearer token;
- private/link-local/loopback resolution is refused, including DNS changes;
- duplicate tools and bounds violations fail safely;
- failure outcomes contain no token or raw credential-bearing response;
- a concurrent server edit does not affect the record already loaded for the
  probe, and the Platform discards an obsolete in-flight response;
- repeated discovery contacts the MCP server every time and no inventory,
  schema, cursor, or response body is written to PostgreSQL;
- a failed refresh shows the current failure without a last-known-good tool
  fallback; a later successful refresh shows only that response;
- All mode stores no server `allowedTools`; Selected mode stores the exact
  server-level names; missing selected names remain authored;
- session/profile MCP links serialize only `serverId`, reject the removed
  fields, and materialize the exact server-level allowlist, approval, and
  deferred-loading values;
- two server ids may reuse an endpoint and compatible credential while carrying
  different tool/approval policies;
- generated API, Configurator MCP, TypeScript client, Platform backend, demo,
  and web tests remain in sync.

The serialized `temporal_live_mcp_and_session_links_materialize` acceptance now
covers the first `single`-mode bullet against a real Configurator child process
and a real local Gateway HTTP edge. It also retains the server-id-only session
link and server-owned policy materialization assertions. The `api-key` and
external OAuth cases remain explicit live follow-ups.

## Acceptance

P143 is complete when:

1. A universe manager can discover tools for no-auth, bearer, and OAuth MCP
   servers without starting a model run.
2. Each discovery/refresh obtains a fresh bounded inventory directly from the
   MCP server; no discovered inventory or last-known-good copy is persisted.
3. The Platform clearly distinguishes the current request's loading, ready,
   empty, and failed states.
4. The server page displays a bounded, sanitized live inventory and edits the
   server-owned `allowedTools` policy with explicit All/Selected semantics.
5. Discovery cannot invoke tools, expose credentials, reach private network
   targets, follow credential-leaking redirects, or persist tool metadata or
   raw responses.
6. An empty authenticated inventory produces actionable scope/access/admin
   governance guidance.
7. Session and profile MCP links contain only `serverId`; the resolved remote
   MCP tool uses the selected server record's allowlist, approval, and
   deferred-loading configuration as resolved at config admission or
   reconciliation.
8. Session execution remains provider-native, and provider send-time MCP tool
   listing and execution remain unchanged.
9. Configurator MCP passes first-party live acceptance in both `single` and
   `api-key` modes, including inventory display and allowlist selection, before
   any external OAuth account is used.
10. A later low-privilege external OAuth acceptance proves login, refresh if
   needed, `tools/list`, UI inventory, session link, and one separately
   approved read-only model-time call end to end without introducing
   provider-specific behavior.

## Non-Goals

- Direct model-time MCP execution by Lightspeed.
- Converting remote tools into engine function tools.
- Calling a remote tool as part of discovery or health checking.
- Returning or leasing MCP bearer tokens to the browser.
- Automatically expanding OAuth scopes or bypassing provider governance.
- Automatically changing server status, allowlists, profiles, or sessions from
  discovery output.
- Scheduled/background probing in v1.
- Persisting, caching, or retaining current or historical tool inventories.
- Per-session or per-profile MCP tool, approval, or deferred-loading overrides.
- Trusting remote read/write/destructive annotations as authorization facts.
- Solving P143 by asking an LLM provider to list tools.

## Follow-ups

- Refactor `crates/auth/src/mcp_oauth.rs` onto `rmcp`'s current MCP OAuth
  discovery and authorization machinery through Lightspeed-specific HTTP and
  durable state-store adapters; keep the existing grant/secret broker as the
  sole credential authority.
- Periodic health checks and alerting after real operational demand.
- Provider-specific setup guidance sourced from a maintained catalog.
- Approval-policy suggestions based on annotations, still requiring a human
  decision and saving only the resulting authored server policy.
- A safe connection test for MCP resources/prompts if those become product
  surfaces; they are outside tool discovery.
