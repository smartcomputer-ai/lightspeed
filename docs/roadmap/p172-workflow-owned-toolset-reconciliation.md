# P172 — Workflow-Owned Session Preparation

**Status:** Implemented, including atomic preparation publication, 2026-09-12.
Covers workflow-owned session setup,
profile/configuration application, tool and context reconciliation, targeted
steering, and removal of session-bound environment provisioning. Non-live
validation is complete; live suites remain opt-in.

## Problem

Previously, before submitting each new run, the gateway called
`configure_session_toolset()`. It rebuilt the desired tools from session
features, workflow bindings, and current MCP server records and auth grants,
then signals `PatchTools` and optionally waits before submitting `RequestRun`.
This keeps MCP exposure, allowlists, endpoint, execution, approval, and auth
settings current, but puts session execution ordering in the gateway. Run
producers that submit directly to the workflow do not share this preparation.

This is database-backed policy reconciliation, not remote MCP `tools/list`
discovery. Unlike prompt/skill refresh, there is no equivalent workflow step
that makes the gateway call safe to simply delete.

## Decision

The session workflow owns tool preparation and its ordering relative to run
admission. Extract shared reconciliation logic from the gateway and execute
record/grant reads in a workflow activity. The activity returns desired tool
changes; the workflow applies them through the existing command/event path.
Keep all I/O outside the deterministic engine and workflow logic.

The same ownership applies to setup and explicit refresh operations. The
gateway validates and translates requests, submits durable intent, and waits
for a correlated outcome. It does not orchestrate a sequence of session
mutations. Persist resolved profile/setup intent so retries finish the original
setup, even if the named profile changes or the request process exits.

- Retain gateway request validation and early MCP link/grant validation when
  saving configuration. Run submission must not separately patch and wait.
- Preserve built-in and workflow-backed tools: the current reconciler does
  more than MCP. Account for missing core bindings, name collisions, removals,
  and session setup/configuration callers when extracting it.
- Preserve active and queued run ordering. Reconciliation must cover new run
  requests while work is already active or queued; do not reuse the
  prompt/skill refresh's idle-only guard. Never change a toolset frozen for an
  in-flight turn, or allow the new run to use tools before its patch applies.
- Use current session state when applying results; reject or recompute stale
  results after configuration changes. Preserve submission idempotency and
  surface reconciliation failures through admission outcomes.
- Observe MCP policy for each new submission. Tools remain session-wide:
  publication at a safe turn boundary can affect subsequent turns of the
  current run. Queued runs do not acquire independent tool snapshots or an
  additional policy refresh merely by starting. A queued run must not execute
  before the preparation associated with its admission has been applied.
- Preserve prompt queue acknowledgement and control responsiveness while
  preparation is pending. Cancellation, steering, approvals, and effect
  completion must continue to progress. Preparation errors belong to their
  correlated operation; they must not fail the entire session workflow.
- Resolve duplicate run submissions before mutable policy reads. A retry of
  an accepted submission remains successful after a linked server is disabled.
- Activities return desired tools, required system bindings, and a source
  identity. Compute the final patch against current workflow state and discard
  obsolete observations. Never put credential values in workflow history.

## Extended scope

### Durable session initialization and profile application

Capture the resolved setup intent in workflow input. Opening the session is
not sufficient evidence that setup completed: initialize tools, profile
instructions, selection of an existing environment, and runtime context before reporting readiness or driving runs. Retries resume
unfinished setup rather than returning early after a workflow-start conflict.
Existing-session profile application is a correlated durable operation with
explicit revision checks and one atomic publication. Profiles may select
an existing environment or inherit the parent session’s selection. They never
create environments or bind credentials.

Preparation builds a private candidate using deterministic command admission,
prepares runtime context against that candidate, and validates all resulting
commands before publishing the complete event batch once against the original
session head. Preparation failures and stale-source rejection leave the proposed
changes unpublished. Source invalidations are included in the candidate, avoiding
additional cleanup appends after commit. Initial setup and explicit refresh use
the same commit boundary; controls remain responsive during reads.

The existing store append activity confirms an exact batch after a lost commit
response. Commit errors propagate as workflow errors without recording a failed
operation receipt, because an uncertain commit is not a definitive rejection.
Preparation rejections and successful commits retain their ordinary receipts.
Caller-supplied API operation IDs and audio preprocessing remain separate work.

### Configuration and runtime context

The workflow sequences configuration replacement with tool and context
reconciliation. Identical configuration still requests repair of derived state.
Setup, configuration/profile application, and explicit skill reads reuse the
workflow refresh path. Skill reads retain their current freshness contract by
waiting for a correlated refresh result. Gateway request validation and early
reference validation remain available, while authoritative state checks happen
at admission. Pure reads project the resulting durable state.

Preparation retains at most 256 completed operation receipts, containing an
intent fingerprint and result rather than the full profile/configuration input.
Retries preserve their operation ID, submission timestamp, and intent. Retained
receipts return the original result; conflicting intent returns a conflict.
Eviction advances a persisted submission-time watermark and retires the whole
oldest timestamp group. Requests at or below that watermark, including delayed
first deliveries, return an explicit expired-receipt conflict without executing.
Callers must reload session state before submitting a new operation after expiry.
The cache and watermark survive workflow rollover.

Profile preparation validates references and materializes its toolset in one
activity. The workflow publishes that observation directly instead of repeating
configuration validation and MCP reads in another preparation activity. Resolved
profiles carry their document; unused registry identity is not copied into setup.

### Steering target

Carry the requested run ID on steering commands and validate it against the
active run during deterministic admission. A run transition between gateway
validation and signal processing must reject the steering rather than deliver
it to another run. Include replay coverage.

### Environment service boundary

Extract environment lifecycle and idle-power reconciliation from the gateway
API implementation into a shared environment service. Keep existing runtime
role ownership and recovery behavior. This is not a transfer of environment
lifecycle into the session workflow. Gateway endpoints and background
reconcilers call the same service.

Remove the unused profile-based provisioning abstraction entirely. Environments
are created, credentialed, powered, and closed independently through the
environment API. Remove profile provision intents, environment retention tied
to session closure, session-derived provision request IDs, environment session
origin records/filters, and session-deletion cleanup. Session closure or deletion
must never close an environment. Keep session environment access policy and
existing/inherited selection: those describe use, not resource ownership.
Update public contracts, persisted schema, UI, demo fixtures, tests, and current
documentation to match this boundary.

Selection is a registry-only check shared by profiles, explicit activation, and
selection tools. It checks existence, access, and nonterminal lifecycle even
when selecting the same environment again. It never changes desired power or
probes a data route; readiness and wake-on-use remain on actual-use paths.

## Acceptance

- API and internal run submissions follow the same reconciliation path.
- MCP policy edits are reflected before the affected new run uses its tools;
  disabled or invalid links retain their explicit failure behavior.
- Unchanged settings produce no tool patch; changed settings are applied once.
- Tests cover idle, active, and queued submissions, concurrent configuration
  changes, retries/replay, and preservation of built-in/workflow tools.
- The gateway run-start path performs no toolset reconciliation or waiting.
- Setup and profile application survive request-process failure and retries;
  a session cannot run with partially applied initial setup.
- Configuration outcomes cover derived tools and runtime context, including
  identical-document repair, with no gateway multi-step mutation sequence.
- Explicit skill refreshes run through the workflow and retain freshness.
- Controls remain responsive during delayed or failing preparation; a later
  valid operation succeeds after a correlated preparation failure.
- Steering cannot target a different run after a concurrent run transition.
- Environment lifecycle/power orchestration has a shared service owner outside
  the gateway API. Closing or deleting a session leaves environments intact.
- Regenerate workflow contracts and any changed public API contracts; update
  architecture and user documentation. Run focused deterministic/unit suites
  and component checks. Credentialed live tests require separately confirmed
  safe local services and are not part of the default validation run.

## Implementation progress

- [x] Expand the design and acceptance criteria before implementation.
- [x] Extract tool preparation and environment reconciliation services.
- [x] Add workflow-owned run preparation and correlated operation outcomes.
- [x] Make setup and profile/configuration application durable.
- [x] Route explicit context refresh through the workflow.
- [x] Admit steering against its requested run ID.
- [x] Remove all profile provisioning and session-bound environment lifecycle.
- [x] Complete regression coverage, generated contracts, and documentation.
- [x] Bound preparation receipts, consolidate profile preparation, remove the
  unused profile wrapper, and move reconciliation tests beside the shared diff.
- [x] Separate registry-only selection from use-time readiness and wake-up.
- [x] Prepare the complete profile/configuration candidate before atomic publication.

Audio preprocessing is intentionally unchanged; its admission behavior will be
handled in the planned audio refactor.

## Validation and rollout

This is a coordinated breaking deployment requiring fresh sessions and workflow
histories. Close existing sessions with the old runtime, stop old workers, apply
schema migration 10, and deploy matching runtime and clients before creating new
sessions. Retain stored sessions for historical reads and retain environment
resources; no database wipe is required. Restarting workers alone still replays
old histories and is not a migration strategy.

Preparation patch gates, legacy continuation readiness/receipt defaults, and
untargeted steering replay have been removed. Existing running histories and
old continuation payloads are unsupported; new continuations explicitly carry
readiness and operation receipts, and steering always names its target run.


- Compatibility cleanup validation passed: 222 engine and 133 workflow unit
  tests, plus `cargo check --workspace --all-targets`. The workflow contract
  exporter produced no artifact changes. Live histories were not replayed.
- Rust tests passed for `engine`, `temporal-workflow`, `temporal-server`,
  `profiles`, `environments`, `store-pg`, and `api`, including engine replay
  and committed contract checks. Final workflow/runtime unit reruns passed.
- `cargo check --workspace --all-targets` passed. Live suites, including the
  replacement environment-selection/independent-cleanup regression, compile;
  credentialed and service-dependent tests were not run.
- TypeScript typechecks, the full consumer test command, and production
  client/configurator/web/demo builds passed. The web suite has 328 tests,
  including start/close/delete preserving the selected environment.
- API and workflow contracts were regenerated. TypeScript consumers and the
  configuration reference were regenerated and verified stable across a
  second generation. `npm run check` reaches its Git-based generated-file
  check and reports the intentional uncommitted contract changes; its remaining
  typecheck, consumer-test, and build steps passed separately.
- Schema revision 10 removes environment session-origin columns and indexes.
  It preserves existing environments. Release metadata verification passed.
  The migration is included but was not applied to local services.
- Cleanup validation passed: 129 workflow and 328 runtime unit tests (one
  ignored), plus `cargo check --workspace --all-targets`. Receipt tests cover
  duplicate/conflicting intent, bounded eviction, delayed/expired requests,
  timestamp ties, rollover, and omission of large profile inputs. The workflow
  contract exporter completed with no additional artifact changes.
- Registry-only selection validation passed: 330 runtime and 129 workflow unit
  tests (one ignored). New regressions cover all nonterminal statuses without a
  gateway, unchanged power state, and fresh access/lifecycle checks on reselection;
  existing wake-on-use tests remain green.
- Atomic publication validation passed: 133 workflow and 331 runtime unit tests
  (one ignored), plus `cargo check --workspace --all-targets`. New coverage checks
  proposed sources, late validation failure without live mutation, concurrent
  closure/configuration changes, source invalidation inside the batch, full-batch
  replay, and storage confirmation after a lost commit response. The workflow
  contract was regenerated without additional artifact changes. Live Temporal
  tests were not run. This change needs no new schema migration or public API.

Environment creation, credentials, power, and cleanup now belong exclusively
to environment operations and their runtime service. Profiles retain only
existing-environment selection and inheritance. No session close/delete path
or session activity calls the environment lifecycle service.
