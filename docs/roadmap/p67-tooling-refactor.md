# P67: Tooling Refactor

**Status**
- Implemented on 2026-06-09.
- Breaking changes were accepted.
- Companion to P67 direct remote MCP and P68 remote MCP registry/linking.

## Goal

Forge now models tools around one concept:

```text
active session tools = the set of tools included in the next model request
```

The engine no longer has alternate visibility groups or profile-local
configuration. A session has one active model-facing tool map. External systems
can replace that map or patch it by tool name.

This keeps direct MCP, hosted web search, `web_fetch`, host filesystem/process
tools, and future hosted capabilities on the same surface:

- a tool producer materializes deterministic `ToolSpec`s;
- the engine records those specs as event-sourced session state;
- LLM planning snapshots the active set into each planned provider request;
- runtime adapters lower the snapshotted specs to provider-native request fields
  or client-effect tool execution.

Context remains separate. Context entries have immutable ids, ordering,
consumption state, compaction, and provider-native continuity concerns. Tools
are keyed capability state, so their API uses map semantics.

## Engine State

`ToolingState` stores active tools directly:

```rust
pub struct ToolingState {
    pub revision: u64,
    pub tools: BTreeMap<ToolName, ToolSpec>,
    pub routing: ToolRoutingState,
}
```

`ToolingState.revision` is a monotonic active-tool revision, analogous to
`ContextState.revision` and `config_revision`. API callers use it for optimistic
concurrency when patching tools independently.

## Tool Choice

`ToolChoice` lives in model/run/turn configuration.

`tool_choice` means "how the model may use the tools that are present in this
request". It is not a tool and not a visibility rule.

```rust
pub struct TurnConfig {
    pub max_output_tokens: Option<u32>,
    pub tool_choice: Option<ToolChoice>,
    pub provider_request_defaults: ProviderRequestDefaults,
}

pub struct RunConfig {
    pub max_turns: Option<u32>,
    pub max_tool_rounds: Option<u32>,
    pub model_override: Option<ModelSelection>,
    pub max_output_tokens: Option<u32>,
    pub provider_request_defaults: Option<ProviderRequestDefaults>,
    pub tool_choice: Option<ToolChoice>,
}
```

Effective resolution:

```text
effective tool_choice = run override, else session turn config, else provider default
```

Validation for `ToolChoiceMode::Specific { tool_name }` happens against the
active tool set when a config update is admitted and again when an LLM request is
planned. Planning still rejects if a previously valid specific tool was removed
before the next turn.

## Core Commands

Tool membership is updated with two commands:

```rust
CoreAgentCommand::ReplaceTools {
    expected_revision: Option<u64>,
    tools: BTreeMap<ToolName, ToolSpec>,
}

CoreAgentCommand::PatchTools {
    expected_revision: Option<u64>,
    patch: ToolPatch,
}
```

Routing commands remain separate:

```rust
CoreAgentCommand::SetDefaultToolTarget { target }
CoreAgentCommand::ClearDefaultToolTarget { namespace }
```

Patch shape:

```rust
pub struct ToolPatch {
    pub upsert: Vec<ToolSpec>,
    pub remove: Vec<ToolName>,
}
```

Patch semantics:

- `upsert` inserts or replaces by `ToolSpec.name`;
- `remove` removes by `ToolName`;
- a name cannot appear in both `upsert` and `remove`;
- duplicate names inside either list are rejected;
- removing a missing tool is a hard engine rejection;
- empty patches are no-ops at admission.

This gives MCP the operations it needs:

- link one server: one `PatchTools.upsert`;
- unlink one server: one `PatchTools.remove`;
- refresh a batch of MCP links: one patch with many upserts/removes;
- reset all tools: `ReplaceTools`.

## Core Events

Tool membership is persisted with revisioned events:

```rust
ToolConfigEvent::ToolsReplaced {
    base_revision: u64,
    tools: BTreeMap<ToolName, ToolSpec>,
}

ToolConfigEvent::ToolsPatched {
    base_revision: u64,
    patch: ToolPatch,
}
```

Applying either event validates `base_revision == state.tooling.revision`,
applies the change, validates the resulting tool map, and increments
`state.tooling.revision`.

Target routing events remain unrevisioned:

```rust
ToolConfigEvent::DefaultTargetSet { target }
ToolConfigEvent::DefaultTargetCleared { namespace }
```

## Tool Validation

The engine validates the resulting active map:

- map key matches `ToolSpec.name`;
- each `ToolSpec` validates;
- remote MCP server labels are unique across active `ToolKind::RemoteMcp`
  entries;
- `ToolChoiceMode::Specific` in effective config references an active tool;
- provider-native tool API kind compatibility is checked during request planning;
- remote MCP provider support is checked during request planning.

Remote MCP uniqueness is active-map-wide because the active map is exactly what
the provider sees.

## LLM Planning

The LLM planner reads the active tool set directly:

```rust
fn active_tools(
    state: &CoreAgentState,
    api_kind: &ProviderApiKind,
) -> Result<Vec<ToolSpec>, PlanningError>
```

Planning now:

1. builds the planned context snapshot;
2. reads and validates the active tool set for the selected provider API kind;
3. resolves effective `tool_choice` from config;
4. validates `tool_choice` against the active tool set;
5. stores `tools` and `tool_choice` in the immutable `LlmRequest`.

Tool updates affect future planned turns. They do not mutate an already planned
`LlmRequest`.

## Tool Call Acceptance

Tool call acceptance checks the planned request snapshot, not live tooling state.

Reason: a model can only call tools that were sent in that request. If the live
tool map changes while a run is active, already-returned tool calls are judged
against the request that produced them, not the latest session state.

The helper is:

```rust
fn planned_tool_for_call(
    active_run: &ActiveRun,
    turn_id: TurnId,
    tool_name: &ToolName,
) -> Option<ToolSpec>
```

`initial_tool_call_status` uses that planned spec to decide whether a call is
accepted or unavailable. `decide_active_tool_batch_invocations` and
`start_tool_call` also use the planned spec for `invokes_client_effect` and
`target_requirement`.

Default execution targets remain live routing state. Routing is execution
placement, not model-visible tool membership.

## Session Config And Gateway Defaults

Session config still controls desired built-in tool defaults, but it is not the
arbitrary active-tools API.

Current public fields:

```json
{
  "tools": {
    "webSearch": true,
    "webFetch": true,
    "host": "edit"
  }
}
```

Internally this is standard tool defaults:

```rust
pub struct ToolDefaultsConfig {
    pub web_search: Option<bool>,
    pub web_fetch: Option<bool>,
    pub host: Option<HostToolMode>,
}
```

Gateway behavior on session start/update:

1. Resolve built-in defaults into the standard hosted/host tool specs.
2. Compare with the previously materialized standard tool names.
3. Submit one `PatchTools` with upserts/removes for the standard tool namespace.
4. Preserve non-standard tools, including MCP links.
5. Set or clear default execution targets for host tools as today.

The tools crate returns a flat `ResolvedToolset` with:

- `BTreeMap<ToolName, ToolSpec>`;
- runtime catalog entries;
- generated documents.

## Public API

Generic session tool updates are public:

```text
session/tools/update
```

Patch request:

```json
{
  "sessionId": "session_1",
  "expectedToolsRevision": 4,
  "update": {
    "type": "patch",
    "upsert": [
      {
        "toolId": "mcp_crm",
        "kind": {
          "type": "remoteMcp",
          "serverLabel": "crm",
          "serverUrl": "https://crm.example.com/mcp",
          "allowedTools": ["lookup_customer"],
          "approval": "never",
          "deferLoading": false,
          "authRef": {
            "namespace": "auth_grant",
            "id": "authgrant_123"
          }
        }
      }
    ],
    "remove": ["mcp_old"]
  }
}
```

Replace request:

```json
{
  "sessionId": "session_1",
  "expectedToolsRevision": 4,
  "update": {
    "type": "replace",
    "tools": []
  }
}
```

The API DTOs are:

```rust
pub struct SessionToolsUpdateParams {
    pub session_id: SessionId,
    pub expected_tools_revision: Option<u64>,
    pub update: SessionToolsUpdateInput,
}

pub enum SessionToolsUpdateInput {
    Replace { tools: Vec<ToolView> },
    Patch { upsert: Vec<ToolView>, remove: Vec<String> },
}
```

`session/mcp/link`, `session/mcp/unlink`, and `session/mcp/list` remain
MCP-specific convenience APIs. Link/unlink materialize catalog snapshots and
submit `PatchTools`; list reads active `ToolKind::RemoteMcp` entries.

`SessionView.activeTools` exposes the active tools:

```rust
pub struct ActiveToolsView {
    pub revision: u64,
    pub tools: Vec<ToolView>,
}
```

Tool events project only revision and summary information:

```rust
SessionEventKindView::ToolsReplaced {
    base_revision: u64,
    revision: u64,
}

SessionEventKindView::ToolsPatched {
    base_revision: u64,
    revision: u64,
    upserted: Vec<String>,
    removed: Vec<String>,
}
```

`session/read` provides the current active tool view.

## MCP Impact

P68 session linking now resolves an MCP catalog record, validates any auth grant
handle, materializes a deterministic `ToolSpec::RemoteMcp`, and submits:

```text
PatchTools { upsert: [tool], remove: [] }
```

Unlink submits:

```text
PatchTools { upsert: [], remove: [tool_name] }
```

Batch updates use the same patch command. No profile selection is involved.

P67 direct MCP documents that `RemoteMcp` specs live in the active tool map.

## Completed Migration

- [x] Engine data model stores active tools directly on
  `ToolingState.tools: BTreeMap<ToolName, ToolSpec>`.
- [x] Legacy profile ids, profile validation, and profile selection state were
  removed from the engine.
- [x] `tool_choice` moved into config.
- [x] `ReplaceTools` and `PatchTools` replaced the registry/profile update
  surface.
- [x] Tool update events are revisioned.
- [x] LLM planning reads the active set and config-owned `tool_choice`.
- [x] Tool call acceptance checks the planned request snapshot.
- [x] `ResolvedToolset` is flat and no longer carries profile data.
- [x] Gateway built-in tool refresh uses patches and preserves MCP links.
- [x] MCP link/unlink use `PatchTools`.
- [x] Gateway waits on tool revision for tool updates.
- [x] API/projection/CLI expose active tool revision and tool views.
- [x] `session/tools/update` is public.
- [x] MCP convenience commands are backed by the generic patch path.
- [x] P67/P68 docs were updated to active tool map wording.

## Test Coverage

Engine:

- [x] tool patch upserts and removes by name;
- [x] patch rejects duplicate names and upsert/remove conflicts;
- [x] replace and patch bump `tooling.revision`;
- [x] expected tool revision conflicts reject admission;
- [x] remote MCP server labels are unique across active tools;
- [x] `ToolChoiceMode::Specific` rejects missing tools;
- [x] LLM request includes active tools and config-owned `tool_choice`;
- [x] tool calls are accepted against planned request tools, not live tools.

Gateway/API:

- [x] session start materializes standard tools with one flat active tool map;
- [x] session update patches standard tools without removing MCP links;
- [x] MCP link upserts a remote MCP tool;
- [x] MCP unlink removes only that remote MCP tool;
- [x] MCP batch link/unlink can be represented by one patch;
- [x] projected session view includes active tool revision;
- [x] `session/tools/update` accepts replace and patch inputs.

Runtime:

- [x] client-effect tool invocation still dispatches by `tool_name`;
- [x] remote MCP specs do not create Forge tool batches;
- [x] provider-native and remote MCP compatibility checks fail clearly for
  unsupported provider API kinds.

## Decisions

- `session/tools/update` is public and backs replace/patch of the active tool
  map. MCP link/unlink remain convenience APIs over the same core patch path.
- Removing a missing tool is a hard engine rejection. Convenience APIs can
  preflight if they want idempotent behavior.
- Routing targets do not have a separate revision.
- `tool_choice` lives in config and supports session defaults plus run/turn
  effective config.
- The public active tool view is named `ActiveToolsView` to avoid reintroducing a
  second tool-set noun.
