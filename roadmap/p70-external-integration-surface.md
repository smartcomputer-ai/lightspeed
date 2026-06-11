# P70: External Integration Surface

**Status**
- Proposed 2026-06-11.
- Builds on the P58/P60 JSON-RPC gateway and API shape, P63/P65 skills and
  prompts, and P67–P69 MCP/auth.
- Motivated by the first private deployment: the gateway runs on owned infra,
  reachable over a private network (for example Tailscale, no public IP), and
  private Python/TypeScript Temporal workflows orchestrate Forge sessions as
  one step in larger workflow systems.

## Goal

Make Forge consumable from an external, polyglot workflow ecosystem without
adding anything to Forge that knows about that ecosystem:

- a machine-readable API contract exported from the Rust types (schemars),
  with generated TypeScript and Python clients;
- an idempotency key on `run/start` so external retry machinery (Temporal
  activity retries, HTTP retries) cannot start duplicate runs;
- long-poll support on `session/events/read` so "start a run, await its
  result" is one clean call loop instead of tight polling.

The external integration story is: external workflow -> activity ->
JSON-RPC over the private network -> Forge gateway -> Forge's own Temporal.
Forge's use of Temporal stays an implementation detail behind `api`.

## Design Position: Integrate Via The API, Not Workflow-To-Workflow

External Python/TS Temporal workflows must call the JSON-RPC gateway from
activities. They must not signal `AgentSessionWorkflow` directly, even though
the payloads are plain JSON and a foreign-language Temporal client could
technically do it:

- the `submit_admission` signal payload is a `DynamicCommand` wrapping
  `CoreAgentCommand` — engine-internal, versioned but not a public contract.
  The "keep clients on `api`" rule exists precisely for this boundary;
- direct signals bypass gateway admission validation (input validation, CAS
  blob handling, provider-param checks) and would write unvalidated commands
  into the event-sourced session log;
- there is no payoff: the workflow never completes (it loops with
  continue-as-new), so child-workflow await semantics do not apply. Direct
  integration would still be signal-and-poll, which the HTTP API already
  provides with validation.

Consequences worth stating as product contract:

- the external workflow ecosystem can run on its own Temporal cluster and
  namespace; only HTTP reachability to the gateway is required;
- `AgentSessionWorkflow` signal/query names, DTOs, task queue, and
  continue-as-new behavior remain free to change without notice;
- everything an external system needs must therefore be expressible through
  `api` — gaps are fixed by extending `api`, never by reaching around it.

## Current State

- Transport: HTTP POST `/rpc`, JSON-RPC 2.0 style; `/health`; OAuth callback
  routes. ~51 methods dispatched in `crates/api/src/lib.rs`
  (`dispatch_json_rpc`, `METHOD_*` constants), plus server-to-client
  notification kinds (`NOTIFY_*`).
- Types are Rust serde structs only; no schema export, no non-Rust client.
  The CLI (`crates/cli/src/api_client.rs`) is the only API consumer.
- Events are pull-only: `session/events/read` with `EventCursor` pagination.
  No wait/long-poll parameter, no SSE/WebSocket.
- `RunStartParams` has no client-supplied idempotency key. The engine already
  threads `Option<SubmissionId>` through `CoreAgentCommand::RequestRun` into
  `RunEvent::Accepted` (`crates/engine/src/core/admit.rs`), but the gateway
  never sets it and nothing deduplicates on it.
- The gateway has no per-request auth. For this deployment the network
  boundary (Tailscale ACLs) is the access control; see Non-Goals.

## G1: Schema Export From `api`

Derive `schemars::JsonSchema` across the public types in `crates/api` and
export a committed, machine-readable contract.

Design notes:

- emit **draft-07** JSON Schema (schemars `SchemaSettings`), not the 2020-12
  default — downstream generators (`json-schema-to-typescript`, quicktype,
  `datamodel-code-generator`) handle draft-07 far more reliably;
- types with hand-written `Serialize` impls need matching manual `JsonSchema`
  impls; the existing tagged-enum + camelCase serde style derives cleanly;
- add a **method manifest**: a static table in `crates/api` next to the
  `METHOD_*` constants mapping `method name -> (params type, result type)`,
  plus the `NOTIFY_*` notifications mapped to their payload types. A unit
  test must assert the manifest and `dispatch_json_rpc` cover exactly the
  same method set, so the exported contract cannot drift from what the
  server serves;
- an export binary (`cargo run -p api --bin export-schema`) writes:
  - `schemas/api.schema.json` — all params/result/view types bundled under
    `$defs`;
  - `schemas/methods.json` — the method/notification manifest;
  - `schemas/openrpc.json` — an OpenRPC document assembled from the two.
    OpenRPC is the standard JSON-RPC contract format and feeds the OpenRPC
    Inspector/Playground for free docs, but nothing downstream may depend on
    OpenRPC tooling for codegen — its generator ecosystem is thin;
- commit the artifacts; a CI test re-exports and diffs so a type change that
  alters the wire contract fails the build until the artifacts (and
  regenerated clients, G4) are refreshed.

Acceptance criteria:

- [x] all types reachable from any method's params/result derive or implement
  `JsonSchema` (all 201 serializable `api` types derive it);
- [x] manifest <-> dispatch drift test exists and fails when a method is
  added to one but not the other — superseded by a stronger guarantee: a
  single `api_methods!` macro table generates `dispatch_json_rpc`,
  `method_manifest()`, and the per-method schema registration, so the three
  cannot drift by construction (a manifest uniqueness/count test remains);
- [x] `export-schema` is deterministic (stable ordering) so the CI diff test
  is meaningful (definitions collected into a `BTreeMap`; verified by
  double-run diff);
- [x] exported schemas round-trip: a serialized example of each top-level
  params/result type validates against its exported schema (spot-check via
  test fixtures, not exhaustively — `RunStartParams`,
  `AgentApiOutcomeOfRunStartResponse` incl. a notification,
  `SessionEventsReadParams`, validated with the `jsonschema` dev-dependency).

Implemented 2026-06-11: `schemars` derives across `crates/api` (generic
definition names templated via `#[schemars(rename = "AgentApiOutcomeOf{T}")]`
/ `FieldPatchOf{T}` to avoid collision-counter names), the `api_methods!`
macro (replacing ~260 lines of hand-written dispatch), `NOTIFICATION_METHODS`
with a schema-variant drift test, the `schema_export` module +
`export-schema` binary, committed `schemas/api.schema.json` (248 definitions)
/ `methods.json` (51 methods) / `openrpc.json`, and the
`schema_artifacts` currency + ref-resolution + fixture-validation tests.
The result envelope on the wire is `AgentApiOutcome<Response>`; manifest and
schemas describe the envelope, not the bare response type.

## G2: Idempotent `run/start`

Expose a client-supplied submission id and deduplicate deterministically, so
any retry layer (Temporal activity retry, HTTP client retry, gateway signal
retry) is safe.

Design notes:

- add optional `submissionId` to `RunStartParams`; the gateway passes it into
  `CoreAgentCommand::RequestRun` (plumbing already exists end-to-end);
- dedup belongs in the deterministic core's admit path, not in gateway
  memory: `CoreAgentState` tracks the submission ids of accepted runs, and a
  `RequestRun` whose `submission_id` matches an already-accepted run admits
  to **no new event** (idempotent accept), regardless of which retry layer
  re-delivered it. Gateway-side dedup alone cannot cover duplicated signal
  delivery into the workflow;
- response semantics: on a duplicate, `run/start` returns the existing run's
  `RunView` (the gateway resolves the run by submission id from projected
  state) — idempotent success, not an error. Submitting the same
  `submissionId` with *different* input/config is a client bug and fails
  with a typed invalid-request error rather than silently returning the
  prior run;
- `session/start` already accepts a client-supplied `session_id`; define and
  test its retry semantics to match (re-calling with an existing id returns
  the existing session view instead of failing), so the "create session +
  start run" activity pair is retryable end to end;
- scope: submission ids are unique per session, not globally.

Acceptance criteria:

- [x] `run/start` with a repeated `submissionId` (same input) returns the
  original run and appends no second `RunAccepted` event, verified at the
  session-log level (engine drive tests assert no appended events for
  queued and completed originals; the live suite asserts the same run id
  comes back);
- [x] `run/start` with a repeated `submissionId` and different input fails
  with a typed error (`CommandRejectionKind::DuplicateSubmission` ->
  admission failure -> API `Rejected`);
- [x] duplicate admission delivered at the workflow layer (simulated
  re-signal) also results in no second accepted run — the dedup lives in
  the deterministic admit path the workflow itself drives
  (`drive.admit_command`), so re-signals are covered by the same engine
  tests;
- [x] `session/start` with an existing `session_id` is idempotent and
  documented as such (config on the retried call is ignored; session config
  applies only at creation; a closed session returns its closed view);
- [x] CLI passes a generated submission id by default.

Implemented 2026-06-11: engine dedup in `CoreAdmitCommand` (duplicate check
precedes all other admission checks so retries resolve even after state
moves on), `RunRecord.submission_digest` (FNV-1a over the serde_json payload,
computed in the reducer at completion — not stored in events) for
input-equality checks against completed runs, `submissionId` on
`RunStartParams`, gateway passthrough with `SubmissionId` validation, and
idempotent `start_session` (the previous `open_or_start_session` wrapper
behavior moved into the wire method; the wrapper now delegates). The
duplicate-response path reuses the existing `wait_for_run_accepted`
submission-id correlation unchanged.

## G3: Long-Poll `session/events/read`

Add `waitMs` to `SessionEventsReadParams` so event consumers block server-side
instead of tight-polling.

Design notes:

- semantics: if events exist past the `after` cursor, return immediately;
  otherwise hold the request until an event lands or `waitMs` elapses, then
  return a normal (possibly empty) page with an unchanged cursor. Zero or
  absent `waitMs` preserves today's immediate-return behavior exactly;
- cap `waitMs` server-side (default cap ~30s, deployment-configurable); the
  gateway HTTP request timeout must exceed the cap;
- implementation: a simple internal store-poll (~250ms) to satisfy waiters is
  enough at this scale; upgrade the wakeup path to Postgres `LISTEN/NOTIFY`
  only if measured load demands it. The long-poll parameter is the contract;
  the internal wakeup mechanism is not;
- no separate `run/wait` method: "await run completion" is a loop over
  long-poll `session/events/read` until the run's terminal event, which the
  G4 clients wrap as a helper. One primitive, not two;
- SSE/WebSocket is explicitly deferred (see Non-Goals). The cursor design
  keeps it cheap later: an SSE endpoint can serve the same event stream with
  `Last-Event-ID` mapped to `EventCursor.seq`, so nothing built here is
  throwaway. Long-poll is also the better fit for the primary consumer:
  Temporal activities want discrete, retryable, idempotent calls, not held
  streams fighting heartbeat/retry semantics. Token-level streaming is an
  event-granularity question (delta events in the engine model), not a
  transport question, and is out of scope here;
- a long-poll request parked on the gateway must observe `session/close` and
  return rather than hold until timeout.

Acceptance criteria:

- [x] `waitMs` returns early when an event arrives mid-wait and empty at the
  cap otherwise, with cursor semantics identical to the immediate path (the
  loop re-runs the exact existing read; the live suite asserts a parked
  read at head holds for its wait and returns an empty complete page);
- [x] `waitMs` above the server cap is clamped, not rejected (clamped via
  `min` against the cap before the deadline is computed);
- [x] concurrent long-poll readers on the same session each receive the new
  events (reads stay independent cursor reads; parking adds no consume
  semantics);
- [x] CLI chat uses the long-poll path and drops its client-side sleep loop.

Implemented 2026-06-11: `waitMs` on `SessionEventsReadParams`, gateway park
loop in `read_session_events` (store re-read every
`min(poll_interval, 250ms)`, `events_wait_cap` on the builder, default 30s),
and the CLI follow loop now passes a 2s wait on its drains instead of
sleeping 250ms between them (first drain stays immediate to flush backlog).
Session close wakes parked readers naturally because closing appends a
lifecycle event. `run/wait` was not added, per the one-primitive decision.

## G4: TypeScript And Python Clients

Generated types plus a thin hand-written transport, per language.

Design notes:

- layout: top-level `clients/typescript/` and `clients/python/`, outside the
  cargo workspace. Private consumption only (path/git/private-registry
  installs); no npm/PyPI publishing;
- types are **generated, never hand-written**: `json-schema-to-typescript`
  for TS, `datamodel-code-generator` (Pydantic v2) for Python — or quicktype
  for both if one toolchain wins; decide during implementation. Inputs are
  the G1 artifacts only;
- the transport is deliberately tiny (~50 lines per language): `call(method,
  params)` doing the POST, JSON-RPC `error` mapped to a typed exception
  carrying code/message/data, plus a typed method map generated from
  `methods.json`. JSON-RPC needs no routing/verb/status-code modeling, which
  is what makes full-client codegen unnecessary;
- convenience helpers (hand-written, thin, on top of typed calls):
  - `startRun(input, {submissionId, config})` — generates a submission id
    when the caller does not supply one;
  - `readEvents(after, {waitMs})` — one long-poll page;
  - `awaitRun(sessionId, runId, {after})` — long-poll loop until the run's
    terminal event, returning the terminal state and final cursor. Designed
    to be resumable from a cursor so a retried Temporal activity continues
    instead of re-reading from the start;
- each client ships a short Temporal-activity usage example (create/reuse
  session, idempotent `run/start`, `awaitRun` with heartbeats);
- regeneration: `clients/*/generate.sh` (or task runner) from the committed
  G1 artifacts; CI verifies generated output is current alongside the G1
  schema diff.

Acceptance criteria:

- [ ] both clients are fully generated-or-trivial: no hand-maintained type
  definitions;
- [ ] integration smoke test per language against a running gateway
  (`#[ignore]`-equivalent opt-in, mirroring the live-test convention):
  session/start -> run/start (idempotent retry asserted) -> awaitRun;
- [ ] JSON-RPC errors surface as typed exceptions with the server error
  code/data preserved;
- [ ] CI fails when committed schemas and generated clients drift.

## Deployment Notes: Universes

The universe is a server-side deployment binding, not a wire concept. The API
and the G4 clients carry no universe parameter: a client selects a universe by
selecting a gateway base URL.

- One universe per gateway+worker deployment, bound at startup via
  `FORGE_PG_UNIVERSE_ID`. This is a deliberate invariant, not a limitation:
  P69's auth/store isolation holds "by construction" because every store and
  broker is instantiated universe-bound. Multi-universe workers would convert
  that guarantee into per-call discipline (universe ids threaded through
  workflow args, activity DTOs, and every store access) and are deferred until
  fleet/multi-tenancy makes many universes real.
- The default Temporal task queue derives from the universe:
  `forge-universe-{FORGE_PG_UNIVERSE_ID}` (`FORGE_TASK_QUEUE` remains as an
  explicit override), replacing the static `forge-agent` default. Workflow
  IDs are bare session ids with no universe prefix, so queue-per-universe is
  the mechanism that keeps one deployment's workers from picking up another
  universe's sessions against the wrong store; deriving the queue from the
  universe makes that isolation impossible to misconfigure silently. Gateway
  and worker derive the same default from the shared config path.
  Implemented 2026-06-11 (`task_queue_from_env` in `temporal-server`).

## Non-Goals

- **Gateway request auth.** Deliberately deferred for this deployment; the
  private network (Tailscale ACLs) is the access boundary, which means a
  misconfigured ACL is fatal. A static bearer check on `/rpc` is the first
  thing to add when exposure widens beyond a personal tailnet, and the G4
  client transports should accept an optional bearer token from day one so
  adding it later is config, not code.
- **SSE/WebSocket event transport.** Deferred; see G3.
- **Workflow-to-workflow bindings.** Signaling `AgentSessionWorkflow` from
  external Temporal clients is explicitly unsupported, not merely
  undocumented; see Design Position.
- **Public SDK publishing, semver, or compat guarantees.** The exported
  schema is a point-in-time contract for private clients regenerated in
  lockstep; cross-version compatibility policy arrives when there are
  external consumers not regenerated from this repo.

## Future Work

- SSE endpoint over the same event stream (`Last-Event-ID` = cursor seq) for
  browser UIs with many concurrent watchers.
- Delta/streaming item events in the engine event model (token streaming).
- Postgres `LISTEN/NOTIFY` wakeup for long-poll at scale.
- Bearer-token gateway auth, then real principals (P69 already reserves
  `PrincipalRef`).
- Schema/version negotiation via `initialize` once external consumers stop
  being regenerated in lockstep.
