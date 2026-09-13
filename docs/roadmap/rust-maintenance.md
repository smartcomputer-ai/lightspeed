# Rust maintenance audit follow-up

## Scope

The source audit found repeated live-test setup, daemon byte-redaction helpers,
and native MCP injection policy. Consolidate those within their existing
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
