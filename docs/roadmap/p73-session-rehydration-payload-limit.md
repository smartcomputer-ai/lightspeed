# P73: Session Rehydration Payload Limit

**Status**
- Proposed 2026-06-16.
- Raised from the `ls.bot` Hetzner incident where a Telegram audio message
  surfaced as `Lightspeed could not answer this message: agent workflow not
  found`.

## Incident

After deploying P72 audio transcription support, an addressed Telegram audio
message failed in the bridge with:

```text
Lightspeed could not answer this message: agent workflow not found
```

Temporal showed the session workflow for the bound Telegram session had failed
immediately at bootstrap:

```text
WorkflowActivities::create_or_load_session
ActivityTaskFailed: Complete result exceeds size limit.
```

The affected session was `bridge_MblNXESwcwsD0PO4grDGVHtF1m5tws0E`. It had 537
persisted session events, about 1.1 MB as Postgres JSONB and about 2.5 MB as
JSON text. Most of the volume came from repeated `lightspeed.core.turn.planned`
events that include full model/tool request details.

This was not an audio preprocessing failure. The audio message merely exercised
the existing bridge session after a restart. The workflow could not rehydrate,
failed, and later bridge calls mapped Temporal `NotFound`/failed workflow
interaction errors to `agent workflow not found`.

## Root Cause

`AgentSessionWorkflow::initialize` calls the
`WorkflowActivities::create_or_load_session` activity. That activity:

1. creates or loads the session record;
2. reads every persisted `session_events` row for the session;
3. returns all entries in `CreateOrLoadSessionResult`.

Temporal records activity results in workflow history. Returning the full
session event log is therefore bounded by Temporal payload/history limits. Once
a long-lived session accumulates enough event volume, any new workflow execution
or continue-as-new execution that re-runs initialization can fail before the
session opens.

The current continue-as-new path does not solve this because it continues with
the same `AgentSessionArgs`, not with compact workflow state. The new execution
still calls `create_or_load_session` and reloads the full durable event log.

## Operational Recovery

For `ls.bot`, the immediate recovery was:

1. keep the old session rows and Temporal history intact for audit;
2. rotate the Telegram binding from `sessionKey: "personal"` to
   `sessionKey: "personal-20260616"`;
3. update `state/bridge-state.json` to point the existing Telegram binding at
   the newly derived session id;
4. clear that binding's event cursor;
5. restart the bridge.

This restores service but starts a fresh conversation context. It is acceptable
as an emergency operation, not as the product fix.

## Design Position

Do not pass full durable session history through Temporal activity results.

The workflow should remain the deterministic agent orchestrator. It owns the
reduced agent state, active run/turn/tool state, context metadata needed for
planning, cancellation/retry/timer/signal flow, and the decisions about what
should happen next.

Storage and CAS should own bulky data: blob payloads, full provider-neutral LLM
requests, provider responses, large tool descriptions/schemas, request
manifests, and optional checkpoints.

The workflow needs a deterministic in-memory `CoreAgentState`, the current head,
and run-submission correlation data. It does not need every historical entry in
Temporal history when bootstrapping a long-lived session. The durable event log
remains the audit source of truth; Temporal history should carry compact
workflow state and refs needed to continue orchestration.

Pagination alone is not sufficient if the workflow records every page of events
as activity results. That still moves the same large data into Temporal history.
The bootstrap contract must become compact.

Likewise, moving all planning into activities is too far. If planning, context
selection, and state transitions all become opaque activity-side database
mutations, Temporal becomes a retry wrapper instead of the durable agent state
machine. The target design is: workflow decides logical state; activities
materialize bulky physical requests from persisted deltas; CAS stores canonical
artifacts.

## Generation Materialization Boundary

The current `lightspeed.core.turn.planned` event stores a full `LlmRequest`.
That request includes the full context snapshot and the current tool catalog.
Even though individual entry contents and tool schemas are CAS-backed, the full
list of context entry metadata is copied into every planned turn. Long sessions
therefore accumulate repeated snapshots.

The engine does need to know which context entries are in context to plan the
next turn. The fix is not to remove context metadata from workflow state. The
fix is to stop recording and transporting repeated full request snapshots across
Temporal boundaries.

Introduce a generation materialization cursor. The workflow/engine keeps the
reduced state and emits a deterministic "prepare generation" effect with target
revisions and expected hashes. A storage activity materializes the bulky
provider-neutral request from persisted context/tool deltas, writes the
canonical artifacts to CAS, and returns compact refs.

Sketch:

```rust
struct GenerationMaterializationCursor {
    session_position: SessionPosition,

    context_revision: u64,
    context_manifest_ref: BlobRef,
    context_manifest_hash: String,

    toolset_revision: u64,
    toolset_manifest_ref: BlobRef,
    toolset_manifest_hash: String,

    provider_state_ref: Option<BlobRef>,
}

struct PrepareGenerationInput {
    session_id: SessionId,
    run_id: RunId,
    turn_id: TurnId,

    base_cursor: Option<GenerationMaterializationCursor>,

    target_position: SessionPosition,
    target_context_revision: u64,
    target_toolset_revision: u64,

    model: ModelSelection,
    tool_choice: Option<ToolChoice>,
    output_limit: Option<u32>,
    compaction: Option<CompactionPolicy>,
    params_ref: Option<BlobRef>,

    expected_context_manifest_hash: String,
    expected_toolset_manifest_hash: String,
    planner_version: u32,
}

struct PrepareGenerationResult {
    request_ref: BlobRef,
    request_fingerprint: String,
    cursor: GenerationMaterializationCursor,
}
```

The prepare activity:

1. loads the previous context/tool manifests from `base_cursor`, if present;
2. reads persisted session event deltas from `base_cursor.session_position` to
   `target_position`;
3. applies only materialization-relevant patches, such as
   `context.entries_applied`, `context.entries_removed`,
   `context.state_replaced`, `tool_config.tools_patched`, and
   `tool_config.tools_replaced`;
4. reconstructs the target context and tool manifests;
5. verifies their hashes match the workflow's expected hashes;
6. builds the canonical provider-neutral `LlmRequest`;
7. stores the full request and manifests in CAS;
8. returns `request_ref`, `request_fingerprint`, and the new cursor.

Then `turn.planned` records a compact fact instead of the full request:

```json
{
  "turn_id": 48,
  "run_id": 19,
  "request_ref": "sha256:...",
  "request_fingerprint": "...",
  "cursor": {
    "session_position": "...",
    "context_revision": 123,
    "context_manifest_ref": "sha256:...",
    "context_manifest_hash": "...",
    "toolset_revision": 9,
    "toolset_manifest_ref": "sha256:...",
    "toolset_manifest_hash": "..."
  }
}
```

The `request_ref` is therefore not invented by the reducer. The reducer emits a
logical prepare-generation intent; the activity constructs and stores the bulky
artifact, and the workflow records the returned ref as the authoritative planned
request.

This preserves the useful role of Temporal:

- the workflow remains the deterministic planner and orchestrator;
- activities perform I/O and materialize large artifacts;
- Temporal history carries revisions, hashes, cursors, and refs;
- CAS carries the full request for audit, replay, and provider debugging.

Checkpoints are a separate optimization on top. They can make cold recovery and
continue-as-new cheaper, but the first-order fix is this materialization
boundary and compact event shape.

## Proposed Fix

### G1: Compact Bootstrap Activity Result

Replace or extend `CreateOrLoadSessionResult` so the activity returns a compact
rehydration result:

- `SessionRecord` or at least the current `SessionPosition`;
- replayed `CoreAgentState`;
- `run_submissions`;
- any other small workflow-only indices currently reconstructed from the full
  event list.

The replay can happen inside the activity by reading the durable session log and
applying `CoreApplyEvent`. The workflow then receives only the reduced state.
Because activity output is still recorded in Temporal history, add a serialized
size guard and fail with a clear typed bootstrap error before Temporal rejects
the completion.

This does not require checkpoints as the first step. Checkpoints can later
reduce replay cost, but the correctness fix is to avoid returning the full event
log through the activity boundary.

### G2: Continue-As-New Carries Compact State

Change continue-as-new from "same args, reload from storage" to "resume from
compact state":

- introduce an `AgentSessionBootstrap` or versioned `AgentSessionArgs` variant;
- first execution uses `CreateOrLoad { session_id, session_config, ... }`;
- continue-as-new uses `Resume { session_id, session_config, head,
  core_state, run_submissions, admission_failures, ... }`;
- the resumed execution must not call `create_or_load_session` unless it is a
  true cold recovery path.

Keep the resume payload below a conservative size budget. If `CoreAgentState`
itself grows too large, add state compaction before continue-as-new rather than
falling back to event-log replay.

### G3: Compact Planned Request Events

Refactor `TurnEvent::Planned` so it records the prepared request ref and
materialization cursor, not the full `LlmRequest`.

The full canonical `LlmRequest` should be written to CAS by the prepare
activity. The event log should keep enough information to prove which request
was planned and to rematerialize/debug it:

- `request_ref`;
- `request_fingerprint`;
- `context_revision`;
- `context_manifest_ref` and hash;
- `toolset_revision`;
- `toolset_manifest_ref` and hash;
- optional provider continuity state ref;
- planner/materializer version.

During replay, the workflow should rebuild logical state from compact events and
refs. It should not need the full provider request inline in the event.

### G4: Generation And Tool Materialization Activities

Add a prepare/materialize activity before model generation.

The workflow passes a base cursor, target session position, target context/tool
revisions, and expected manifest hashes. The activity applies persisted
context/tool patches since the base cursor, writes manifests and the canonical
request to CAS, and returns compact refs.

The generation activity should consume `request_ref`, load the canonical request
from CAS/storage, call the provider, write output artifacts to CAS, and return
compact completion facts.

### G5: Startup And Bridge Failure Semantics

Make the gateway/bridge failure clearer:

- distinguish "session workflow failed during bootstrap" from generic workflow
  not found;
- when `session/start` finds an existing failed workflow for the same session
  id, either start a new run only if bootstrap can succeed or return a typed
  `session_bootstrap_failed` error with the root cause;
- bridge should surface this as a system/session recovery problem, not as an
  ordinary message-answer failure.

### G6: Event Volume Reduction

Reduce durable event bloat separately:

- avoid persisting repeated context snapshots and full tool catalogs in every
  `turn.planned` event when stable request/context/toolset refs are enough;
- store large repeated request artifacts in CAS and keep event payloads as refs;
- add metrics/tests around per-event size and per-session accumulated size.

This is not the only bootstrap fix, but it is required to avoid continued
quadratic-ish growth after bootstrap is made compact.

### G7: Migration And Recovery Tooling

Add an operator path for existing large sessions:

- a CLI or admin method that reports sessions near bootstrap payload risk;
- a safe "rotate bridge binding" runbook or command for emergency recovery;
- an optional "materialize compact checkpoint" command once compact bootstrap
  exists, so old sessions can be recovered without losing context.

## Acceptance Criteria

- A session with multiple megabytes of persisted event log can be restarted or
  continued-as-new without returning the full event log through an activity
  result.
- Continue-as-new does not call full-history session rehydration in the normal
  idle path.
- `turn.planned` no longer embeds a full `LlmRequest`; it records compact refs,
  revisions, hashes, and the materialization cursor.
- A prepare/materialize activity can construct the canonical `LlmRequest` from a
  base cursor plus persisted context/tool patches, then store it in CAS and
  return a `request_ref`.
- The workflow still owns deterministic planning and context selection; the
  materialization activity performs I/O and bulk request construction only.
- Bootstrap payloads have an explicit size budget and fail with a typed,
  diagnosable error before Temporal reports `Complete result exceeds size
  limit`.
- Bridge users no longer see `agent workflow not found` for this class of
  failure; they see a clear session recovery/configuration message.
- A regression test constructs a session event log larger than Temporal's
  default activity-result payload limit and verifies cold bootstrap succeeds
  through the compact path.
