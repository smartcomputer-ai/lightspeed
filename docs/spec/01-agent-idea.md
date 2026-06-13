# New Agent

I want to completely rewrite and redesign the current lightspeed agent. The goal is to build an agent that can run on Temporal.

The last few weeks, I worked on a different project that, called "agent os" or AOS. It's a very different system, but the core idea is that it is all event sourced. You can read the specs here: refs/aos-spec/

I copied the relevant code over from the old repo:
- refs/aos-agent/
- refs/aos-cli/ (see esp. refs/aos-cli/src/chat/)

That agent is _conceptually_ further along than the lightspeed agent.

Because we want to start from scratch, I reset the lightspeed agent crate (crates/lightspeed-agent/). The old version, that we used to have, currently is here: refs/lightspeed-agent/ there is some good stuff in there too. Note that crates/lightspeed-attractor/ is currently not buildig due to that, which is fine for now. We will later have to redesign attractor too.

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
  exposing reducer or provider internals. Codex uses thread/turn/item; Lightspeed
  should expose the same shape with Lightspeed vocabulary: session/run/item.
- Serialize requests by logical scope where needed, especially per session.
- Let server notifications carry state deltas/events for rich clients while
  still allowing simple clients to call `session/read`.


