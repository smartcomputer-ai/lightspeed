# Rust maintenance audit follow-up

## Scope

The source audit found repeated live-test setup, daemon byte-redaction helpers,
native MCP injection policy, worker universe resolution, batch dispatch, and
engine string-ID definitions. Consolidate those within their existing
components before attempting larger command/state-machine refactors.

## Implementation

- [x] Move live provider configuration and dotenv lookup into
  `crates/llm-runtime/tests/support/config.rs`. Responses and Anthropic suites
  share client/model defaults; suite-specific model overrides and Anthropic MCP
  beta headers remain explicit. All suites now skip malformed dotenv lines and
  remove only matching outer quotes, using the existing shared-parser behavior.
- [x] Share byte-redaction and secret-value collection privately within
  `environment-daemon`. Preserve the current per-buffer algorithm and process/job
  lifecycle behavior.
- [x] Share native MCP inventory retrieval, ordering, exposed-name filtering,
  error mapping, and cumulative request-cap accounting in `llm-runtime::mcp`.
  Provider adapters still own native request construction and catalog collision
  checks; search/provider-hosted exposure is unchanged.
- [x] Add offline tests for configuration precedence and dotenv edge cases,
  redaction of binary/repeated output, and MCP filtering, cumulative caps,
  resolver errors, and duplicate-name retention.

## Validation

Completed 2026-09-13:

- `cargo test -p llm-runtime --lib --test live_config`: 150 runtime tests and
  seven offline configuration tests passed.
- `cargo test -p environment-daemon --lib`: 80 tests passed.
- `cargo test -p llm-runtime --tests --no-run`: all integration/live targets
  compiled without running credentialed suites.
- `cargo clippy -p llm-runtime -p environment-daemon --all-targets --no-deps -- -D warnings`
  passed, as did formatting checks for changed Rust files and `git diff --check`.

Larger changes to admission, tool-result handling, and filesystem traversal
remain follow-up work; they need targeted behavioral/replay coverage before
restructuring.

## Worker dispatch and engine ID follow-up

- [x] Share fixed/runtime universe resolution between bot and channel activity
  adapters. Wrong or unknown universes remain non-retryable; runtime lookup
  failures remain retryable. Role-specific activity registration stays separate.
- [x] Classify each ordinary batch call once and use one dispatch loop for
  workflow, concurrency, environment-control, job-read, and inline calls.
  Preserve the special-only path's skipped domain setup, denial precedence,
  await handling, input order, shared promise numbering, workflow sibling caps,
  and environment cleanup on success or an ordinary error.
- [x] Share the engine's identical string-ID macro privately between session and
  core IDs, retaining validators, serialization, and optional schema derives.
  Numeric-ID macros retain their differing default behavior.
- [x] Add offline checks for universe mismatch/retryability and for equivalent
  special-call dispatch with and without an inline VFS sibling, including
  failed workflow validation and cumulative emission caps.

Validation on 2026-09-13:

- `cargo test -p engine --features contract`: 225 tests passed, including ID
  validation/serialization and replay coverage with schema derives enabled.
- `cargo test -p temporal-server --lib worker::`: 97 tests passed; one existing
  test remained ignored. No live or credentialed suites ran.
- `cargo test -p api --test schema_artifacts -p temporal-workflow --test workflow_contract`:
  all ten tests passed, including the committed-artifact staleness gates.
- Formatting checks for changed Rust files and `git diff --check` passed.
- Strict Clippy (`--lib --tests --features engine/contract --no-deps -- -D warnings`
  for `engine` and `temporal-server`) found five pre-existing diagnostics in
  untouched code: a blank line after a doc comment, two collapsible conditionals,
  and two test-module ordering warnings. These are outside this refactor. The
  same command without `-D warnings` completed with only those diagnostics.

## MCP rendering and OpenAI configuration follow-up

- [x] Share binary MCP result admission, result-wide media caps, MIME
  normalization, omission notes, and media record construction in a private
  renderer. Preserve per-kind numbering, asset indices, resource names, audio
  rejection, and validation precedence when the shared cap is already full.
- [x] Share OpenAI base-URL/organization/project environment overrides and
  organization/project header validation privately within `llm-clients`.
  Preserve empty-value overrides, field-specific errors, endpoint defaults,
  request timeouts, and each client's JSON or multipart content type.
- [x] Add a mixed image/resource cap regression, offline configuration tests
  using an injected environment lookup, and local HTTP checks for all three
  clients' outgoing headers and endpoint paths.

Validation on 2026-09-13:

- `cargo test -p llm-clients --lib --test openai_endpoint_override`: 42 unit
  tests and six local HTTP/endpoint tests passed.
- `cargo test -p temporal-server --lib worker::mcp::`: all 15 MCP tests passed.
- `cargo clippy -p llm-clients --all-targets --no-deps -- -D warnings` passed.
- `cargo clippy -p temporal-server --lib --tests --no-deps` completed with only
  the same five pre-existing diagnostics recorded above.
- Formatting checks for changed Rust files and `git diff --check` passed.
  No live or credentialed suites ran.
