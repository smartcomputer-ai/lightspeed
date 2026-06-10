# P68: Remote MCP Registry And Session Linking

**Status**
- In progress.
- Split from the original P68 registry/auth plan.
- Companion to P67 direct remote MCP and P69 generic auth/token broker.
- Owns the MCP-specific control plane for registering remote MCP servers and
  linking configured servers into sessions.
- Does not implement generic OAuth, encrypted secret storage, refresh, token
  brokering, GitHub auth, provider request lowering, or MCP tool execution.

**Implementation Notes**
- First cut implemented: `mcp-registry` owns provider-independent registry
  DTOs, validation, store trait, and in-memory test adapter.
- First cut implemented: `store-pg` owns the universe-scoped `mcp_servers`
  table, migration wiring, and `PgStore` implementation of the registry trait.
- Implemented: API/gateway/CLI registry create/list/read/delete.
- Implemented: session MCP link/list/unlink materializes catalog snapshots into
  `ToolKind::RemoteMcp` entries in the active engine tool set.
- Implemented: link/unlink uses `PatchTools` from `p67-tooling-refactor.md`;
  profile selection has been removed.
- Implemented in P67: OpenAI Responses provider request lowering for no-auth
  remote MCP servers and provider-opaque recording of OpenAI MCP output items.
- Implemented: MCP server create defaults `approvalDefault` to `never` so
  no-auth/public OpenAI MCP links work without provider approval continuation.
- Implemented 2026-06-10 (with P69 G1): link-time auth grant validation
  against the P69 grant store — grant existence, `Active` status,
  provider-kind/auth-policy compatibility, and audience coverage of the
  server URL; universe equality holds by construction. Static bearer grants
  work end to end (`forge auth grant import` -> `forge mcp link
  --auth-grant-id` -> runtime injection).
- Still pending: Anthropic provider lowering, OAuth grants (P69 G2+), and
  principal-policy checks (deferred until Forge has user identity).
- Still pending: `mcp/servers/update` (listed in the API surface but not
  implemented). G3 metadata discovery needs it for auth-policy writes and
  status transitions such as `unverified -> active`; until then catalog edits
  are delete + recreate.
- Note for G3: RFC 9728 protected resource metadata lists
  `authorization_servers` as an array; the single `authorization_server`
  field in `McpServerAuthPolicy` will need to become a list when discovery
  lands.
- Grant compatibility rules for G2 are defined in P69 (G4 section): provider
  kind class matches the server auth policy, grant audience covers the server
  resource, grant status is `Active`; universe equality holds by construction
  because gateway stores are universe-bound.

## Goal

Add a universe-scoped remote MCP server catalog so Forge can configure remote
MCP servers once and attach them to sessions as model-facing tools.

The first product flow is no-auth/public MCP:

```text
forge mcp server add https://echo.example.com/mcp --id echo --label echo
  -> Forge stores a sanitized MCP server catalog record in the universe
  -> user links MCP server "echo" to a session tool set
  -> gateway materializes ToolKind::RemoteMcp(RemoteMcpToolSpec)
  -> P67 lowers that active tool to provider-native MCP request fields
```

For authenticated servers, P68 records MCP-specific auth requirements and uses
auth handles produced by P69:

```text
forge auth login mcp:crm
  -> P69 stores the OAuth/static credential grant
forge mcp link --session session_1 crm --auth-grant authgrant_123
  -> P68 validates the grant handle belongs to the universe/server
  -> P68 materializes auth_ref into RemoteMcpToolSpec
```

The engine remains deterministic. It records only the sanitized MCP server spec
and an optional auth reference in the event-sourced tool registry. Server
catalog edits, metadata discovery, OAuth, token refresh, secret storage, and
connectivity checks are outside `engine`.

## Relationship To P67 And P69

P67 owns the engine model and provider request shape:

- `ToolKind::RemoteMcp`;
- `RemoteMcpToolSpec`;
- provider compatibility checks;
- provider-native OpenAI/Anthropic lowering;
- provider-opaque MCP output recording.

P68 owns MCP-specific catalog and linking:

- universe-scoped MCP server records;
- validation and normalization of server config;
- session link/unlink/list operations;
- materializing catalog snapshots into `RemoteMcpToolSpec`;
- mapping a selected P69 auth handle into `RemoteMcpToolSpec.auth_ref`.

P69 owns generic auth and secret infrastructure:

- encrypted secret store;
- static bearer credentials;
- OAuth clients, flows, grants, refresh;
- runtime token broker;
- GitHub App/OAuth support;
- token leases for VMs, sandboxes, tools, and provider runtimes.

## Design Position

Split direct remote MCP into three layers:

1. **MCP catalog**: durable, mutable universe records for known MCP servers.
2. **Auth handles**: generic credentials/grants owned by P69 and referenced by
   P68 when linking authenticated MCP servers.
3. **Session tool state**: event-sourced sanitized `RemoteMcpToolSpec` entries
   selected into a session's tool registry.

The catalog is not event-sourced session state. It is control-plane state, like
VFS workspaces or hosted configuration records. A session links to catalog
records by materializing a sanitized snapshot into the engine tool registry.
This avoids nondeterministic replay if the catalog record is later edited.

Authenticated access is explicit. Server configuration is universe-scoped, but
credentials are attached through P69 auth handles tied to a principal:

```text
server config: universe + mcp_server_id
auth grant: universe + auth_grant_id + principal + provider binding
session link: session + mcp_server_id + optional auth_grant_id
```

P68 should not infer or silently attach a user/universe credential. A session
link either omits auth for public servers or names the auth handle to use.

## Non-Goals

- Do not implement OAuth flows in P68.
- Do not implement encrypted secret storage in P68.
- Do not implement token refresh or a runtime token broker in P68.
- Do not make `engine` perform MCP HTTP discovery, tool listing, OAuth, token
  lookup, or secret resolution.
- Do not implement a Forge-hosted MCP bridge/client runtime.
- Do not implement private/on-prem tunnels.
- Do not convert discovered MCP tools into Forge function tools.
- Do not require every MCP server to use OAuth.
- Do not silently attach a universe-level auth grant to a session.

## Reference Points

P68 should understand MCP auth metadata enough to record requirements and hand
off to P69, but P69 owns the actual OAuth implementation.

References:

- https://modelcontextprotocol.io/specification/2025-06-18/basic/authorization
- https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization
- https://modelcontextprotocol.io/extensions/auth/oauth-client-credentials

## Data Model

Postgres already treats a universe as the tenant/project/workspace boundary.
P68 should add universe-scoped MCP server catalog tables to `store-pg` or a
narrow sibling store crate.

Recommended logical records:

```rust
pub struct McpServerRecord {
    pub universe_id: Uuid,
    pub server_id: String,
    pub display_name: Option<String>,
    pub server_url: String,
    pub transport: RemoteMcpTransport,
    pub default_server_label: String,
    pub description: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    pub approval_default: RemoteMcpApprovalPolicy,
    pub defer_loading_default: Option<bool>,
    pub auth_policy: McpServerAuthPolicy,
    pub status: McpServerStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

pub enum RemoteMcpTransport {
    StreamableHttp,
    Sse,
    Auto,
}

pub enum McpServerAuthPolicy {
    None,
    OptionalBearer,
    RequiredBearer,
    OptionalOAuth {
        resource: String,
        scopes_default: Vec<String>,
        protected_resource_metadata_url: Option<String>,
        authorization_server: Option<String>,
    },
    RequiredOAuth {
        resource: String,
        scopes_default: Vec<String>,
        protected_resource_metadata_url: Option<String>,
        authorization_server: Option<String>,
    },
}

pub enum McpServerStatus {
    Active,
    NeedsAuthConfig,
    Unverified,
    Disabled,
}
```

P68 may cache non-secret MCP auth metadata on `McpServerRecord`, but it should
not store OAuth clients, tokens, refresh tokens, PKCE verifiers, or bearer
secrets. Those are P69 records.

## Server Registration

User or admin adds a server:

```bash
forge mcp server add https://crm.example.com/mcp --id crm --label crm
```

The gateway/control plane should:

- normalize and validate the server URL;
- reject credentials embedded in URLs;
- record the server config in the universe catalog;
- optionally discover MCP protected resource metadata;
- record whether auth appears required, optional, absent, or unknown;
- avoid failing no-auth/manual registration only because metadata discovery is
  unavailable.

If discovery fails for an authenticated server, allow a manual record only with
explicit flags and a clear status such as `needs_auth_config` or `unverified`.

## Session Linking

Session linking should reference the universe catalog, then materialize a
sanitized snapshot into the engine tool registry.

Candidate session config input:

```json
{
  "tools": {
    "remoteMcp": [
      {
        "serverId": "crm",
        "toolId": "mcp_crm",
        "serverLabel": "crm",
        "allowedTools": ["lookup_customer"],
        "approval": "never",
        "auth": {
          "grantId": "authgrant_123"
        }
      }
    ]
  }
}
```

The gateway resolves `serverId` and optional `grantId`, validates that both
belong to the same universe, checks that the grant is compatible with the MCP
server, and commits a `PatchTools` upsert containing:

```rust
ToolKind::RemoteMcp(RemoteMcpToolSpec {
    server_label: "crm".to_owned(),
    server_url: "https://crm.example.com/mcp".to_owned(),
    description_ref: None,
    allowed_tools: Some(vec!["lookup_customer".to_owned()]),
    approval: RemoteMcpApprovalPolicy::Never,
    defer_loading: Some(false),
    auth_ref: Some(SecretRef {
        namespace: "auth_grant".to_owned(),
        id: "authgrant_123".to_owned(),
    }),
})
```

This event-sourced snapshot is what replay uses. If the catalog record changes,
existing sessions do not change until the user explicitly refreshes or relinks
that MCP server.

## API Surface

First-cut JSON-RPC methods can be narrow and product-shaped:

```text
mcp/servers/list
mcp/servers/create
mcp/servers/read
mcp/servers/update
mcp/servers/delete

session/mcp/link
session/mcp/unlink
session/mcp/list
```

The API should return auth policy and auth handle refs, never token values.

`session/mcp/link` may be syntactic sugar over the existing session config/tool
registry update path. The important behavior is that the engine receives a
resolved `RemoteMcpToolSpec`, not a mutable catalog pointer.

## CLI Surface

Candidate commands:

```bash
forge mcp server add https://crm.example.com/mcp --id crm --label crm
forge mcp server list
forge mcp server read crm
forge mcp server remove crm

forge mcp link --session session_1 crm --tool-id mcp_crm
forge mcp link --session session_1 crm --tool-id mcp_crm --auth-grant authgrant_123
forge chat --new --mcp crm
```

Auth commands belong to P69. P68 may provide MCP-specific convenience wrappers
later, but the first shared auth surface should be generic.

## Crate And Module Shape

Suggested first-cut changes:

```text
crates/store-pg/src/mcp/
  universe-scoped MCP server catalog tables and queries

crates/api/src/mcp.rs
  public request/response DTOs for server registry and session linking
  auth policy and auth handle refs, never plaintext tokens

crates/temporal-server/src/mcp/
  JSON-RPC handlers for catalog and linking operations
  optional non-secret metadata discovery

crates/cli/src/mcp.rs
  mcp server/link commands
```

If a new shared crate becomes necessary, keep it narrow, for example
`mcp-registry` for DTOs and traits used by both gateway and CLI. Do not create
a general Forge MCP client/runtime crate in P68; Forge is still not executing
MCP tool calls in this milestone.

## Security And Policy

Minimum rules:

- reject credentials embedded in MCP server URLs;
- require HTTPS for authenticated remote MCP servers, except explicit local
  development cases;
- a linked auth grant must belong to the same universe as the server and
  session using it;
- a linked auth grant must be compatible with the server auth policy;
- deleting or disabling a server should prevent new links and make refresh or
  relink operations fail clearly;
- deleting a grant in P69 must make future token resolution fail clearly;
- raw provider request logging must be redacted by P67/P69 runtime paths.

Longer-term policy can add:

- allowed MCP domains;
- admin approval for new servers;
- egress review for server descriptions and tool outputs;
- per-session or per-run restrictions on which grants can be used.

## G1: Static Registry And No-Auth Servers

Implement the universe-scoped MCP server catalog and session linking for public
or no-auth MCP servers.

Acceptance criteria:

- [x] server records are scoped by `universe_id`;
- [x] CLI/API can add, list, read, and delete server records;
- [x] sessions can link a catalog server into `ToolKind::RemoteMcp`;
- [x] linked session state contains a sanitized snapshot, not a live catalog pointer;
- [x] P67 can lower linked no-auth servers to OpenAI Responses provider
  requests.
- [ ] P67 can lower linked no-auth servers to Anthropic Messages provider
  requests.

## G2: Auth-Handle Linking

Add MCP-specific validation for linking a P69 auth grant to an MCP server.

Acceptance criteria:

- [x] MCP server records can declare optional/required bearer or OAuth auth
  policy;
- [x] session linking accepts an explicit P69 auth grant handle;
- [x] the gateway validates universe (by construction), server compatibility
  (grant kind vs auth policy, audience coverage), grant status, and auth
  requirement before committing session state; principal policy is deferred
  until Forge has user identity;
- [x] linked session state contains `auth_ref: SecretRef { namespace:
  "auth_grant", id: ... }`;
- [x] no secret values or OAuth protocol records are stored in P68 tables or
  engine events.

Static bearer grants are fully linkable; OAuth grants validate the same way
once P69 G2/G4 produce them.

## G3: MCP Metadata Discovery

Add optional non-secret MCP protected resource metadata discovery.

Acceptance criteria:

- server registration can discover MCP protected resource metadata when
  available;
- discovered non-secret auth hints populate `McpServerAuthPolicy`;
- failures produce explicit server status without blocking manual no-auth
  registration;
- metadata discovery runs outside `engine`.

## Future Work

- MCP server health checks and preflight tool listing outside `engine`.
- Hosted UI for managing MCP servers and session links.
- MCP-specific convenience wrappers over P69 auth login/status/revoke commands.
- Approval UI for provider-hosted MCP calls where provider APIs surface
  approval requests.
- Broader rich API projection of provider MCP observations for clients,
  beyond the OpenAI `mcp_call` display summaries implemented in P67.
- Policy controls for allowed MCP domains, server allowlists, and data egress
  review.
