# P59: Claw Durable Workflow Hardening

**Status**
- Done

**Progress**
- G1 completed: implemented content-idempotent Claw append confirmation without append IDs
  or Pg schema changes. The activity now confirms an exact already-committed
  batch after `ExpectedHeadMismatch`, with focused tests for retry success,
  conflict preservation, and empty append no-op behavior.
- G2 completed: Claw now checks for continue-as-new only at idle admission
  boundaries, using Temporal's continue-as-new suggestion plus a configurable
  workflow history-length threshold.
- G3 completed: `run/start` now requires an existing session; session creation
  remains explicit through `session/start`.
- G4 first cut completed: Claw status now carries correlated admission failures for
  malformed `CoreCommand` admissions and `CommandError::Rejected`, and
  `run/start` maps matching rejections to `agent-api` errors instead of
  timing out or poisoning the workflow. Step-limit admission handling is
  explicitly deferred; step limits still fail the workflow until resume
  semantics after partial run progress are specified.
- G5 completed: Claw activities now use worker-owned `ClawActivityState`
  instead of rebuilding Pg/LLM/tool dependencies inside each activity call.
  Activity code is split under `agents/claw/src/activities/`, with narrow
  storage, LLM, and tool dependency views delegated to individual modules.

## Goal

Harden the Temporal-backed Claw prototype so it behaves like a reliable durable
workflow runtime, not only an end-to-end integration proof.

P57 and P58 established the shape:

```text
client
  -> agent-api / JSON-RPC
  -> ClawAgentApi
  -> Temporal ClawSessionWorkflow
  -> Pg session log + CAS
  -> LLM/tool activities
```

P59 keeps that architecture and focuses on operational correctness:

- activity retry safety
- long-lived workflow history management
- clear API session/run semantics
- non-poisoning admission failures
- explicit worker/activity dependency wiring

## Non-Goals

- LLM session compaction or context pruning.
- Streaming notifications, SSE, WebSockets, or token streams.
- Multi-tenant auth or hosted gateway policy.
- Replacing Pg/CAS as the Lightspeed session-log source of truth.
- Changing the `agent-api` client-facing DTO model unless required to express
  clear error semantics.

## Priority Order

Implement these one by one. The first two are the critical durable-workflow
correctness items.

1. Idempotent session-log append activity. **Done.**
2. Continue-as-new for long-lived Claw workflows. **Done.**
3. Clear `run/start` session-existence semantics. **Done.**
4. Keep invalid admissions from killing the workflow. **Done for malformed
   commands and command rejections; step limits explicitly deferred.**
5. Inject worker/activity dependencies instead of reading env inside
   activities. **Done.**

## G1: Idempotent Appends

Temporal may retry `append_events` after Pg has already committed the batch but
before Temporal records the activity result. The current append path can then
see `ExpectedHeadMismatch` and fail the workflow even though the Lightspeed session
log is correct.

Target behavior:

```text
append_events(request) = append this exact batch or confirm it was already
appended
```

Acceptable first implementation:

- If Pg append succeeds, return the committed entries as today.
- If Pg append returns `ExpectedHeadMismatch`, read the entries after the
  request's `expected_head`.
- If the next entries exactly match the requested uncommitted events after
  encoding/commit envelope assignment, return those existing entries as a
  successful idempotent replay.
- If they do not match, preserve the conflict as a real append error.

Important details:

- The comparison should be structural, not string-based.
- The check must include event payload, joins, and observed timestamp.
- It should not treat any arbitrary head mismatch as success.
- Keep Pg as the authoritative session-log appender.

Tests:

- Unit/integration test for "same append retried after success returns the same
  committed entries."
- Test that a different append at the same expected head still fails.
- Test that empty append remains a no-op.

## G2: Continue-As-New

Claw sessions are intended to be long-lived. Even if CoreAgent state becomes
bounded after future compaction work, Temporal workflow history grows with every
signal, activity, and workflow task. The session log should be the durable
transcript; Temporal history should remain bounded execution history.

Target behavior:

- When the workflow is idle and a threshold is crossed, continue-as-new.
- The new workflow run uses the same workflow id/session id.
- Initialization reloads CoreAgent state and head from Pg/CAS.
- No pending admission is dropped.

Implemented first cut:

- Triggered only after an admission batch has fully processed and the workflow
  is idle.
- Uses `ctx.continue_as_new_suggested()` or
  `ctx.history_length() >= continue_as_new_history_threshold`.
- Default history threshold is `10_000`; tests can override it through
  `ClawSessionArgs`.
- Continue-as-new restarts with the same `ClawSessionArgs`; initialization
  replays CoreAgent state from Pg/CAS as before.

Possible thresholds:

- processed admission count
- appended event batch count
- completed run count
- Temporal history length if exposed by the Rust SDK

Rules:

- Continue-as-new only at an idle boundary.
- Do not continue-as-new while an LLM/tool activity is in flight.
- Do not continue-as-new while `pending_admissions` is non-empty unless the
  admissions are explicitly carried into the new run args.
- Keep the Lightspeed session log as the recovery source, not workflow-local state.

Tests:

- Workflow-level test or ignored live test that drives enough small fake runs to
  cross the threshold and still completes a later run.
- Verify the projected session after continue-as-new includes prior committed
  runs via Pg projection.

Coverage:

- Unit tests cover the continue-as-new trigger policy.
- Ignored live test
  `temporal_live_continue_as_new_completes_later_fake_run` sets a low history
  threshold, completes a later fake run, and verifies projected session history
  includes both runs.

## G3: Clear `run/start` Session Semantics

`ClawAgentApi::start_run` currently uses Signal-With-Start. This can start a
missing workflow using default model arguments. That may be convenient, but the
public semantics should be explicit.

Decision to make:

- `run/start` requires an existing opened session and returns `NotFound` if the
  session does not exist.
- Or `run/start` is explicitly allowed to open a default session implicitly.
- Or implicit open remains only in an inherent helper such as
  `open_or_start_session`, while `agent-api::run/start` requires a session.

Chosen target:

- `agent-api::run/start` requires an existing Lightspeed session.
- `ClawAgentApi::open_or_start_session` remains a convenience helper.
- `session/start` remains session creation only; it does not carry initial run
  submissions.
- CLI flows that want convenience should call `session/start` first before
  submitting runs.

Implemented:

- `ClawAgentApi::start_run` no longer uses Signal-With-Start. It loads the
  existing Lightspeed session to obtain run config, then signals the existing
  workflow.
- Missing sessions now fail before a signal is sent, preserving `NotFound`
  semantics.

Tests:

- `start_run` against a missing session returns `NotFound`.
- Existing `session/start` then `run/start` path still works.
- Convenience helper still supports open-or-start behavior where intended.

Coverage:

- Ignored live test
  `temporal_live_run_start_missing_session_returns_not_found` verifies missing
  sessions fail as `NotFound`.
- Ignored live tests
  `temporal_live_session_start_then_run_start_completes_fake_runs` and
  `temporal_live_session_start_then_run_start_completes_openai_run` exercise the
  explicit session-start-then-run-start path.

## G4: Non-Poisoning Admission Failures

The workflow should not die for expected client/admission failures. A malformed
or rejected admission should not poison a long-lived session.

Target behavior:

- Infrastructure and invariant failures may still fail the workflow.
- Provider/tool expected failures remain reducer-visible run/turn/tool facts.
- Invalid commands, rejected commands, unsupported admission variants, and step
  limits should be reported without killing the session when possible.

Possible implementation:

- Add an admission result/error record to `ClawSessionStatus`.
- Keep `last_error` for workflow-level failures, but distinguish it from
  admission rejection.
- Map command rejection to `AgentApiErrorKind::Rejected` at the gateway.
- For asynchronous signals that cannot return directly, keep enough status to
  let the gateway correlate rejection to a submission/admission id.

Tests:

- Invalid or rejected command admission does not terminate the workflow.
- A later valid text run still completes.
- Gateway maps correlated rejection to the expected API error kind.

## G5: Inject Activity Dependencies

Activities currently rebuild Pg clients and choose LLM mode from environment
variables inside activity handlers. That is acceptable for the prototype, but
production worker behavior should be explicit at worker startup.

Target behavior:

- `claw-worker` parses environment/config once.
- The worker owns configured Pg/CAS access, LLM runtime registry, and tool
  executor registry.
- Activity handlers use injected dependencies rather than reading env per call.
- Tests can construct fake dependencies directly.

Suggested shape:

```text
ClawActivityState
  store: Arc<PgStore>
  llm_runtime: Arc<dyn ...>
  tools: Arc<dyn ...>
  config: ClawActivityConfig
```

Keep this practical. Do not introduce a broad plugin system in P59.

Tests:

- Fake activity state can run the fake LLM/tool loop without env mutation.
- OpenAI live test still reads credentials at process startup and remains
  ignored by default.

Implemented:

- `ClawActivityState` owns worker-scoped storage, LLM, and tool dependencies.
- `ClawActivities::from_env()` parses environment-backed worker dependencies
  once before registration.
- Activity handlers are thin Temporal wrappers; implementation modules receive
  only the dependency view they need.
- `claw-worker` and Temporal live tests register an injected `ClawActivities`
  value.

## Verification

Default verification:

```bash
cargo test -p claw
cargo test -p agent-api -p agent-projection -p cli --tests
```

Live verification, when local Temporal/Postgres are available:

```bash
cargo test -p claw --test temporal_live -- --ignored temporal_live_session_start_then_run_start_completes_fake_runs
cargo test -p claw --test temporal_live -- --ignored temporal_live_continue_as_new_completes_later_fake_run
cargo test -p claw --test temporal_live -- --ignored temporal_live_run_start_missing_session_returns_not_found
cargo test -p claw --test temporal_live -- --ignored temporal_live_admission_failures_do_not_poison_workflow
```

Provider-backed verification remains opt-in:

```bash
cargo test -p claw --test temporal_live -- --ignored temporal_live_session_start_then_run_start_completes_openai_run
```

## Done Criteria

- Retried appends cannot corrupt or falsely fail an already-correct session log.
- Long-running sessions have a defined continue-as-new strategy.
- `run/start` behavior for missing sessions is documented and tested.
- Malformed command admissions and command rejections do not unnecessarily
  terminate a healthy session workflow.
- Claw activity behavior is configured by worker construction, not hidden
  per-activity environment reads.

Deferred follow-up:

- Step-limit admission handling remains workflow-failing until resume semantics
  after partial run progress are specified.
