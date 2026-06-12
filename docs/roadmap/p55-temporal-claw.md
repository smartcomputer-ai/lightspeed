# P55: Temporal Claw

**Status**
- Implemented

**Progress**
- Added `agents/claw` with a Temporal worker, Signal-With-Start submitter,
  workflow, activities, fake LLM/tool loop, and Postgres-backed Forge storage.
- Extended the local dev stack with Temporal defaults and Claw environment
  variables.
- Added focused tests for workflow queueing, max-step failure, CoreCommand
  codec shape, and the fake LLM -> fake tool -> fake LLM run loop.
- Added an ignored live Temporal/Postgres test for two Signal-With-Start
  submissions against one session workflow.
- Added an ignored OpenAI-backed Temporal/Postgres live test for the real LLM
  activity path.

## Goal

Add the first "claw" personal-assistant agent under `agents/claw/`.

This first version should prove the production execution model we want:

- one long-lived Temporal workflow chain owns one Forge session
- external inputs arrive through Temporal signals
- CoreAgent admits inputs into its existing run queue
- the workflow drives CoreAgent to quiescence through Temporal activities
- Forge session log/CAS remains the user-facing durable agent history
- Temporal history remains the workflow execution history

WhatsApp, calendar, email, host filesystem tools, and hosted API gateways are
out of scope for P55.

## Decision

Use one Temporal workflow chain per Forge session.

```text
session_id=session_abc
workflow_id=session_abc
```

Clients submit inputs with Signal-With-Start against the workflow id. If the
workflow is already running, Temporal appends the signal to the current run's
history. If it is not running, Temporal starts the workflow and delivers the
signal atomically.

Do not create several independent workflows for the same Forge session. The
Temporal workflow id is the Forge session id, so a Claw workflow owns the Forge
session whose id equals its workflow id. `SessionStore::append` `expected_head`
checks remain the second guard against accidental rogue writers.

`Continue-As-New` is intentionally deferred from the first implementation cut.
The P55 design should not block future continue-as-new support: keep workflow
state serializable and keep the Forge session log/CAS as the external durable
history, but do not implement run rollover yet.

## Temporal Signal Model

Signals are asynchronous messages to a running workflow execution.

When a signal arrives while the workflow is awaiting an activity, Temporal
records a signal event in workflow history and schedules a workflow task. The
workflow can handle the signal and mutate workflow state, then keep waiting for
the activity result. The client call returns when Temporal accepts the signal;
it does not wait for the agent run to complete.

For Claw, the signal handler should be tiny and deterministic:

```text
submit_admission(admission):
  pending_admissions.push(admission)
```

The workflow must not keep a second durable queue for already-admitted user
messages. `CoreAgentState` already owns durable run queue state:

```text
CoreAgentState.runs.active
CoreAgentState.runs.queued
CoreAgentState.runs.completed
```

The workflow-local `pending_admissions` queue covers only the gap between
"Temporal signal received" and "command admitted into CoreAgent". After a
command is admitted and appended, the source of truth is the Forge session log
and the reduced `CoreAgentState`.

Text input is a convenience admission, not the only signal shape. Advanced
clients may submit encoded CoreAgent commands directly when they already have
the required CAS blob refs or configuration structures.

Process `pending_admissions` FIFO. The main workflow loop must admit and drive
one logical stream of work at a time; signals received while an LLM/tool
activity is pending are appended to `pending_admissions` and admitted after the
current activity boundary resumes.

## V1 Interface

Add `agents/claw` to the Rust workspace.

Provide two binaries:

- `claw-worker`: registers the workflow and activities on task queue
  `forge-claw`.
- `claw-submit`: submits one text-run admission via Signal-With-Start, polls
  workflow query status, and reads the completed output blob from Postgres.

Workflow:

```text
ClawSessionWorkflow
```

Workflow id:

```text
{session_id}
```

Workflow input:

```text
ClawSessionArgs {
  session_id,
  model,
  instructions_ref,
  max_steps_per_input,
}
```

Signal:

```text
submit_admission(ClawAdmission)

ClawAdmission::TextRun {
  text,
  run_config,
  submission_id,
}

ClawAdmission::CoreCommand {
  command,
}
```

Query:

```text
status() -> ClawSessionStatus
```

`ClawSessionStatus` should include whether the session is initialized, active
run summary, pending admission count, and CoreAgent queued/completed run
summary.

`ClawAdmission::CoreCommand.command` is a `DynamicCommand` decoded through
`CoreAgentCodec::decode_command` and admitted through the default
`CoreAdmitCommand`. For `TextRun`, the workflow stores `text` in CAS and
converts the admission to `CoreAgentCommand::RequestRun`.

Do not add a separate admission/request id in P55. Run correlation uses
`submission_id` when provided. Duplicate retries without natural command
idempotency are an accepted v1 limitation.

Use workflow id only for client routing. Do not require clients to know the
current Temporal run id.

`CoreCommand` is a trusted internal interface in P55. Clients that submit
commands with `BlobRef` fields are responsible for ensuring those blobs already
exist in the configured CAS. Do not add blob-ref preflight validation in the
first cut.

Do not include `universe_id` in `ClawSessionArgs` or workflow signals. P55
assumes one configured PgStore universe per Claw worker deployment. The
Temporal workflow id is the Forge `session_id`; `universe_id` is storage
tenancy loaded from worker/activity configuration.

## Core Loop

The workflow owns the reduced CoreAgent state while it is alive.

On startup:

1. Call `create_or_load_session`.
2. If the loaded session has no events, store default instructions and
   append/apply `CoreAgentCommand::OpenSession`.
3. If the loaded session already has events, replay them into `CoreAgentState`
   and continue from the existing head.

Main loop:

```text
loop:
  wait until pending_admissions is non-empty

  while pending_admissions has entries:
    if TextRun:
      put input text into CAS through activity
      build CoreAgentCommand::RequestRun
    if CoreCommand:
      decode DynamicCommand with CoreAgentCodec
    admit with CoreAdmitCommand
    append/apply admitted events

  drive CoreAgent until idle:
    plan deterministic CoreAgent events
    append/apply planned events

    if next_generation_request exists:
      flush staged events
      call llm_generate activity
      convert result to CoreAgent event proposals
      append/apply result events
      continue

    if next_tool_batch_request exists:
      flush staged events
      call tool_invoke_batch activity
      convert result to CoreAgent event proposals
      append/apply result events
      continue

    break
```

Only committed Forge events may mutate `CoreAgentState`.

The workflow code must not perform provider calls, filesystem operations,
network I/O, Postgres I/O, or wall-clock reads. Use Temporal workflow time for
`observed_at_ms` and use activities for every side effect.

## Activities

First activity set:

- `create_or_load_session`
- `put_blob`
- `append_events`
- `llm_generate`
- `tool_invoke_batch`
- `read_blob`

Use `store-pg` as the only P55 Forge session/CAS backend. The workflow and
activities should not depend on filesystem stores.

Activities construct `PgStoreConfig` from environment. `FORGE_PG_UNIVERSE_ID`
must be stable across worker restarts; generating a new universe id on startup
would make existing sessions and blobs invisible to the worker. `BlobRef`
values are interpreted only within this configured universe.

`append_events` should accept encoded dynamic uncommitted events plus the
expected head, append through `SessionStore::append`, and return committed
entries plus the new head. The workflow decodes and applies returned entries
with `CoreAgentCodec`.

`llm_generate` supports:

- `fake`: deterministic test provider
- `openai`: existing `llm-runtime` OpenAI Responses adapter

`tool_invoke_batch` supports fake tools only in P55. OpenAI mode uses the same
fake tool profile and activity-backed fake tool execution as fake LLM mode.

## Fake Tool Profile

Install one fake function tool profile during session initialization:

```text
profile_id=claw_fake_tools
tool=claw_echo
```

`claw_echo` arguments:

```json
{ "text": "string" }
```

Fake LLM behavior:

- first generation for a run emits a `claw_echo` tool call
- after the tool result is present, second generation emits a final assistant
  message

This proves the full LLM -> tool -> LLM loop without exposing host tools.

## Local Dev

Extend the local dev stack with Temporal.

Use the modern Temporal development server image/command rather than
deprecated `temporalio/auto-setup`.

Default environment:

```text
FORGE_CLAW_TASK_QUEUE=forge-claw
FORGE_CLAW_POSTGRES_URL=${FORGE_TEST_POSTGRES_URL}
FORGE_PG_UNIVERSE_ID=00000000-0000-0000-0000-000000000001
FORGE_CLAW_LLM=fake
FORGE_CHAT_PROVIDER=openai
FORGE_CHAT_MODEL=<existing default>
```

Document:

```bash
dev/local/up.sh
cargo run -p claw --bin claw-worker
cargo run -p claw --bin claw-submit --session session_1 "hello"
```

If the final crate package name cannot be `claw` because of workspace naming
constraints, use `agent-claw` as the package name but keep the path
`agents/claw/` and binary names above.

## Test Plan

Unit tests:

- text-run admission only stages raw input before CAS write
- text-run admission appears in `CoreAgentState.runs.queued`
- CoreCommand admission uses `CoreAgentCodec` and `CoreAdmitCommand`
- fake LLM -> fake tool -> fake LLM completes a run
- max step limit fails clearly instead of spinning
- existing session logs replay into `CoreAgentState` on workflow startup
- pending admissions are processed FIFO

Activity tests:

- `append_events` respects expected-head mismatches
- `put_blob`/`read_blob` round trip through the configured store
- fake `llm_generate` and `tool_invoke_batch` produce reducer-compatible
  results

Ignored live integration test:

- requires local Temporal plus Postgres
- starts worker
- submits input with Signal-With-Start
- polls `status`
- verifies completed run and readable assistant output blob

Acceptance:

- `cargo test` continues to pass
- `cargo test -p claw` passes without live services
- ignored live test passes against `dev/local` stack

Implementation note: the ignored live Temporal/Postgres integration test is
present but is not run by default.

## Non-Goals

- WhatsApp sync
- calendar/email integrations
- host filesystem/process tools
- streaming token notifications
- cancellation UX
- multi-user authorization
- hosted gateway/API service
- Python bridge
- using Temporal history as the Forge session log
