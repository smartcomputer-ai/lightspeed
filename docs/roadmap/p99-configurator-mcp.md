# P99: Configurator MCP — Universe API over Streamable HTTP

**Status:** Implemented 2026-07-11.

Implemented as `interop/configurator-mcp`: a private Node/TypeScript package
using the stable MCP SDK v1 Streamable HTTP transport, a generated universe-only
descriptor map with self-contained input schemas, a committed exact-method
filter (71 of the current 81 universe methods advertised), request-scoped
single/trusted-header/API-key forwarding, upstream authentication probes,
structured results/errors, host/origin checks, and real MCP-client integration
tests over the HTTP edge. The optional output schemas were deliberately omitted
after generation showed that repeating the `AgentApiOutcome` notification
closure made `tools/list` several megabytes; complete outcomes are still
returned as structured content and JSON text.

Documentation follow-up implemented 2026-07-11: every universe and operator
method now carries a compact summary and operational description in the Rust
dispatch manifest. The exporter propagates them to `methods.json`, OpenRPC, and
the generated Markdown API reference; the TypeScript client exposes
`METHOD_INFO` plus JSDoc; Configurator MCP uses the same text for model-facing
tool descriptions.

## Goal

Expose a generated, explicitly configurable subset of the universe-scoped
Lightspeed TypeScript client surface as a remote MCP server over Streamable
HTTP.

The Configurator MCP is a protocol facade over the existing JSON-RPC gateway,
not a second control plane. It derives MCP tools from the committed API
contract, forwards each tool call through `LightspeedClient`, and preserves the
gateway as the sole admission, authorization, validation, concurrency, and
universe-isolation boundary.

One Configurator MCP deployment can mediate calls for many universes. The
calling MCP client authenticates on every HTTP request; that request-scoped
identity determines the credentials and tenant headers used for the upstream
Lightspeed call. A Configurator process is universe-pinned only when its
upstream Lightspeed gateway runs in `single` auth mode, which is primarily a
local/test topology.

The first cut uses one committed exact-method exclusion list at generation
time. It does not add runtime read-only/configuration/all profiles, per-client
filtering, approval policy, or tool-safety overlays. The five deployment-scoped
`operator/*` methods are never eligible for this server.

## Product shape

```text
MCP client A -- Streamable HTTP + credential A --\
                                                  \
MCP client B -- Streamable HTTP + credential B ----> Configurator MCP
                                                  /      |
MCP client C -- Streamable HTTP + credential C --/       | request-scoped
                                                         | LightspeedClient
                                                         v
                                                  Lightspeed /rpc
                                                         |
                                                         v
                                             universe resolved by gateway
```

The Configurator is intentionally thin:

1. authenticate the incoming HTTP request according to the deployment's
   Lightspeed auth mode;
2. select only the generated universe-scoped method descriptor;
3. validate MCP tool arguments against that method's exported params schema;
4. call the same method through the private TypeScript client with
   request-scoped upstream headers;
5. return the complete `AgentApiOutcome` as MCP structured content, or map the
   typed Lightspeed error to an MCP tool error.

It must not reproduce gateway business logic, resolve catalog relationships,
apply config defaults, synthesize patch operations, interpret revisions, or
read Lightspeed stores directly.

## Current contract boundary

The committed manifest in `interop/contract/methods.json` is the source of
truth. At proposal time it contains 86 methods:

- 81 with `scope: "universe"`;
- 5 with `scope: "operator"`.

The generated TypeScript client currently includes both scope classes because
it represents the complete HTTP JSON-RPC contract. P99 first selects manifest
entries whose scope is `universe`, then removes the exact method names in
`interop/configurator-mcp/tool-filter.json`. The implemented default excludes
10 methods and advertises 71 tools.

The exclusion is semantic, not only a `tools/list` filter:

- no `operator/*` tool descriptor is generated;
- the runtime dispatcher accepts only a generated `UniverseMethod` map;
- a hand-crafted `tools/call` using an operator-derived name returns unknown
  tool before any upstream request;
- contract tests assert the generated universe count and the absence of every
  operator manifest entry.

This rule is unchanged in `single` mode. A Configurator never becomes a
deployment operator merely because the upstream gateway would accept an
operator call.

### Committed method filter

`tool-filter.json` is the tunable generation input. It contains one
`excludeMethods` array of exact universe method names. Generation fails for an
unknown/non-universe name, duplicate, empty value, or unknown config key so a
stale or misspelled exclusion cannot silently change the surface.

The implemented defaults exclude:

- `initialize` — the Configurator already uses the handshake internally;
- `environments/providers/register|heartbeat|unregister` — provider/bridge
  presence writers, while retaining `environments/providers/list` for
  visibility;
- `environments/jobs/create|read|list|cancel` — environment execution rather
  than configuration;
- `outbox/read|ack` — messaging delivery-worker operations.

This is a build/deployment-wide surface, not caller authorization. Editing the
file and regenerating changes `tools/list` for every client of that artifact.

## Package placement

Add a separate private TypeScript package alongside the API client:

```text
interop/
  contract/
  ts-client/
  configurator-mcp/
    package.json
    tsconfig.json
    src/
      server.ts
      transport.ts
      request-auth.ts
      upstream-client.ts
      tool-result.ts
      generated/
        tools.ts
    scripts/
      generate-tools.mjs
    test/
```

Do not put MCP SDK dependencies, HTTP server policy, environment parsing, or
request authentication into `interop/ts-client`. That package remains a small,
transport-level JSON-RPC client. The Configurator depends on it and owns the
adapter concerns.

## Streamable HTTP first

P99 implements only Streamable HTTP. It does not implement stdio or legacy
HTTP+SSE compatibility.

The initial server is stateless and uses JSON response mode:

- one `POST /mcp` endpoint;
- `sessionIdGenerator: undefined`;
- `enableJsonResponse: true`;
- no `Mcp-Session-Id` state;
- no server-initiated notifications, resumability, or standalone GET/SSE
  stream;
- one independent authentication decision per HTTP request.

This is still the MCP Streamable HTTP transport. The server simply does not
need its optional session and server-to-client streaming features yet. All P99
tools are ordinary request/response operations. Upstream long-poll calls such
as `session/events/read` and `outbox/read` may hold the POST open until their
normal JSON-RPC response arrives; the Configurator's HTTP timeout must exceed
the upstream long-poll cap.

Use the production-recommended stable major of the official TypeScript MCP SDK
at implementation time. As of this proposal the SDK project still recommends
v1.x for production while v2 is pre-release. Pin the selected major and exact
minimum version in `package-lock.json`; do not code against a moving branch.

Reference points:

- <https://modelcontextprotocol.io/specification/2025-06-18/basic/transports>
- <https://ts.sdk.modelcontextprotocol.io/server>
- <https://github.com/modelcontextprotocol/typescript-sdk>

### HTTP edge requirements

The server must:

- validate `Origin` on every Streamable HTTP request;
- validate `Host` against an explicit allow-list when bound beyond localhost;
- bind to localhost by default;
- support TLS termination directly or, preferably, behind a trusted reverse
  proxy in deployed environments;
- enforce request body and response size limits compatible with the
  Lightspeed gateway's configured maximum;
- propagate client disconnect/abort to the upstream `fetch` request;
- expose a non-sensitive `GET /health` endpoint that does not enumerate
  universes or validate caller credentials.

No browser CORS policy is enabled implicitly. A deployment that needs browser
MCP clients must configure explicit allowed origins and headers.

## Request-scoped authentication and universe resolution

P99 supports the same three deployment modes as the Lightspeed HTTP gateway:
`single`, `trusted-header`, and `api-key`. A Configurator instance is configured
for exactly one mode, matching the upstream gateway it fronts. It does not
choose among modes based on caller-supplied headers.

```ts
type RequestAuthContext =
  | { mode: "single" }
  | {
      mode: "trusted-header";
      universeId: string;
      principal?: string;
    }
  | {
      mode: "api-key";
      apiKey: string;
    };
```

The auth context exists only for the lifetime of one MCP HTTP request. It is
never stored in a process-global mutable variable, MCP tool descriptor, cache
entry, response, or log field. Tool handlers obtain it through an explicit SDK
request context when available, or a narrowly scoped Node `AsyncLocalStorage`
when the stable SDK transport does not expose the necessary hook.

There is no single authenticated `LightspeedClient` shared by the server.
Handlers create a lightweight caller from the fixed upstream endpoint plus the
current request context:

```ts
function clientFor(auth: RequestAuthContext): LightspeedClient {
  return new LightspeedClient({
    endpoint: configuredRpcEndpoint,
    headers: upstreamHeaders(auth),
  });
}
```

A shared unauthenticated client factory is fine; captured request credentials
are not.

### `single`

The Configurator sends no tenant or authorization headers. The upstream
gateway selects its configured universe. The MCP server rejects incoming
`Authorization`, `x-lightspeed-universe`, and `x-lightspeed-principal` headers
rather than silently ignoring conflicting identity claims.

The server is therefore automatically single-universe in this topology. It
does not need, accept, or expose the configured universe id.

### `trusted-header`

The Configurator accepts `x-lightspeed-universe` and optional
`x-lightspeed-principal` only from a trusted reverse proxy which owns caller
authentication and authorization. It validates their syntax and forwards them
unchanged to the upstream trusted-header gateway.

The mode fails closed:

- the universe header is required and must be a UUID;
- there is no default universe;
- arbitrary direct clients must not be able to reach a trusted-header listener
  while bypassing the authenticating proxy;
- the principal follows the gateway grammar (`user:<id>`,
  `service_account:<id>`, or a bare user id);
- a bearer `Authorization` header is rejected to avoid two competing identity
  sources.

Proxy trust is a deployment boundary, not evidence contained in the header
itself. The Configurator must document the required network isolation and
header stripping/injection behavior.

### `api-key`

The MCP client sends its Lightspeed deployment API key as:

```http
Authorization: Bearer lsk_...
```

The Configurator forwards that credential to the upstream API-key gateway for
each Lightspeed call. It does not resolve, cache, hash, persist, or map the key
locally. The upstream gateway remains authoritative for revocation, universe
binding, and principal resolution. Incoming Lightspeed universe/principal
headers are rejected in this mode.

This is deliberately **Lightspeed-native API-key authentication for a
Lightspeed-owned protocol facade**, not MCP OAuth. General OAuth bearer-token
passthrough is forbidden by the MCP authorization specification because tokens
must be audience-bound to the MCP resource. P99 does not claim OAuth protected
resource conformance for `lsk_` keys. If standards-based remote MCP OAuth is
added later, it must terminate an MCP-audience token at the Configurator and
exchange or map it to a separate upstream Lightspeed identity rather than
forwarding that OAuth token.

Reference:
<https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization>

### Authentication of protocol-only requests

Stateless MCP requests such as `initialize`, `ping`, and `tools/list` may not
otherwise call Lightspeed. They still must authenticate; invalid API keys and
unknown trusted-header universes must not be able to establish MCP protocol
access or enumerate tools.

The Configurator calls upstream Lightspeed `initialize` with the request's auth
context before dispatching every MCP POST. This includes `tools/call`: schema
validation and unknown-tool rejection happen locally, so relying on the target
method alone would leave those error paths unauthenticated. A valid tool call
therefore makes the small authentication probe followed by its requested
Lightspeed call. This keeps invalid/revoked API keys and unknown trusted-header
universes out of every protocol and error path without giving the Configurator
direct access to the API-key store.

Missing or malformed edge credentials fail at HTTP authentication. An
upstream rejection during validation must be mapped to an appropriate 4xx
response without returning credentials, upstream headers, or an internal
response body. Tool-call admission failures remain MCP tool errors because the
MCP request was authenticated but the requested Lightspeed operation failed.

## Generated MCP tool surface

`interop/contract/methods.json` and `interop/contract/api.schema.json` remain
the only sources of wire truth. Extend generation rather than hand-maintaining
the advertised adapters.

For each universe method remaining after the committed filter, generate:

- a deterministic MCP-safe tool name, for example
  `session/config/put` -> `lightspeed_session_config_put`;
- the exact Lightspeed method string;
- a self-contained MCP `inputSchema` derived from the params schema;
- the result type name, while omitting the optional MCP `outputSchema` in the
  first cut to keep `tools/list` bounded;
- a handler which invokes `client.call(method, arguments)`.

Generation must fail on normalized tool-name collisions. The generated module
exports a closed `UniverseMethod`/tool descriptor map; runtime dispatch never
accepts an arbitrary method string supplied by an MCP caller.

### Schema handling

The Lightspeed bundle uses draft-07 definitions and internal
`#/definitions/...` references. An MCP tool schema must be independently
usable outside the bundle. The generator should compute the transitive
definition closure for each method and embed only the reachable definitions,
or fully dereference the schema when this can be done without breaking
recursive types. It must not attach the entire Lightspeed schema bundle to all
advertised tools.

Prefer the MCP SDK's low-level tool request handlers if its high-level API
would require a lossy JSON Schema -> Zod reconstruction. The Rust-exported
schema is authoritative; P99 must not create a second hand-written validation
model.

Generated artifacts are committed. `npm run check:generated` regenerates and
fails on a diff, matching the existing TypeScript client workflow. An API wire
change therefore requires:

```bash
cargo run -p api --bin export-schema
cd interop/ts-client && npm run generate
cd ../configurator-mcp && npm run generate
```

### Descriptions and safety metadata

The Rust method manifest is the canonical source for a concise summary and an
operational description. Descriptions emphasize lifecycle, concurrency,
idempotency, capability prerequisites, and secret-handling facts that are not
obvious from field names; field-level detail remains on the Rust DTOs and flows
through JSON Schema. The MCP generator uses this canonical prose rather than
type-name boilerplate.

Structured MCP annotations such as `readOnlyHint`, `destructiveHint`, and
`idempotentHint` remain deferred. They are non-enforcing client hints and should
later enrich the canonical Rust method manifest without changing dispatch or
the committed method filter.

## Results and errors

Successful calls preserve the complete TypeScript client result envelope:

```ts
return {
  structuredContent: outcome,
  content: [{ type: "text", text: JSON.stringify(outcome) }],
};
```

The text copy preserves compatibility with MCP clients that do not consume
structured content. If an output schema is advertised, `structuredContent`
must validate against it.

P99 does not simplify notifications, unwrap `result`, invent higher-level
workflow helpers, automatically await runs, retry revision conflicts, or
offload large results to MCP resources. Calls such as `blobs/read` therefore
remain capable of returning large payloads within the configured HTTP limit.
Profiles, pagination defaults, resource links, and result offloading can be
added later without changing the generated method identity.

Map failures as follows:

- `LightspeedRpcError` -> MCP tool result with `isError: true`, preserving the
  stable JSON-RPC `code`, Lightspeed error `kind`, message, and sanitized data;
- `LightspeedTransportError` -> MCP tool error with HTTP status when known and
  no raw response body unless it is proven non-sensitive;
- input-schema failure -> MCP invalid tool arguments, before the upstream call;
- client abort -> abort the upstream request and do not convert it to a
  successful error payload.

No error or diagnostic may contain an API key, authorization header, OAuth
code, imported grant secret, or complete sensitive tool arguments.

## Concurrency and tenant isolation

The Configurator is a concurrent multi-tenant HTTP service. Request A's auth
context must remain attached to request A across asynchronous MCP parsing,
schema validation, handler execution, upstream fetch, and error mapping.

Required invariants:

- no credentials in module-level mutable state;
- no last-caller/current-universe field on a shared MCP server;
- no default/fallback universe in multi-tenant modes;
- no auth-context reuse based on connection, remote address, HTTP keep-alive,
  or a caller-supplied MCP identifier;
- no credential-bearing retry after a request context has ended;
- no cross-request batching of Lightspeed calls with different identities.

The isolation test must run interleaved requests for at least two universes
through the same server instance and assert at the captured upstream HTTP edge
that every method received only its request's credential/header set. Include
both successful and failing calls so error paths cannot leak or reuse context.

## Configuration

The first cut needs process-level configuration for:

- MCP bind address;
- allowed `Host` values and `Origin` values;
- public MCP resource/base URL for diagnostics and a future OAuth boundary;
- upstream Lightspeed `/rpc` URL;
- auth mode (`single`, `trusted-header`, or `api-key`);
- request/response byte limits;
- upstream connect/request timeout and graceful shutdown timeout;
- trusted proxy/network policy where applicable;
- structured log level.

Environment variable names should use a `LIGHTSPEED_CONFIGURATOR_MCP_*` prefix
except where intentionally sharing the deployment's existing
`LIGHTSPEED_AUTH_MODE`. Startup validation rejects contradictory or incomplete
settings. In particular, no process-level universe id or API key is accepted in
the multi-universe modes.

## Implementation slices

### Slice 1: Package, transport, and contract generator

1. Add `interop/configurator-mcp` with the stable MCP TypeScript SDK, HTTP
   adapter, build/test scripts, and private package metadata.
2. Serve stateless JSON-response Streamable HTTP at `/mcp`, plus `/health`,
   with host/origin validation and abort-aware request handling.
3. Generate the closed tool descriptor map from the committed manifest and
   schema bundle; commit it and add a currency check.
4. Apply the committed exact-method filter, expose the resulting universe tools
   and no operator tool, initially against a mock `RpcCaller`.

### Slice 2: Request auth and upstream forwarding

1. Implement the fail-closed auth-mode parser and request-scoped auth context.
2. Build request-scoped `LightspeedClient` callers without global credentials.
3. Validate protocol-only MCP requests through upstream `initialize`; forward
   tool calls directly.
4. Map structured outcomes and typed errors, with secret redaction and abort
   propagation.
5. Add concurrent multi-universe isolation tests for trusted-header and
   API-key modes, plus single-mode tests.

### Slice 3: Live integration and deployment documentation

1. Exercise an MCP SDK client against the real Streamable HTTP endpoint and a
   local Lightspeed gateway in each auth mode.
2. Assert tool discovery count, representative read/write/delete calls,
   revision conflicts, long-poll behavior, invalid/revoked credentials, and
   operator exclusion.
3. Document reverse-proxy requirements, example client configuration, health
   checks, shutdown, and API-key handling.
4. Add the package checks to the repository's normal CI/test entry points.

## Acceptance criteria

- [x] A remote MCP client can initialize and list tools over Streamable HTTP;
  no stdio transport is present.
- [x] `tools/list` advertises exactly the current universe-scoped manifest
  entries minus `tool-filter.json` (71 of 81 at implementation time), with
  valid self-contained input schemas.
- [x] No operator method is generated, advertised, or dispatchable.
- [x] Every generated tool forwards its exact method name and params through
  `LightspeedClient` and returns the complete `AgentApiOutcome`.
- [x] `single` mode sends no auth/tenant headers and rejects caller tenant
  claims.
- [x] `trusted-header` mode requires a valid universe header from the trusted
  proxy path, forwards the optional principal, and has no fallback universe.
- [x] `api-key` mode requires a bearer `lsk_` credential, forwards it only to
  the configured Lightspeed endpoint, and rejects tenant headers.
- [x] Protocol-only MCP requests validate their identity upstream; invalid or
  revoked credentials cannot enumerate tools successfully.
- [x] Concurrent calls for different universes cannot cross credentials,
  principals, params, results, or errors.
- [x] Origin/host validation, byte limits, TLS/proxy documentation, graceful
  shutdown, and abort propagation are covered.
- [x] Logs and error payloads contain no API keys, auth headers, OAuth codes,
  imported secrets, or raw sensitive request bodies.
- [x] Generated artifacts fail their currency check when the API manifest or
  reachable schemas change.
- [x] Package typecheck, unit tests, generated checks, and real Streamable HTTP
  integration tests pass.

## Explicit non-goals

- No `operator/*` methods or deployment administration.
- No stdio or legacy HTTP+SSE transport.
- No stateful MCP sessions, resumability, notification stream, or distributed
  session routing.
- No runtime read-only/configuration/all profiles, caller-specific allow/deny
  lists, or dynamic surface filtering; only the committed generation filter.
- No method safety annotations, human approval flow, or integration with the
  later MCP approval roadmap item.
- No standards-based MCP OAuth protected resource in the first cut.
- No local universe/API-key registry, credential persistence, token refresh,
  or direct Postgres access.
- No replacement for Lightspeed gateway authorization or validation.
- No convenience tools, automatic run waiting, retries, patches, or result
  transformations beyond faithful MCP encoding.
- No MCP resource facade over Lightspeed blobs, sessions, profiles, or
  catalogs.

## Follow-ups

Natural later additions, each separately designed:

1. canonical method descriptions and safety metadata;
2. advertised surface profiles (`read-only`, `configuration`, `all`) and
   explicit allow/deny policy;
3. MCP OAuth termination with audience validation and a non-passthrough
   upstream identity exchange;
4. stateful Streamable HTTP only if server notifications or resumability have
   a concrete product use;
5. MCP resources/resource links for large blob and event results;
6. approval integration for destructive or externally consequential methods.
