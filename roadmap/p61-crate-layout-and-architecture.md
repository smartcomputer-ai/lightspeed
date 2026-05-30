# P61: Product Crate Layout And Architecture

**Status**
- Accepted direction
- Implemented

**Progress**
- First mechanical crate rename slice completed:
  `agent-api -> api`, `agent-projection -> api-projection`,
  `agent-core -> engine`, `agent-tools -> tools`, and `agent-eval -> eval`.
- Imports, dependency keys, workspace members, and active repository docs have
  been updated to the new names.
- The prototype Temporal package has been split into `workflow`, `worker`, and
  `gateway`.
- Public deployable names, server metadata, task queue defaults, and local dev
  env vars have moved off the Claw codename.
- `agent-local` has been removed as a runtime. Its runner-only pieces now live
  in `test-support`, which intentionally exposes no `AgentApiService`.
- The CLI is API-first and requires a gateway URL via `--api-url` or
  `FORGE_API_URL`.
- The API now has typed session/run generation config inputs, the CLI sends
  draft settings through `run/start`, and gateway `run/start` returns after
  run acceptance/start instead of waiting for final completion.
- The engine's built-in domain module has moved from `src/core_agent` to
  `src/core`, and dynamic envelope kinds now use the `forge.core.*` namespace
  without backwards compatibility for the old `forge.core_agent.*` names.
- Workflow admission now wraps a single encoded CoreAgent command. Gateway owns
  `run/start` input CAS writes and `CoreAgentCommand::RequestRun` construction;
  the old workflow-local text-run admission variant has been removed.
- The CLI no longer depends on `engine`; public session id validation now lives
  at the `api` boundary.

**Decision, 2026-05-30**

Forge is pivoting from a general agent SDK posture toward a single hosted agent
product built around a Temporal-backed runtime. The deterministic event-sourced
core still matters, but it should be treated as the product's engine rather
than a public SDK kernel.

Breaking changes are acceptable. We should move relatively fast toward the
desired architecture instead of preserving compatibility with the current
`claw`/local-runtime split.

## Goal

Restructure the workspace so the crate layout matches the product runtime:

```text
client
  -> api
  -> gateway
  -> workflow
  -> worker activities
  -> engine
  -> store-pg / CAS
```

The hosted Temporal path should become the primary path. Local in-process
execution should no longer be maintained as a parallel production runtime.

## Desired Workspace

```text
crates/api/              client protocol DTOs, method constants, JSON-RPC envelopes/errors
crates/api-projection/   engine log/state -> api views
crates/engine/           deterministic reducer, event log domain, drive machine
crates/workflow/         Temporal workflow, signals, queries, activity DTOs
crates/worker/           Temporal worker binary and activity implementations/wiring
crates/gateway/          HTTP/JSON-RPC gateway over Temporal + Pg/CAS
crates/tools/            host/tool package
crates/llm-clients/      provider-native OpenAI/Anthropic clients
crates/llm-runtime/      product LLM adapters from engine requests to llm-clients
crates/store-pg/         hosted Pg session log + CAS
crates/store-fs/         local/dev filesystem store, if still needed
crates/cli/              command-line client, API-first
crates/eval/             in-process core-loop eval harness via test-support
crates/test-support/     optional fast harness for tests/evals, not a production runtime
```

`test-support` should exist only if it keeps tests and evals meaningfully fast.
It should not expose a second supported `AgentApiService` implementation.

## Rename Map

```text
crates/agent-api/        -> crates/api/
crates/agent-projection/ -> crates/api-projection/
crates/agent-core/       -> crates/engine/
agents/claw/             -> split across workflow, worker, and gateway
crates/agent-tools/      -> crates/tools/
crates/agent-eval/       -> crates/eval/
crates/agent-local/      -> remove or repurpose as crates/test-support/
```

The `claw` name should not remain as a crate or deployable name. It was useful
as a prototype codename, but the product architecture should use role-oriented
crate names.

## Crate Responsibilities

### `api`

Own the stable client boundary:

- session/run/item DTOs
- method constants
- JSON-RPC request/response/notification envelopes
- API error kinds and transport-neutral service traits

`api` must not depend on `engine`, Temporal, stores, provider clients, tools, or
gateway code.

### `api-projection`

Project committed engine state and log entries into `api` views.

It may depend on:

- `api`
- `engine`
- blob-store read traits needed to materialize user-visible text

It must not admit commands, append events, execute tools, call LLM providers, or
talk to Temporal.

If the gateway becomes the only user of this crate, we can later fold it into
`gateway`, but projection logic should not move into `api`.

### `engine`

Own the central deterministic machinery:

- session ids, event positions, dynamic event envelopes, codecs
- CoreAgent commands, events, state, reducer logic, planning
- context-window planning and compaction records
- provider-native LLM request records required by the reducer
- tool request/result records required by the reducer
- substrate-neutral drive actions

`engine` must stay deterministic. It must not perform provider calls, shell
commands, filesystem operations, network I/O, Temporal activities, or database
I/O.

The rename from `agent-core` to `engine` is intentional. The crate is the
central product engine, not a generic external SDK core.

### `workflow`

Own Temporal workflow code and workflow-facing types:

- workflow struct and deterministic workflow loop
- signals and queries
- workflow args
- activity request/response DTOs
- Temporal helper types that must be shared by gateway and worker

`workflow` should contain orchestration logic only. Activity implementations and
process wiring belong in `worker`.

### `worker`

Own the Temporal worker process:

- worker binary
- activity implementations
- Pg/CAS dependency construction
- LLM runtime construction
- tool runtime construction
- environment/config parsing for worker-owned dependencies

The worker depends on `workflow`, but the gateway should not depend on worker
activity implementations.

### `gateway`

Own public serving:

- HTTP/JSON-RPC server
- `api` service implementation
- Temporal client start/signal/query calls
- Pg/CAS projection reads
- request validation and API error mapping
- future auth, tenancy, rate limits, and streaming transports

The gateway depends on `workflow` for signal/query/workflow types. It does not
register workers or run activities.

### `tools`

Own host/tool packages used by the product:

- filesystem/process tools
- host target abstractions
- tool catalogs/profiles
- tool runtime helpers

It may depend on `engine` for tool request/result types, but it should not
depend on `gateway` or `workflow`.

### `llm-clients`

Remain provider-native clients. Do not turn this into a universal LLM model.

### `llm-runtime`

Own product adapters from engine-planned LLM requests to provider-native
`llm-clients` calls.

If the adapter layer becomes mostly worker-specific, it can remain a separate
crate while it has real tests and reuse. It should not grow into an SDK layer.

### `cli`

Become API-first.

The normal CLI path should talk to `gateway`. Local mode should not require a
separate production runtime; local development should run the local Temporal
stack, worker, and gateway. A fast in-process path may remain only as
test-support or an explicitly marked dev shortcut.

### `eval`

Evaluate the core agent loop in process through `test-support`. The eval
harness is for prompt/tool behavior against the deterministic engine and live
provider adapters, not for Temporal/gateway durability semantics.

End-to-end product API or hosted workflow evals may be added as a separate mode
when needed, but `eval` should not grow a second public `AgentApiService` or
local runtime facade.

## Dependency Shape

Target dependencies:

```text
api

engine

api-projection
  -> api
  -> engine

workflow
  -> engine
  -> temporalio-sdk / temporalio-macros

worker
  -> workflow
  -> engine
  -> store-pg
  -> llm-runtime
  -> tools

gateway
  -> api
  -> api-projection
  -> engine
  -> workflow
  -> store-pg

tools
  -> engine
  -> host-protocol / host-client as needed

llm-runtime
  -> engine
  -> llm-clients

cli
  -> api
```

The CLI must not depend on engine/runtime crates.

## Local Runtime Position

`agent-local` is currently useful as a fast harness, but it duplicates too much
product behavior:

- a second `AgentApiService` implementation,
- a second drive fulfillment loop,
- local lifecycle semantics that can diverge from the hosted path,
- duplicated error-result helpers and notification assembly.

Do not keep it as a supported runtime. Either remove it or repurpose the useful
pieces into `test-support`.

`test-support` may provide:

- in-memory stores,
- scripted LLM/tool executors,
- helper functions for opening sessions and driving the engine,
- assertion helpers for projected API views.

It should not provide:

- a production `api` service,
- a second gateway,
- local-only lifecycle semantics that clients rely on.

## API Position

Keep the public API product-shaped and typed for now:

```text
initialize
session/start
session/update
session/read
session/events/read
session/close
run/start
run/cancel
```

Do not move to a generic command/query/interface protocol until a second real
product shape or cross-language SDK pressure exists.

The hosted API uses asynchronous run semantics:

```text
run/start -> accepted queued/running run
client follows session/events/read or streaming notifications
```

The gateway waits only until the submitted run is visible as active or
terminal. One-shot terminal output should be produced by clients such as the
CLI, not by extra gateway helper binaries.

`session/start` carries product-level config fields rather than exposing
CoreAgent's internal `SessionConfig` directly. `session/update` is patch-shaped
and revision-checked, so clients can update instructions, model/generation,
context, or run defaults without read/merge/write replacement races.

## Migration Plan

Move in coarse, breaking slices:

1. Done. Rename `agent-api` to `api` and update imports.
2. Done. Rename `agent-projection` to `api-projection` and update imports.
3. Done. Rename `agent-core` to `engine` and update imports.
4. Done. Split `agents/claw` into `workflow`, `worker`, and `gateway`.
5. Done. Move activity implementations and worker binary into `worker`.
6. Done. Move gateway API/HTTP code into `gateway` and remove the Claw naming
   from public server metadata and binaries.
7. Done. Rename `agent-tools` to `tools`.
8. Done. Rename `agent-eval` to `eval`; keep it as an in-process core-loop
   harness through `test-support`, without exposing a product API service.
9. Done. Remove `agent-local` as a runtime, or extract only test helpers into
   `test-support`.
10. Done. Make the CLI API-first and remove local runtime construction from the
    default path.

During this migration, prefer deleting obsolete compatibility shims over
preserving old crate names or duplicate paths.

## Non-Goals

- Preserve the `claw` crate or binary names.
- Preserve `agent-local` as a public runtime.
- Publish these crates as a stable SDK.
- Introduce a generic fleet API, marketplace, plugin registry, or
  cross-language reducer interface.
- Move projection logic into the client `api` crate.
- Hide Temporal behind an abstract workflow-engine layer before the product path
  proves what it needs.

## Done When

- The workspace crate names reflect the desired product roles.
- The hosted path is the primary execution path.
- Worker and gateway are separate deployables.
- Gateway and worker share workflow types through `workflow`, not through each
  other.
- The CLI can use the product API without depending on local engine/runtime
  crates.
- Any remaining fast in-process harness is clearly test support, not a second
  supported runtime.
