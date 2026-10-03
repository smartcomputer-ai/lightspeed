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

- `cargo test -p harness --features contract`: 225 tests passed, including ID
  validation/serialization and replay coverage with schema derives enabled.
- `cargo test -p temporal-runtime --lib worker::`: 97 tests passed; one existing
  test remained ignored. No live or credentialed suites ran.
- `cargo test -p api --test schema_artifacts -p temporal-workflow --test workflow_contract`:
  all ten tests passed, including the committed-artifact staleness gates.
- Formatting checks for changed Rust files and `git diff --check` passed.
- Strict Clippy (`--lib --tests --features harness/contract --no-deps -- -D warnings`
  for `harness` and `temporal-runtime`) found five pre-existing diagnostics in
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
- `cargo test -p temporal-runtime --lib worker::mcp::`: all 15 MCP tests passed.
- `cargo clippy -p llm-clients --all-targets --no-deps -- -D warnings` passed.
- `cargo clippy -p temporal-runtime --lib --tests --no-deps` completed with only
  the same five pre-existing diagnostics recorded above.
- Formatting checks for changed Rust files and `git diff --check` passed.
  No live or credentialed suites ran.

## Targeted command-admission cleanup

Completed 2026-09-13:

- Reuse the already-validated active run in approval decisions, removing an
  unreachable missing-run branch and a redundant run-ID comparison. Preserve
  the remaining validation order and keep the exhaustive command match.
- Share pending completion-promise failure proposals between workflow-tool
  delivery and start failures. Preserve completion-key order, missing/terminal
  promise handling, and each command's leading event and retry checks.
- Add a regression for mixed pending, missing, and terminal promises with
  completion-key order different from promise-ID order.
- `cargo test -p harness --features contract`: 226 tests passed, including
  existing replay and workflow-tool failure/idempotency coverage.
- `cargo clippy -p harness --all-targets --features contract --no-deps -- -D warnings`
  passed, as did changed-file formatting and `git diff --check`.

## Gateway session lifecycle cleanup

The [gateway lifecycle record](gateway-session-lifecycle.md) describes the
completed extraction, removal of discarded full-view projection and repeated
state loads, shared retry completion, retained declaration admission, and shared
workspace validation. Seven new offline tests exercise recovery and readiness;
the complete server library suite passed with 344 tests and one existing
ignored test. Live suites were not run.

## Harness and runtime naming

Implemented 2026-10-03:

- [x] Rename the deterministic agent crate to `harness` and the hosted
  composition crate to `temporal-runtime`, preserving their existing boundaries.
- [x] Rename the executable to `lightspeed-runtime` and align local launchers,
  CI, container entrypoints, release archives, and manifest artifact names.
- [x] Update source references and documentation, regenerating contract and
  consumer documentation from the authored sources.

Validation:

- `cargo test --workspace --locked`: 2116 tests passed; 237 ignored.
  No live or credentialed tests ran.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` passed.
- `npm run check:dev`, `npm run test:build`, `npm run typecheck`,
  `npm run check:docs`, and `npm run build:docs` passed.
- `cargo build -p temporal-runtime --bin lightspeed-runtime --locked` passed.
- Release metadata and shell syntax checks, Rust formatting, and
  `git diff --check` passed. The renamed executable reports
  `lightspeed-runtime` in its version and help output.
- Regenerated API and workflow schemas retain the same structure outside
  descriptions; the Cargo lockfile changes only package and dependency names.

## JavaScript package and backend naming

Implemented 2026-10-03:

- [x] Standardize JavaScript workspace packages on `@lightspeed-ai`, name the
  public SDK `@lightspeed-ai/sdk`, and retain private publication settings
  on internal packages.
- [x] Rename the Platform backend package and directory to
  `@lightspeed-ai/platform-backend` and `platform/backend`.
- [x] Align imports, generated policy imports, workspace commands, npm
  lockfiles, local startup, release staging/publication, and documentation.

Validation:

- `npm test`: 1,170 workspace tests passed.
- `npm run build`, `npm run typecheck`, `npm run check:dev`,
  `npm run check:identity`, `npm run test:build`, and `npm run check:docs`
  passed. The build includes the public client, Configurator, live web UI,
  and demo UI.
- Generated client/schema, Configurator, policy, and profile-reference files
  regenerate without further changes.
- Built a staged `@lightspeed-ai/sdk` tarball, installed it into the
  staged Configurator with its standalone lockfile, and verified imports.
  Staged both runtime bundles and verified backend and connector imports
  from their extracted artifacts. No packages were published.
- Registry dependency versions and integrity hashes remain unchanged;
  internal packages retain `private: true`. Obsolete workspace lock entries
  were removed. Shell syntax and `git diff --check` passed.

SDK name finalized on 2026-10-03: the public package is `@lightspeed-ai/sdk`,
including its `/workflow` and schema exports. Installation examples, imports,
workspace commands, publication metadata, and the bundled `sdk.tgz` agree.
`npm run check`, `npm run check:docs`, and `npm run test:build` passed.
Staged SDK, Configurator, backend, and connector packaging/import checks passed;
no package was published. Dependency versions remain unchanged.

## Coordinated JavaScript dependency update

Implemented 2026-10-03:

- [x] Update Node to 24.21.0 across CI and all four container images, including
  verified image digests, and require that version for repository development.
- [x] Align shared dependencies across workspaces and refresh the root, SDK,
  and Configurator lockfiles. Major toolchain updates include TypeScript 6.0.3,
  Vite 8.3.2, Vitest 5.0.3, jsdom 30.1.1, and React's Vite plugin 6.1.1.
  React/React DOM are 19.3.0; Temporal packages are uniformly 1.24.0;
  MCP client/server are 2.3.0 with their compatible Node adapter 2.1.1.
- [x] Declare Better Auth explicitly for database auth code generation,
  remove TypeScript's deprecated `baseUrl`, and adapt test mock types and
  browser/Markdown assertions to the upgraded libraries.
- [x] Retain TypeScript 6 while `@astrojs/check` supports only 5/6, Mermaid 11
  while `astro-mermaid` supports only 10/11, and Node type declarations on 24
  to match the runtime. Retain stable Drizzle releases and the existing
  Baileys release-candidate track.

Security review after the update:

- Root audit findings decreased from 28 to 17 package entries (13 high,
  four moderate), arising from three underlying advisories. SDK and
  Configurator standalone audits are clean, including development dependencies;
  the connectors' production audit is also clean.
- The remaining high findings are
  [braces in shadcn tooling](https://github.com/advisories/GHSA-vfj7-8cjw-p6xm)
  and [http-cache-semantics in Astro](https://github.com/advisories/GHSA-ch52-4w7c-c8xp).
  Neither advisory currently has a patched release. The documentation site
  and web UI are built as static assets.
- The four moderate findings are the deprecated esbuild-kit chain in stable
  Drizzle Kit, ending in [esbuild's development-server advisory](https://github.com/advisories/GHSA-67mh-4wv8-2f99).
  Better Auth's optional peer also brings this chain into the backend's
  production dependency graph; the application does not run an esbuild server.
  No forced downgrades or unverified dependency overrides were applied.

Validation on Node 24.21.0 and npm 11.19.0:

- Clean `npm ci`, `npm run check`, `npm test` (1,170 workspace tests),
  `npm run test:build` (16 checks), and `npm run check:docs` passed.
  Generated contracts and consumer files remain unchanged.
- The SDK tarball builds from its standalone lockfile; the staged Configurator
  installs and imports it. Both runtime bundles stage and import successfully.
  Temporal's native core initializes and shuts down without a server.
- Workspace dependency ranges agree, all three lockfiles match their manifests,
  and `git diff --check` passes. No package was published.
- Live Temporal, SSO, and PostgreSQL migration suites were not run; offline
  backend/auth tests passed, including their PGlite coverage. Container images
  were not rebuilt locally.
