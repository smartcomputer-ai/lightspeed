# P67: Tooling Refactor

**Status**
- Implemented.
- Breaking changes are allowed.
- Companion to P67 direct remote MCP and P68 remote MCP registry/linking.
- Motivated by the first MCP implementation, where session MCP link/unlink had
  to synthesize full `ToolRegistry` replacements and mutate a selected profile
  even though the product model only needs "the tools visible to the model".

## Goal

Simplify Forge's tool state and tool API around one concept:

```text
active session tools = the set of tools included in the next model request
```

The engine should not have tool profiles, selected profiles, or profile-local
visibility. A session has one active model-facing tool set. External systems
can replace it or patch it by tool name.

This keeps direct MCP, hosted web search, `web_fetch`, host filesystem/process
tools, and future hosted capabilities on the same surface:

- a tool producer materializes deterministic `ToolSpec`s;
- the engine records those specs as event-sourced session state;
- LLM planning snapshots the active set into each planned provider request;
- runtime adapters lower the snapshotted specs to provider-native request
  fields or client-effect tool execution.

## Problem

The current engine state is profile-centered:

```rust
pub struct ToolingState {
    pub registry: ToolRegistry,
    pub selected_profile_id: Option<ToolProfileId>,
    pub routing: ToolRoutingState,
}

pub struct ToolRegistry {
    pub tools: BTreeMap<ToolName, ToolSpec>,
    pub profiles: BTreeMap<ToolProfileId, ToolProfile>,
}

pub struct ToolProfile {
    pub profile_id: ToolProfileId,
    pub visible_tools: Vec<ToolName>,
    pub tool_choice: Option<ToolChoice>,
}
```

That shape made sense when planning for multiple alternate tool profiles, but
the current product direction does not need profiles. It creates practical
friction:

- MCP link/unlink must clone and rewrite the whole registry, then ensure the
  right profile exists and is selected.
- Built-in toolset refreshes need merge logic to preserve remote MCP links in
  the selected profile.
- A single model-facing concept is split across `tools`, `profiles`, and
  `selected_profile_id`.
- `tool_choice` is attached to profiles even though it is model-generation
  behavior, not tool-set membership.
- Incremental updates are awkward; there is only full registry replacement.

Context is different. Context entries have immutable ids, ordering, consumption
state, compaction, and provider-native continuity concerns. Tools are keyed
capability state. The tools API should feel similar to context edit commands
where useful, but it should use map semantics instead of context-entry
semantics.

## Design Position

Use a single active tool set.

```rust
pub struct ToolingState {
    pub revision: u64,
    pub tools: BTreeMap<ToolName, ToolSpec>,
    pub routing: ToolRoutingState,
}
```

`ToolRegistry` and `ToolProfile` go away. The active tool map lives directly on
`ToolingState`; avoid a wrapper type that reintroduces a second tool-set noun.

`ToolingState.revision` is a monotonic active-tool-set revision, analogous to
`ContextState.revision` and `config_revision`. It is used for optimistic
concurrency by API callers that patch tools independently.

## Tool Choice

Move `ToolChoice` out of tooling state and into model/run/turn configuration.

`tool_choice` means "how the model may use the tools that are present in this
request". It is not a tool and not a visibility rule.

Target engine shape:

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
    pub tool_choice: Option<OptionalConfigPatch<ToolChoice>>,
}
```

The exact run override type can be adjusted. The rule is:

```text
effective tool_choice = run override, else session turn config, else provider default
```

Validation for `ToolChoiceMode::Specific { tool_name }` happens against the
active tool set when a config update is admitted and again when an LLM request
is planned. Planning must still reject if a previously valid specific tool was
removed before the next turn.

## Core Commands

Replace registry/profile commands:

```rust
CoreAgentCommand::SetToolRegistry { registry }
CoreAgentCommand::SelectToolProfile { profile_id }
```

with:

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

Keep routing commands:

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
- removing a missing tool should be rejected by default, like
  `RemoveContext`, unless a gateway wants to preflight and no-op;
- empty patches are no-ops at admission.

This gives MCP exactly what it needs:

- link one server: one `PatchTools.upsert`;
- unlink one server: one `PatchTools.remove`;
- refresh a batch of MCP links: one patch with many upserts/removes;
- reset all tools: `ReplaceTools`.

## Core Events

Replace registry/profile events:

```rust
ToolConfigEvent::RegistryChanged { registry }
ToolConfigEvent::ProfileSelected { profile_id }
```

with revisioned events:

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
applies the change, validates the resulting tool set, and increments
`state.tooling.revision`.

Target routing events can stay unrevisioned unless there is a demonstrated need
for routing-level optimistic concurrency:

```rust
ToolConfigEvent::DefaultTargetSet { target }
ToolConfigEvent::DefaultTargetCleared { namespace }
```

## Tool Set Validation

Validate the resulting active set, not a selected profile:

- map key matches `ToolSpec.name`;
- each `ToolSpec` validates;
- remote MCP server labels are unique across active `ToolKind::RemoteMcp`
  entries;
- `ToolChoiceMode::Specific` in effective config references an active tool;
- provider-native tool API kind compatibility is checked during request
  planning;
- remote MCP provider support is checked during request planning.

Remote MCP uniqueness changes from "unique within selected profile" to "unique
within active tool set". That is the intended simplification because the active
set is exactly what the provider sees.

## LLM Planning

Replace `selected_tools_and_choice` with active-set selection:

```rust
fn active_tools(
    state: &CoreAgentState,
    api_kind: &ProviderApiKind,
) -> Result<Vec<ToolSpec>, PlanningError>
```

The LLM planner:

1. builds the planned context snapshot;
2. reads and validates the active tool set for the selected provider API kind;
3. resolves effective `tool_choice` from config;
4. validates `tool_choice` against the active tool set;
5. stores `tools` and `tool_choice` in the immutable `LlmRequest`.

Tool updates affect future planned turns. They do not mutate an already planned
`LlmRequest`.

## Tool Call Acceptance

The current tool batch path accepts calls by checking live tooling state. After
this refactor, acceptance should check the turn's planned request snapshot.

Reason: a model can only call tools that were sent in that request. If the live
tool set changes while a run is active, already-returned tool calls should be
judged against the request that produced them, not the latest session state.

Recommended helper:

```rust
fn planned_tool_for_call(
    active_run: &ActiveRun,
    turn_id: TurnId,
    tool_name: &ToolName,
) -> Option<ToolSpec>
```

`initial_tool_call_status` should use that planned spec to decide whether a
call is accepted or unavailable. `decide_active_tool_batch_invocations` and
`start_tool_call` should also use the planned spec for `invokes_client_effect`
and `target_requirement`.

Default execution targets remain live routing state. That is acceptable because
routing is execution placement, not model-visible tool membership. If this
becomes surprising, add routing revisions later.

## Session Config And Gateway Tool Defaults

Keep session config for desired built-in tool defaults, but do not treat it as
the active arbitrary tool API.

Current public fields can remain initially:

```json
{
  "tools": {
    "webSearch": true,
    "webFetch": true,
    "host": "edit"
  }
}
```

Internally, call this `ToolDefaultsConfig` or similar:

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
4. Preserve non-standard tools, including MCP links, without special profile
   merge logic.
5. Set or clear default execution targets for host tools as today.

The tools crate should return a `ResolvedToolset` that contains a flat
`BTreeMap<ToolName, ToolSpec>` plus runtime catalog/documents, not a registry
with a profile.

## Public API

Add a generic session tool update method:

```text
session/tools/update
```

Candidate request:

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

Candidate replace:

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

`session/mcp/link`, `session/mcp/unlink`, and `session/mcp/list` can remain as
MCP-specific convenience APIs. Link/unlink should materialize catalog snapshots
and submit `PatchTools`; list should read active `ToolKind::RemoteMcp` entries.

Expose active tools in `SessionView`:

```rust
pub struct ToolSetView {
    pub revision: u64,
    pub tools: Vec<ToolView>,
}
```

Project events as:

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

Avoid exposing reducer-internal full specs in events unless a client needs
them. `session/read` can provide the current active tool view.

## MCP Impact

P68 session linking should change from:

```text
clone registry
insert ToolKind::RemoteMcp
ensure selected profile exists
append visible_tools
SetToolRegistry
SelectToolProfile
```

to:

```text
resolve server catalog record
validate auth grant handle
materialize ToolSpec::RemoteMcp
PatchTools { upsert: [tool], remove: [] }
```

Unlink becomes:

```text
PatchTools { upsert: [], remove: [tool_name] }
```

Batch updates use the same patch command. No profile selection is involved.

P67 direct MCP should be updated to say that `RemoteMcp` specs live in the
active tool set, not in a selected tool profile.

## Migration Plan

1. Engine data model:
   - add `ToolingState.revision` and store active tools directly as
     `ToolingState.tools: BTreeMap<ToolName, ToolSpec>`;
   - remove `ToolProfile`, `ToolProfileId`, profile validation, and selected
     profile state;
   - move `tool_choice` into config.

2. Engine commands/events:
   - add `ReplaceTools` and `PatchTools`;
   - remove `SetToolRegistry` and `SelectToolProfile`;
   - add revision checks and event application tests.

3. Planning and tool execution:
   - plan from the active set;
   - resolve `tool_choice` from config;
   - validate tool calls against the planned request snapshot.

4. Tools crate:
   - make `ResolvedToolset` flat;
   - remove `DEFAULT_TOOLSET_PROFILE_ID`;
   - return standard tool names so the gateway can patch built-ins cleanly.

5. Gateway:
   - replace profile merge logic with standard-tool patching;
   - change MCP link/unlink to `PatchTools`;
   - wait on tool revision or expected MCP tool-name set, not selected profile.

6. API/projection/CLI:
   - remove profile-selected events;
   - expose active tool revision and tool views;
   - add `session/tools/update` if generic tool updates should be public now;
   - keep MCP convenience commands backed by the generic patch path.

7. Docs:
   - update P67 and P68 wording from "selected profile" to "active tool set";
   - update README/AGENTS only if external command behavior changes.

## Tests

Engine:

- tool patch upserts and removes by name;
- patch rejects duplicate names and upsert/remove conflicts;
- replace and patch bump `tooling.revision`;
- expected tool revision conflicts reject admission;
- remote MCP server labels are unique across active tools;
- `ToolChoiceMode::Specific` rejects missing tools;
- LLM request includes active tools and config-owned `tool_choice`;
- tool calls are accepted against planned request tools, not live tools.

Gateway/API:

- session start materializes standard tools with one flat active tool set;
- session update patches standard tools without removing MCP links;
- MCP link upserts a remote MCP tool;
- MCP unlink removes only that remote MCP tool;
- MCP batch link/unlink can be represented by one patch;
- projected session view includes active tool revision.

Runtime:

- client-effect tool invocation still dispatches by `tool_name`;
- remote MCP specs do not create Forge tool batches;
- provider-native and remote MCP compatibility checks still fail clearly for
  unsupported provider API kinds.

## Decisions

Resolved for this implementation:

- `session/tools/update` is public and backs replace/patch of the active tool
  set. MCP link/unlink remain convenience APIs over the same core patch path.
- Removing a missing tool is a hard engine rejection. Convenience APIs can
  preflight if they want idempotent behavior.
- Routing targets do not have a separate revision.
- `tool_choice` lives in config and supports session defaults plus run/turn
  effective config.

Implemented surface:

- `ToolingState { revision, tools, routing }` with tools stored directly as the
  active tool map, plus `ToolPatch`.
- `CoreAgentCommand::ReplaceTools` and `CoreAgentCommand::PatchTools`.
- `ToolConfigEvent::ToolsReplaced` and `ToolConfigEvent::ToolsPatched`.
- LLM planning reads the active tool set and config-owned `tool_choice`.
- Tool call acceptance checks the planned request snapshot.
- Standard hosted/host tool refresh and MCP link/unlink submit patches instead
  of rewriting profiles.
- `SessionView.activeTools` exposes the current active tool set and revision.
