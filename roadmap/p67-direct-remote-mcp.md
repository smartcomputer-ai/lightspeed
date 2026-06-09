# P67: Direct Remote MCP

**Status**
- Proposed; core foundation partially implemented on 2026-06-09.
- Implemented foundation: `RemoteMcpToolSpec`, `RemoteMcpApprovalPolicy`,
  `SecretRef`, `ToolKind::RemoteMcp`, engine validation, provider compatibility
  checks, request fingerprint participation, and client-effect exclusion.
- Provider MCP request lowering, runtime auth injection, redacted provider
  request persistence, and provider MCP output recording remain pending.
- Tool visibility now uses the active session tool map from
  `p67-tooling-refactor.md`; profile selection has been removed.
- Direct provider-hosted MCP only: the model provider connects to public remote
  MCP servers during the model call.
- No Forge MCP bridge, no local/on-prem tunnel support, no Forge-hosted MCP
  client runtime, and no arbitrary REST wrapping in this milestone.
- Builds on P49 tool packages, P50 provider-native request materialization,
  P64 provider-opaque context preservation, and P66's pattern for hosted tools.

## Goal

Let a Forge session expose remote MCP servers to supported provider APIs while
preserving the event-sourced capability state that explains why a model saw
those servers.

The first cut should support direct public remote MCP servers:

- OpenAI Responses remote MCP tools;
- Anthropic Messages remote MCP toolsets;
- deterministic session/run configuration for which MCP servers are visible;
- runtime-only secret injection for MCP authorization tokens;
- provider-opaque recording of MCP list/call output items.

The engine remains deterministic. It records configured capabilities and plans
provider-native request data, but it does not connect to MCP servers, list MCP
tools, call MCP tools, refresh OAuth tokens, open tunnels, or execute network
I/O.

## Design Position

Direct remote MCP is a configured model-facing capability, not a Forge-executed
function tool.

It is close to a VFS mount in one respect: a session has an attached external
capability, and the event log must say which capability was attached. It differs
from a VFS mount because it directly affects the provider request shape. The
planned LLM request must therefore carry the sanitized MCP server spec used for
that turn.

Configured MCP servers are **capability state**. Provider-returned MCP items
such as OpenAI `mcp_list_tools` / `mcp_call` and Anthropic MCP tool use/result
blocks are **context observations**. Do not model provider-discovered MCP tools
as Forge `ToolSpec`s in the first cut.

Use a `ToolKind::RemoteMcp` variant in the active session tool map, not a
standalone MCP subsystem in `engine`:

```rust
pub enum ToolKind {
    Function(FunctionToolSpec),
    ProviderNative(ProviderNativeToolSpec),
    RemoteMcp(RemoteMcpToolSpec),
}
```

`RemoteMcp` means "remote MCP server exposed through a provider-hosted MCP
client." Forge owns configuration and audit state. The model provider owns
discovery and execution for direct MCP calls.

P68 owns the universe-scoped MCP registry and session linking. P69 owns
encrypted secrets, OAuth grants, and runtime token brokering. P67 consumes the
sanitized `RemoteMcpToolSpec` snapshots from P68 and runtime auth handles from
P69.

## Non-Goals

- Do not build a Forge MCP bridge/client runtime in P67.
- Do not convert discovered MCP tools into Forge function tools.
- Do not support private/on-prem MCP servers or tunnels in P67.
- Do not wrap arbitrary REST APIs as MCP in P67.
- Do not add MCP server discovery, marketplace search, or dynamic install.
- Do not put bearer tokens, OAuth access tokens, cookies, or API keys in
  engine events, CAS blobs, planned request records, or stored provider request
  blobs.
- Do not make `engine` perform MCP list-tools calls, MCP tool calls, HTTP
  transport, OAuth refresh, DNS resolution, or connectivity checks.
- Do not parse MCP tool results in reducers for branching decisions unless a
  later milestone introduces a concrete deterministic fact.

## Core Model

`engine` has a declarative, non-secret MCP server spec:

```rust
pub struct RemoteMcpToolSpec {
    pub server_label: String,
    pub server_url: String,
    pub description_ref: Option<BlobRef>,
    pub allowed_tools: Option<Vec<String>>,
    pub approval: RemoteMcpApprovalPolicy,
    pub defer_loading: Option<bool>,
    pub auth_ref: Option<SecretRef>,
}

pub enum RemoteMcpApprovalPolicy {
    ProviderDefault,
    Always,
    Never,
}

pub struct SecretRef {
    pub namespace: String,
    pub id: String,
}
```

Names may be adjusted during implementation, but the important split is:

- `server_label`, `server_url`, allowlists, approval mode, and defer-loading
  policy are deterministic config and belong in event-sourced state.
- `auth_ref` is only a pointer to a runtime secret.
- actual secret values are resolved only by `llm-runtime` immediately before
  sending the provider request.

`RemoteMcp` specs live in the active session tool map:

```text
ToolingState.tools:
  mcp_echo -> ToolKind::RemoteMcp(...)
  read_file -> ToolKind::Function(...)
```

The active tool map validates:

- stable, non-empty server labels;
- HTTP or HTTPS server URLs;
- no credentials embedded in URLs;
- unique provider-facing server labels across active remote MCP entries;
- valid `allowed_tools` names for provider API constraints;
- `auth_ref` syntax only, not secret existence or value.

`ToolSpec::invokes_client_effect()` should return false for `RemoteMcp`. The
provider performs list/call execution, so the CoreAgent tool planner must not
emit `ToolEvent::CallStarted` for direct MCP.

## Request Planning

The turn planner reads `RemoteMcp` specs from the same active tool map as
function and provider-native tools.

`engine` should preserve provider compatibility without knowing provider wire
details. A `RemoteMcp` spec is compatible with provider API kinds that have a
runtime lowering implementation:

- `ProviderApiKind::OpenAiResponses`;
- `ProviderApiKind::AnthropicMessages`.

`OpenAiCompletions` should reject `RemoteMcp` until a real provider API path
exists.

Provider-specific request structs should carry enough sanitized MCP data for
replay and fingerprinting:

- OpenAI may keep MCP as `ToolKind::RemoteMcp` in the planned `tools` list and
  lower it in `llm-runtime`.
- Anthropic should not make `mcp_servers` the source of truth. It should derive
  `mcp_servers` and `tools` from the same planned `RemoteMcp` specs during
  materialization.

The planned request fingerprint should include the sanitized `RemoteMcp` spec,
including `auth_ref`, but not the resolved secret value.

## Provider Lowering

`llm-runtime` owns provider lowering because OpenAI and Anthropic use different
wire shapes.

### OpenAI Responses

For each planned `ToolKind::RemoteMcp`, lower to one OpenAI tool entry:

```json
{
  "type": "mcp",
  "server_label": "echo",
  "server_description": "Echo test tools",
  "server_url": "https://echo.example.com/mcp",
  "allowed_tools": ["hello"],
  "require_approval": "never",
  "defer_loading": true
}
```

If `auth_ref` resolves to a token at runtime, inject:

```json
{
  "authorization": "<runtime token>"
}
```

Do not put `authorization` in engine state or persisted provider request blobs.

### Anthropic Messages

For each selected `ToolKind::RemoteMcp`, lower to one `mcp_servers` entry and
one `tools` entry from the same source spec:

```json
{
  "mcp_servers": [
    {
      "type": "url",
      "name": "echo",
      "url": "https://echo.example.com/mcp"
    }
  ],
  "tools": [
    {
      "type": "mcp_toolset",
      "mcp_server_name": "echo"
    }
  ]
}
```

If `auth_ref` resolves to a token at runtime, inject the provider's auth field,
for example:

```json
{
  "authorization_token": "<runtime token>"
}
```

Anthropic MCP may require a provider beta header. That belongs in provider
client/runtime configuration, not in `engine`.

## Secret Injection And Redaction

Introduce an `llm-runtime` secret resolver, not an engine dependency:

```rust
#[async_trait]
pub trait SecretResolver: Send + Sync {
    async fn resolve(&self, secret_ref: &SecretRef) -> Result<Option<String>, SecretError>;
}
```

The resolver is used only during provider request send. Materialization should
produce two request values:

- a send request with resolved auth fields;
- a redacted request for `provider_request_ref`.

Redacted request blobs should preserve the fact that auth was configured:

```json
{
  "authorization": "<redacted>"
}
```

or:

```json
{
  "authorization_token": "<redacted>"
}
```

If a required `auth_ref` cannot be resolved, the generation should fail clearly
before provider I/O. If auth is optional and absent, omit provider auth fields.

## Output Recording

Provider-hosted MCP execution must not become a Forge client-effect tool batch.

Record provider-returned MCP observations as provider-opaque context entries:

```text
ContextEntryKind::ProviderOpaque
media_type = "application/json"
provider_kind = "openai.responses.mcp_list_tools"
provider_kind = "openai.responses.mcp_call"
provider_kind = "anthropic.messages.mcp_tool_use"
provider_kind = "anthropic.messages.mcp_tool_result"
```

The exact provider-native item/block should be stored in CAS. Previews should be
short and audit-oriented, for example:

```text
OpenAI Responses MCP tool call: echo.hello
Anthropic MCP tool result: echo.hello
```

Do not synthesize `ObservedToolCall` for direct MCP unless a provider requires
client-side continuation. Direct MCP is already executed by the provider.

Continuation behavior should follow provider rules:

- For OpenAI, keep relevant MCP output items in context when they are returned
  as provider output/input items and the provider expects them for efficient
  continuation.
- For Anthropic, keep MCP tool use/result blocks in the native message context
  when needed for continuity.

Reducers should treat these entries as opaque unless a future milestone adds a
specific deterministic branch fact.

## API And Configuration

Expose remote MCP as tool/capability configuration, not as context text.

Candidate public config:

```json
{
  "tools": {
    "remoteMcp": [
      {
        "id": "mcp_echo",
        "serverLabel": "echo",
        "serverUrl": "https://echo.example.com/mcp",
        "description": "Echo test tools",
        "allowedTools": ["hello"],
        "approval": "never",
        "deferLoading": true,
        "authRef": {
          "namespace": "env",
          "id": "ECHO_MCP_TOKEN"
        }
      }
    ]
  }
}
```

The gateway/tooling layer should translate this into:

- `PatchTools { upsert: [ToolKind::RemoteMcp(...)], remove: [] }`;
- an active `ToolingState.tools["mcp_echo"]` entry after the event applies;
- optional provider request default patches only when a provider requires them.

Do not expose provider-generated `mcp_list_tools` as the configured tool list.
It is an observation of what the server returned for a given call.

## Crate And Module Shape

Suggested first-cut changes:

```text
crates/engine/src/core/components/tooling.rs
  RemoteMcpToolSpec
  RemoteMcpApprovalPolicy
  SecretRef
  ToolKind::RemoteMcp
  validation and invokes_client_effect behavior

crates/tools/src/mcp/
  mod.rs
  model.rs
  builder.rs

crates/llm-runtime/src/openai_responses.rs
  lower RemoteMcp to OpenAI Responses mcp tools
  inject/redact authorization
  preserve mcp_list_tools and mcp_call output items

crates/llm-runtime/src/anthropic_messages.rs
  lower RemoteMcp to mcp_servers + mcp_toolset
  inject/redact authorization_token
  preserve MCP content blocks as provider-opaque context

crates/llm-runtime/src/secrets.rs
  SecretResolver trait and no-op/env-backed first implementation
```

Do not add a production MCP transport/client crate in P67. Non-secret MCP
metadata/preflight checks belong outside engine in P68; OAuth/token work belongs
to P69; actual Forge-executed MCP calls belong to a later bridge milestone.

## G1: OpenAI Responses Direct Remote MCP

Implement `RemoteMcp` registry support and OpenAI Responses lowering first.

Acceptance criteria:

- A selected `RemoteMcp` spec appears in the planned request fingerprint without
  secret values.
- OpenAI request materialization emits `tools[]` entries with `type: "mcp"`.
- `auth_ref` values are resolved only in `llm-runtime`.
- persisted provider request blobs redact auth fields.
- OpenAI `mcp_list_tools` and `mcp_call` output items are preserved as
  provider-opaque context.
- No Forge tool batch is scheduled for provider-executed MCP calls.

Tests:

- unit test for `RemoteMcp` validation and client-effect behavior;
- request materialization test for OpenAI MCP lowering;
- redaction test proving send request has auth while stored request does not;
- result parsing fixture for `mcp_list_tools` and `mcp_call`;
- ignored live test against a harmless public MCP server.

## G2: Anthropic Messages Direct Remote MCP

Add Anthropic lowering once the first OpenAI path is stable.

Acceptance criteria:

- A selected `RemoteMcp` spec lowers to both `mcp_servers[]` and `tools[]`
  `mcp_toolset` entries.
- The two Anthropic provider blocks are derived from one source spec and cannot
  drift.
- runtime auth injection uses the provider auth field and redacts persisted
  request blobs.
- required Anthropic beta headers are configured through provider runtime/client
  options.
- provider MCP content blocks are preserved as provider-opaque context.

Tests:

- materialization test for paired `mcp_servers` + `mcp_toolset` output;
- duplicate server-label validation test;
- auth redaction test;
- result parsing fixture for Anthropic MCP tool use/result blocks;
- ignored live test gated by API key and explicit beta/header requirements.

## Future Work

- Forge-hosted MCP bridge that discovers MCP tools and exposes them as ordinary
  Forge function tools.
- Secure tunnel support for private/on-prem MCP servers.
- Connector-specific product UX and OAuth flows.
- MCP server health checks and preflight list-tools validation outside engine.
- Approval UI for provider-hosted MCP calls where provider APIs surface
  approval requests.
- Rich API projection of provider MCP observations for clients.
- Policy controls for allowed MCP domains, server allowlists, and data egress
  review.
