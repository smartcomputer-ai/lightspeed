# P132 — Workflow Contract: Generated Emission Protocol for Controllers

**Status**

- Implemented 2026-08-25. Rust now exports and staleness-checks the schema,
  manifest/vectors, and integrator reference; the TypeScript client ships the
  pure `@lightspeed-ai/client/workflow` subpath; Bots and Channels consume
  it and their handwritten emission contracts and workflow-id ports are gone.
  The client vector suite, both platform unit suites, and the Bots/Channels
  Temporal integration suites pass after cutover.
- Builds on P100/P100b/P106 (emission spine, producer authorization,
  push/pull dispatch, start-on-call recipes) and P124 (generated contract
  enforcement across languages). P124 already asked for this in one line —
  "Replace the duplicated Channels `contracts/emissions.ts` shapes with
  generated-client exports … Do not introduce any new platform-local copy of
  Rust wire vocabulary" — and Bots introduced a second copy anyway, because
  nothing generated existed to import.

## Why

Controllers (Bots, Channels, any P100b plugin) are Temporal workflows in the
core namespace. Core pushes them `deliver_emission` signals — run terminals
with tokens, pushed tool invocations, cancellation notices — and they answer
joined tools by signalling `deliver_emission` back at the session workflow
with a `source_resolution`. That is deliberate, not incidental: P100 rejected
a raw `signal(workflow_id, name, json)` surface as "difficult to authorize",
and P100b made *authority itself* a Temporal identity — the session stores
the exact producer workflow id at managed-session admission and rejects
resolutions from anyone else (`process_pending_source_resolutions` in
`crates/temporal-workflow/src/workflows/session/promise_sources.rs`). The
transport stays.

What is wrong is where the contract lives. The envelope is `engine::emission`
plus `temporal_workflow::types`; the `api` crate does not depend on `harness`
(clients stay on `api` by design) and `harness` has no `schemars`, so none of
it reaches `crates/api/contract/` or the generated TypeScript client. Every
TypeScript receiver therefore hand-mirrors it:

- `platform/bots/src/contracts/emissions.ts` and
  `platform/channels/src/contracts/emissions.ts` — ~340 lines each, a
  ~30-line diff between them (naming), both re-typing `EmissionEnvelope`,
  `EmissionProducer`, `EmissionBody`, `WorkflowToolInvocation`,
  `PromiseResolution`, `RunStatus` and re-implementing
  `parseEmissionEnvelope`;
- `sourceResolutionEmissionId` — a by-hand port of
  `EmissionId::for_source_resolution` (sha256 over the
  `lightspeed.emission.v1` domain with u64-be length-prefixed parts). Drift
  here does not fail loudly: the session dedupes and authorizes by that id,
  so a wrong derivation is a silently dropped reply;
- `lightspeedSessionWorkflowId` in `platform/bots/src/contracts/bots.ts` —
  a port of `compose_workflow_id`;
- the reserved `reply` completion key convention (`replyPromiseId` /
  `joinedReplyPromiseId`), stated only in TypeScript comments.

The third receiver copies again. P132 makes the workflow-side protocol a
generated contract with the same enforcement the API contract has, and
deletes the mirrors.

## The contract

Everything a receiver needs to *speak* to core, and nothing the session
worker keeps private (activity DTOs, `AgentSessionArgs`, admissions):

| Group | Items |
|---|---|
| Signal | `deliver_emission` on `AgentSessionWorkflow` and `EnvironmentJobWorkflow` |
| Envelope | `EmissionEnvelope`, `EmissionProducer::{session, workflow}`, `EmissionBody::{run_terminal, source_resolution, tool_invocation, invocation_cancellation}`, `WorkflowToolInvocation`, `PromiseResolution`, `RunStatus`, and the id/ref newtypes they carry (schema'd via their string form) |
| Id derivations | `EmissionId::{for_run_terminal, for_source_resolution, for_tool_invocation, for_invocation_cancellation}`; canonical-id validation (`emission:sha256:` prefix); the reserved `reply` completion key |
| Workflow ids | `compose_workflow_id` (`<universe>/<session>`), `compose_environment_job_workflow_id`, `split_workflow_id` |
| Start-on-call | `WorkflowToolStartArgs`, `WORKFLOW_TOOL_RECOVERY_QUERY` + `WorkflowToolRecoveryResult`, `WorkflowToolRecipeV1` + `WORKFLOW_TOOL_RECIPE_FORMAT_V1` + `workflow_tool_recipe_fingerprint` (`wtr:sha256:`), `WORKFLOW_TOOL_EXECUTION_KIND` |

One field is added rather than exported: bound pushed
`WorkflowToolInvocation`s gain `holder_workflow_id` (start args already
carry it), so receivers stop composing the session workflow id themselves.

## Design

### 1. Rust is the source of truth

Derive `JsonSchema` on the exported types where they are defined, so the
schema *is* the serde shape and cannot drift. `harness` gains `schemars`
behind a cargo feature `contract` (a derive, no runtime behaviour; the
determinism rule is about side effects, not derives). `temporal-workflow`
enables `harness/contract`, derives on its own contract types, and exposes
`workflow_contract::export()` built the way `api::export_schemas()` is: a
Draft-07 generator, every type under `definitions`, plus a small constants
manifest and a generated reference. Mirror DTOs inside Rust were considered
and rejected — the same drift risk one hop earlier.

### 2. Committed artifacts and a staleness gate

`cargo run -p temporal-workflow --bin export-workflow-contract` writes
`crates/temporal-workflow/contract/`:

- `workflow.schema.json` — the type bundle;
- `workflow.json` — constants (signal and query names, recipe formats,
  execution kind, id prefixes and domains, workflow-id templates) **and
  known-answer vectors** for every derivation: an emission id of each kind,
  a recipe fingerprint, composed/split workflow ids. Vectors are the drift
  guard for the one thing a schema cannot express — hashing;
- `workflow-contract.md` — the integrator reference: transport, envelope
  semantics, producer authorization, push vs pull, the recovery query, the
  `reply` key, dedupe expectations, and what the lifecycle controller
  gates (terminal routing, non-branchability, the self-receiver deadline)
  versus what any tool receiver may do. Today this exists only in
  archived P-docs and code comments.

`cargo test -p temporal-workflow` fails while the committed artifacts are
stale, exactly as `cargo test -p api` does for the API contract.

### 3. Generated TypeScript

`clients/typescript/scripts/generate.mjs` additionally compiles
`workflow.schema.json` into `src/generated/workflow-types.ts`, and the
client ships a `./workflow` subpath (`@lightspeed-ai/client/workflow`):

- the generated types;
- `DELIVER_EMISSION_SIGNAL`, `WORKFLOW_TOOL_RECOVERY_QUERY`, recipe
  constants — read from `workflow.json`, never retyped;
- `parseEmissionEnvelope`, `sourceResolutionEnvelope`, `emissionId.*`,
  `sessionWorkflowId`, `environmentJobWorkflowId`, `splitWorkflowId`,
  `recipeFingerprint`, `replyPromiseId` — written once, asserted against
  the committed vectors in the client's test suite.

Constraints: the module imports no `@temporalio/*` package (types and pure
functions only, usable from workflow code and activities alike) and uses a
pure-JS synchronous sha256 (`@noble/hashes`, already a Bots dependency) so
it runs inside the Temporal workflow sandbox and in browsers. `npm run
check:generated` covers the new outputs.

### 4. Cutover

Bots and Channels delete `contracts/emissions.ts`,
`lightspeedSessionWorkflowId`, and their local derivation tests, importing
from the client instead; `platform/channels/test/emissions.test.ts` becomes
the client's vector test. The Rust plugin live suite
(`workflow_tool_plugins_live`) already uses the crate types and is
unaffected.

### 5. Documentation

README's "Managed sessions and workflow-backed tools" bullet links
`workflow-contract.md`; AGENTS.md lists the regeneration command beside
`export-schema`; P124's open line about the Channels copy is marked done.

## Deferred: an API reply path

Not in P132. A pure-HTTP receiver (the A2A adapter, a serverless handler)
would need `session/workflowTools/resolve {sessionId, invocationId, key,
resolution}` plus polling `session/events/read` instead of push. The open
question is authority: an HTTP caller cannot present a workflow id. The
P100b-faithful answer is a reply capability declared at
`session/managed/start` (hash stored on the binding, presented on the API
call) so authority stays fixed at admission; universe-level auth alone would
let any session driver fabricate tool replies. Build it when such a receiver
exists; P132's generated types are its input either way.

## Slices

1. Rust: feature-gated derives, `workflow_contract::export()`, export bin,
   vectors, staleness test.
2. TypeScript: generator extension, `./workflow` subpath, vector tests,
   `check:generated`.
3. Cutover: Bots and Channels on the generated module, mirrors deleted,
   integration suites green (`npm run test:integration:bots`,
   `npm run test:integration:channels`), docs.

Each slice is under a day; 1 and 2 can land together.

## Tests

- Rust: vectors assert every derivation; schema round-trips for each
  envelope body; committed-contract staleness.
- TypeScript: the same vectors through the client module;
  `parseEmissionEnvelope` rejects unknown body kinds and non-canonical ids
  (today's Channels cases, moved).
- Platform: the existing Bots/Channels Temporal integration scenarios,
  unchanged — the point of the slice is that behaviour does not move.

## Non-goals

- Changing the envelope, the signal, or producer authorization.
- Exporting session-worker activity DTOs or admission types.
- A shared TypeScript controller library (P130's `controller-kit`) — a
  separate extraction; P132 only gives it a typed foundation.
- The API reply method above.
