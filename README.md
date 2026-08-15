<p align="center">
  <img src="docs/images/logo.png" alt="Lightspeed logo" width="100">
</p>

# Lightspeed

Lightspeed is a powerful agent harness built for durable workflow engines. It allows you to run complex agents and sub-agents that survive restarts, run for months, and scale to thousands, without needing a dedicated VM for each one.

[Temporal](https://temporal.io/) is fully supported today; others are coming soon: [Restate](https://www.restate.dev/), [Inngest](https://www.inngest.com/), Hatchet, AWS Step Functions, etc. The core is written in Rust. The production data backend is Postgres and optional S3.

## Why?
**The goal of Lightspeed is to build as powerful an agent as Claude Code, Codex, or OpenClaw, but running _outside_ operating systems, thus separating the harness from compute. Plus, making this tenable for workflow engines**.

Concretely, that means the harness—the agent loop, context management, session state—runs as a lightweight durable workflow, while OS-level work (shells, code execution, full file systems) happens on machines the agent attaches to only when a task needs them. The result: thousands of agents managed by a single worker node.

<p align="center">
  <img src="docs/images/readme-why-overview.png" alt="Comparison: on the left, one OS per agent — four large VMs each hosting a single tiny agent; on the right, Lightspeed with dozens of agents packed into one worker node, borrowing VMs and sandboxes via dashed connections only when needed" width="750">
</p>

Frontier agent harnesses like Claude Code, Codex, OpenCode, or OpenClaw are designed to run inside a guest OS and need an entire OS for themselves, which makes them difficult to scale and secure. Hence the emerging pattern to ["separate the harness from compute"](https://openai.com/index/the-next-evolution-of-the-agents-sdk/#:~:text=long%2Drunning%20task.-,Separating%20harness%20from%20compute%20for%20security%2C%20durability%2C%20and%20scale,-Agent%20systems%20should), and to run agents inside workflow engines for durability. This is especially interesting in enterprise or multi-tenant settings, where you cannot easily co-locate agents on the same VM.

But most agent SDKs are not designed for workflow engines: they do not separate the deterministic core from effects such as LLM or tool calls, and they pass too much data between the core workflow logic and the effectful "tasks" or "activities"—e.g. the entire chat history back and forth—which bloats workflow histories.

One caveat we take seriously: frontier models are optimized to the hilt (via RL) assuming they control a full POSIX-compatible OS, so an agent with just MCPs and provider-native tools will underperform one with a real machine. Bridging that gap is a central goal of Lightspeed: agents can borrow compute (dedicated VMs via a bridge daemon, ad-hoc sandboxes, delegated coding-agent jobs) while the harness stays outside the OS.

**What you can build with Lightspeed**:
- An insanely **scalable OpenClaw-style personal assistant**: thousands of users, very low cost (besides tokens)
- A fully **autonomous software factory**: a fleet of agents that build, test, and critique your next feature — and keep running for weeks
- **Research agents** that spin up compute for long-running experiments, stay live for days, and supervise progress
- ...and much more!

## Features
What constitutes an "agent harness" is a rapidly expanding set of table-stakes features. Lightspeed is not 1.0 yet, but it is far enough along to try: everything checked below works today (see [Run Lightspeed Locally](#run-lightspeed-locally)), and the unchecked items are actively in flight:

**Models & providers**
- [x] **OpenAI and Anthropic, provider-native**: reasoning traces, native compaction, advanced tool configs, provider tools, files and images, OAuth login, multiple API keys
- [ ] **Other providers** via the "Completions API" standard

**Agent capabilities**
- [x] **Virtual file system**: dedicated `vfs_*` tools read and edit linked
  snapshots/workspaces without an OS attached
- [x] **Web access**: fetch, search, and extract tools
- [x] **Skills**, automatically cataloged and loaded from linked VFS roots
- [x] **Hosted MCP**, with universe-configured API-key and OAuth identities
  shared by every session selecting that MCP server id
- [x] **Flexible prompt & instruction configuration**
- [x] **Sub-agents (aka "fleets")**: agents that start and manage other agents
- [x] **Agent profiles**: reusable session setups, shared across clients and fleets

**Durability & scale**
- [x] **Long-running agents**: sessions that last weeks to months and survive restarts
- [x] **Session fork & clone**: cheap forks of a running agent's full state, straight from the event-sourced log
- [x] **Managed sessions and workflow-backed tools**: trusted workflow
  controllers can create sessions with immutable tool bindings, durable
  emissions, keyed completions, deadlines, and cancellation
- [x] **Eval harness** for regression-testing agent and tool workflows
- [ ] **Timers, schedules, wake-ups**
- [x] **Multi-tenancy**: many isolated universes (tenants) on one worker, with pluggable gateway auth

**Borrowed compute**
- [x] **Dedicated VMs**, connected as universe environment instances that
  sessions use through event-sourced active environment state; model discovery
  and selection is a separate, default-off `selectionTools` grant. Ordinary
  file and process tools always operate on the selected environment and never
  on linked VFS content. The in-repo stateless Incus provider supplies
  durable full-VM provisioning, explicit takeover of existing VMs, and
  on-demand envd routes; real
  Incus deployment still requires node certificates, an immutable image, and
  provider policy configuration
- [x] **Provider-owned jobs** for long-running work: downloads, experiments,
  and delegated coding-agent runs with optional session/run supervision. Jobs
  are an advanced, default-off environment grant and appear as model tools
  when the environment feature grants them; live availability is checked when
  invoked
- [ ] **Ad-hoc sandboxes**

**Security & auth**
- [x] **Encrypted secrets**: AEAD-encrypted secret store, plus an OAuth token broker with automatic refresh
- [x] **Credential injection**: secrets reach environments and jobs without ever being exposed to the model

**Interfaces**
- [x] **Typed JSON-RPC API**: committed schema contract, generated TypeScript client
- [x] **Configurator MCP**: a configurable universe API surface as generated tools over
  multi-tenant Streamable HTTP
- [x] **CLI** to connect to running agent sessions

The generated [JSON-RPC API reference](crates/api/contract/api-reference.md) is
derived from the same Rust manifest and schemas that drive OpenRPC, the
TypeScript client, and Configurator MCP tool descriptions.

## Design
At the heart of every agent is a carefully engineered state machine that manages what goes into the context window of the LLM.

In Lightspeed, that state machine is an event-sourced, deterministic core: it replays a session's event log into state, decides the next step, and emits effect _intents_ that runtime adapters execute against real LLM providers and tools. The core itself performs no I/O, which is exactly the shape that plays well with durable workflow engines.

Two more decisions make this practical inside a workflow engine:
1) **Minimal provider abstraction.** We extract only the information needed to decide and branch inside the deterministic core; provider-native data stays opaque and blob-backed, instead of being converted into a fake universal LLM message model.
2) **Offloading to CAS.** All data not directly needed by the workflow logic goes to content-addressed storage, so the payloads passed between workflow and activities are extremely thin and the workflow history stays small.

Lightspeed's plugin infrastructure lets external workflows add durable tools
to an agent. A plugin can create and manage a session, provide tools backed by
its own workflows, and rely on Lightspeed to deliver calls, wait for results,
handle timeouts, and cancel work. Plugins stay independent from the core
session worker.

The full design walk-through is in [docs/design.md](docs/design.md).

<p align="center">
  <img src="docs/images/readme-design-overview.png" alt="Lightspeed architecture: clients reach a session workflow holding the deterministic core inside Temporal; thin effect intents and result refs cross to activities that talk to LLM providers and borrowed compute; both sides share a session log and CAS" width="750">
</p>

## Quick Start

Prerequisites:
- Rust toolchain with edition 2024 support (e.g. [rustup](https://rustup.rs/))
- Docker with Compose for the local Postgres, MinIO, and Temporal stack
- `OPENAI_API_KEY` for live OpenAI-backed chat, tests, and eval runs
- `ANTHROPIC_API_KEY` for live Anthropic tests and eval runs

Easiest is to copy `.env_example` to `.env` and set provider keys there. The
hosted server worker mode registers real provider adapters and session-mounted
VFS tools; for OpenAI-backed local chat, set `OPENAI_API_KEY`.

Build and test:

```bash
cargo build
cargo test
```

Release construction, artifact identities, and the explicit database migration
workflow are documented in [docs/releasing.md](docs/releasing.md).

## Run Lightspeed Locally

The hosted path runs three pieces locally:

1. Docker infra: Postgres/CAS catalog, MinIO object storage, Temporal.
2. `temporal-server`: registers the Temporal workflow/activities and exposes
   the public JSON-RPC API on HTTP. Its binary is named `server`, and it
   can also run only the worker or only the gateway.
3. `cli`: starts or resumes sessions and submits chat messages through the
   gateway.

### 1. Start Local Infra

From the repository root:

```bash
local/up.sh
```

This starts Postgres on `localhost:15432`, MinIO on `localhost:29000`,
Temporal on `localhost:7233`, and the Temporal UI on `http://localhost:8233`.

Each shell that runs Lightspeed commands should load the local environment:

```bash
source local/env.sh
```

### 2. Run The Server

Open a first shell:

```bash
source local/env.sh

# export OPENAI_API_KEY=...  # omit this if it is already in .env

cargo run -p temporal-server -- migrate
cargo run -p temporal-server
```

With no subcommand, the `lightspeed-server` binary runs the gateway and Temporal worker
together in one process. The gateway listens on `http://127.0.0.1:18080` by default.
Optional health check:

```bash
curl http://127.0.0.1:18080/health
```

For split deployments, run the two roles separately:

```bash
cargo run -p temporal-server -- worker
cargo run -p temporal-server -- gateway
```

### 3. Start Chatting With The CLI

Open another shell:

```bash
source local/env.sh
cargo run -p cli -- chat --new
```

That starts an interactive TUI session. `LIGHTSPEED_API_URL` is exported by
`local/env.sh`, so you do not need to pass `--api-url`.

For OpenAI-backed chat, the CLI sends typed session/run configuration through
the API. Use `--model ...` on a command, or set `LIGHTSPEED_CHAT_MODEL`, if you want
a specific model.

Profiles can be managed through the same gateway. `profiles import` and
`profiles check` accept either one profile object or a non-empty JSON array of
profile objects:

```bash
cargo run -p cli -- profiles list
cargo run -p cli -- profiles check path/to/profile.json
cargo run -p cli -- profiles import path/to/profile.json
cargo run -p cli -- profiles read <profile-id>
cargo run -p cli -- profiles export <profile-id> --out /tmp/profile.json
```

To chat with a local directory linked as a writable CAS-backed VFS workspace:

```bash
cargo run -p cli -- chat --new --mount docs/
```

The CLI snapshots the directory locally, uploads missing blobs, creates a VFS
workspace from that snapshot, places a workspace link at `/workspace` in the
initial session config, and starts the chat session with `/workspace` as the
working directory. `--mount` and `--mount-path` are CLI convenience spellings;
the durable API/config vocabulary is workspace links.

The `cli` package builds the `lightspeed` binary, so installed usage is equivalent:

```bash
lightspeed chat --new
```

### Multi-Tenancy

One deployment serves many isolated *universes* (tenants): one gateway, one
worker, one Postgres pool, one object-store bucket. Every universe's sessions,
profiles, registries, and blobs are fully isolated; Temporal workflow ids are
composed as `{universe_id}/{session_id}` on a shared task queue.

The API never carries a universe parameter. The gateway resolves the tenant
per request based on `LIGHTSPEED_AUTH_MODE`:

- `single` (default) — the whole deployment is pinned to
  `LIGHTSPEED_PG_UNIVERSE_ID`; no credentials. This is the local/dev mode.
- `trusted-header` — bring your own auth: an upstream gateway authenticates
  callers and injects `x-lightspeed-universe: <uuid>` (optionally
  `x-lightspeed-principal: user:<id>` or `service_account:<id>`). Requests
  without the header are rejected, and unknown universes fail closed —
  universes exist only through explicit creation.
- `api-key` — built-in credentials for directly exposed deployments:
  `Authorization: Bearer lsk_…` resolves to a universe and principal.

Deployment-level administration is exposed as operator-scoped JSON-RPC
methods on the same `/rpc` endpoint (`operator/universes/create|list|read|
delete` and `operator/api-keys/create|list|revoke`), callable in
`trusted-header` and `single` modes only. API-key management is explicitly
universe-scoped; create returns the plaintext secret once, while list and
revoke return only metadata. Deleting a universe terminates its live session
workflows, sweeps its externally stored blobs, and cascades every
universe-scoped row.

Manage universes and keys with the server binary (the key secret prints
exactly once):

```bash
cargo run -p temporal-server -- universe create --slug acme
cargo run -p temporal-server -- api-key create --universe-id <uuid> --name acme-prod
cargo run -p temporal-server -- api-key list
cargo run -p temporal-server -- api-key revoke <key-prefix>
```

The CLI sends credentials from `LIGHTSPEED_API_KEY` (api-key mode) or
`LIGHTSPEED_UNIVERSE` (trusted-header mode) automatically.
See [docs/roadmap/p90-multi-tenancy.md](docs/roadmap/p90-multi-tenancy.md)
for the design.

### Configurator MCP

`platform/configurator-mcp` exposes a generated, configurable subset of the
universe-scoped JSON-RPC methods as MCP tools over stateless Streamable HTTP.
Its committed `tool-filter.json` tunes the advertised surface; deployment-level
`operator/*` methods are categorically ineligible. Each MCP POST authenticates
independently using the gateway's configured `single`, `trusted-header`, or
`api-key` mode, so one Configurator deployment can safely mediate many
universes.

With the server running locally in the default single-universe mode:

```bash
npm install
npm run build --workspace @lightspeed/configurator-mcp
LIGHTSPEED_AUTH_MODE=single node platform/configurator-mcp/dist/bin.js
```

The MCP endpoint defaults to `http://127.0.0.1:18081/mcp`; see
`platform/configurator-mcp/README.md` for multi-tenant proxy and API-key
configuration.

### Platform UI and channels

The first-party TypeScript management plane lives under `platform/`; the public
generated client lives at `clients/typescript/`. Install and check the complete
Node workspace from the repository root:

```bash
npm install
npm run check
```

Run `npm run dev` for the platform server, web UI, local Postgres, and stub
gateway development loop. See [platform/README.md](platform/README.md) for
component roles, configuration, and optional channel workers.

### Stop Or Reset Local Infra

```bash
local/down.sh
```

To reset persisted local state while keeping containers available:

```bash
local/reset.sh
```

## Testing
Default deterministic tests:

```bash
cargo test
```

Ignored live provider tests require API keys and may cost money:

```bash
cargo test -p llm-clients -- --ignored
```

## Contributing
See [CONTRIBUTING.md](CONTRIBUTING.md)

## License
[Apache 2.0](LICENSE)
