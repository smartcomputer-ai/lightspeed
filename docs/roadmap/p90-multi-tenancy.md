# P90: Multi-Tenancy — Multiple Universes Per Deployment

**Status**
- Proposed 2026-07-03.
- Phase 1 implemented 2026-07-03: composed workflow ids
  (`compose_workflow_id`/`split_workflow_id` in `temporal-workflow`,
  bootstrap assertion, `universe_id` on `AgentSessionArgs`), shared
  deployment task queue default (`lightspeed-agent`), `DeploymentStores`
  (shared pool + object store + per-universe `PgStoreConfig` template),
  `UniverseRuntime` (lazy per-universe `PgStore` + `GatewayAgentApi` +
  `ActivityState`, shared by gateway and worker in `both` mode),
  `GatewayState` edge resolution with `single` and `trusted-header` modes
  (`LIGHTSPEED_AUTH_MODE`, `LIGHTSPEED_UNIVERSE_AUTO_CREATE`, fail closed,
  header rejected in non-header modes), OAuth callback universe resolution
  via deployment-level `find_auth_flow_universe(state_hash)`, and the
  separator-invariant/edge-resolution unit tests plus an ignored
  two-universes-one-worker live isolation test. Live coverage caught one
  cross-workflow addressing site the static sweep missed: fleet
  `agent_wait` subscribe/unsubscribe signals addressed sibling sessions by
  bare session id; they now compose the parent's own universe prefix
  (`sibling_workflow_id` in `fleet_waits.rs`). Full live suite
  (`temporal_live`, `preprocess_live`) passes; the
  `environment_provider_live` host-bridge agent test fails identically on
  the pre-P90 tree (pre-existing doubled-path bug in fs routing, tracked
  separately).
- One mechanism deviation from the proposal: the worker derives the
  universe by splitting the workflow id carried in each activity task's
  `ActivityContext` rather than reading `AgentSessionArgs` or a per-request
  DTO field. The composed workflow id is the asserted tenancy identity, so
  this keeps a single source of truth and leaves the activity DTOs
  unchanged; a mismatched or uncomposed workflow id fails the activity
  non-retryably. `args.universe_id` remains the workflow-side value the
  bootstrap assertion checks against.
- Phases 2 and 3 implemented 2026-07-03: deployment-scoped `api_keys` table
  (`008_api_keys.sql`; hash-only storage, unique display prefix), inbound
  `auth::api_keys` module (`mint_api_key`, `ApiKeyStore`) with the Postgres
  impl on the shared pool (`PgApiKeyStore`), gateway `api-key` mode
  (`Authorization: Bearer lsk_…`, tenant headers rejected, unknown/revoked
  keys indistinguishable), request-principal propagation via a task-local
  scope around dispatch (`gateway::principal`) replacing the
  `universe_default()` stamping on grant/flow creation, the optional
  `x-lightspeed-principal` header in trusted-header mode, admin subcommands
  (`server universe create|list`, `server api-key create|list|revoke`; the
  secret prints exactly once), client credentials in the CLI and messaging
  bridge (`LIGHTSPEED_API_KEY`/`LIGHTSPEED_UNIVERSE` env, bridge config
  fields), README/AGENTS/env docs, and an ignored api-key live test covering
  fail-closed, cross-universe misses, principal stamping, header rejection,
  and immediate revocation.
- Deferred from Phase 3: per-binding bridge credentials (universes mixed
  within one bridge process). Requires persisting auth on bridge binding
  state and threading it through the outbox poller; one bridge process
  serves one universe until then.
- Runtime-footprint follow-up implemented 2026-07-03: `UniverseRuntime`
  evicts cached states opportunistically on every `state_for` touch (4h
  idle timeout, LRU beyond a 1024-state cap, the just-used entry never
  evicted; safe because states hold no durable data and in-flight work
  keeps its own `Arc`), plus a 10-minute background sweeper
  (`spawn_idle_sweeper`, `Weak`-held so it exits with the runtime) covering
  fully quiet processes; all
  universe-agnostic HTTP clients (OpenAI responses/audio, Anthropic, OAuth
  token/metadata, GitHub) moved to a deployment-scoped `DeploymentClients`
  shared across universes, so a cached universe's marginal footprint is the
  resolver layers and tool registry only; and the gateway session-metadata
  map is bounded by session lifetime (empty metadata never occupies an
  entry, `close_session` removes it). There is no CAS/blob cache in the
  runtime — blob reads always go to Postgres/S3 — so no per-universe blob
  memory accrues.
- Builds on **P55 (Temporal Claw)**, which introduced the `universes` table and
  scoped every Postgres table by `universe_id`, but deliberately fixed one
  configured universe per worker/gateway process (`universe_id` is
  storage-only; see p55 notes). P90 removes that restriction.
- Builds on **P58 (JSON-RPC gateway)** for the dispatch boundary and **P69
  (generic auth token broker)** for `PrincipalRef`, which was designed as the
  seam where caller identity would later attach.
- Owns the roadmap item: *"Multi-tenant support in worker"*.

## Goal

Run many isolated universes (tenants) on one deployment — one gateway, one
Temporal worker, one Postgres pool, one object-store bucket — with universe
selection moved from process configuration into the request path, without
changing the public API surface.

Explicitly in scope:

1. One worker process serving sessions from any number of universes.
2. Per-request universe resolution at the gateway edge, pluggable enough that
   enterprises bring their own auth system in front.
3. Structural isolation: no request, session, workflow, listing, or fleet
   spawn can cross universes.

## Problem

The tenant boundary already exists in the persistence layer. Every table in
`crates/store-pg/migrations/` leads its primary key with `universe_id`, every
foreign key includes it (cross-universe references are structurally
impossible), and object-store keys embed `universes/{universe_id}/...`
(`crates/store-pg/src/object.rs`). The schema comment in `001_core.sql` calls
a universe "the tenant/project/workspace boundary".

What is single-tenant is everything above the rows:

- `PgStore` binds one `universe_id` at construction
  (`crates/store-pg/src/lib.rs`), read once from `LIGHTSPEED_PG_UNIVERSE_ID`
  (`crates/temporal-server/src/config.rs`). Every runtime singleton built from
  it — `GatewayAgentApi`, `ActivityState` (LLM runtime, tool registry, token
  broker) — serves exactly one universe per process.
- The Temporal task queue is derived from that one universe
  (`lightspeed-universe-{id}`), so hosting N universes means N worker
  processes.
- The workflow id is the raw session id
  (`gateway/service/workflow.rs`, asserted in
  `temporal-workflow/src/workflow/bootstrap.rs`). Two universes on a shared
  Temporal namespace could collide on client-chosen session ids.
- The JSON-RPC endpoint has no caller identity at all: `dispatch_json_rpc`
  (`crates/api/src/rpc.rs`) takes `(service, request)` and any caller can
  address any session by id.

## Design Position

### Tenant = universe; the API never carries it as a parameter

The universe id appears in exactly two places per request: the resolved auth
context, and (internally) the Temporal workflow id. No JSON-RPC method gains a
universe parameter. Session-scoped methods are checked against the caller's
universe; registry and list methods (`profiles/*`, MCP catalog, environments,
auth grants/secrets) implicitly scope to the caller's universe. The committed
API contract does not change shape.

### Public session ids stay clean; workflow ids are namespaced internally

Session ids remain client-visible, client-choosable names (`session_mybot` is
fine; many clients will use UUIDs, but that is not enforced). Uniqueness scope
is per-universe, which the Postgres PK `(universe_id, session_id)` already
states.

The Temporal workflow id becomes the composed form:

```text
workflow_id = {universe_id}/{session_id}
```

- All universes share one deployment task queue (default `lightspeed-agent`);
  the composed workflow id is what makes shared-queue hosting collision-free,
  including for identical client-chosen session names in different universes.
- `validate_session_id` (`crates/api/src/ids.rs`) already rejects `/`; that
  restriction is now load-bearing — `/` is the reserved separator that makes
  the composed id unambiguously splittable (universe ids are UUIDs and cannot
  contain it either). Document this on the validator.
- `AgentSessionArgs` gains `universe_id`. Activities route storage and LLM
  resources by it, and the bootstrap assertion becomes
  `workflow_id == "{args.universe_id}/{args.session_id}"`. Continue-as-new
  carries it unchanged.
- Diagnostics emitted from workflow/worker code should log the composed id (or
  both parts) so Temporal histories and gateway logs remain greppable against
  each other.

`LIGHTSPEED_TASK_QUEUE` remains as an explicit override; a per-universe queue
override (dedicated queues for noisy tenants) is possible later via a column
on `universes`, but is not part of this item.

### Per-universe store instances, unchanged store traits

None of the ~15 store traits (`SessionStore`, `BlobStore`, `ProfileStore`,
`McpRegistryStore`, auth stores, environment stores, `OutboxStore`) gains a
universe parameter. Instead, the deployment holds a lazy registry:

```text
UniverseRegistry: universe_id -> Arc<UniverseState>
UniverseState    { pg_store, llm_runtime, session_tools, token_broker, ... }
```

- One shared `PgPool`; `PgStore::new(pool.clone(), PgStoreConfig::new(id))`
  per universe on first use. `PgStore::ensure_universe` already exists.
- `ActivityState` and the gateway service resolve `UniverseState` per
  call — from `AgentSessionArgs.universe_id` on the worker side, from the auth
  context on the gateway side — instead of holding one universe's singletons.
- The in-memory gateway session metadata cache and the
  `SessionEnvironmentManager` static environment map become per-universe
  (keyed inside `UniverseState`), or statically-injected environments are
  restricted to `single` mode.
- Fleet needs no changes: `AgentApiFleetRuntime` wraps the gateway service, so
  child sessions inherit the parent's universe through the same resolved
  context. This must be asserted by test, not assumed.

### Auth layer: Lightspeed requires a resolved tenant, not a particular auth

The system bakes in one small, stable contract — every dispatched request
carries an `AuthContext`:

```text
AuthContext
  universe_id   UniverseId — the resolved tenant
  principal     PrincipalRef — opaque caller identity, recorded not enforced
```

`dispatch_json_rpc` gains this context parameter. How the context is produced
is a deployment-selected resolution mode at the HTTP edge
(`LIGHTSPEED_AUTH_MODE`):

1. **`single`** (default; today's behavior). Universe pinned by
   `LIGHTSPEED_PG_UNIVERSE_ID`, no credentials. Existing deployments, the
   local stack, and dev flows keep working unchanged.
2. **`trusted-header`** — the bring-your-own-auth interface. The deployment
   sits behind the team's own gateway/proxy, which authenticates however the
   enterprise authenticates and injects `X-Lightspeed-Universe: <uuid>`
   (optionally `X-Lightspeed-Principal: <opaque id>`). The extension point is
   HTTP, not a Rust trait: any upstream auth stack works. This mode also
   covers the plain "no auth, universe supplied in headers" deployment.
3. **`api-key`** — minimal built-in for directly exposed deployments. A
   deployment-level table maps hashed keys to `(universe_id, principal)`.
   Key provisioning is out-of-band initially (SQL or a small server
   subcommand); no self-serve key management API in this item.

JWT/OIDC validation (JWKS, issuer, claim-to-universe mapping) is a natural
fourth mode but is deferred until someone needs it; `trusted-header` already
covers every deployment that has an identity provider.

Rails, regardless of mode:

- **Fail closed.** In `trusted-header` mode a request without the header is
  rejected; there is never a fallback universe. In `api-key` mode an invalid
  or absent key is rejected.
- **No header smuggling.** In `single` and `api-key` modes, incoming
  `X-Lightspeed-*` headers are rejected (or stripped and ignored), so tenant
  claims cannot be injected past a real authenticator.
- **Unauthenticated routes resolve their universe from server-side state,
  never from request-supplied values.** The MCP OAuth callback is hit by
  external providers and cannot carry the header; its `state` parameter
  resolves to an `auth_flows` row that is already universe-scoped, and that
  row is the sole source of the callback's universe.
- **Principal pass-through, no authorization.** The upstream principal is
  stamped into `PrincipalRef` (replacing the hardcoded
  `PrincipalRef::universe_default()` call sites) and recorded on grants and
  flows for audit. Who may do what *within* a universe stays the upstream
  system's decision in this item; native per-principal policy can build on the
  recorded data later.

### Universe provisioning

- `single` mode: the configured universe is ensured at startup (today's
  behavior).
- `trusted-header` mode: config flag `LIGHTSPEED_UNIVERSE_AUTO_CREATE`
  (default off). On: first use of an unknown universe id creates it (the
  upstream gateway is trusted to only forward valid tenants). Off: unknown
  universe ids are rejected and universes are provisioned out-of-band.
- `api-key` mode: creating a key implies/verifies the universe exists.

### Peripheral decisions

- **`store-fs` is declared dev-only single-tenant.** Its path layout has no
  universe segment and gains none in this item.
- **One secrets master key per deployment.** All universes' `auth_secrets`
  stay under the single `LIGHTSPEED_SECRETS_MASTER_KEY`. The existing `key_id`
  column anticipates per-universe or KMS-envelope keys later; out of scope
  here.
- **No cross-universe CAS dedup.** `(universe_id, digest)` keying is a
  deliberate isolation property, not an inefficiency to fix.
- **Messaging bridge** (`interop/messaging`) is universe-blind today and
  reaches one gateway. It gains per-binding gateway credentials/headers (an
  api-key or trusted-header pair per binding), which routes each binding to
  its universe without the bridge learning any new concepts. Bridge work is
  the last phase and can trail the Rust work.

### Migration

Greenfield stance, consistent with project practice: workflow id composition
is unconditional (also in `single` mode, for uniformity), and no compatibility
shim addresses workflows started before this change. Running local/dev
sessions are reset (`local/reset.sh`). Existing single-universe deployments
upgrade by resetting or draining sessions.

## Implementation Phases

### Phase 1 — universe-aware runtime + header modes

The full "multiple universes on one worker" capability, minus built-in
credentials:

1. Composed workflow ids, `universe_id` in `AgentSessionArgs`, updated
   bootstrap assertion, shared default task queue.
2. `UniverseRegistry` over the shared pool; `GatewayAgentApi` and
   `ActivityState` resolve per-universe state per call; per-universe gateway
   metadata and environment maps.
3. `AuthContext` threaded through `dispatch_json_rpc`; `single` and
   `trusted-header` modes with fail-closed and auto-create behavior; OAuth
   callback universe resolution from flow state.
4. Isolation test suite (below).

### Phase 2 — api-key mode and principal pass-through

1. Deployment-level API key table (hashed keys → universe + principal),
   resolution mode, out-of-band provisioning path. Placement: this is the
   first **inbound** auth surface — callers authenticating against Lightspeed,
   as opposed to everything in `crates/auth` today, which is outbound (the
   agent authenticating against other systems). The record type, `ApiKeyStore`
   trait, and hashing helpers still live in `crates/auth` (it owns
   `PrincipalRef`, which is what a key resolves to), in a module named for the
   inbound direction. The table is deployment-scoped, not universe-scoped —
   lookup happens before the universe is known, making it the second
   deployment-level table after `universes` — so its Postgres implementation
   hangs off a deployment-level handle over the shared pool at the gateway
   edge resolver, not off the universe-bound `PgStore` instances. Keys are
   server-generated high-entropy strings (`lsk_<random>`); store a SHA-256
   hash plus a plaintext display prefix — no KDF, no AEAD/master-key
   involvement (the key never needs to be recovered, only recognized).
2. Header rejection in `single`/`api-key` modes.
3. `PrincipalRef` populated from the auth context at the current
   `universe_default()` call sites; recorded on grants/flows.

### Phase 3 — bridge and operations

1. Per-binding gateway credentials/headers in `interop/messaging`.
2. Universe admin surface as needed (list/create via server subcommand or
   gateway method restricted by mode).
3. Docs: README multi-tenancy checkbox, deployment guide for the three modes.

## Non-Goals

- Per-user isolation *within* a universe. Sessions, profiles, and registries
  have no owner column; `list_*` returns the whole universe. `PrincipalRef`
  is recorded for audit only. Native per-principal policy (and the roadmap's
  capability model for agents) is a separate item.
- JWT/OIDC resolution mode.
- Per-universe secrets master keys / KMS envelopes.
- Per-universe Temporal namespaces or task queues; quotas, rate limits,
  billing, noisy-neighbor controls.
- Multi-tenant `store-fs`.
- Self-serve API key management surface.

## Testing Requirements

- **Two universes, one worker, one queue.** Sessions run concurrently in both
  universes on a single worker process; each session's activities read/write
  only its own universe's rows and blobs.
- **Workflow id collision.** The same client-chosen session id started in two
  universes yields two distinct workflows and two distinct sessions; neither
  can signal, query, or read the other.
- **Idempotent adoption is universe-scoped.** `session/start` with an id that
  exists in another universe creates a fresh session in the caller's universe
  rather than adopting the foreign one.
- **List scoping.** Profiles, MCP servers, environment providers, and auth
  grants created in universe A never appear in universe B's list or read
  calls; reads by a foreign universe's ids return not-found, not forbidden
  (no existence leak).
- **Fleet inheritance.** A fleet spawn from a session in universe A creates
  the child in universe A; session links never cross universes.
- **Fail closed.** `trusted-header` mode rejects requests without the header;
  auto-create off rejects unknown universes; `single`/`api-key` modes reject
  requests carrying `X-Lightspeed-*` headers.
- **api-key resolution.** Valid key resolves universe and principal; invalid,
  revoked, and absent keys are rejected; the recorded `PrincipalRef` on a
  grant created through an authenticated call matches the key's principal.
- **OAuth callback.** A callback for a flow started in universe A lands its
  grant in universe A with no tenant header present on the callback request.
- **Separator invariant.** `validate_session_id` rejects `/`; a test pins
  this with a comment naming the workflow-id composition as the reason.
- Isolation tests use the standard Postgres-backed test setup and must be
  parallel-safe (unique universe ids per test).
