# P61: Fleet API

**Status**
- Preliminary
- Deferred until after P60's single-agent interface API proves out.

## Goal

Define the future shape for managing many logical agents without adding that
surface to P60.

P61 should eventually support:

- a registry of public agent types,
- many logical agent instances,
- one logical agent backed by one or more sessions,
- simple agent graph links,
- command/query/event routing through the P60 actor-style API waist.

This document is intentionally preliminary. It records the direction and the
conditions that should justify implementation. It is not a commitment to build
fleet storage, graph links, or policy machinery before the single-agent product
path is working well.

## When To Build

Start P61 only after at least one of these becomes true:

- there is a real second public agent type, not just another CoreAgent config,
- one logical agent needs multiple durable sessions or conversations,
- clients need to list, label, group, or reconnect to many long-lived agents
  independently of raw session ids,
- agent-to-agent links need durable semantics,
- hosted operation needs a registry layer for lifecycle, ownership, or routing.

Until then, many agents should be represented as many CoreAgent/Claw sessions.

## Design Position

Fleet is a registry and routing layer around agents. It should not replace the
session log or the P60 command/query/event interface.

Keep these identities separate:

- `session_id` is the durable event-stream identity.
- `agent_id` is a logical product identity that may wrap one or more sessions.
- `type_id` identifies the public agent type and its interface.

In the first real implementation, `agent_id` may wrap a single `session_id`.
Do not split them until a product workflow needs the distinction.

## Concepts

### Agent Type

An agent type describes a public runtime/composition that can be instantiated.

First-cut fields:

```text
type_id
display_name
version
interface
runtime_kind
stability
metadata
```

Initial type:

```text
type_id = forge.claw
runtime_kind = temporal.claw
interface = forge.claw interface from P60
```

Do not turn the type registry into a marketplace. It can start as a static
server-published list.

### Agent Node

An agent node is a logical running agent.

First-cut fields:

```text
agent_id
type_id
primary_session_id
display_name
status
created_at_ms
updated_at_ms
tags
metadata
```

For `forge.claw`, the first implementation can map one agent node to one Claw
session. Later, a node can own multiple sessions when one logical agent needs
separate conversations, tasks, or memory scopes.

### Agent Link

An agent link is a durable relation between two logical agents.

First-cut fields:

```text
from_agent_id
to_agent_id
relation
grants
created_at_ms
updated_at_ms
metadata
```

`relation` and `grants` should start as simple strings. Do not add a full
policy engine until agent-to-agent execution requires it.

## API Shape

Use the P60 transport methods:

```text
command/send
query/read
events/read
interface/read
```

Fleet operations are named commands and queries, not new top-level JSON-RPC
methods.

Candidate queries:

```text
forge.agent.types.list
forge.agent.type.read
forge.agent.list
forge.agent.read
forge.agent.links.list
```

Candidate commands:

```text
forge.agent.create
forge.agent.update_metadata
forge.agent.archive
forge.agent.links.upsert
forge.agent.links.delete
```

Candidate events:

```text
forge.agent.created
forge.agent.metadata_updated
forge.agent.archived
forge.agent.link_upserted
forge.agent.link_deleted
```

## Routing

P60 targets sessions directly:

```json
{ "target": { "id": "session_123" } }
```

P61 can add explicit target variants:

```json
{ "target": { "sessionId": "session_123" } }
{ "target": { "agentId": "agent_123" } }
```

Routing by `agentId` should resolve to the current backing session or runtime
endpoint for that agent node. The command/query payload shape should remain the
type's published interface.

## Storage Position

Fleet metadata should live outside individual agent session logs. A future
fleet registry may have its own event log or catalog tables, but individual
CoreAgent sessions should not need to replay fleet membership to reconstruct
their own state.

Agent creation may compile a template or manifest into ordinary P60 commands:

```text
forge.session.start
forge.core.config.update
forge.core.tools.set_registry
forge.core.tools.select_profile
```

The running session's effective configuration is still read through queries
against replayed session state.

## Non-Goals

- A public third-party marketplace.
- Arbitrary plugin or reducer loading.
- A full authorization or policy engine.
- Cross-language runtime loading.
- Replacing session/run/item product views.
- Moving CoreAgent state into a fleet registry.
- Supporting multi-agent coordination before links have concrete workflows.

## Preliminary Phases

### G1: Static Type Discovery

- Expose a static `forge.agent.types.list` query.
- Include only `forge.claw` at first.
- Reuse the P60 interface descriptor for the type.

### G2: Agent Node Registry

- Add `AgentNode` records for logical agents.
- Let `agent_id` wrap one `primary_session_id`.
- Support create/read/list/archive.

### G3: Agent Target Routing

- Allow `command/send` and `query/read` to target `agentId`.
- Resolve the agent node to its current backing session/runtime endpoint.
- Preserve direct `sessionId` targeting for lower-level clients.

### G4: Links

- Add simple durable links between agent nodes.
- Keep relation and grant values as strings.
- Do not enforce fine-grained policy yet.

### G5: Operational Hardening

- Add ownership/tenant fields if hosted operation requires them.
- Add lifecycle/status projection for long-lived agents.
- Add migration rules for nodes whose backing sessions change.

## Open Questions

- What concrete workflow first requires `agent_id` to differ from `session_id`?
- Should the type registry be static server metadata or durable catalog data?
- Should fleet registry changes have their own event log?
- Which agent links require enforcement rather than display/routing metadata?
- How should archived agents relate to retained session logs and CAS blobs?

## Success Criteria

- P61 is only implemented after P60 is stable enough to route commands and
  queries through the generic interface API.
- A client can discover available public agent types.
- A client can create, read, list, and archive logical agents.
- A client can target an agent by `agentId` without knowing the backing
  session id.
- Agent links are durable enough to support a real product workflow without
  introducing a premature policy engine.
