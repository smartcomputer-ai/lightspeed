# Fleet Idea: Future Agent Graph Concepts

**Status**

- Preliminary / deferred.
- Not part of the current product API plan.
- Keep this as a design scratchpad for future fleet pressure, not as an
  implementation target.

## 1. Context

Lightspeed's current direction is a single configurable agent, not a general agent
SDK or fleet runtime. The public client boundary should stay product-shaped and
typed:

```text
initialize
session/start
session/read
session/events/read
session/close
session/runs/start
run/cancel
```

For now, multiple "agents" should be represented as multiple sessions of the
same Lightspeed/CoreAgent/hosted workflow shape with different configuration: model,
instructions, tool profile, workspace, and host targets. We should add concrete
typed API methods when the product needs them, rather than introducing a generic
command/query/interface layer.

Fleet becomes relevant later if sessions stop being enough as the product
identity. The strongest triggers are:

* A real second public agent type, not just another CoreAgent configuration.
* One logical agent needing multiple durable sessions or conversations.
* Clients needing to list, label, group, or reconnect to long-lived agents
  independently of raw session ids.
* Durable agent-to-agent links with product semantics.
* Hosted operation needing lifecycle, ownership, or routing metadata above a
  session.

The rest of this document describes that future surface. It should be read as a
set of concepts to revisit, not as a reason to build fleet machinery now.

## 2. Future Fleet Context

Longer term, agents may be designed for long-running workflow management.
The core thesis is that traditional workflow engines are not really needed in
the same way, because the agents themselves can manage the workflows.

However, the workflows are nested. The system is basically a graph of agents that communicate with each other.

This graph can support sophisticated communication patterns:

* An agent can talk to many other agents.
* Agents can spawn other agents.
* A child agent, or even a further downstream child agent, may need to talk back to a top-level agent.
* Agents may start, stop, kill, restart, configure, and modify other agents.
* The graph is not fully defined ahead of time. It is defined and modified in real time as agents operate.

There is also an ownership or permission structure, because agents can create or instantiate other agents, and this raises questions about who is allowed to query, message, configure, or modify whom.

## 3. Agent Types and Agent Implementations

A future fleet may need to support different types of agents with different
capabilities. Do not build this catalog while Lightspeed has only one public agent
shape.

An agent type refers to the code and structure of a specific agent
implementation. It should stay concrete at first. The catalog should not become
a marketplace or plugin registry until a real second public type exists.

Examples include:

* Core agents implemented by us.
* Agents designed to run inside Temporal.
* Agents with different functionality or capabilities.
* Third-party agents, such as cloud code agents or similar external agents.

For this, fleet may eventually need a catalog of agent types.

This catalog should act as a type lookup: it should answer what types of agents are available, what their capabilities are, and possibly what configuration surfaces they expose.

## 4. Agent Definition / Manifest

There is a key concept around the agent definition or manifest. I am not yet sure which word is better: “definition” or “manifest.”

In the current product, this should mostly be ordinary session configuration:
model, instructions, tool profile, workspace, host targets, and other typed
fields. A separate manifest layer should wait until we have agent identities or
agent types that cannot be described as session configuration.

In a future fleet, this would become the current configuration of an agent
within the realm of what that agent type allows to be configured.

The agent implementation itself is partly fixed in code. Some parts are hard-coded or defined by the agent type. But other parts are configurable. The manifest or definition describes the current configuration of that specific agent instance.

This is one of the key challenges if Lightspeed becomes an SDK: we would need to
standardize the agent definition surface somewhat, while also allowing new
agent implementations to extend it.

The manifest may need to define things like:

* What prompts exist.
* Which prompts can be configured.
* Which prompts are fixed.
* What inputs are available.
* What tools are available to the agent.
* Which tools can be turned on or off.
* Which tools are fixed.
* Whether the agent has access to skills.
* Whether the agent supports MCP.
* Which MCP servers are configured.
* Which skills are configured.
* Whether the agent is allowed to query other agents.
* Whether the agent is allowed to edit other agents.
* Whether the agent is allowed to configure the graph of agents.
* Whether the agent is allowed to modify its own manifest or definition.

Some of this can come later, especially around skills and MCP support, but the basic idea is that the entire surface of the agent needs to be defined and queryable.

That surface should be queryable by:

* The agent itself.
* Other agents.
* An outside user through an API.
* A human operator.
* The system managing the agent graph.

If a lower-level fleet API is later introduced, it should be usable both by
humans and by agents operating on the agent graph itself. That is not a reason
to replace the current typed product API now.

## 5. API Surface vs. Manifest

There is an open question about whether the manifest and the API surface should be treated as separate concepts or as closely related concepts.

For the current single-agent product, keep them separate in practice: the API is
the typed `session`/`run` surface, and configuration is passed through typed
session/configuration fields. Do not make clients discover and invoke arbitrary
manifest-defined commands.

In a future fleet, the manifest or agent definition may describe the
configuration of the agent itself. It defines what the agent is and how it is
currently configured.

The API surface is what can be done with the agent. For example:

* Calling the agent.
* Sending messages to the agent.
* Querying the agent’s state.
* Enabling or disabling tools.
* Setting prompts.
* Querying the current definition.
* Modifying the current definition, if allowed.

It may be that the API surface is itself part of the definition of the agent. Some API calls invoke the agent, some configure it, some query its state, and some modify the graph around it.

This is not how the current Lightspeed API is designed. It is a possible future
shape, and it should only be introduced after the typed product API cannot carry
the real workflows anymore.

The goal should be a minimal API that still captures the full set of required operations.

## 6. Agent State and State Queries

In the current product, deterministic state queries are typed reads such as
`session/read` and `session/events/read`. A future fleet would need a broader
understanding of the state surface of an agent.

This includes:

* What state an agent has.
* What parts of that state can be queried.
* What queries are available.
* Whether the queries are deterministic.
* Who is allowed to query that state.

There should be a distinction between querying state in a deterministic manner and communicating with the agent by asking it a question.

For example, one type of query might be a structured state query:

> What is the current status of this agent?

Another type might be a text-based question sent to the agent:

> What are you currently working on?

The first is more like a deterministic API query. The second is more like a message sent through the agent messaging system, where the agent decides how to respond.

The latter probably belongs more properly to the messaging system and therefore to the API surface or manifest of the agent.

## 7. Messaging Surface

Future agents may need to communicate with each other. We should define what
kinds of messages an agent accepts and what kinds of responses can be expected
only once there is a concrete agent-to-agent workflow.

This is part of the API surface of the agent.

The messaging surface may define:

* What messages can be sent to the agent.
* What inputs are expected.
* What responses are possible.
* Whether text-based questions are accepted.
* Whether structured messages are accepted.
* Whether other agents can call specific operations.
* Whether downstream agents can communicate back upstream.

This may not be exactly the same as the core manifest or agent definition, because the core manifest is the configuration of the agent itself. But the messaging surface is still part of what defines how the agent can be interacted with.

## 8. Permissions and Capabilities

Permissions are a major part of a future fleet. They should not be pulled into
the single-agent API before there is a concrete hosted, multi-agent, or
agent-to-agent requirement.

Some permissions are internal to the agent itself. For example:

* Can the agent modify its own manifest?
* Can it query its own state?
* Can it enable or disable its own tools?
* Can it modify its own prompts?

Other permissions concern relationships between agents. For example:

* Can agent A message agent B?
* Can agent A query agent B’s state?
* Can agent A query agent B’s manifest?
* Can agent A modify agent B’s manifest?
* Can agent A create or instantiate agent B?
* Can agent A stop, kill, or restart agent B?
* Can agent A configure the graph around agent B?

This may be modeled as permissions between nodes in the agent graph.

An agent’s permission toward itself can be modeled the same way as permissions toward any other agent: as an edge or relationship in the graph.

There is also the question of ownership. If agent A spawns agent B, does agent A own agent B? Does that imply additional permissions? I am not sure yet whether we should introduce an explicit concept of ownership, but we need some way to express who is allowed to do what.

## 9. The Agent Graph

A future fleet system may become a graph. The current Lightspeed product is still a
set of durable sessions, not a graph of logical agents.

The graph consists of:

* Agents as nodes.
* Relationships between agents.
* Knowledge of which agents know about which other agents.
* Communication permissions between agents.
* Configuration permissions between agents.
* Ownership-like relationships, if we choose to model them.
* Runtime state about active, stopped, killed, restarted, or modified agents.

The graph is dynamic. It is not defined ahead of time. Agents can configure, start, stop, kill, restart, and modify other agents in real time.

The graph can include:

* Core agents.
* Different implementations of internal agents.
* Temporal-backed agents.
* Third-party agents.
* Agents with different capabilities and permission models.

We should only add graph queries when product workflows need graph identities or
relationships that cannot be represented by sessions and configuration.

## 10. Future Requirements Identified So Far

A future fleet may need the following concepts and surfaces. These are not
current implementation requirements.

### Agent Type Catalog

A registry or catalog of available agent types.

This should describe what kinds of agents exist and what each type supports.

### Agent Manifest / Definition

A representation of the current configuration of a specific agent instance.

This includes configurable prompts, tools, inputs, skills, MCP configuration, permissions, and other supported settings.

### Extensible Definition Surface

The manifest needs to be standardized enough for the SDK, but extensible enough that new agent implementations can add their own configuration surface.

### State Query Surface

A way to query agent state deterministically.

This should be separate from simply asking the agent a text-based question.

### Messaging Surface

A definition of what kinds of messages an agent accepts and what kinds of responses it can provide.

This includes agent-to-agent communication and potentially human-to-agent communication.

### Permission Model

A model for what agents are allowed to do to themselves and to other agents.

This includes querying, messaging, modifying, configuring, creating, stopping, killing, and restarting agents.

### Agent Graph Model

A model of the runtime graph of agents, including which agents know about which other agents and what operations are allowed between them.

### Minimal API Surface

The API should be as minimal as possible while still supporting the full system.

It should be designed for both human users and agents operating on the graph.

For the current product, the minimal API remains the typed
`initialize`/`session`/`run` API. Fleet should not introduce a generic
command/query surface until there is concrete pressure for it.

## 11. Open Questions

There are several open design questions:

### Definition vs. Manifest

What is the right word: definition or manifest?

The concept is the current configuration of an agent within what the agent type allows.

### Manifest vs. API Surface

Should the agent manifest and the agent API surface be separate concepts?

Or should the API surface be treated as part of the agent’s definition?

### State Query vs. Message

Where is the boundary between querying the state of an agent and sending a message to an agent?

A deterministic state query feels different from asking the agent a natural-language question, but both are ways of interacting with the agent.

### Ownership

Should we introduce an explicit concept of ownership?

If agent A spawns agent B, does that imply special rights? Or should all such rights be modeled purely as permissions between graph nodes?

### Self-Modification

How should an agent’s permission to modify itself be modeled?

One possibility is to model it exactly like any other permission edge: the agent has permissions toward itself.

### Extensibility

How do we standardize the agent definition enough for the SDK while allowing different agent implementations to extend it?

This is one of the main challenges.

## 12. Core Design Thesis

The future fleet hypothesis is that agents can be treated as long-running
entities that manage workflows themselves.

Instead of using a traditional workflow engine as the primary abstraction, the system should support a dynamic graph of agents, where each agent can communicate, coordinate, configure, and delegate work to other agents.

Temporal provides the long-running execution foundation, but the agent graph provides the workflow structure.

If Lightspeed becomes an SDK/fleet product, the main challenge will be defining the
minimal, extensible surfaces needed for this system:

* Agent types.
* Agent definitions or manifests.
* Agent state.
* Agent messaging.
* Agent permissions.
* Agent graph relationships.
* APIs for both humans and agents.

Do not build those surfaces ahead of evidence. The near-term system should
continue to expose typed session/run views and typed lifecycle methods, while
using session configuration to support different agent personalities and tool
profiles.
