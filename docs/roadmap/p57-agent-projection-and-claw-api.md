# P57: Agent Projection And Claw API

**Status**
- Completed

**Progress**
- Design direction identified after P56.
- Added `crates/agent-projection` as the shared CoreAgent-to-`agent-api`
  projection crate.
- Refactored `agent-local` to use `agent-projection` while keeping local
  session lifecycle, runner driving, and notification assembly local.
- Added `ClawAgentApi` in `agents/claw/src/api.rs` as a Temporal/Pg-backed
  `AgentApiService` gateway.
- Rebuilt `claw-submit` on `ClawAgentApi::start_run`.
- Updated the ignored fake Temporal live test to cover `initialize`,
  `start_session`, `start_run`, `read_session`, and `read_session_events`
  through the API gateway.

## Goal

Add a shared CoreAgent-to-`agent-api` projection layer and use it to expose
Claw through the existing client-facing `AgentApiService` contract.

P56 made CoreAgent driving reusable across local and Temporal substrates. The
next duplicated surface is projection: `agent-local` already knows how to turn
CoreAgent state, events, blob refs, and tool records into `agent-api` session,
run, item, and event views. Claw needs the same projection for a Temporal-backed
API gateway.

The target shape is:

```text
agent-api:
  client-facing protocol types and AgentApiService trait
  no agent-core dependency

agent-projection:
  CoreAgent -> agent-api projection helpers
  depends on agent-core and agent-api

agent-local:
  local in-process AgentApiService implementation
  uses agent-projection

agents/claw:
  Temporal-backed AgentApiService implementation
  uses agent-projection plus Temporal start/signal/query and Pg reads
```

## Boundary Decision

Do not move reusable projection code into `agent-api`.

`agent-api` should remain the stable client contract crate. It must not depend
on CoreAgent reducer internals such as:

- `CoreAgentEntry`
- `CoreAgentState`
- `CoreAgentEventKind`
- `ContextItem`
- `RunEvent`
- `ToolEvent`
- `BlobStore`
- Core IDs like `RunId`, `TurnId`, and `ToolBatchId`

Those dependencies belong in a projection crate that explicitly bridges
`agent-core` to `agent-api`.

## Proposed Crate

Add:

```text
crates/agent-projection
```

Purpose:

- project committed CoreAgent entries into `agent-api::SessionEventView`
- project CoreAgent state plus session record metadata into
  `agent-api::SessionView`
- project CoreAgent runs and context items into `agent-api::RunView`
- read blob-backed item text/arguments/results through `BlobStore`
- provide small helper functions for API IDs and status mapping

This crate should be deterministic except for blob reads needed to materialize
client-facing text. It should not perform command admission, session appends,
LLM calls, tool execution, Temporal operations, or session creation.

## Agent-Local Refactor

Move the reusable projection code out of `agent-local/src/api.rs` and
`agent-local/src/projection.rs` into `agent-projection`.

`agent-local` should keep:

- local session lifecycle and metadata handling
- `LocalAgentApi`
- `SessionRunner` and local runner protocol
- local command driving
- notification assembly specific to local synchronous execution

`agent-local` should no longer own the CoreAgent projection logic that Claw also
needs.

## Claw API

Add a Temporal-backed API layer in `agents/claw`, for example:

```text
agents/claw/src/api.rs
```

Main type:

```rust
pub struct ClawAgentApi { ... }
```

Implement:

```rust
impl agent_api::AgentApiService for ClawAgentApi
```

This API layer is a gateway around Temporal and Postgres/CAS. It should not
replace the workflow's signal/query API.

## Claw API Method Semantics

### initialize

Return `agent-api` server metadata:

```text
server_info.name = "agent-claw"
capabilities.notifications = false or true, depending on whether the gateway
  returns synthesized notifications in request responses
capabilities.history_read = true
capabilities.event_log = true
capabilities.local_execution = false
```

### start_session

Start `ClawSessionWorkflow` with workflow id equal to the session id.

No admission signal is required for a plain session start. The workflow should
initialize the Forge session, open CoreAgent, install default Claw instructions,
and install the fake tool registry/profile exactly as it does today.

Return a projected `SessionView`.

### start_run

Translate `RunStartParams.input` to one text string, allocate a
`SubmissionId`, and use Signal-With-Start:

```text
workflow_id = session_id
signal = submit_admission(ClawAdmission::TextRun { ... })
id_conflict_policy = UseExisting
```

Then wait for the specific `submission_id` to reach a terminal run by polling
the workflow `status` query.

Return a projected `RunView` for that run. For P57 this can be a synchronous
request that waits for completion, matching current `agent-local` behavior and
the existing `claw-submit` binary.

### read_session

Read or load the Forge session from Pg, replay/project CoreAgent state, and
return `SessionView`.

If the workflow is running, the gateway may query `ClawSessionStatus` to expose
workflow-local error/pending-admission state. The Forge session log remains the
source of truth for durable history.

### read_session_events

Read the Pg session log page and project committed entries into
`SessionEventView` through `agent-projection`.

### open_or_start

Do not add this to `agent-api` yet. If Claw needs convenience behavior, keep it
as an inherent method on `ClawAgentApi`, mirroring `LocalAgentApi`.

## Temporal Workflow Boundary

Keep these workflow APIs unchanged:

```text
ClawSessionArgs
ClawAdmission
submit_admission
status() -> ClawSessionStatus
```

The workflow is an internal durable owner, not the public client protocol.
`agent-api` belongs at the gateway layer outside Temporal workflow code.

## Submission Correlation

Keep using `SubmissionId` as the correlation id for P57.

Do not add a separate `request_id` yet. For `start_run`, the gateway allocates
the submission id and polls for a completed/failed/cancelled run with that
submission id in `ClawSessionStatus` or projected session state.

## Failure Semantics

- Command rejection maps to `AgentApiErrorKind::Rejected`.
- Missing session maps to `NotFound`.
- Duplicate start for an existing session maps to `Conflict`, unless using an
  explicit open-or-start helper.
- Workflow last_error maps to `Internal` for P57.
- Provider/tool expected failures remain reducer-visible run/turn/tool facts,
  not API transport errors.
- Temporal infrastructure failures may return `Internal`.

## Refactor Plan

### G1: Add `agent-projection`

- Done. Added workspace crate `crates/agent-projection`.
- Done. Depends on `agent-api` and `agent-core`.
- Done. Moved reusable projection types/helpers from `agent-local`.
- Done. Kept helper APIs small and explicit.

### G2: Update Agent-Local

- Done. Replaced local projection helpers with calls into
  `agent-projection`.
- Done. Preserved `agent-local` API behavior and tests.
- Done. Kept local notification assembly in `agent-local`.

### G3: Add Claw Gateway

- Done. Added `ClawAgentApi` builder/config.
- Done. Holds Temporal client, task queue, model/default config, optional
  instructions ref, max-step config, and Pg store access.
- Done. Implements `AgentApiService`.
- Done. Centralized Signal-With-Start/polling in `ClawAgentApi`.

### G4: Update Claw Submit

- Done. Rebuilt `claw-submit` on `ClawAgentApi::start_run`.
- Done. Preserved current CLI behavior: print final assistant output text.

### G5: Tests

- Done. Unit-tested projection helpers with committed CoreAgent entries.
- Done. Kept `agent-local` local loop tests passing.
- Done. Updated Claw fake live test against Temporal/Postgres:
  - `initialize`
  - `start_session`
  - `start_run`
  - `read_session`
  - `read_session_events`
- Done. Kept OpenAI Claw API live test ignored.

## Non-Goals

- WhatsApp integration
- Calendar/email integration
- Streaming notifications over a hosted server
- HTTP server/gateway implementation
- Continue-As-New
- Adding a generic `agent-temporal` crate
- Moving CoreAgent internals into `agent-api`
- Adding `request_id`

## Done When

- `agent-api` remains independent of `agent-core`.
- `agent-projection` owns reusable CoreAgent-to-API projection.
- `agent-local` uses `agent-projection`.
- `agents/claw` exposes a Temporal-backed `AgentApiService`.
- `claw-submit` uses the new Claw API or shared Claw submit helper.
- `cargo test -p agent-api`, `cargo test -p agent-projection`,
  `cargo test -p agent-local`, and `cargo test -p claw` pass.
- The ignored Claw fake Temporal live test covers the API gateway path.
