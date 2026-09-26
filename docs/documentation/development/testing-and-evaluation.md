# Testing and evaluation

A Lightspeed change can affect the decisions an agent makes, the way those
decisions survive a restart, or how successfully a model uses a tool. Each
needs different evidence. Start with the smallest test that exercises the
changed behavior, then check the components that call it.

| Check | What it establishes |
| --- | --- |
| Unit and reducer tests | A local rule produces the expected state, decision, or error. |
| Replay and checkpoint tests | Recorded facts reconstruct the expected state, including when recovery starts from a checkpoint. |
| In-process runner tests | The core and fake adapters cooperate, including recording failures and continuing execution. |
| Live integration tests | Real services agree across a PostgreSQL, Temporal, daemon, or provider boundary. |
| Prompt evaluations | A selected model completes a case with the available tools under the tested conditions. |

For example, a file-reading evaluation can establish that a model finds a
piece of information. It cannot establish that a worker restart resumes a
run correctly. A replay test can establish reconstruction without making a
single model call. Both are useful; choose them for the property they test.

## Start with a focused check

Use the prerequisites in [Local development](local-development.md), then run
tests from the repository root. Rust tests can select a crate, a test target,
and a name filter:

```bash
cargo test -p engine
cargo test -p temporal-workflow --lib every_checkpoint_cut_reduces_to_the_same_state
cargo test -p temporal-server --lib checkpoint_plus_tail_matches_full_replay
```

The two named tests use local fixtures and in-memory state; they don't need
Temporal or PostgreSQL. `--lib` selects unit tests, while `--test <suite>`
selects a file under a crate's `tests/` directory. Add `-- --nocapture` when
test output will help diagnose a failure.

TypeScript workspaces have their own test and typecheck scripts. For example:

```bash
npm run test --workspace @lightspeed/platform-web -- src/lib/subscriptions.test.ts
npm run test --workspace @lightspeed-ai/agent-client
npm run typecheck --workspace @lightspeed/connectors
```

Read the nearest `package.json` before choosing a script. A package may use
Vitest, Node's test runner, or a focused compiler check; the workspace name
selects which command npm executes.

<a id="write-a-test-at-the-decision-boundary"></a>

## Test the rule and its consequences

Unit tests live beside their implementation in `mod tests`. Integration tests
belong under `tests/` when they exercise a crate boundary or I/O. Keep test
state distinct: use unique identifiers and temporary paths so unrelated tests
can run in parallel. Async Rust tests use Tokio's current-thread flavor unless
concurrency is the behavior being tested.

Assert the behavior that matters. Prefer a typed error to matching incidental
message wording. If a prerequisite is missing, fail with a useful explanation;
external and credentialed tests should be explicitly `#[ignore]`, rather
than silently returning success when an environment variable is absent.

For deterministic engine changes, include replay coverage. A useful pattern
is to apply commands, retain the committed events, reconstruct state from
those events, and compare it with the original result. When checkpoints are
involved, compare a full reduction with checkpoint-plus-tail recovery. The
existing [rehydration test](../../../crates/temporal-workflow/src/rehydrate.rs)
checks every cut in a small opened/closed fixture. That demonstrates the
technique; a new behavior needs a history that actually exercises it.

The engine's
[native-output replay test](../../../crates/engine/src/core/drive.rs) also
checks that recorded provider-neutral facts reconstruct the result without
reading provider blobs. The
[checkpoint tests](../../../crates/temporal-server/src/checkpoint.rs) cover
equivalent recovery and falling back to the authoritative log when a
checkpoint is malformed. These correspond to the durability model explained
in [Agent loop and durability](../how-it-works/agent-loop-and-durability.md).

When the behavior needs an execution loop, use the
[test-support runner](../../../crates/test-support/src/lib.rs). Its fake
adapters and in-memory stores let tests exercise failures such as an LLM I/O
error being recorded before the drive continues. Hosted transport and workflow
behavior need their own tests.

## Check contracts and the wider build

Generated contracts have their own freshness and compatibility tests:

```bash
cargo test -p api --test schema_artifacts
cargo test -p temporal-workflow --test workflow_contract
```

These compare exports with the committed artifacts and validate serialized
examples or protocol vectors. Follow [Changing contracts](changing-contracts.md)
when one fails after an intentional API or workflow change. Fix the authored
source and regenerate its consumers; don't repair a generated file by hand.

For a broader Rust change, the ordinary CI checks include:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
scripts/release/verify-metadata.sh
```

The workspace test command leaves ignored tests unrun. Rust CI also runs a
separate live PostgreSQL migration-ledger test against its provisioned
database, so the ordinary workspace tests alone don't reproduce that whole
job.

The root npm commands cover different amounts of work:

| Command | Coverage and requirements |
| --- | --- |
| `npm run check` | Launcher and product-identity checks, generated-artifact checks, TypeScript checks, consumer tests, and client, Configurator, product web, and demo builds. |
| `npm run test:build` | CI and release-script tests using fixtures and mocked tools. |
| `npm run check:ci` | Everything in `check`, plus build-script tests, the live Platform migration gate, and release runtime staging. |
| `npm run check:docs` | Documentation adapter tests, Astro diagnostics, and the static site build with publication checks. |

`npm run check` plans launcher profiles, which loads the local environment
and can create the daemon's working directory without starting services. Its
generated checks also rewrite outputs and compare them with Git. An
intentional generated change can therefore produce a diff failure even when
the output is correct. Review the source and generated changes together;
[the contract guide](changing-contracts.md#regenerate-the-public-api-and-consumers)
explains that distinction.

`check:ci` requires more setup. The Platform migration gate needs
`LIGHTSPEED_PLATFORM_MIGRATION_TEST_URL` pointing to a suitable test PostgreSQL
server. It creates and drops temporary databases, so its account needs those
privileges. Release staging installs package-local dependencies, uses cached
npm packages for offline runtime assembly, and needs GNU-compatible `tar`
(`gtar` is used when available). Use a focused test while iterating, and the
broader gate when checking the corresponding contribution.

The docs gate is separate from the root consumer check. A documentation change
needs `npm run check:docs` even if `npm run check` has already passed.

<a id="run-live-tests-deliberately"></a>

## Run live tests

Live suites are marked `#[ignore]` and name their prerequisites. Before
running one, establish that the selected local services and credentials are
appropriate for that test. Some tests create or modify durable state; real
provider tests also make billable model calls.

For example, this hosted-session test uses real Temporal and PostgreSQL with
a fake model provider:

```bash
./dev.sh infra
source scripts/dev/env.sh
cargo test -p temporal-server --test sessions_live \
  temporal_live_session_start_then_run_start_completes_fake_runs \
  -- --ignored --test-threads=1 --nocapture
```

It doesn't need a model key. The
[development service guide](../../../scripts/dev/README.md#manual-runtime-roles)
also gives commands for real provider execution and environment lifecycle
tests. The `environment_provider_live` suite uses an in-process provider with
the real database and reconciler; it does not require Incus.

VFS transfer coverage uses real Temporal, PostgreSQL, MinIO, and an envd that
registers with a temporary local gateway. Its scripted model exercises each
provider's tool presentation without a model key. On Linux or macOS, run:

```bash
source scripts/dev/env.sh
cargo test -p temporal-server --test vfs_transfer_live -- --ignored --test-threads=1
cargo test -p store-pg --test store_pg_live \
  pg_live_streamed_blobs_verify_ranges_reuse_and_reject_corruption \
  -- --ignored --test-threads=1
```

These tests cover profile grants, standalone hosted transfer dispatch,
multi-page inventories, binary files above the old inline limit, replacement,
content reuse, executable flags, conditional scans, and streamed CAS integrity.

Temporal live tests share the local Temporal and PostgreSQL state. Always use
`--test-threads=1`, including filtered runs, and don't run these suites
concurrently in separate terminals. Run `runs_live_slow` by itself and allow
roughly half an hour: it deliberately waits out production activity budgets.

Database migration tests have their own isolation. The Rust `migrations_live`
test creates a unique schema inside `LIGHTSPEED_TEST_POSTGRES_URL`; the
Platform gate creates scratch databases. Both need real PostgreSQL. See
[Database migrations](changing-contracts.md#database-migrations) for the
authoring and compatibility checks around them.

## Evaluate model and tool behavior

The `eval` crate runs prompt cases through the in-process agent harness. Each
attempt gets fresh in-memory session and blob stores, a temporary VFS root,
a separate temporary environment root, and an inline tool executor. It calls
the model provider directly; the hosted server, Temporal, PostgreSQL, and
`lightspeed-envd` are not required.

List the cases with:

```bash
cargo run -p eval -- list
```

Listing reads and validates case files without making provider calls. The
harness loads `.env` files before processing commands, including `list`.
`case` and `all` perform live calls. Choose an explicit API kind and model
when comparing a change; replace `YOUR_MODEL` below with an accessible model:

```bash
cargo run -p eval -- --provider openai --model YOUR_MODEL case read-file --runs 3
```

| Provider option | API and credential |
| --- | --- |
| `openai` | OpenAI Responses, using `OPENAI_API_KEY`. This is the default provider. |
| `openai-completions` | OpenAI Chat Completions, using `OPENAI_API_KEY`. |
| `anthropic` | Anthropic Messages, using `ANTHROPIC_API_KEY`. |

Without `--model`, the harness uses the provider's model environment variables
and then its compiled default. OpenAI Responses checks
`OPENAI_RESPONSES_MODEL`, Completions checks `OPENAI_COMPLETIONS_MODEL`, and
both fall back to `OPENAI_LIVE_MODEL`. Anthropic checks
`ANTHROPIC_MESSAGES_MODEL`, then `ANTHROPIC_LIVE_MODEL`. The product's
`LIGHTSPEED_CHAT_MODEL` setting does not configure this harness. Provider base
URL overrides are honored too; record them with a comparison.

### Add a case with observable assertions

Cases are JSON files under `crates/eval/cases/`. A case supplies a prompt,
initial files, an allowed tool surface, and assertions. Here is a small
release-editor case using the same source file as the
[first-agent walkthrough](../getting-started/first-agent.md):

```json
{
  "id": "read-release-change",
  "description": "Read the release source before reporting a change.",
  "prompt": "Read changes.md with the file-reading tool. Report the new export capability in Acorn 1.2.",
  "setup": {
    "files": [
      {
        "path": "changes.md",
        "content": "Acorn 1.2 adds CSV export.\n"
      }
    ]
  },
  "expect": {
    "tool_called": ["vfs.read_file"],
    "assistant_contains": ["CSV"],
    "tool_output_contains": ["Acorn 1.2 adds CSV export."]
  },
  "eval": {
    "runs": 3,
    "min_pass_rate": 1.0
  },
  "run": {
    "max_tokens": 768,
    "allowed_tools": ["vfs.read_file"]
  }
}
```

Save it as `crates/eval/cases/13-read-release-change.json`, then select
`case read-release-change`. Use `all` to run all supported cases, or
`--cases-dir /path/to/cases` to use a separate case directory. The CLI's
`--runs` overrides a case's attempt count.

`tool_called` checks logical IDs such as `vfs.read_file`, so the case does not
depend on a provider's lowered function name. Assistant and tool-output
assertions are ASCII case-insensitive substring checks. The example checks
that the model read the source and mentioned CSV; it doesn't grade the
quality of a complete release note. File assertions can check existence,
exact contents, or a case-sensitive substring when the resulting artifact is
what matters.

Use `setup.files` and `expect.files` for VFS, and `environment_files` under
each object for machine files. These domains remain separate even when they
contain the same relative path. Setup and assertion paths must be relative
and contain no `..`. The existing
[domain-edit case](../../../crates/eval/cases/10-domain-edit-isolation.json)
demonstrates independent edits in both domains.

A case can restrict `providers` to `openai` or `anthropic`. The `openai`
identity covers both OpenAI API kinds. `all` skips cases outside that
allowlist; directly requesting an unsupported case fails. An explicit empty
`allowed_tools` list also fails, so omit that field when the normal tool
surface is intended.

### Interpret and retain a result

Output reports each attempt, tools called, assertion failures, and the case's
pass ratio against its threshold. A threshold failure or execution error
returns a nonzero exit status. There is no persistent results directory or
JSON report; capture terminal output if you need a comparison. Diagnostic
previews can include case prompts, requests, and outputs. Temporary working
directories are removed when the invocation exits.

Record the source revision, case files, provider/API kind, explicit model,
endpoint settings, and attempt count. Repeated attempts estimate behavior
under those conditions. There is no seed or temperature CLI option, and a
hosted model can change between runs. Treat a pass-rate change as evidence to
investigate alongside the actual tool calls and failed assertions.
