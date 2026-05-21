# P54: Composable Agent Kernel

**Status**
- Complete for the current Rust kernel/CoreAgent slice.
- Remaining extension proofs are deferred.

**Progress**
- Design direction recorded.
- First internal session-layer slice implemented in `agent-core::session`:
  generic session ids/log records, dynamic command/event envelopes, codec
  traits, `AgentDomain`, replay helpers, and append/apply workflow helpers
  over `AgentDomain`.
- CoreAgent has moved under `agent-core::core_agent`, with public names
  `CoreAgentCommand`, `CoreAgentEvent`, `CoreAgentEventKind`,
  `CoreAgentState`, `CoreAgentStatus`, `CoreAgentEntry`, and
  `CoreAgentJoins`.
- The canonical `SessionStore` now stores `DynamicEvent` entries. CoreAgent
  uses `CoreAgentCodec` to encode before append and decode during replay/API
  projection.
- `CoreAgentDomain` implements `AgentDomain` for the built-in agent
  composition.
- CoreAgent component vocabulary and domain-local logic now live under
  `agent-core::core_agent::components`:
  `command`, `config`, `context`, `error`, `event`, `ids`, `lifecycle`,
  `llm`, `log`, `run`, `state`, `tooling`, and `turn`. Root `core_agent`
  modules own composition and orchestration: admission, apply dispatch,
  planning composition, codec, domain, workflow, and I/O traits.
- `CoreAgentState` now mirrors component ownership instead of keeping a flat
  bag of reducer fields: lifecycle-owned status/config lives in
  `LifecycleState`, run queue/active/completed state lives in `RunQueueState`,
  and the active run instance is named `ActiveRun`.
- `BlobRef` has moved out of CoreAgent to the generic top-level
  `agent-core::blob` module because blob identity is shared storage/session
  infrastructure, not built-in-agent state.
- Dynamic CoreAgent serde fixtures pin representative lifecycle and tool event
  envelopes.
- `agent-runtime` now has an explicit projection boundary for deriving API
  session/run/item views from committed CoreAgent events instead of treating
  reducer state as the canonical transcript.
- P54 now stops at the Rust session kernel and CoreAgent modularization layer.
  Rust custom composition proofs, application projector hooks, Python package
  work, Skills/MCP modules, and `agent-kernel` crate extraction are deferred.

## Goal

Refactor Forge from one closed core agent into a composable
session/event kernel plus reusable agent modules.

`CoreAgent` should become Forge's built-in LLM/tool agent composition, not the
only shape the SDK can express. Users should be able to build their own final
agent composition in Rust or Python while reusing Forge's session log, reducer
helpers, context planning, LLM request planning, tool routing, host tooling,
skills, MCP integration, and API projection utilities.

This is a breaking refactor. Prefer a clean SDK shape over compatibility with
the current `SessionCommand`, `SessionEventKind`, `SessionState`, and
`CorePlanner` model.

## Design Position

The fundamental abstraction is a session event log.

A Forge session log is a durable transcript and state-transition history for an
agent session. It records semantic agent facts such as admitted input, planned
context, model requests, assistant output, observed tool calls, tool results,
configuration changes, skill changes, MCP server availability, and run
completion.

The log should not be tied to one global enum that must grow forever as the SDK
adds skills, MCP servers, memory, browser state, human approval, planner state,
or domain-specific agent features.

Use a layered architecture:

```text
session           generic session log, envelopes, storage, replay, testing
core-agent        Forge's built-in context/LLM/tool agent modules
agent-modules     optional memory, MCP, skills, browser, approval, eval modules
agent-python      Python bridge for dynamic/user-owned compositions
user-agent        app-owned final command/event/state/workflow composition
```

The first implementation keeps `session` inside `agent-core` as
`agent-core::session`. Extracting it into a separate `agent-kernel` crate should
wait until the boundary is stable or another crate/Python package needs to
depend on the session layer without pulling in CoreAgent.

The final application should own the final command, event, state, and workflow
composition. In Rust, that means the application owns the final closed enums.
In Python, that means the application owns the final Python classes/schemas that
serialize into versioned dynamic envelopes.

## Kernel Contract

The kernel should be generic over an agent domain. The domain is still
session-scoped in practice, but `AgentDomain` better communicates that this is
the composition boundary for building an agent:

```rust
pub trait AgentDomain {
    type Command;
    type Event;
    type Joins;
    type State;
    type Error;

    fn initial_state(&self) -> Self::State;

    fn admit(
        &self,
        state: &Self::State,
        command: Self::Command,
    ) -> Result<Vec<EventProposal<Self::Event, Self::Joins>>, Self::Error>;

    fn apply(
        &self,
        state: &mut Self::State,
        entry: &SessionEntry<Self::Event, Self::Joins>,
    ) -> Result<(), Self::Error>;
}
```

The kernel owns reusable mechanics:

- session ids, event sequence numbers, positions, timestamps, joins, and
  correlation metadata
- append/read session-store contracts
- replay from events into state
- append/apply helpers
- blob store contracts
- deterministic test harnesses
- command rejection and domain error shapes
- optional snapshot hooks once replay cost requires them

The kernel must not know about:

- LLM provider APIs
- host filesystems or process execution
- MCP protocol details
- skill registries
- memory models
- browser/session state
- Temporal SDK types
- Python runtime types

Those are core agent modules, optional modules, runtime dependencies, or
language bridges.

## CoreAgent As Modules

The current Forge agent should be rebuilt as `CoreAgent`, a reusable built-in
agent composition made from smaller modules.

Current CoreAgent components:

```text
lifecycle
run
turn
context
llm
tooling
```

Potential future modules:

```text
tool_routing
skills
mcp
```

Do not add transcript reducer state by default. Transcript/session item views
should be projections from the committed event log. Context items remain
model-visible context facts; they may be used by API projection, but they are
not the universal transcript model.

Each module should expose plain data models and business logic functions:

```rust
pub mod context {
    pub struct State { ... }
    pub enum Event { ... }

    pub fn apply(state: &mut State, event: &Event) -> Result<(), DomainError>;
    pub fn plan_window(input: PlanWindowInput<'_>) -> Result<PlanWindowOutput, DomainError>;
}

pub mod tooling {
    pub struct State { ... }
    pub enum Command { ... }
    pub enum Event { ... }

    pub fn admit(state: &State, command: Command) -> Result<Vec<Event>, DomainError>;
    pub fn apply(state: &mut State, event: &Event) -> Result<(), DomainError>;
    pub fn plan_batch(input: PlanToolBatchInput<'_>) -> Result<PlanToolBatchOutput, DomainError>;
}
```

Avoid requiring every module to implement a large trait. Traits are useful at
the composition boundary, but module internals should stay boring and explicit.

The CoreAgent can then be composed from module events:

```rust
pub enum CoreAgentCommand {
    Lifecycle(lifecycle::Command),
    Run(run::Command),
    Tooling(tooling::Command),
}

pub enum CoreAgentEvent {
    Lifecycle(lifecycle::Event),
    Context(context::Event),
    Run(run::Event),
    Turn(turn::Event),
    Tooling(tooling::Event),
}

pub struct CoreAgentState {
    pub lifecycle: lifecycle::State,
    pub runs: run::State,
    pub context: context::State,
    pub tooling: tooling::State,
}
```

This makes `CoreAgentEvent` a closed enum, but it is only closed for
`CoreAgent`. It is not the SDK's universal event vocabulary. Future built-in
modules such as Skills or MCP can add CoreAgent variants when their contracts
are clear; applications do not need to wait for that because they can own their
own final enums.

## Rust User Composition

Rust applications should own their final closed enums:

```rust
pub enum MyCommand {
    CoreAgent(CoreAgentCommand),
    Memory(memory::Command),
    Browser(browser::Command),
    Custom(custom::Command),
}

pub enum MyEvent {
    CoreAgent(CoreAgentEvent),
    Memory(memory::Event),
    Browser(browser::Event),
    Custom(custom::Event),
}

pub struct MyState {
    pub core_agent: CoreAgentState,
    pub memory: memory::State,
    pub browser: browser::State,
    pub custom: custom::State,
}
```

Forge should provide helper composition patterns, but it should not hide the
final enum ownership. That is the Rust-friendly extensibility model:
application-owned closed enums with reusable module variants.

The kernel does not impose a port model on user agents. `CoreAgent` has its own
two I/O traits for LLM generation and tool invocation. Custom agents can define
their own traits, pass concrete services directly, call Temporal activities
directly, or use any application-specific dependency shape.

The user domain decides:

- which core agent modules to include
- which commands are admitted
- which events are replayed
- which module state fragments exist
- how workflow logic interleaves CoreAgent I/O and custom dependencies
- how custom events project into the client API

## Python Composition

Deferred beyond the current P54 implementation. The design direction remains:
plan for Python as a first-class composition language, but do not block the
Rust kernel cleanup on Python packaging or callbacks.

Python users should be able to define final agent commands, events, state, and
workflow logic in Python while reusing Rust Forge modules where useful.

The Python bridge should not require Python to implement Rust generic traits or
Rust enum variants. The FFI boundary should use versioned dynamic envelopes:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DynamicCommand {
    pub kind: String,
    pub version: u32,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DynamicEvent {
    pub kind: String,
    pub version: u32,
    pub payload: serde_json::Value,
}
```

For large provider/tool payloads, the dynamic payload should carry `BlobRef`
values instead of large inline JSON.

Event kinds must be namespaced and versioned:

```text
forge.run.started@1
forge.context.items_recorded@1
forge.tool.call_completed@1
my.memory.fact_recorded@1
my.browser.page_loaded@1
```

Python-side composition can use dataclasses, Pydantic models, or generated
classes, but the stored event log should remain language-neutral JSON/blob
envelopes.

Example Python shape:

```python
class MyAgent:
    def initial_state(self) -> MyState:
        ...

    def admit(self, state: MyState, command: CommandEnvelope) -> list[EventEnvelope]:
        ...

    def apply(self, state: MyState, event: EventEnvelope) -> None:
        ...

    async def drive(self, session: Session, deps: MyDeps) -> DriveOutcome:
        ...
```

The Python final composition owns the final schema. Rust Forge supplies:

- the session store and blob store bindings
- dynamic event envelopes
- replay helpers
- core-agent module bindings
- LLM request planning helpers
- tool routing helpers
- host tool adapters
- test harness utilities

## Bridge Modes

Deferred beyond the current P54 implementation.

Support two bridge modes, in this order.

### Python-Led Mode

Preferred first Python target.

Python owns the workflow and final composition. Rust exposes Forge kernel and
core agent modules through a Python package, likely implemented with PyO3.

```text
Python workflow/application
  -> calls Rust replay/append helpers
  -> calls Rust core agent planning helpers
  -> awaits Python or Rust-backed services/activities
  -> appends dynamic events
```

This is the natural fit for Python Temporal users: use the Temporal Python SDK
for workflow code and activities, and call only deterministic Rust helpers from
workflow logic.

Rules:

- reducers and planning helpers called from Temporal workflow code must be pure
  and deterministic
- LLM/provider calls, host tools, MCP I/O, filesystem I/O, and network I/O must
  happen in activities or local runtime dependencies
- Python workflow code should store language-neutral dynamic events, not Python
  object pickles

### Rust-Led Python Callback Mode

Later target.

Rust owns the runtime loop and calls Python callbacks for `admit`, `apply`, or
custom workflow hooks.

This mode is useful for embedding Python extensions into a Rust host, but it is
harder because it must handle:

- Python interpreter lifecycle
- GIL behavior
- async runtime interop
- error propagation with useful traces
- deterministic replay constraints
- deployment packaging

Do not block the kernel refactor on Rust-led callback execution.

## Dynamic Store Shape

The canonical kernel session store should be encoded around `DynamicEvent`.
Typed Rust domains should use codecs and typed wrappers on top of that dynamic
store.

The session log is a durable language boundary, not only an in-process Rust
collection. Once Python composition, migrations, optional modules, and
long-lived sessions matter, the stored event representation should be
language-neutral, namespaced, and versioned.

The codec shape:

```rust
pub trait EventCodec {
    type Event;

    fn encode(&self, event: &Self::Event) -> Result<DynamicEvent, CodecError>;
    fn decode(&self, event: &DynamicEvent) -> Result<Self::Event, CodecError>;
}
```

The dynamic Python path uses `DynamicEvent` directly. The CoreAgent Rust agent
uses a codec from `CoreAgentEvent` to `DynamicEvent`. Rust applications with
custom final enums use their own codecs:

```rust
pub struct TypedSessionStore<S, C> {
    store: S,
    codec: C,
}
```

This preserves typed Rust models inside reducers, planners, and workflows while
making persistence stable across language boundaries and Rust refactors.

Do not make the entire SDK use untyped JSON internally. Static Rust modules
should keep typed models. The dynamic envelope is the language boundary and
storage compatibility layer.

## Skills And MCP

Skills and MCP should not keep expanding the central core.

They should be modules that contribute:

- context candidates
- tool specs
- tool bindings
- provider/tool configuration
- runtime bindings or dependencies
- optional durable events for activation, discovery, configuration, or status

MCP is primarily a tool-provider module. It should expose tools and resources
to the core-agent tooling/context modules. It should record session events only
when MCP facts are durable and replay-relevant, such as server configuration,
selected server profile, tool availability snapshot, or resource inclusion.

Skills are primarily context/tool/profile modules. They should record events
when skill activation, version selection, configuration, or durable state
changes matter to replay or audit.

## API Projection

`agent-api` should not require every custom event to become a first-class API
variant.

Projection should support:

- core agent session/run/item views for CoreAgent events
- extension item views for unknown or custom dynamic events
- application-provided projection hooks for custom modules
- event read APIs that can return dynamic event envelopes for clients that know
  the custom schema

The stable UI contract remains session/run/item. Custom events can be surfaced
as richer views when an application provides a projector.

## Refactor Plan

### [x] G1: Carve Out Session Types

- Add an internal `agent-core::session` layer before extracting a separate
  crate.
- Move generic session ids, positions, typed log records, dynamic envelopes,
  codec traits, `AgentDomain`, and replay helpers into the session layer.
- Keep the session layer independent of CoreAgent command/event/state types.
- Move the session store contract to `DynamicEvent`.
- Add generic append/apply helpers over `AgentDomain`.
- Deferred beyond P54: deciding whether/when to extract
  `agent-core::session` into an `agent-kernel` crate.

### [x] G2: Split CoreAgent Modules

- Move the current built-in agent into `agent-core::core_agent`.
- Keep current CoreAgent behavior but express it as `CoreAgentCommand`,
  `CoreAgentEvent`, and `CoreAgentState`.
- Remove assumptions that CoreAgent events are the universal event vocabulary.
- Split event/state ownership and reducer functions into lifecycle, context,
  run, turn, and tooling modules.
- Rename the misleading `core_agent/model` namespace to
  `core_agent/components`, because component files intentionally own both data
  vocabulary and domain-local reducer/planning helpers.
- Move validation helpers out of the central `validation.rs` file and into the
  owning components.
- Extract planning helpers from central `planning.rs` into module-owned
  planning functions.
- Reshape `CoreAgentState` around component state objects:
  `lifecycle`, `runs`, `context`, and `tooling`.
- Keep transcript/session item views as projections from the event log, not
  reducer state.
- Deferred beyond P54: split oversized components such as `tooling` only when
  their internal shape warrants it, and add Skills/MCP modules when those
  concepts have enough shape.

### [x] G3: Compose CoreAgent

- Implement `AgentDomain` for the CoreAgent.
- Rebuild the P53 async workflow driver using core agent modules.
- Keep CoreAgent tests equivalent to the current full agent loop tests.

### [x] G4: Add Dynamic Envelopes And Codecs

- Add `DynamicCommand` and `DynamicEvent`.
- Add event kind naming/versioning rules.
- Add codecs for CoreAgent events.
- Add serde fixtures for representative dynamic CoreAgent events.

### [deferred] G5: Add Python Package Skeleton

- Add an `agent-python` or `forge-py` package using PyO3 or a similar binding
  layer.
- Expose dynamic envelopes, session ids, positions, session store access,
  blob-store access, replay helpers, and selected core agent planning helpers.
- Keep the first Python package local/in-process.

### [deferred] G6: Python-Led Composition Tests

- Add a Python test agent with custom memory events.
- Replay dynamic events into Python state.
- Reuse a Rust core agent helper from Python.
- Drive a scripted LLM/tool loop from Python.
- Assert that stored events are language-neutral dynamic envelopes.

### [deferred] G7: Rust-Led Python Callbacks

- Do not embed Python callbacks into the Rust runner until the Python-led mode
  works.
- Record requirements for async interop, packaging, error reporting, and
  deterministic replay before implementing callback mode.

## Non-Goals

- preserving the current closed `SessionEventKind` as the universal event enum
- forcing Python users to implement Rust generic traits
- storing Python pickles in the session log
- making all Rust modules internally untyped JSON
- embedding provider/tool side effects in reducers
- solving Rust-led Python callback execution in the first cut

## Current Done Criteria

- The session kernel can store, replay, and append events without depending on
  CoreAgent components.
- The CoreAgent is composed from reusable modules.
- CoreAgent events can be encoded into language-neutral dynamic envelopes.
- API projection has an explicit boundary from committed CoreAgent events to
  client-facing session/run/item views.
- Tests cover the CoreAgent Rust composition and session kernel helpers.

## Deferred Follow-Ups

- Add a Rust custom composition proof, such as `CoreAgent + Memory`, with
  app-owned command/event/state enums.
- Add application-provided projection hooks for custom events.
- Add Python package skeleton and Python-led composition tests.
- Add Skills/MCP modules when their contracts are clearer.
- Decide whether `agent-core::session` should become a separate
  `agent-kernel` crate.
