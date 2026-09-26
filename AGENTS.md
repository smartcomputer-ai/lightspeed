# AGENTS.md

Repository-specific guidance for coding agents. Keep this file short and
durable: product behavior and feature-level design belong in the code and the
linked documentation, not in an ever-growing list of historical decisions.

## Start Here

Read only the material relevant to the change:

- `README.md` — product overview, current capabilities, and basic setup.
- `docs/documentation/how-it-works/architecture.md` — architecture overview and
  links to the agent loop, context/storage, and controller workflow design.
- `docs/documentation/development/local-development.md` — local edit loops;
  neighboring guides cover tests, contracts, and releases. `scripts/dev/README.md`
  retains launcher and service details.
- `docs/documentation/reference/environment-variables.md` — authoritative environment-variable reference.
- `platform/README.md` — TypeScript management server, web UI, demo backend,
  connectors, and Configurator MCP.
- `crates/api/contract/api-reference.md` — generated public API reference.
- `docs/roadmap/` and `docs/spec/` — detailed decisions and historical context.

Use `Cargo.toml` for the current Rust workspace and the nearest `Cargo.toml`,
`package.json`, or module documentation to understand a component. Do not copy
crate inventories or feature specifications into this file.

When documentation disagrees with executable code or generated contracts,
verify the intended behavior and update the stale documentation as part of the
change.

## Architecture Boundaries

- `crates/engine` is deterministic and event-sourced. It must not perform
  network, provider, shell, filesystem, database, or workflow I/O. It emits
  intents that effectful adapters execute.
- Keep provider request and response structures native to each provider. The
  engine may retain only the provider-neutral facts needed for deterministic
  branching; wire materialization and transport configuration belong outside
  the engine and outside durable session state.
- Public clients depend on `crates/api`, never reducer internals. Keep public
  wire documentation on the Rust manifest and DTOs so generated consumers stay
  aligned.
- Side effects belong in runtime adapters, Temporal activities, stores, or tool
  packages. Domain crates such as `bots`, `channels`, and `environments` define
  records, validation, and policy without taking on infrastructure I/O.
- The Incus environment provider depends only on the environment protocol
  boundary. It must not depend on Lightspeed stores, API internals, the engine,
  or Temporal runtime.
- The hosted runtime remains one binary with selectable roles. Cross-subsystem
  work uses workflow starts and signals, not activities dispatched onto another
  role's queue. Telegram and WhatsApp connectors remain thin API/activity
  bridges with no database access or bot-routing authority.
- Managed sessions and workflow-backed tools use the generic workflow-tool
  protocol. Do not introduce feature-specific transports into the stable
  session worker.
- VFS workspaces and execution environments are distinct filesystem domains.
  Do not overlay or implicitly synchronize them.
- Preserve Rust 2024 and the existing crate-local `thiserror` style.

Feature-specific invariants (active-run control, catalogs, sub-agents, bots,
channels, MCP, environments, provider lowering, and prompt caching) are tested
and documented beside their implementations and in `docs/roadmap/`. Consult
those sources before changing the corresponding subsystem.

## Build and Test

Prefer checks scoped to the component you changed, then widen when the change
crosses boundaries:

```bash
cargo build
cargo test -p <crate>
cargo test
npm install
npm run check
```

Useful focused forms include:

```bash
cargo test -p <crate> test_name
cargo test -p <crate> -- --nocapture
npm run test --workspace <workspace>
```

Testing rules:

- Unit tests live beside the code in `mod tests`; use integration tests for
  crate boundaries or I/O.
- Async Rust tests use Tokio's current-thread flavor unless concurrency is the
  behavior under test.
- Tests must fail clearly when prerequisites or behavior are missing. External
  and credentialed suites use `#[ignore]`; do not silently skip them behind
  runtime environment checks.
- Prefer typed error assertions to brittle message matching, and keep tests
  parallel-safe with unique state and temporary paths.

### Live tests

Do not run live or credentialed tests unless the task requires them.
Live suites are marked `#[ignore]` under the relevant crate's `tests/` directory.

Temporal live tests share local Temporal and PostgreSQL state. Source the local
environment and always serialize them, including filtered runs:

```bash
source scripts/dev/env.sh
cargo test -p temporal-server --test <suite> [test_name] -- --ignored --test-threads=1
```

Avoid running `runs_live_slow`, it contains tests that wait out production activity budgets and can take roughly 30 minutes. Ask for confirmation if you need to run them (because you made a change that affects these tests directly). 

## Generated Artifacts

After changing public API wire types or method metadata, regenerate the
committed API contract and all TypeScript consumers:

```bash
cargo run -p api --bin export-schema
npm install
npm run check
```

After changing the workflow integration contract, regenerate it:

```bash
cargo run -p temporal-workflow --bin export-workflow-contract
```

The relevant Rust tests intentionally fail when committed generated artifacts
are stale. Never hand-edit generated contract or client files.

## Database and Development Runtime

Use the root launcher as the supported local entry point; details and profiles
are in `scripts/dev/README.md`:

```bash
./dev.sh
./dev.sh --plan full
./dev.sh status
./dev.sh stop
./dev.sh down
```

The Rust server never migrates PostgreSQL implicitly. Before manual startup
against a new or upgraded database, run:

```bash
cargo run -p temporal-server -- migrate
cargo run -p temporal-server -- schema-version  # diagnostic only
```

When adding, removing, or renumbering Rust schema migrations, keep
`crates/store-pg`'s `REQUIRED_SCHEMA_REVISION` and
`LIGHTSPEED_SCHEMA_REVISION` in `release/metadata.env` aligned. Verify the
release boundary with `scripts/release/verify-metadata.sh`.

## Maintenance

- Keep changes focused and preserve unrelated work in a dirty worktree.
- Edit the root `README.md` only with explicit user permission. General requests
  to update documentation do not authorize changes to that file.
- Never cite letter-P numeric roadmap identifiers in source comments, symbols,
  API documentation, test names/data, or durable documentation.
  Roadmap numbering is unstable; explain the current invariant or rationale in
  self-contained domain language instead. Roadmap files may reference one
  another inside `docs/roadmap/`.
- Update the relevant design/spec/roadmap document when a high-level architecture or public capability changes. 
- However, check with the user first before you make changes to the documentation, because he needs to validate and quality check any documentation changes.
- Record implementation progress in an active roadmap document, but do not
  promote completed roadmap detail into this file.
- Add or update tests for behavioral changes. For deterministic engine changes,
  include replay coverage or vectors where appropriate.
- When asked for repository line counts, use `cloc $(git ls-files)`.
