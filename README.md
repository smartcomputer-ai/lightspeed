# Forge

Forge is an SDK for building agent runtimes around a deterministic, event-sourced core.

The current focus is a small deterministic loop that can plan an agent session, record domain events, rebuild state by replaying a session log, and drive LLM/tool work through runtime-provided async traits.

That shape is meant to work well for agents running in durable workflow systems such as Temporal, hosted services, or other controlled runtimes where the agent should not assume direct ownership of an OS process, sandbox, or VM (like mosr coding agents do nowadays).

## New Direction
> Previously this repo hosted an implementation of the Attractor runtime according to StrongDM's [spec](https://github.com/strongdm/attractor), with the goal to build a dark software factory. 

We sinced moved towards building agents that orchestrate workflows directly, instead of deterministic workflow DAGs like Atrractor. But for that to work, agents have to be able to run outside sandboxes or VMs to orchestrate coding agents inside the guest-OSes.

We belive running and coordination agents at scale are best managed by durable workflow engines like [Temporal](https://temporal.io/) or [Inngest](https://www.inngest.com/). Unfortunately there is no good agent runtimes or SDKs to build agents on such platforms. So this project is attempting to close that gap.

## Design Principles
- Keep the core deterministic. `agent-core` owns the generic session log and
  the built-in CoreAgent domain: commands, events, state, planning, workflow
  request/result helpers, and replay.
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
- Keep the client boundary stable. CLIs, TUIs, editors, hosted gateways, and future Temporal frontends should consume `agent-api`, not reducer internals.


## Workspace Crates

| Crate | Path | Purpose |
|-------|------|---------|
| `agent-core` | `crates/agent-core` | Deterministic session kernel plus built-in CoreAgent: dynamic session log storage, CoreAgent command/event/state models, planning, codecs, and runner contracts |
| `agent-api` | `crates/agent-api` | Client-facing session/run/item API types, views, and notifications |
| `agent-runtime` | `crates/agent-runtime` | Local runtime composition over the core runner and CoreAgent LLM/tool traits |
| `agent-tools` | `crates/agent-tools` | Optional host filesystem/process tool package |
| `agent-eval` | `crates/agent-eval` | Eval harness for local agent/tool workflows |
| `llm-runtime` | `crates/llm-runtime` | CoreAgent LLM runtime over provider-native clients |
| `llm-clients` | `crates/llm-clients` | Provider-native OpenAI and Anthropic API clients |
| `cli` | `crates/cli` | Command-line chat host over the local runtime |

## Quick Start

Prerequisites:

- Rust toolchain with edition 2024 support
- `OPENAI_API_KEY` for live OpenAI-backed chat and eval runs
- `ANTHROPIC_API_KEY` for live Anthropic client tests

Build and test:

```bash
cargo build
cargo test
```

Run the local chat CLI:

```bash
cargo run -p cli -- chat --new
```

The `cli` package builds the `forge` binary, so installed usage remains:

```bash
forge chat --new
```

## Runtime Model

At a high level, an agent session works like this:

1. A client starts or opens a session through `agent-api`.
2. The runtime admits input as a command to `agent-core`.
3. The core appends deterministic events and updates replayable session state.
4. Planning decides whether the next step is another deterministic event or an
   async LLM/tool call.
5. The runner awaits runtime-provided `CoreAgentLlm` and `CoreAgentTools`
   implementations, then appends LLM/tool result domain events.
6. The API projects internal events/state into client-facing session, run, and
   item views.

Local mode uses weak replay semantics for those calls. Stronger durability and
retry semantics belong in a Temporal or hosted workflow runtime.

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
| `FORGE_CHAT_PROVIDER` | Default chat provider ID |
| `FORGE_CHAT_MODEL` | Default chat model |
