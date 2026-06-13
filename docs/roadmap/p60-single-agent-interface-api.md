# P60: Single-Agent Interface API

**Status**
- Deferred / not pursuing now

**Decision, 2026-05-30**

Do not replace the public `agent-api` surface with a meta
`interface/read` + `command/send` + `query/read` protocol right now.

Lightspeed's current product direction is a single configurable agent, not a
general extensible agent SDK. The public API should therefore stay
product-shaped and typed:

```text
initialize
session/start
session/read
session/events/read
session/close
run/start
run/cancel
```

This does not reject the internal architecture. `agent-core` can remain
deterministic, event-sourced, substrate-neutral, and internally dynamic through
`kind + version + payload` envelopes. The decision is only about the public
client boundary: clients should not have to speak a meta command/query API
until Lightspeed has concrete pressure from a second public agent shape or a real
cross-language SDK use case.

For now, support multiple configured agents through typed session creation and
configuration fields such as model, prompt/instructions, tools, workspace, and
profiles. Add concrete product API methods as needed, such as the typed
`session/close` and `run/cancel` lifecycle methods, or explicit
configuration/tool-profile updates, rather than introducing generic
command/query envelopes.

The remainder of this document is preserved as an archived design proposal for
future reference, not as an active implementation plan.

## Goal

Replace the current method-per-chat-operation API shape with a smaller
actor-style API waist that can support:

- one batteries-included CoreAgent/Claw product path,
- many differently configured sessions of that same agent type,
- extension modules inside that agent, such as tools, MCP, skills, memory,
  prompts, model config, and host targets,
- future custom agent compositions without forcing the current product API to
  become fully dynamic everywhere.

The first implementation should not try to expose a general plugin ecosystem,
third-party reducer registry, fleet registry, agent graph, or arbitrary
cross-language agent runtime. The goal is a narrow, durable interface
foundation:

```text
commands mutate
queries read or project
events expose committed history
interface describes commands, queries, events, and JSON Schemas
```

Lightspeed should be product-shaped first and SDK-capable underneath. CoreAgent
remains the flagship agent composition. Claw remains the first Temporal-backed
runtime for that composition. The API and storage boundaries should keep enough
room for later custom compositions, Python/Pydantic-authored schemas, skills,
MCP, memory, and additional agent types, but this roadmap builds only one
public agent type.

## Context

The current `agent-api` was intentionally client-facing and CoreAgent-shaped:

```text
initialize
session/start
session/read
session/events/read
run/start
```

This was the right first cut for proving local and Temporal-backed execution:

```text
client
  -> agent-api / JSON-RPC
  -> ClawAgentApi
  -> Temporal ClawSessionWorkflow
  -> Pg session log + CAS
  -> CoreAgent drive loop
  -> LLM/tool activities
```

However, this shape hard-codes `session`, `run`, and `item` as top-level API
concepts. Those are useful product views, but they are not the narrow waist of
the system. P54 already moved the kernel in a more general direction: the
session log stores dynamic event envelopes, and typed Rust domains use codecs
over those envelopes. The public API should align with that direction without
turning Lightspeed into a fully open programmable substrate before one excellent
agent exists.

## Design Position

Use a scoped hybrid architecture:

- Keep product views such as `SessionView`, `RunView`, and `SessionItemView`
  where they do real projection work.
- Move the protocol center to a small actor-style surface:

```text
initialize
interface/read
command/send
query/read
events/read
```

Everything else is a named command, query, or event described by
`interface/read`.

This gives clients and tools a way to introspect the CoreAgent/Claw surface
without requiring a general agent-type marketplace or arbitrary user-defined
module loading in the first cut.

P60 is not a fleet roadmap. A client can still run many sessions, but the
public type identity remains the single CoreAgent/Claw composition.

## Narrow Waist

The narrow waist should be language-neutral and JSON Schema based.

Core concepts:

```text
Interface
CommandSpec
QuerySpec
EventSpec
CommandEnvelope
QueryEnvelope
EventEnvelope
```

Avoid putting the `Agent` prefix on every subtype. `AgentInterface` or
`Interface` is fine as the document name, but the subtypes should be generic:
`CommandSpec`, `QuerySpec`, `EventSpec`, `CommandEnvelope`, etc.

### Interface

The interface is an introspection document. It is not the current state of the
agent and it is not a separate source of configuration truth.

It describes:

- command kinds the actor accepts,
- query kinds the actor answers,
- public event kinds the actor may emit,
- ordinary JSON Schemas for command payloads, query params/results, and event
  payloads,
- short documentation for humans and agents,
- optional stability/visibility metadata.

Example shape:

```json
{
  "schemaVersion": "lightspeed.interface.v1",
  "type": "lightspeed.claw",
  "version": "0.1.0",
  "commands": [
    {
      "kind": "lightspeed.run.start",
      "version": 1,
      "description": "Submit input to an opened session and wait for the resulting run.",
      "paramsSchema": { "$ref": "#/$defs/RunStartParams" },
      "resultSchema": { "$ref": "#/$defs/RunStartResponse" }
    }
  ],
  "queries": [
    {
      "kind": "lightspeed.session.view",
      "version": 1,
      "description": "Read a projected session view.",
      "paramsSchema": { "$ref": "#/$defs/SessionReadParams" },
      "resultSchema": { "$ref": "#/$defs/SessionView" }
    }
  ],
  "events": [
    {
      "kind": "lightspeed.core.run.started",
      "version": 1,
      "payloadSchema": { "$ref": "#/$defs/CoreRunStarted" }
    }
  ],
  "$defs": {}
}
```

The schema layer must be vanilla JSON Schema. Do not introduce an AIR-style
custom type language. Rust implementations can derive JSON Schema with a crate
such as `schemars`; future Python implementations should derive the same
documents from Pydantic models with `model_json_schema()`.

### Commands

Commands mutate, message, or request work from an agent.

Transport shape:

```json
{
  "target": { "id": "session_123" },
  "command": {
    "kind": "lightspeed.run.start",
    "version": 1,
    "payload": {}
  },
  "idempotencyKey": "optional",
  "correlationId": "optional"
}
```

For the current CoreAgent/Claw path, commands include:

```text
lightspeed.session.start
lightspeed.run.start
lightspeed.core.config.update
lightspeed.core.tools.set_registry
lightspeed.core.tools.select_profile
lightspeed.core.tools.set_default_target
lightspeed.core.run.steer
lightspeed.core.run.cancel
lightspeed.session.close
```

The first cut may expose only the subset needed by Claw and the CLI. Trusted
internal CoreAgent command admission can remain hidden or marked internal until
there is a clear public contract.

### Queries

Queries read deterministic state or return projections. They do not mutate.

Transport shape:

```json
{
  "target": { "id": "session_123" },
  "query": {
    "kind": "lightspeed.session.view",
    "version": 1,
    "params": {}
  }
}
```

Queries should be the only way to get current configuration, status, or
projected views. There should not be a separate manifest source of truth for a
running agent.

Examples:

```text
lightspeed.session.view
lightspeed.run.view
lightspeed.transcript.view
lightspeed.session.events_view
lightspeed.core.state
lightspeed.core.config
lightspeed.core.tooling
lightspeed.core.runs
lightspeed.claw.status
```

### Events

`events/read` reads committed durable history. It should return raw dynamic
session entries, not UI projections.

Canonical event page:

```text
position
observed_at_ms
joins
event { kind, version, payload }
```

Projected event views, if needed by a UI, are queries:

```text
query/read lightspeed.session.events_view
```

Do not use a `raw | view` flag. Raw history and projected event views are
different operations with different contracts.

Event specs are needed only for event kinds that are part of the public
history/subscription contract. Private reducer events may remain internal if
they are not exposed through `events/read` or interface discovery.

## Raw State Versus Views

Views should exist only when transformation is valuable. Avoid creating views
just for the sake of hiding a real domain type.

Use raw queries when the response is already a stable domain contract and does
not require blob reads, aggregation, or UI shaping:

```text
lightspeed.core.state      -> CoreAgentState
lightspeed.core.config     -> SessionConfig
lightspeed.core.tooling    -> ToolingState
lightspeed.core.runs       -> RunQueueState
lightspeed.claw.status     -> ClawSessionStatus
events/read           -> DynamicSessionEntry page
```

Use views when the response is a projection:

```text
lightspeed.session.view
  combines session record, lifecycle state, cwd metadata, model summary, and
  run summaries.

lightspeed.run.view
  reconstructs a run-facing transcript from context events.

lightspeed.transcript.view
  dereferences blob-backed messages/tool output into readable items.

lightspeed.session.events_view
  maps CoreAgent event internals into client-friendly event variants.

lightspeed.tool_call.view
  reads argument/output blobs and adds display metadata.
```

The existing `agent-projection` crate remains useful because it performs real
projection work: blob reads, event aggregation, status mapping, display metadata
derivation, and transcript reconstruction.

## Configuration And Manifest Position

Do not add a separate manifest as the source of truth for a running agent.

Configuration is changed by commands:

```text
open/start session
update session config
set tool registry
select tool profile
set default tool target
configure future modules such as memory/MCP/skills
```

Effective configuration is read by queries:

```text
lightspeed.core.config
lightspeed.core.tooling
future module-specific config queries
```

A saved "manifest" can exist later as authoring sugar: a template or command
bundle used to create/configure an instance. It should compile into commands.
It should not be a second durable truth model after the session exists.

## Product First, SDK-Capable Underneath

Lightspeed should not currently optimize for a fully open programmable substrate.
The first version should focus on a complete CoreAgent/Claw runtime.

Keep generic:

- session log kernel,
- dynamic command/event envelopes,
- codecs,
- replay/apply helpers,
- `AgentDomain`,
- API projection boundary,
- provider-native LLM/tool adapter boundary,
- command/query/event interface description.

Keep fixed for now:

- one canonical CoreAgent composition,
- one public configuration surface,
- one main runtime path,
- Claw as the first Temporal-backed runtime,
- one API shape clients use,
- extension mostly through tools, MCP, skills, prompts, model config, and host
  targets.
- no fleet registry, graph store, or separate agent identity layer.

Custom `AgentDomain` compositions should remain possible but not be the center
of the first public product story. Promote them only after a second real agent
forces the extension points to harden.

## Deferred Fleet Shape

Fleet and graph work is deferred to `docs/roadmap/p61-fleet-api.md`. For P60, many
running agents means many CoreAgent/Claw sessions. `session_id` remains the
durable identity exposed by the API.

## Claw First Cut

Claw should publish the `lightspeed.claw` interface from gateway/worker code, not
from each Temporal workflow instance.

Claw runtime responsibilities remain:

- one long-lived Temporal workflow chain owns one Lightspeed session,
- signals admit work into the workflow,
- activities perform storage, LLM, and tool side effects,
- Pg session log/CAS remains the durable history,
- `CoreAgentDrive` remains the deterministic loop.

The first published interface should cover:

Commands:

```text
lightspeed.session.start
lightspeed.run.start
```

Queries:

```text
lightspeed.session.view
lightspeed.run.view
lightspeed.transcript.view
lightspeed.session.events_view
lightspeed.core.config
lightspeed.core.tooling
lightspeed.core.runs
lightspeed.claw.status
```

Events:

```text
raw DynamicSessionEntry events via events/read
public CoreAgent event specs for lifecycle, run, turn, context, and tool events
```

The trusted `ClawAdmission::CoreCommand` path can remain internal unless and
until the public command envelope contract is ready for arbitrary CoreAgent
commands.

## API Transition

The new protocol center should be:

```text
initialize
interface/read
command/send
query/read
events/read
```

The existing methods can be temporarily reimplemented as wrappers during the
transition:

```text
session/start        -> command/send lightspeed.session.start
run/start            -> command/send lightspeed.run.start
session/read         -> query/read lightspeed.session.view
session/events/read  -> query/read lightspeed.session.events_view
```

However, P60 does not require backward compatibility. If keeping wrappers slows
the implementation or muddies the design, remove or break the old methods.

During implementation, some crates, binaries, tests, or clients may be broken
temporarily. This roadmap explicitly permits working through an API-breaking
slice rather than preserving dual APIs, aliases, or compatibility shims.

## Non-Goals

- A public third-party agent marketplace.
- A fleet registry or agent graph API.
- A complete plugin or reducer-module ABI.
- AIR's custom type language or canonical CBOR machinery.
- A separate manifest truth model for running agents.
- Governance/proposal/shadow/apply workflows.
- A full permission or policy engine.
- Multiple public agent types.
- Long-term backward compatibility with the current `session/*` and `run/*`
  top-level methods.

## Implementation Phases

### G1: Define The Interface API Types

- Add interface/command/query/event DTOs in `agent-api` or a small shared crate.
- Use vanilla JSON Schema references for payload/result schemas.
- Add method constants for:

```text
interface/read
command/send
query/read
events/read
```

- Add JSON-RPC dispatch support for the new methods.
- Keep naming concise: `Interface`, `CommandSpec`, `QuerySpec`,
  `EventSpec`, `CommandEnvelope`, `QueryEnvelope`, `EventEnvelope`.

### G2: Add Raw Event Reads

- Add `events/read` returning raw dynamic session entries.
- Preserve cursor/page semantics from the existing event read path.
- Do not project or decode CoreAgent events for this endpoint.
- Keep projected event pages as a named query, not as a `format` flag.

### G3: Convert Product Reads Into Queries

- Implement named queries:

```text
lightspeed.session.view
lightspeed.run.view
lightspeed.transcript.view
lightspeed.session.events_view
lightspeed.core.config
lightspeed.core.tooling
lightspeed.core.runs
lightspeed.claw.status
```

- Move old `session/read` and `session/events/read` behavior behind query
  handlers or delete the old methods if compatibility is not worth preserving.
- Keep `agent-projection` as the projection implementation for view queries.

### G4: Convert Product Mutations Into Commands

- Implement named commands:

```text
lightspeed.session.start
lightspeed.run.start
lightspeed.core.config.update
lightspeed.core.tools.set_registry
lightspeed.core.tools.select_profile
lightspeed.core.tools.set_default_target
lightspeed.session.close
```

- Start with the subset required for current CLI/Claw flows.
- Route command envelopes to typed CoreAgent/Claw operations through explicit
  decoding and validation.

### G5: Publish The Claw Interface

- Add a Claw interface descriptor.
- Include JSON Schemas for the command/query/event payloads Claw exposes.
- Return that descriptor through `interface/read`.
- Expose only public/stable command/query/event specs; mark internal entries or
  omit them.

### G6: Update CLI And Gateway

- Update the JSON-RPC gateway to serve the new API shape.
- Update the CLI to use `command/send` and `query/read` for normal flows.
- Keep or remove legacy methods based on implementation cost.

### G7: Documentation And Tests

- Update `README.md`, `AGENTS.md`, and relevant specs after the API shape lands.
- Add tests for:
  - interface discovery,
  - raw event reads,
  - projected query reads,
  - command dispatch,
  - CoreAgent extension/config command dispatch where exposed,
  - Claw gateway behavior.

## Open Questions

- Should the interface document live in `agent-api`, `agent-core::session`, or a
  new small crate?
- Which Rust schema generation crate should be used for first-cut JSON Schema?
- Should raw `CoreAgentState` be exposed publicly in `agent-api`, or should raw
  CoreAgent state queries remain an advanced/internal interface at first?
- Which extension/config commands should be public in the first Claw interface
  versus internal-only?
- Which event kinds should be considered public in `lightspeed.claw` v1?

## Success Criteria

- A client can discover Claw's command/query/event interface at runtime.
- A client can start a session and run through `command/send`.
- A client can read projected session/run/transcript data through `query/read`.
- A client can read raw committed dynamic events through `events/read`.
- Current configuration can be queried from replayed state rather than from a
  separate manifest.
- The design still centers CoreAgent/Claw as the product path while preserving
  the generic session/event waist needed for future custom agents.
- The design does not introduce fleet storage, graph links, or a second public
  agent identity before there is concrete product pressure for them.
