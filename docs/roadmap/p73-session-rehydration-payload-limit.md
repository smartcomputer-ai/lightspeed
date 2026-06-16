# P73: Session Rehydration Payload Limit

**Status**
- Proposed 2026-06-16.
- Implemented 2026-06-16 (G1–G3/G5). G4 (operator reporting tooling) dropped as
  unnecessary — after G1 the typed bootstrap guard catches the real risk, and
  raw-log-size reporting is a loose indicator better served by P74 or ad-hoc
  SQL. Problem B (durable event-log growth) split to P74.
- Raised from the `ls.bot` Hetzner incident where a Telegram audio message
  surfaced as `Lightspeed could not answer this message: agent workflow not
  found`.

**Implementation summary**
- G1: `CreateOrLoadSessionResult` now carries reduced `CoreAgentState` +
  `run_submissions` + `head` + `replayed_event_count` instead of the full event
  list. Reduction runs inside the `create_or_load_session` activity via the
  shared `temporal_workflow::reduce_session_entries`. A serialized-size guard
  (`DEFAULT_BOOTSTRAP_PAYLOAD_BUDGET_BYTES`, 1.5 MB) fails with a typed
  `SessionBootstrapPayloadTooLarge` before Temporal rejects the completion.
- G2: continue-as-new keeps passing the same `AgentSessionArgs`; because the
  activity now reduces internally, the idle path no longer transports the full
  log. No `Resume` args variant was introduced (see G2 rationale below).
- G3/G5: added `AgentApiErrorKind::SessionBootstrapFailed`. The workflow records
  bootstrap failures distinctly (`AgentSessionStatus::bootstrap_failed`);
  `query_status_optional` maps that to the typed error, and signal `NotFound` is
  classified via `describe` so a failed/closed workflow returns
  `session_bootstrap_failed` instead of "agent workflow not found".
- Regression test: `bootstrap_returns_compact_state_for_large_log` drives a
  ~1.5 MB durable log and asserts cold bootstrap returns compact state far
  smaller than the raw log.

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

## Scope: Two Problems, One Symptom

This incident has two independent causes that share a symptom. They are
separated into two roadmap items so the live incident fix does not wait on the
larger engine refactor.

- **Problem A — bootstrap transport (this document, P73).** The
  `create_or_load_session` activity returns the entire event log through its
  result, and Temporal records activity results in workflow history. Any
  long-lived session eventually exceeds the payload limit at bootstrap or
  continue-as-new. This is fixed by reducing inside the activity and returning
  compact state. It does not require any engine or event-schema change.

- **Problem B — durable event-log growth (see P74).** Every
  `lightspeed.core.turn.planned` event embeds a full `LlmRequest` containing a
  fresh copy of the active context entry list and tool catalog metadata. Long
  sessions accumulate one full snapshot per turn, which is the dominant source
  of the 1.1 MB log in this incident and grows roughly quadratically. This is an
  engine/reducer change and is tracked separately in
  `docs/roadmap/p74-planned-request-event-bloat.md`.

These are genuinely independent. Fixing Problem B without Problem A still fails
bootstrap, because the activity would still return the whole (smaller) log.
Fixing Problem A without Problem B resolves the incident and stops the bootstrap
failure, but the durable log keeps growing and should be addressed by P74. P73
is the urgent fix; P74 is the durability/cost fix.

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

Reducing the durable log itself (so each turn does not re-snapshot the full
context) is a separate concern, tracked in P74. P73 makes bootstrap compact
regardless of log size; P74 reduces how fast the log grows.

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

### G2: Continue-As-New Stays Cheap

Continue-as-new passes the same `AgentSessionArgs` today, so the resumed
execution re-runs `initialize` and re-rehydrates. Once G1 makes rehydration
compact (the activity reduces and returns `CoreAgentState`, not the event log),
this path is correct again: the resumed execution reduces from the durable log
inside the activity and never crosses the payload boundary with the full log.

Prefer this over a new `AgentSessionArgs::Resume` variant that carries
`CoreAgentState` forward in workflow args. A resume variant introduces a second,
divergence-prone way to produce `CoreAgentState` (carried-forward state vs.
in-activity replay) that must stay byte-identical with the reducer forever, for
a benefit that only matters once replay cost is proven to hurt. Defer it until
profiling shows the compact reload is actually too expensive at idle
continue-as-new; if it is, the first lever is reducing replay cost (P74 shrinks
the log; checkpoints shrink replay) before changing the workflow-args contract.

`CoreAgentState` is bounded by active context, not history: `ContextEntry`
carries a `content_ref: BlobRef` rather than inline content, and `ToolSpec`
carries a `description_ref`, so the reduced state is entry/tool *metadata* plus
refs. Add an assertion/metric on reduced-state size so a regression that inlines
payloads into state is caught early.

### G3: Startup And Bridge Failure Semantics

Make the gateway/bridge failure clearer:

- distinguish "session workflow failed during bootstrap" from generic workflow
  not found;
- when `session/start` finds an existing failed workflow for the same session
  id, either start a new run only if bootstrap can succeed or return a typed
  `session_bootstrap_failed` error with the root cause;
- bridge should surface this as a system/session recovery problem, not as an
  ordinary message-answer failure.

Durable event-log volume reduction (so the log itself stops accumulating a full
context snapshot per turn) is out of scope for P73 and tracked in P74. P73
makes bootstrap survive a large log; P74 keeps the log from growing that fast.

### G4: Migration And Recovery Tooling (dropped)

Originally proposed an operator reporting path (a CLI that lists sessions near
bootstrap payload risk). Dropped during implementation: after G1 the typed
bootstrap guard catches the real risk at the moment it matters, and raw-log-size
reporting is only a loose leading indicator — better served once P74 shrinks the
log, or by an ad-hoc SQL query when an operator actually needs it. The emergency
"rotate bridge binding" recovery remains documented under Operational Recovery.

## Acceptance Criteria

- A session with multiple megabytes of persisted event log can be restarted or
  continued-as-new without returning the full event log through an activity
  result.
- Continue-as-new at idle does not move the full event log through an activity
  result; it reduces inside the activity and carries only compact state.
- Bootstrap payloads have an explicit size budget and fail with a typed,
  diagnosable error before Temporal reports `Complete result exceeds size
  limit`.
- Bridge users no longer see `agent workflow not found` for this class of
  failure; they see a clear session recovery/configuration message.
- A regression test constructs a session event log larger than Temporal's
  default activity-result payload limit and verifies cold bootstrap succeeds
  through the compact path.
