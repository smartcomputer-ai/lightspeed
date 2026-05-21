# New Agent

I want to completely rewrite and redesign the current forge agent. The goal is to build an agent that can run on Temporal.

The last few weeks, I worked on a different project that, called "agent os" or AOS. It's a very different system, but the core idea is that it is all event sourced. You can read the specs here: refs/aos-spec/

I copied the relevant code over from the old repo:
- refs/aos-agent/
- refs/aos-cli/ (see esp. refs/aos-cli/src/chat/)

That agent is _conceptually_ further along than the forge agent.

Because we want to start from scratch, I reset the forge agent crate (crates/forge-agent/). The old version, that we used to have, currently is here: refs/forge-agent/ there is some good stuff in there too. Note that crates/forge-attractor/ is currently not buildig due to that, which is fine for now. We will later have to redesign attractor too.

## How to build an agent

- do not abstract the model message structures: speak in their language.
- parse only what is absolutely necessary, in order to branch on, and leave the rest as opaque data
- store opaque data in a CAS
- consequently, once a session starts, the api type cannot be changed (ie. it is mostly dependent on the API, because a provider like openrouter pipes different models and providers through the same api)

- focus on building good primitives to mange the context window
- maintain the simplest possible deterministic state machine to plan the context window of the next turn, but no simpler
- LLMs are fundamentally "single-threaded": either an LLM generates an output, or we plan the next turn
- since we need to maintain a log of what happened, and since we're building the harness as a deterministic state machine, it makes most sense to build this as an event sourced system
- first cut: a session is the durable event-stream identity; session state is the named live state produced by replaying that session's entries
- the public persistence surface can be a session store rather than a separate journal store until we need forked/shared/rewriteable histories
- first cut has no durable session-state snapshots; resume always reconstructs `SessionState` by replaying the session log from the beginning. Snapshots can be added later as an optimization once replay cost is a demonstrated problem.

- an agent consists basically of:
	- setting up the initial conditions of the session state and event log
	- prompts, model config
	- a set of tools
	- specific context management configurations and/or implementations
	- admission of external inputs over time, of which user inputs are just one category
	- further logic to adjust the agent configuration over time, but mostly driven via events

- compaction must be a first class concept, because it rewrites the active context window
- rollback, forking, and journal rewrite/migration are out of scope for the first session-store cleanup; start with one linear event log per session
- most agents are configured in code; code-defined agents should emit resolved session config and input events rather than requiring an agent definition/version catalog in the runtime core

## `forge-llm` refactor

The next `forge-llm` should stop trying to be a unified LLM abstraction. The
agent should not pretend that `openai:responses`, `anthropic:messages`, and
`openai:completions` share one real message model. They have
different context rules, tool-call encodings, streaming events, cache/compaction
features, error shapes, and continuation semantics. The lower-level crate should
expose those API kinds natively and make it cheap for the agent runner to speak
each provider's language.

Target shape:

- `forge-llm` is a thin provider API client crate, not an agent SDK.
- Each supported API kind has its own module and typed request/response/stream
  event records:
  - `openai:responses` in `openai::responses`
  - `openai:completions` in `openai::completions`
  - `anthropic:messages` in `anthropic::messages`
- Native structs should stay close to the provider JSON. They can use serde,
  typed known fields, and escape hatches for extra provider fields, but should
  not translate into a fake common message tree before the call.
- Shared code is limited to real shared mechanics: HTTP client setup, auth
  headers, timeout/retry helpers, SSE parsing, error classification, rate-limit
  metadata, redaction/debug formatting, and small test utilities.
- The crate must not depend on `forge-agent`, CXDB, CAS, `BlobRef`,
  `ResolvedTurnContext`, session logs, or any agent persistence primitive.
- The crate should still be designed for the agent as its primary consumer:
  request builders should make tool definitions, tool results, reasoning
  controls, cache controls, streaming, token counting, and raw response capture
  straightforward for an agent adapter.

What to remove or move out:

- Remove the provider-neutral `Request`, `Message`, `ContentPart`, `Response`,
  `StreamEvent`, `ToolDefinition`, `ToolChoice`, compaction, and token-count
  types as the public center of the crate.
- Remove `ProviderAdapter`, provider factory registration, default-provider
  routing, and `Client::from_env()`. Provider selection is an agent/runner
  decision, not a global SDK decision.
- Remove the middleware chain unless a concrete need remains after the native
  clients exist. Retrying, tracing, and logging can be explicit transport
  wrappers or runner concerns.
- Move CLI black-box agent providers (`claude`, `codex`, `gemini`) out of
  `forge-llm`. They are agent backends, not LLM API clients.
- Retire cross-provider conformance tests. Replace them with provider API
  contract tests that verify the exact JSON sent to each API kind and the exact
  stream events parsed from each API kind.
- Treat the current `spec/01-unified-llm-spec.md` as legacy once this plan is
  accepted; replace it with a provider-native LLM API client spec instead of
  trying to patch the old unified abstraction.

Boundary with `forge-agent`:

- The agent owns session identity, durable event logs, CAS/blob storage,
  provider compatibility metadata, context-window planning, compaction policy,
  tool execution, retries at the workflow level, and domain events that capture
  LLM/tool results.
- `forge-llm` owns making a provider API call correctly and returning native
  provider output plus classified transport/provider failure information.
- `forge-agent-llm` is the thin CoreAgent LLM adapter layer between the agent
  core and `forge-llm`: it materializes provider-native requests from planned
  `LlmRequest` values and blob-backed context, calls the provider client, stores
  raw/native outputs, and extracts reducer facts.
- `forge-agent-tools` owns optional standard tool packages such as host
  filesystem/process tools. The core sees durable tool specs and planned tool
  invocation batches; the runtime executes them through a tool trait.
- Tool calls can carry a semantic execution target such as `host:local` or
  `host:sandbox_123`. The core records the selected target on the tool call
  start event/request, but host lifecycle, health, credentials, and VM/container
  dispatch stay with runtime/tool adapters.
- `forge-agent-runtime` composes the core runner with concrete LLM/tool
  adapters for local SDK use. Production runners can reuse the same adapters
  behind process or Temporal activity dispatch.
- `forge-agent-api` owns the client-facing session/run/item protocol. It must
  not expose core reducer internals such as `SessionEventKind`, `RunId`,
  `BlobRef`, tool/LLM execution requests, or provider-native request objects. Local runners,
  hosted/Temporal gateways, CLIs, TUIs, editors, and web frontends should meet
  at this boundary.
- The LLM adapter is intentionally thin: it converts one committed
  provider-native `LlmRequest` plus the resolved context-window items into one
  native provider request, stores raw/native outputs in the agent's blob store,
  and extracts only the fields the reducer must branch on: finish reason, usage,
  provider response id, tool calls, reasoning summary, context-limit pressure,
  and token counts.
- Real LLM/tool execution receives request records containing the session id and
  planned request/call data. Executable LLM requests already carry resolved
  context item metadata, so adapters do not need full replayed session state to
  materialize provider input.
- API kind is part of session compatibility. A session can switch models within
  an API kind only when the agent can prove the native context remains valid.
  Switching API kinds mid-session requires an explicit future rewrite or
  compaction operation; it is out of scope for the first cut.

One-shot implementation rule:

- Do the rewrite directly to the ideal provider-native structure.
- Breaking changes are acceptable.
- Do not keep the old unified API beside the new API.
- Do not add compatibility aliases, adapters, feature flags, or dual test
  suites.
- Delete the old abstraction in the same implementation slice that introduces
  the final native API modules.
- Update callers, tests, README, `AGENTS.md`, and the LLM spec to the final
  provider-native crate boundary only.

P45 design choices:

- Keep `openai:completions` in `forge-llm` as an explicit API kind, not as a
  generic provider fallback path.
- Streaming should expose a small per-provider parsed enum plus the raw SSE
  event, because the agent needs to branch on tool calls, output completion,
  and usage without reparsing JSON everywhere.
- Prefer plain provider-shaped structs first. Add builders only where provider
  JSON is too easy to assemble incorrectly.

## Temporal

With agent os, we tried to basically build somethign similar to Temporal. The better approach is to just go with temporal.

So, I want the new agent to be desgined _for_ running on temporal. We do need to decide if we can make the core temporal agnostic or if we should just build it deeply into teporal from the get-go.

## Codex

Codex is a an advanced agent written in Rust. You can find the code here: /Users/lukas/dev/tmp/codex

Another option is to link/vendor in codex and make the backend work better with Temporal instead of building an agent from scratch.

Codex app-server lessons to copy:

- Keep protocol types in a dedicated crate, separate from server
  implementation.
- Speak JSON-RPC-like request/response/notification messages at the transport
  boundary, even when an in-process client uses the same service directly.
- Project internal execution details into a client-facing view instead of
  exposing reducer or provider internals. Codex uses thread/turn/item; Forge
  should expose the same shape with Forge vocabulary: session/run/item.
- Serialize requests by logical scope where needed, especially per session.
- Let server notifications carry state deltas/events for rich clients while
  still allowing simple clients to call `session/read`.


## CLI

The cli should work similar to the codex cli or the cli we've built here: refs/aos-cli/src/chat (which was also codex inspired)

First Forge API cut:

- `initialize` returns protocol/server capabilities.
- `session/start` creates or opens a logical agent session. In the local
  implementation this creates a session log and opens it with resolved config;
  in Temporal it should signal/start the workflow behind the same method.
- `session/read` returns a full projected session view for refresh/reconnect.
- `session/events/read` returns durable session-log events after a cursor. This
  is the replayable source for CLI/TUI refresh, reconnect, polling, long-polling,
  SSE, and JSON-RPC subscriptions. It is distinct from later ephemeral token or
  tool-output streaming deltas.
- `run/start` appends user input and returns the projected run. The API run
  corresponds to a user submission/core run, not a single model reducer turn,
  because one run may require multiple model/tool rounds.
- API projection DTOs should be named as views (`SessionView`, `RunView`,
  `SessionItemView`) to keep them distinct from reducer state models such as
  `SessionState`, `RunState`, and provider-native context items.
- Notifications include `session/started`, `session/status/changed`,
  `run/started`, `item/completed`, `run/completed`, and `error`.
- The local implementation may run in-process with the CLI at first, but the
  CLI must consume only `forge-agent-api` types so moving the server behind
  stdio, UDS, HTTP, or Temporal does not change the UI contract.
