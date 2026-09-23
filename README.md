<p align="center">
  <a href="https://ls.bot/" target="_blank" rel="noopener">
    <img src="docs/images/ls-logo-2026-v1-ls.svg" alt="Lightspeed logo" width="92">
  </a>
</p>

# Lightspeed

<p align="center"><strong>Run thousands of agents. Efficient, durable, auditable.</strong></p>

Lightspeed is open-source infrastructure for running managed agent fleets as durable workflows.

"Managed agents" is an emerging pattern that separates the core agent loops from the VM or sandbox they use. Agents survive restarts, can run for months, and stay cheap when idle. When they
need an operating system, they borrow a real machine for as long as the task
requires.

<p align="center">
  <a href="https://ls.bot/demo/u/software-factory/bots/implementer" target="_blank" rel="noopener">
    <img src="docs/images/ls-screenshot-factory.png" alt="Lightspeed software factory with a fleet of specialized bots and an implementer supervising a sub-agent" width="1100">
  </a>
</p>

Lightspeed's Rust core runs on [Temporal](https://temporal.io/) today and stores
production data in Postgres with optional S3. The frontend is TypeScript and
React.

## Why Lightspeed?

Lightspeed aims for the capability of Claude Code, Codex, and OpenClaw without
requiring one operating system per agent. 

Most frontier harnesses live inside a guest OS, which makes them difficult to scale and secure. Hence the emerging pattern to
["separate the harness from compute"](https://openai.com/index/the-next-evolution-of-the-agents-sdk/#:~:text=long%2Drunning%20task.-,Separating%20harness%20from%20compute%20for%20security%2C%20durability%2C%20and%20scale,-Agent%20systems%20should) or the [brain and hands split](https://www.anthropic.com/engineering/managed-agents). This is especially
useful in enterprises with more stringent supervision and scaling requirements.

So, in Lightspeed, the harness (the agent loop, context, and session state) runs as a lightweight
durable workflow. Shells, code execution, and full filesystems run on machines
attached only when needed. One worker can therefore manage hundreds of agents.

<p align="center">
  <img src="docs/images/readme-why-overview.png" alt="Comparison: traditional infrastructure runs one agent per full VM, while Lightspeed packs many durable agents into one worker and attaches VMs or sandboxes only when needed" width="900">
</p>

**What you can build with Lightspeed**:

- **Personal assistants** for thousands of users without one idle VM per user
  (<a href="https://ls.bot/demo/u/personal-assistant" target="_blank" rel="noopener">Assistant Demo</a>)
- **Autonomous software factories** that coordinate agents to build, test, and
  critique features for weeks at a time
  (<a href="https://ls.bot/demo/u/software-factory" target="_blank" rel="noopener">Software Factory Demo</a>)
- **On-call operations agents** that investigate alerts, propose fixes, and
  report back through chat
  (<a href="https://ls.bot/demo/u/technical-support" target="_blank" rel="noopener">Technical Support Demo</a>)
- **Research agents** that spin up compute for long-running experiments, stay live for days, and supervise progress
- and more...

## Quick start

You need Rust with edition 2024 support, Node.js 24 or newer, and Docker with
Compose. Then start the complete
local product:

```bash
./dev.sh
```

You can set the LLM API keys directly in the UI. But you can also set them via environment variable:
```bash
cp .env.example .env
# Set OPENAI_API_KEY or ANTHROPIC_API_KEY in .env
# Then restart ./dev.sh
```

When the readiness checks pass in the CLI, open
[http://localhost:5173/app/](http://localhost:5173/app/) and sign in with the
development account printed by the launcher. The defaults are
`admin@lightspeed.dev` and `lightspeed-dev-password`.

The launcher installs dependencies, starts local infrastructure and application
processes, applies migrations, and waits until the product is ready.

For other development profiles, service addresses, resets, and live tests, see
the [local development guide](docs/documentation/development/local-development.md). See
[Environment variables](docs/documentation/reference/environment-variables.md) for environment variables.

## Features

Lightspeed covers the table stakes of a modern agent harness. Everything below works today.

**Models & providers**

- [x] **OpenAI and Anthropic**: support for reasoning, compaction, tools,
  files, images, OAuth, and multiple credentials
- [x] **Media from tools**: images and PDFs returned by MCP servers, read from
  files, or handed up by sub-agents reach the model natively
- [x] **OpenAI-compatible providers**: OpenRouter, DeepSeek, vLLM, Ollama, and
  similar servers, each configured with its own endpoint and credential
- [x] **Prompt caching**: automatic cache breakpoints and stable cache keys

**Agent capabilities**

- [x] **Virtual file system**: agents read and edit persistent files without an
  OS attached
- [x] **Web access**: provider-hosted search/fetch for Anthropic Messages,
  hosted search for OpenAI Responses
- [x] **Environment attachments**: a session attaches the machines it may use
- [x] **Skills** are sourced from either virtual file system or the attached environment or both
- [x] **MCP**: connect local or remote servers with API keys
  or OAuth; you can let the model provider handle the MCP tool calls or let Lightspeed manage them (including progressive discovery through tool search)
- [x] **Sub-agents**: delegate work to supervised child agents with configurable profiles and limits
- [x] **Agent profiles**: reusable session setups, shared across clients and sub-agents

**Bots & channels**

- [x] **Bots**: create always-on agents that wake up for scheduled tasks,
  incoming webhooks, data changes, or chat messages; session instructions
  identify their conversation, thread kind, and original routing key
- [x] **Bot federation**: bots talk to each other and coordinate work
- [x] **Triggers**: bots can create and manage their own schedules, webhooks,
  and pollers
- [x] **Chat channels**: talk to bots via Telegram and WhatsApp

**Durability & scale**

- [x] **Long-running agents**: sessions last weeks to months and survive restarts
- [x] **Active-run control**: cancel or steer a run, or queue the next message
- [x] **Session fork & clone primitives**: share stored history for branches or start from copied configuration
- [x] **Workflow-backed plugins**: external Temporal workflows can extend session with various tools and custom logic
- [x] **One backend binary**: run every role in one process or scale them
  independently across Temporal workers

**Borrowed compute**

- [x] **Dedicated VMs**: attach an existing machine or provision one through the
  included Incus provider
- [x] **Bring your own compute**: `lightspeed-envd` is the daemon that connects a running system with Lightspeed. Run it anywhere with a
  registration key and it dials in and registers itself, so NATed VMs,
  Kubernetes pods, and benchmark sandboxes need no inbound address
- [x] **Power states and idle policy**: environments pause, suspend, or stop when
  idle, then wake automatically when needed
- [x] **VFS–environment transfer**: materialize and capture files or trees on Linux
  and macOS, with whole-file reuse, bounded streaming, and atomic replacement
- [x] **Environment jobs**: run downloads, experiments, or delegated coding work in the background and check the results later

**Security & auth**

- [x] **Encrypted secrets**: credentials are encrypted at rest, with automatic OAuth token refresh
- [x] **Credential injection**: environments and jobs receive secrets without exposing them to the model
- [x] **Multi-tenant by default**: isolate tenants in universes on one deployment or run dedicated per-tenant deployments

## Design

In Lightspeed, every agent is driven by an event-sourced, deterministic core. The runtime replays the session log, decides the next step, and emits effect _intents_ that adapters execute against LLM providers and tools. The core itself performs no I/O, which makes it a natural fit for durable workflow engines.

Two more decisions make this practical inside a workflow engine:

1. **Minimal provider abstraction.** The core extracts only the facts needed to make decisions; provider-native data stays opaque and blob-backed.
2. **Offloading to CAS.** Large payloads live in content-addressed storage, keeping workflow histories small. Blobs nothing reaches any more are collected after a grace period, so deleting sessions frees their storage.

The [architecture walkthrough](docs/documentation/how-it-works/architecture.md)
introduces the system. Continue with the
[agent loop and durability](docs/documentation/how-it-works/agent-loop-and-durability.md),
[context and storage](docs/documentation/how-it-works/context-and-storage.md), and
[tools and controller workflows](docs/documentation/how-it-works/tools-and-controller-workflows.md)
to understand better how everything works and fits together.

<p align="center">
  <img src="docs/images/readme-design-overview.svg" alt="Lightspeed architecture: clients reach a session workflow holding the deterministic core inside Temporal; thin effect intents and result refs cross to activities that talk to LLM providers and borrowed compute; both sides share a session log and CAS" width="750">
</p>

## Development checks

```bash
cargo test
npm run check
```

See [Testing and evaluation](docs/documentation/development/testing-and-evaluation.md)
for focused checks, replay coverage, live-test prerequisites, and model evaluations.

## Documentation

- [Product documentation](docs/documentation/index.md) — concepts, first agent,
  compute, and self-hosting
- [Architecture and design](docs/documentation/how-it-works/architecture.md)
- [Local development](docs/documentation/development/local-development.md)
- [Environment variables](docs/documentation/reference/environment-variables.md)
- [Universes, tenant isolation, and gateway authentication](docs/documentation/deployment/multi-tenancy.md)
- [JSON-RPC API reference](crates/api/contract/api-reference.md)
- [Contributing and releasing](docs/documentation/development/contributing-and-releasing.md)

Preview the Starlight manual with `npm run dev:docs`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md)

## License

[Apache 2.0](LICENSE)
