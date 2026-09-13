# Gateway session lifecycle cleanup

Implemented 2026-09-13.

## Implementation

- [x] Group session creation wrappers, argument construction, start recovery,
  readiness waiting, managed-creation validation, and preparation operations in
  `gateway/service/session_lifecycle.rs`. Public `AgentApi` methods remain thin;
  general workflow interaction stays in `workflow.rs`, profile resolution/CRUD
  in `profiles.rs`, and activity materialization in `session_preparation.rs`.
- [x] Return `LoadedSession` after readiness. The start response and managed
  fingerprint check reuse that state. Start no longer builds a discarded full
  session view, looks up its retention root, or reloads the same state for the
  response. Full-view projection errors consequently no longer block a start
  response that does not require that projection.
- [x] Share retry completion for stored open sessions, running workflows whose
  session row is not ready, and concurrent-start conflicts. Closed sessions
  return their already-loaded state without signaling or waiting.
- [x] Retain the admitted managed declaration for materialization and creation
  fingerprint comparison within the gateway start request. Original workflow
  arguments and independent workflow/engine admission remain unchanged.
- [x] Share workspace-target validation on `SessionPreparationService`, with
  gateway preflight delegating to it. Move the preparation-service factory
  beside that service. Keep both API-time and activity-time validation.

Known stored sessions validate a supplied managed declaration before signaling
setup retry. A running workflow without ready durable state validates after
setup completes. Recovery still precedes resolution of mutable named profiles.
Blob-grace refresh still precedes the Temporal start; sub-agent retention,
setup intent, and metadata validation order remain unchanged.

Sub-agent profile validation retains its existing boundary-specific error
mapping. Environment checks at creation and preparation retain their distinct
work and timing. This refactor does not merge those policies.

## Validation

Seven new offline tests use a private four-operation I/O interface around the
actual recovery and readiness code. Ordered scripts fail on unexpected or
missing loads, describes, retry signals, and status queries. They cover:

- Matching/conflicting managed declarations against closed/open sessions,
  including validation before any retry signal.
- A running workflow with a missing or new session row, readiness polling,
  validation after setup, and returning recovered state before fresh creation.
- Missing/new sessions without a running workflow proceeding to creation.
- Concurrent-start conflict recovery and propagation of a missing-row error.
- Setup/workflow errors taking precedence over readiness, query errors,
  successful readiness followed by exactly one state load, and timeout.
- Load, describe, and retry-signal errors terminating further I/O.

The existing fingerprint-matching test moved beside its implementation.

- `cargo check -p temporal-server --lib` passed.
- `cargo test -p temporal-server --lib gateway::service::`: 128 tests passed.
- `cargo test -p temporal-server --lib`: 344 tests passed; one existing test
  remained ignored.
- The eight lifecycle tests passed again after a test-fixture lint cleanup.
- `cargo clippy -p temporal-server --lib --tests --no-deps` completed with four
  pre-existing diagnostics and no new warnings. Moving the lifecycle methods
  removed the previous test-module-ordering warning in `workflow.rs`.
- Changed-file formatting and `git diff --check` passed.

No live or credentialed suites ran. The offline recovery tests do not replace
end-to-end Temporal/PostgreSQL coverage of first creation or profile mutation
between requests. The existing live suites remain available under the
repository's local-service confirmation requirement.

## Preserved race behavior

After a concurrent start returns a conflict, the gateway still immediately
loads the session row. A missing row remains an error in that branch. Extending
it to the earlier describe/recovery path would be a separate behavior change;
this refactor explicitly preserves and tests the existing result.
