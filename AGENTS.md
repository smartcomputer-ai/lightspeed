# AGENTS.md

Guidance for agents working in this repository.

Note: `CLAUDE.md` is a symlink to `AGENTS.md`.

## Project Shape

Lightspeed is moving toward a single hosted agent product built around a
deterministic, event-sourced engine and a Temporal-backed runtime. The current
direction is product-first, not a general agent SDK or an Attractor/factory
pipeline runner.

Use these files as the index:

- `README.md` — current overview, crate map, runtime model, commands.
- `docs/design.md` — public design walk-through (deterministic core, context
  management, CAS offloading, Temporal hosting), moved out of the README.
- `docs/spec/01-agent-idea.md` — working design notes for the new agent direction.
- `Cargo.toml` — workspace membership.
- `interop/` — API contract artifacts, private clients, and bridge packages.
- `local/` — local Docker stack, environment exports, and reset helpers.
- `docs/roadmap/` — implementation plans and historical milestones.

## Build & Test

```bash
cargo build
cargo test
cargo test -p engine
cargo test -p api
cargo test -p api-projection
cargo test -p temporal-workflow
cargo test -p temporal-server
cargo test -p test-support
cargo test -p tools
cargo test -p store-fs
cargo test -p store-pg
cargo test -p mcp
cargo test -p messaging
cargo test -p auth
cargo test -p environments
cargo test -p llm-runtime
cargo test -p llm-clients
cargo test -p eval
cargo test -p cli --tests
cargo test -p llm-clients test_name
cargo test -p llm-clients -- --nocapture
```

Live provider tests are ignored by default and require API keys:

```bash
cargo test -p llm-clients --test openai_responses_live -- --ignored
cargo test -p llm-clients --test openai_completions_live -- --ignored
cargo test -p llm-clients --test anthropic_messages_live -- --ignored
cargo test -p llm-runtime --test openai_responses_live -- --ignored
cargo test -p llm-runtime --test anthropic_messages_live -- --ignored
```

Additional per-capability live suites exist for both providers under
`crates/llm-runtime/tests/` (`*_compaction_live`, `*_mcp_live`,
`*_prompts_live`, `*_skills_live`).

After changing `api` wire types, regenerate the committed contract artifacts
under `interop/contract/` (`cargo test -p api` fails while they are stale):

```bash
cargo run -p api --bin export-schema
```

CLI usage:

```bash
cargo run -p cli -- chat --api-url http://127.0.0.1:18080/rpc --new
cargo run -p cli -- chat --api-url http://127.0.0.1:18080/rpc --new "summarize this repository"
cargo run -p cli -- chat --api-url http://127.0.0.1:18080/rpc --new --json "summarize this repository"
# Run the server before using --api-url.
cargo run -p temporal-server
cargo run -p cli -- chat --api-url http://127.0.0.1:18080/rpc --session session_1 "hello"
```

## Crates

- `crates/engine/` — deterministic session kernel plus built-in CoreAgent:
  dynamic session log storage, CoreAgent command/event/state models, planning,
  codecs, storage traits, and the substrate-neutral drive machine.
- `crates/api/` — client-facing session/run/item/profile API types, views,
  notifications, and JSON-RPC method DTOs.
- `crates/api-projection/` — shared CoreAgent-to-`api` projection
  helpers for local and workflow-backed gateways.
- `crates/temporal-workflow/` — Temporal workflow, signals, queries, and
  activity DTOs.
- `crates/temporal-server/` — hosted runtime binary and modules for the Temporal
  worker, HTTP/JSON-RPC gateway, profile applier, and combined
  local/small-deployment mode.
- `crates/test-support/` — fast in-process runner harness for tests/evals. It
  is not a production runtime and must not expose an `AgentApiService`.
- `crates/tools/` — optional tool packages for session filesystems,
  environment actions, web, messaging, prompts, and skills.
- `crates/store-fs/` — filesystem-backed session log and content-addressed blob
  store adapters.
- `crates/store-pg/` — PostgreSQL-backed session store, CAS catalog, MCP server
  catalog, agent profile catalog, environment registry, and AEAD-encrypted auth
  grant/secret storage.
- `crates/messaging/` — channel-neutral outbound message types and the
  delivery outbox store trait backing the messaging tools and bridges (P71).
- `crates/mcp/` — provider-independent remote MCP server catalog DTOs,
  validation, and store traits.
- `crates/profiles/` — agent profile registry validation helpers,
  errors, update records, and the substrate-neutral `ProfileStore` trait over
  `api` profile DTOs.
- `crates/auth/` — generic auth grant/secret/provider records,
  OAuth client and authorization-flow records, PKCE helpers, the MCP OAuth
  and GitHub App drivers, store traits, typed broker errors, the runtime
  token broker with single-flight refresh and on-demand minting (P69), and
  deployment-scoped inbound API keys for gateway authentication (P90).
- `crates/environments/` — environment provider, host target, and
  session environment binding DTOs, validation, errors, and store traits.
- `crates/eval/` — eval harness for agent/tool workflows.
- `crates/llm-runtime/` — CoreAgent LLM runtime from planned requests to
  provider-native client calls.
- `crates/llm-clients/` — provider-native OpenAI and Anthropic API clients.
- `crates/cli/` — command-line chat client for the API gateway.

## Architecture Rules

- Keep `engine` deterministic. It should not execute provider calls, shell
  commands, filesystem operations, network I/O, or workflow activities.
- Execute side effects outside the core through runtime adapters, workflow
  activities, or tool packages. CoreAgent uses separate LLM and tool traits
  rather than a generic effect event lifecycle.
- Keep provider message/request/response structures native to each API kind.
  Do not rebuild a fake universal LLM message model.
- Parse only reducer facts needed for deterministic branching; keep other
  provider-native data opaque/blob-backed.
- Keep provider request vocabulary out of `engine`. The core plans a
  provider-neutral `LlmRequest` intent with opaque `ProviderParams`
  (`api_kind` + versioned JSON body) transiently for runtime execution; durable
  planned-turn events store only fingerprints and revisions. Typed param schemas
  and wire-request materialization live in `llm-runtime` adapters, and admission
  boundaries validate params before they enter the session log. Transport config
  (base URLs, credentials, headers) stays in runtime deployment config, not in
  `ModelSelection` or the session log.
- Keep clients on `api`. CLIs, TUIs, editors, hosted gateways, and future
  Temporal frontends should not consume reducer internals directly.
- Treat hosted `run/start` as an acceptance/start boundary, not a final-output
  boundary. Clients should follow `session/events/read` or refresh
  `session/read` for progress and completion.
- Preserve Rust 2024 and the existing crate-local `thiserror` error style.
- Use `tokio` current-thread tests where async tests are needed.

## Environment

Local commands load a root `.env` file when present. The `.env` file usually exists in most develeopment environments, and various live commands can be run checking with the developer first.

| Variable | Purpose |
|---|---|
| `OPENAI_API_KEY` | OpenAI provider authentication |
| `ANTHROPIC_API_KEY` | Anthropic provider authentication |
| `OPENAI_BASE_URL` | Override OpenAI API endpoint |
| `ANTHROPIC_BASE_URL` | Override Anthropic API endpoint |
| `LIGHTSPEED_CHAT_PROVIDER` | Default chat provider ID |
| `LIGHTSPEED_CHAT_MODEL` | Default chat model |
| `LIGHTSPEED_SECRETS_MASTER_KEY` | Base64 32-byte AES key for the encrypted secret store |
| `LIGHTSPEED_PUBLIC_BASE_URL` | Externally reachable gateway base URL for the OAuth callback (defaults to `http://{bind}`) |
| `LIGHTSPEED_AUTH_MODE` | Gateway tenant resolution: `single` (default), `trusted-header`, `api-key` (P90) |
| `LIGHTSPEED_UNIVERSE_AUTO_CREATE` | `trusted-header` mode: create unknown universes on first use (default false) |
| `LIGHTSPEED_API_KEY` | Client-side (CLI/bridge): bearer key sent to an `api-key`-mode gateway |
| `LIGHTSPEED_UNIVERSE` | Client-side (CLI/bridge): universe header sent to a `trusted-header`-mode gateway |
| `LIGHTSPEED_BLOB_CACHE_BYTES` | CAS blob cache budget per process (`0` disables; default 256MiB) |

## Test Rules

- Unit tests live next to code in `mod tests`; integration tests go under
  `tests/` when they cross crate boundaries or hit I/O.
- Tests must fail when the thing they test does not work.
- Do not silently skip tests with runtime env-var gates. Use `#[ignore]` for
  tests that require API keys, external services, or other opt-in resources.
- When an ignored test is explicitly run, it must fail clearly if its
  prerequisites are missing.
- Prefer asserting error kinds/types over brittle string matching.
- Keep tests parallel-safe: avoid shared global state and non-unique temp paths.

## Maintenance

- If high-level architecture changes, update `README.md`, this file, and the
  relevant docs under `docs/spec/` or `docs/roadmap/`.
- When a roadmap item is completed or partially completed, mark what changed in
  that roadmap file.
- When asked how many lines of code, use `cloc $(git ls-files)`.
