# Forge

Forge is a hosted agent product in progress, built around a deterministic,
event-sourced engine and a Temporal-backed runtime.

The current focus is a small deterministic loop that can plan an agent session, record domain events, rebuild state by replaying a session log, and emit substrate-neutral actions for LLM/tool work.

That shape is meant to work well for agents running in durable workflow systems such as Temporal, hosted services, or other controlled runtimes where the agent should not assume direct ownership of an OS process, sandbox, or VM (like most coding agents do nowadays).

## New Direction
> Previously this repo hosted an implementation of the Attractor runtime according to StrongDM's [spec](https://github.com/strongdm/attractor), with the goal to build a dark software factory. 

We sinced moved towards building agents that orchestrate workflows directly, instead of deterministic workflow DAGs like Atrractor. But for that to work, agents have to be able to run outside sandboxes or VMs to orchestrate coding agents inside the guest-OSes.

We belive running and coordination agents at scale are best managed by durable workflow engines like [Temporal](https://temporal.io/) or [Inngest](https://www.inngest.com/). Unfortunately there is no good agent runtimes or SDKs to build agents on such platforms. So this project is attempting to close this gap.

## Design Principles
- Keep the engine deterministic. `engine` owns the generic session log and
  the built-in CoreAgent domain: commands, events, state, planning, action
  emission, request/result helpers, and replay.
- Execute side effects outside the core. LLM calls, host tools, filesystem
  access, process execution, MCP, human input, timers, retries, and cancellation
  belong in runtimes, adapters, workflow activities, or tool packages.
- Speak provider APIs natively. `openai:responses`,
  `openai:completions`, and `anthropic:messages` are different APIs with
  different context rules, tool encodings, streaming events, cache behavior,
  continuation semantics, and error shapes.
- Parse only required reducer facts for deterministic branching. Provider-native data that the reducer does not need to branch on should remain opaque and blob-backed.
- Store the rest of the user inputs, context, files, and model respones in content addressed storage and only pass refs to that data, so that the objects traveling between deterministic workflow and effexts stays thin and minimal.
- Treat context management as a first-class agent concern. The core plans context windows, records context items, and leaves room for compaction as an explicit future operation.
- Keep the client boundary stable. CLIs, TUIs, editors, hosted gateways, and future Temporal frontends should consume `api`, not reducer internals.


## Workspace Crates

| Crate | Path | Purpose |
|-------|------|---------|
| `engine` | `crates/engine` | Deterministic session kernel plus built-in CoreAgent: dynamic session log storage, CoreAgent command/event/state models, planning, codecs, and the substrate-neutral drive machine |
| `api` | `crates/api` | Client-facing session/run/item API types, views, and notifications |
| `api-projection` | `crates/api-projection` | Shared CoreAgent-to-`api` projection helpers for local and workflow-backed gateways |
| `workflow` | `crates/workflow` | Temporal workflow, signals, queries, and activity request/response DTOs |
| `worker` | `crates/worker` | Temporal worker binary and activity implementations over Pg/CAS, LLM, and tools |
| `gateway` | `crates/gateway` | HTTP/JSON-RPC gateway over Temporal and Pg/CAS |
| `test-support` | `crates/test-support` | Fast in-process runner harness for tests/evals; not a production runtime |
| `tools` | `crates/tools` | Optional host filesystem/process tool package |
| `store-fs` | `crates/store-fs` | Filesystem-backed session log and content-addressed blob store adapters |
| `store-pg` | `crates/store-pg` | PostgreSQL-backed session store and CAS catalog schema |
| `eval` | `crates/eval` | Eval harness for agent/tool workflows |
| `llm-runtime` | `crates/llm-runtime` | CoreAgent LLM runtime over provider-native clients |
| `llm-clients` | `crates/llm-clients` | Provider-native OpenAI and Anthropic API clients |
| `cli` | `crates/cli` | Command-line chat client for the API gateway |

## Quick Start

Prerequisites:

- Rust toolchain with edition 2024 support (e.g. [rustup](https://rustup.rs/))
- Docker with Compose for the local Postgres, MinIO, and Temporal stack
- `OPENAI_API_KEY` for live OpenAI-backed chat and eval runs
- `ANTHROPIC_API_KEY` for live Anthropic client tests

Easiest is to copy `.env_example` to `.env` and set provider keys there. The
hosted worker registers real provider adapters and session-mounted VFS tools;
for OpenAI-backed local chat, set `OPENAI_API_KEY`.

Build and test:

```bash
cargo build
cargo test
```

## Run Forge Locally

The hosted path runs four pieces locally:

1. Docker infra: Postgres/CAS catalog, MinIO object storage, Temporal.
2. `worker`: registers the Temporal workflow and executes activities.
3. `gateway`: exposes the public JSON-RPC API on HTTP.
4. `cli`: starts or resumes sessions and submits chat messages through the
   gateway.

### 1. Start Local Infra

From the repository root:

```bash
dev/local/up.sh
```

This starts Postgres on `localhost:15432`, MinIO on `localhost:29000`,
Temporal on `localhost:7233`, and the Temporal UI on `http://localhost:8233`.

Each shell that runs Forge commands should load the local environment:

```bash
source dev/local/env.sh
```

### 2. Run The Worker

Open a first shell:

```bash
source dev/local/env.sh

# export OPENAI_API_KEY=...  # omit this if it is already in .env

cargo run -p worker
```

Keep this process running.

### 3. Run The Gateway

Open a second shell:

```bash
source dev/local/env.sh
cargo run -p gateway
```

The gateway listens on `http://127.0.0.1:18080` by default. Optional health
check:

```bash
curl http://127.0.0.1:18080/health
```

### 4. Start Chatting With The CLI

Open a third shell:

```bash
source dev/local/env.sh
cargo run -p cli -- chat --new
```

That starts an interactive TUI session. `FORGE_API_URL` is exported by
`dev/local/env.sh`, so you do not need to pass `--api-url`.

For OpenAI-backed chat, the CLI sends typed session/run configuration through
the API. Use `--model ...` on a command, or set `FORGE_CHAT_MODEL`, if you want
a specific model.

To send one message and exit:

```bash
cargo run -p cli -- chat --new "hello from the hosted path"
```

The non-interactive command prints `connected session=...`; reuse that session
ID to continue the same conversation:

```bash
cargo run -p cli -- chat --session session_1
cargo run -p cli -- chat --session session_1 "continue the conversation"
```

To get machine-readable output for a one-shot run:

```bash
cargo run -p cli -- chat --new --json "summarize this repository"
```

To chat with a local directory mounted as a writable CAS-backed VFS workspace:

```bash
cargo run -p cli -- chat --new --mount .
```

The CLI snapshots the directory locally, uploads missing blobs, creates a VFS
workspace from that snapshot, mounts it at `/workspace`, and starts the chat
session with `/workspace` as the working directory. Use `--mount-path` to pick
a different VFS mount path.

The `cli` package builds the `forge` binary, so installed usage is equivalent:

```bash
forge chat --new
```

To upload a local directory as a CAS-backed VFS snapshot:

```bash
cargo run -p cli -- vfs snapshot .
```

The command walks the directory locally, uploads missing content-addressed blobs
through the gateway, commits the VFS manifest, and prints the resulting
`snapshotRef`. Use `--json` for a machine-readable summary.

To materialize a snapshot back to a local directory:

```bash
cargo run -p cli -- vfs materialize sha256:... ./out
```

The command downloads only blobs needed for files that do not already match
locally, writes under the selected destination, and refuses destination
symlinks that could escape that directory.

To create and mount VFS workspaces explicitly:

```bash
cargo run -p cli -- vfs workspace create sha256:...
cargo run -p cli -- vfs workspace read workspace_...
cargo run -p cli -- vfs workspace update --expected-revision 0 workspace_... sha256:...
cargo run -p cli -- vfs workspace update workspace_... sha256:...
cargo run -p cli -- vfs workspace delete workspace_...
cargo run -p cli -- vfs mount put --session session_1 --path /workspace --workspace workspace_...
cargo run -p cli -- vfs mount delete --session session_1 --path /workspace
cargo run -p cli -- vfs mount list --session session_1
```

Snapshot mounts are read-only; workspace mounts can be read-only or read-write.

### Stop Or Reset Local Infra

```bash
dev/local/down.sh
```

To reset persisted local state while keeping containers available:

```bash
dev/local/reset.sh
```

## Runtime Model

At a high level, an agent session works like this:

1. A client starts or opens a session through `api`.
2. The runtime admits input as a command to the CoreAgent drive machine.
3. The core emits append, LLM, or tool actions without performing I/O.
4. The runtime or workflow substrate fulfills those actions through stores,
   adapter traits, or workflow activities.
5. Only committed session entries are resumed into the core state.
6. `api-projection` projects internal events/state into client-facing
   session, run, and item views.

`test-support` owns an inline `SessionRunner` harness for tests and evals only.
Durability and retry semantics belong in the Temporal hosted path, where core
actions are fulfilled through activities.

The hosted `run/start` path is asynchronous at the API boundary: it returns
after the workflow has accepted and started or observed the run, while clients
continue following `session/events/read` or refreshing `session/read` for tool
activity and final output. `session/start` accepts a product-level config block
for model, instructions, generation, context, and run defaults; instructions can
be supplied as inline text or an existing CAS blob ref. `session/update` applies
revision-checked patches to idle sessions without requiring clients to resubmit
the full config. `run/start` accepts typed per-run model, generation, and limit
overrides, and input can be supplied as inline text or an existing text CAS blob
ref. The gateway owns API-to-command conversion for `run/start`: it writes
inline run input to CAS or validates the supplied ref, builds
`CoreAgentCommand::RequestRun`, wraps the encoded core command as a workflow
admission, and signals the workflow.

## Testing

Default deterministic tests:

```bash
cargo test
```

Ignored live provider tests require API keys and may cost money:

```bash
cargo test -p llm-clients -- --ignored
```

## Environment Variables

Local commands load a root `.env` file when present. Use
[.env_example](.env_example) as the template for provider credentials, or set
the same variables directly in your shell.

| Variable | Purpose |
|----------|---------|
| `OPENAI_API_KEY` | OpenAI provider authentication |
| `ANTHROPIC_API_KEY` | Anthropic provider authentication |
| `OPENAI_BASE_URL` | Override OpenAI API endpoint |
| `ANTHROPIC_BASE_URL` | Override Anthropic API endpoint |
| `FORGE_CHAT_PROVIDER` | Default chat provider ID |
| `FORGE_CHAT_API_KIND` | Default chat provider API kind |
| `FORGE_CHAT_MODEL` | Default chat model |
| `FORGE_CHAT_REASONING_EFFORT` | Default OpenAI Responses reasoning effort |
| `FORGE_CHAT_MAX_TOKENS` | Default max output token setting for chat runs |
| `FORGE_API_URL` | CLI JSON-RPC gateway URL |
| `FORGE_POSTGRES_URL` | PostgreSQL session/CAS database URL |
| `FORGE_PG_UNIVERSE_ID` | Hosted store universe UUID |
| `FORGE_TASK_QUEUE` | Temporal task queue used by worker and gateway |
| `FORGE_OBJECT_STORE_ENDPOINT` | S3-compatible object store endpoint |
