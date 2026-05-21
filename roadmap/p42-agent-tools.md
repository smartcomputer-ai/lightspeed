# P42: Agent Tool Dispatcher and SDK Contracts

**Status** - Complete

Implemented so far:

- G1-G9 are implemented in `forge-agent`.
- `cargo test -p forge-agent` passes with deterministic unit coverage.

## Goal

Implement the tool execution SDK layer described by
`spec/04-new-agent-spec.md`, building on the P41 reducer/decider loop without
putting host-specific tools into `forge-agent`.

P41 can plan tool batches and emit `ToolInvoke` effect intents. P42 turns those
intents into an open, runner-friendly dispatch contract:

```text
core loop -> ToolInvoke intent -> dispatcher prepares request
runtime driver executes request(s) -> terminal receipt
receipt -> journal -> reducer -> next LLM turn
```

The key design constraint is that `forge-agent` must not bake Tokio task
semantics into the core tool contract. Local runners may use Tokio, but Temporal
runners must use Temporal activity futures, selectors, timers, and cancellation
scopes.

## Spec References

- Spec of record: `spec/04-new-agent-spec.md`
- Prior phase: `roadmap/p41-agent-loop.md`
- Conceptual references:
  - `/Users/lukas/dev/tmp/codex/codex-rs/core/src/tools/parallel.rs`
  - `/Users/lukas/dev/tmp/codex/codex-rs/core/src/tools/router.rs`
  - `/Users/lukas/dev/tmp/codex/codex-rs/core/src/unified_exec/`
  - `refs/aos-agent/src/helpers/workflow/tool_batch.rs`

## Design Position

Tool execution is split into three layers.

**Core loop**

- Plans tool batches from LLM receipts.
- Tracks accepted/unavailable/queued/pending/succeeded/failed/cancelled calls.
- Emits `AgentEffectKind::ToolInvoke`.
- Reduces terminal `ToolInvocationReceipt` events.
- Does not execute tools.

**Tool dispatcher**

- Resolves a `ToolInvocationRequest` to a registered handler.
- Validates request shape and JSON arguments.
- Loads argument refs when needed.
- Checks capabilities and handler binding.
- Provides artifact/runtime context to the handler.
- Converts handler success/error into `ToolInvocationReceipt`.
- Does not own Tokio tasks, joins, channels, sleeps, or Temporal workflow APIs.

**Runtime driver**

- Owns how a group is executed.
- Local driver may use Tokio primitives.
- Temporal driver uses workflow/activity primitives.
- Test driver is deterministic and controlled.
- Emits terminal receipts as calls settle.

## Codex Behavior To Preserve Conceptually

Codex starts tool futures as completed model stream items arrive, allows
parallel-safe tools to run concurrently, and drains tool outputs before the next
follow-up LLM turn.

The "model can reason while a command is still running" behavior comes from
Codex's unified exec tool returning a terminal model-visible result after a
yield window with a process/session handle and an output snapshot. The
underlying process continues in a process manager, and the model can call a
follow-up polling/interaction tool later.

Forge should use the same concept:

- normal tool receipts mean the logical call is complete
- long-running tools can return a "still running" receipt with a durable handle
- follow-up poll/write/interrupt/close tools expose continued interaction
- the dispatcher does not inject partial tool output into an active LLM request
- live tool stream frames can be added later as an observability extension

## Scope

### In scope

- Public tool-handler SDK traits for implementers.
- Tool registry validation helpers and handler binding.
- Dispatcher for single `ToolInvoke` requests.
- Dispatch-batch/group preparation from P41 `ActiveToolBatch`.
- Runtime-driver abstraction that keeps concurrency outside the dispatcher.
- Local/testing async driver with deterministic fake handlers.
- Terminal receipt construction and model-visible tool errors.
- Stable model-visible result ordering for out-of-order runtime completions.
- Resumable/background tool receipt shape and tests.
- Temporal mapping notes/tests with mocked activity-style driver.

### Out of scope

- Real shell/filesystem/process tools in `forge-agent`.
- Full permission, approval, sandbox, or policy framework.
- MCP client implementation beyond generic request/handler contracts.
- Production Temporal workflow implementation.
- Production artifact store implementation.
- Tool progress frame sinks and live stdout/stderr observation.
- Letting the LLM consume arbitrary stream frames without a terminal receipt.

## Target Module Shape

Planned `crates/forge-agent/src/` additions:

- `tools/mod.rs`
  - public SDK surface and re-exports
- `tools/handler.rs`
  - `ToolHandler`, `ToolInvocationContext`, `ToolExecutionError`
- `tools/dispatcher.rs`
  - handler registry, request validation, dispatch preparation, receipt
    conversion
- `tools/driver.rs`
  - runtime-neutral dispatch request/group/outcome structs and driver trait
- `tools/artifacts.rs`
  - minimal artifact access trait used by handlers, if not already provided by
    a broader storage phase
- `testing/tools.rs`
  - deterministic fake handlers and fake driver helpers

Existing model modules remain data-only:

- `model/tooling.rs`
- `model/effects.rs`
- `model/batch.rs`

## Priority 0: SDK Boundary

### [x] G1. Tool handler contract

- Define the implementer-facing async handler trait.
- Handler input must be normalized `ToolInvocationRequest` plus context.
- Handler output must be one terminal `ToolInvocationReceipt` or a structured
  tool execution error.
- Include access to argument refs/artifact refs without exposing reducer state.

Acceptance:

- A custom handler can be written without importing loop internals.
- Handler API contains no `SessionState`, reducer, decider, journal, or planner
  dependency.
- Handler API does not require Tokio-specific types.

Implementation:

- Added `tools/handler.rs` with `ToolHandler`, `ToolInvocationContext`, and
  `ToolExecutionError`.
- Added `tools/artifacts.rs` with minimal artifact access for argument refs and
  output refs.

### [x] G2. Dispatcher registry and validation

- Add a dispatcher-side handler registry keyed by handler id and/or tool id.
- Validate duplicate bindings, missing handlers, unknown tools, and invalid
  executor binding.
- Validate JSON arguments against tool schema where practical.
- Check required capabilities against `ToolRuntimeContext`.
- Convert validation failures into model-visible tool receipts when they are
  tool-level failures.

Acceptance:

- Unavailable/misconfigured tools produce deterministic failed receipts.
- Runner/system failures remain distinguishable from model-visible tool
  failures.

Implementation:

- Added `ToolDispatcher` and `ToolDispatcherBuilder`.
- Validates unknown tools, missing handlers, invalid JSON, required schema
  fields, and required tool capabilities.
- Added `ToolSpec::required_capabilities`.
- Model-visible tool failures become failed `ToolInvocationReceipt` records;
  system handler failures remain dispatcher errors.

### [x] G3. Artifact and output shaping contract

- Provide a minimal artifact access trait for handlers and dispatcher output
  shaping.
- Preserve full output refs and model-visible truncated output refs.
- Keep large arguments/output/raw metadata by ref.
- Define deterministic fallback refs for synthetic tool errors in tests.

Acceptance:

- A handler can return full output and model-visible output separately.
- Dispatcher can construct receipts without embedding large output bodies.

Implementation:

- Added `ToolArtifactStore`, `ToolArtifactWrite`, and
  `InMemoryToolArtifactStore`.
- Dispatcher loads argument refs before validation.
- Synthetic tool errors use deterministic `forge://tool-error/{call_id}` refs.

## Priority 1: Dispatch Semantics

### [x] G4. Dispatch request and group preparation

- Convert active batch planned calls into runtime-neutral dispatch requests.
- Preserve planned-call order and execution group membership.
- Include effect ids, run/turn/tool joins, handler binding, arguments, and
  metadata.
- Keep preparation pure/deterministic.

Acceptance:

- Prepared groups match `ToolExecutionPlan`.
- Non-parallel/resource-conflicting calls are separated before reaching the
  runtime driver.

Implementation:

- Added `DispatchRunRequest::from_active_batch`, `DispatchGroup`, and
  `DispatchCall`.
- Added `PreparedToolDispatch::from_intent` for effect-intent joins.

### [x] G5. Runtime driver abstraction

- Define driver-owned group execution.
- Driver receives a dispatch group and returns observed dispatch events or
  terminal outcomes.
- Driver, not dispatcher, owns concurrency, waiting, cancellation, and timers.
- Avoid public Tokio primitives in the shared trait.

Acceptance:

- Local/test driver can run multiple parallel-safe calls concurrently.
- A mocked Temporal-style driver can schedule activities without changing
  dispatcher code.

Implementation:

- Added runtime-neutral `ToolDispatchDriver`.
- Added `InProcessToolDispatchDriver`, which owns async group execution using
  runtime-neutral futures rather than Tokio task APIs.
- Temporal activity-style driver coverage remains in G9.

### [x] G6. Out-of-order completion with stable model order

- Let runtime drivers return receipts in completion order.
- Reducer can apply receipts as they arrive.
- Planner presents completed tool results to the LLM in explicit stable order:
  planned-call order unless a future policy says otherwise.

Acceptance:

- A test where call B finishes before call A still produces deterministic
  model-visible tool-result ordering.

Implementation:

- Added `DispatchOutcome::stable_model_order`.
- Added deterministic fake driver coverage where completion order differs from
  planned/model order.

## Priority 2: Long-Running and Temporal-Friendly Tools

### [x] G7. Resumable/background tool contract

- Define receipt metadata for "still running" logical completions:
  handle/process/job id, output snapshot ref, continuation tool ids, and status.
- Add fake background handler that returns a handle after a yield window.
- Add fake poll handler that returns later output/completion for that handle.

Acceptance:

- The model can receive a terminal tool result indicating work is still running.
- A later tool call can poll or interact with the same background handle.
- The dispatcher does not keep the LLM turn open for arbitrary background work.

Implementation:

- Added `ToolResultMetadata`, `ToolResultStatus`, `ToolRuntimeHandle`, and
  `ToolRuntimeSnapshot`.
- Added deterministic `BackgroundStartHandler` and `BackgroundPollHandler`
  test helpers.

### [x] G8. Cancellation and interruption mapping

- Propagate run interruption into driver cancellation.
- Drivers explicitly emit cancelled/abandoned receipts or lifecycle events.
- Background handles expose interrupt/close semantics through tools or driver
  cleanup hooks.

Acceptance:

- Interrupting a batch does not silently drop pending tool effects.
- Background work has explicit cancelled/abandoned state.

Implementation:

- Added `DispatchCancellation` and `DispatchCancellationMode`.
- Added `ToolDispatchDriver::cancel_group`.
- Added deterministic cancelled/abandoned terminal receipt construction for
  pending dispatch calls.
- Added background interrupt test helper.

### [x] G9. Temporal mapping tests and notes

- Add mocked activity-style driver tests.
- Show that the dispatcher can be used without Tokio spawning.
- Document where Temporal activities and terminal receipts map.

Acceptance:

- No core dispatcher API forces Tokio task primitives.
- Temporal runner can implement group execution using Temporal-native async
  machinery.

Implementation:

- Added `ActivityStyleDriver` test helper that records activity-like scheduling
  and cancellation without exposing spawn/join APIs.
- Added test coverage for activity-style execution and cancellation against the
  shared `ToolDispatchDriver` trait.

Temporal mapping:

- `DispatchGroup` maps to the set of tool activity requests a workflow chooses
  to schedule together.
- `ToolDispatchDriver::execute_group` maps to scheduling activities and waiting
  with workflow-native futures/selectors.
- `ToolDispatchDriver::cancel_group` maps to workflow cancellation scopes and
  explicit terminal cancelled/abandoned receipts for unsettled calls.
- `DispatchCompletion` maps to the terminal activity result that should be
  journaled as an effect receipt.

## Testing

Default tests must remain deterministic and provider-free.

Required tests:

- [x] custom handler success receipt
- [x] unknown handler/tool -> model-visible failed receipt
- [x] invalid executor binding -> model-visible failed receipt
- [x] JSON argument validation failure
- [x] capability failure
- [x] full output ref plus model-visible output ref
- [x] parallel group with two calls completing out of order
- [x] serial grouping for non-parallel/resource-conflicting calls
- [x] resumable/background tool returns handle and output snapshot
- [x] poll/interaction tool consumes background handle
- [x] cancellation/interruption settles or abandons pending calls
- [x] mocked Temporal driver schedules activity-like calls without Tokio-specific
  API leakage

## Acceptance

- `cargo test -p forge-agent` passes.
- `forge-agent` exposes an open tool-handler SDK.
- Tool implementations can be provided by runners or external crates.
- The shared dispatcher is runtime-neutral and Temporal-compatible.
- Local/test runtime can dispatch parallel tool groups asynchronously.
- Terminal receipts remain the only authoritative reducer transition.
- Long-running tools use resumable/background receipts plus explicit follow-up
  tools, not implicit partial LLM context injection.
- Host shell/filesystem/process tools remain outside `forge-agent`.

## Suggested Order

1. G1 handler contract
2. G2 dispatcher registry and validation
3. G4 dispatch request/group preparation
4. G5 local/test driver
5. G6 stable ordering
6. G7 resumable/background tools
7. G8 interruption
8. G9 Temporal mapping notes/tests
9. G3 artifact/output shaping can be done earlier if handler tests need it
