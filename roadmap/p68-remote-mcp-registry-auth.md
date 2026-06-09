# P68: Remote MCP Registry And OAuth

**Status**
- Proposed.
- Companion to P67 direct remote MCP.
- Owns the control plane for registering MCP servers, authorizing them, storing
  OAuth credentials securely, and linking configured servers into sessions.
- Does not implement provider request lowering. P67 consumes the sanitized
  server specs and runtime auth handles produced here.

## Goal

Add a universe-scoped remote MCP registry and OAuth credential store so Forge can
configure remote MCP servers once, authorize them through a CLI or API flow, and
attach them to sessions as model-facing tools.

The intended product flow is:

```text
forge mcp server add https://crm.example.com/mcp --id crm
  -> Forge discovers MCP OAuth metadata
  -> Forge opens/prints an authorization URL
  -> user completes OAuth in a browser
  -> Forge stores encrypted token material in the DB-backed secret store
  -> user links mcp server "crm" to a session/tool profile
  -> P67 lowers that link to provider-native MCP request fields
  -> llm-runtime injects a fresh access token at send time
```

The engine remains deterministic. It records only the sanitized MCP server spec
and an auth reference in the event-sourced tool registry. OAuth discovery,
browser redirects, token exchange, refresh, revocation, encryption, and secret
storage are runtime/control-plane concerns.

## Reference Points

The current MCP authorization model is OAuth-based for HTTP transports:

- a protected MCP server acts as an OAuth resource server;
- an MCP client acts as an OAuth client;
- authorization servers may be hosted with the MCP server or separately;
- protected resource metadata is used to discover authorization servers;
- authorization server metadata is used to discover authorization and token
  endpoints;
- authorization-code flows use PKCE;
- access tokens are sent as bearer tokens on MCP HTTP requests;
- refresh tokens are optional and must be stored securely when issued.

References:

- https://modelcontextprotocol.io/specification/2025-06-18/basic/authorization
- https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization
- https://modelcontextprotocol.io/extensions/auth/oauth-client-credentials

## Design Position

Split MCP into three layers:

1. **Universe MCP catalog**: durable, mutable records for known MCP servers in a
   universe.
2. **MCP authorization store**: durable grants and encrypted token material for
   a principal to use a server.
3. **Session tool state**: event-sourced sanitized `RemoteMcpToolSpec` entries
   selected into a session's tool registry.

The catalog is not itself event-sourced session state. It is control-plane
state, like VFS workspaces or future hosted configuration records. A session
links to catalog records by materializing a sanitized snapshot into the engine
tool registry. This avoids nondeterministic session replay if the catalog record
is later edited.

Auth is not universal by default. Server configuration is universe-scoped, but
OAuth grants are tied to a principal:

```text
server config: universe + server_id
grant: universe + server_id + principal
session link: session + server_id + grant selection
```

The first principal kinds should be:

- `user`: a human user's OAuth grant;
- `service_account`: a non-human integration grant;
- `universe_default`: optional admin-managed default for shared automation.

Use `universe_default` carefully. Many MCP servers expose user data, so the
normal product path should prefer explicit user or service-account grants.

## Implementation Ownership

Forge should implement the MCP registry and auth control plane itself. The
catalog, OAuth grants, session linking, encrypted secret store, redaction rules,
and audit model are product state, not library state.

Use low-level crates where they reduce protocol and security risk, but keep them
behind Forge-owned traits and records:

- `url` or equivalent URL parsing for server URL normalization, validation, and
  credential stripping checks;
- `oauth2` or equivalent OAuth primitives for authorization-code + PKCE, token
  exchange, refresh, revocation, and safe HTTP client configuration;
- `reqwest` or the existing workspace HTTP stack for metadata discovery and
  token endpoint calls;
- `secrecy` / `zeroize` style wrappers for in-memory token and client-secret
  handling;
- AEAD/envelope-encryption crates or platform/KMS clients for the DB-backed
  secret store;
- `serde` DTOs for MCP OAuth metadata, OAuth server metadata, and Forge API
  payloads.

An MCP SDK such as `rmcp` can be evaluated for narrow protocol helpers, OAuth
metadata handling, or preflight list-tools checks, but it must not own Forge's
durable registry, credential model, session-linking model, or provider request
materialization. If used, wrap it behind traits such as `McpAuthDriver`,
`McpMetadataDiscovery`, or `McpPreflightClient` so it can be replaced without a
schema or API migration.

Do not depend on a third-party hosted connector registry, MCP auth manager, or
external control plane as the source of truth. Forge's Postgres-backed universe
catalog and secret store are authoritative.

## Non-Goals

- Do not put access tokens, refresh tokens, client secrets, authorization codes,
  PKCE verifiers, or provider request auth headers in engine events or CAS.
- Do not make `engine` perform OAuth, HTTP discovery, token refresh, or secret
  lookup.
- Do not implement a Forge-hosted MCP bridge in P68.
- Do not implement private/on-prem tunnels in P68.
- Do not delegate the universe MCP catalog, OAuth grants, session linking, or
  credential storage to an external MCP SDK or hosted connector service.
- Do not require every MCP server to use OAuth. Public/no-auth servers and
  manually configured static bearer tokens should remain representable.
- Do not add a full user/organization permission engine. Use the existing
  universe boundary and add minimal principal fields until hosted auth exists.
- Do not silently attach a universe-level OAuth grant to a session without an
  explicit config/link operation.

## Data Model

Postgres already treats a universe as the tenant/project/workspace boundary.
P68 should add universe-scoped MCP and secret tables to `store-pg` or a sibling
store crate.

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
    pub auth_config: McpServerAuthConfig,
    pub provider_options: serde_json::Value,
    pub status: McpServerStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

pub enum RemoteMcpTransport {
    StreamableHttp,
    Sse,
    Auto,
}

pub enum McpServerAuthConfig {
    None,
    OAuth {
        resource: String,
        protected_resource_metadata_url: Option<String>,
        authorization_server: Option<String>,
        scopes_default: Vec<String>,
        client_registration: McpClientRegistrationMode,
    },
    StaticBearer {
        secret_ref: SecretRef,
    },
}

pub enum McpClientRegistrationMode {
    Dynamic,
    Configured { client_id: String, client_secret_ref: Option<SecretRef> },
    Manual,
}
```

OAuth client and grant records:

```rust
pub struct McpOAuthClientRecord {
    pub universe_id: Uuid,
    pub server_id: String,
    pub authorization_server: String,
    pub client_id: String,
    pub client_secret_ref: Option<SecretRef>,
    pub registration_access_token_ref: Option<SecretRef>,
    pub registration_client_uri: Option<String>,
    pub metadata_json: serde_json::Value,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

pub struct McpOAuthGrantRecord {
    pub universe_id: Uuid,
    pub grant_id: String,
    pub server_id: String,
    pub principal: PrincipalRef,
    pub authorization_server: String,
    pub client_id: String,
    pub scopes: Vec<String>,
    pub subject_hint: Option<String>,
    pub access_token_ref: Option<SecretRef>,
    pub refresh_token_ref: Option<SecretRef>,
    pub expires_at_ms: Option<i64>,
    pub status: McpOAuthGrantStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

pub enum McpOAuthGrantStatus {
    Active,
    NeedsRefresh,
    NeedsReauth,
    Revoked,
    Failed,
}
```

Short-lived OAuth transaction records:

```rust
pub struct McpOAuthFlowRecord {
    pub universe_id: Uuid,
    pub flow_id: String,
    pub server_id: String,
    pub principal: PrincipalRef,
    pub state_hash: String,
    pub pkce_verifier_ref: SecretRef,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub resource: String,
    pub expires_at_ms: i64,
    pub completed_at_ms: Option<i64>,
}
```

## Secret Store

Add a generic secret store instead of MCP-specific token columns.

Candidate table shape:

```sql
CREATE TABLE secret_records (
    universe_id uuid NOT NULL REFERENCES universes (universe_id) ON DELETE CASCADE,
    secret_id text NOT NULL,
    secret_kind text NOT NULL,
    key_id text NOT NULL,
    ciphertext bytea NOT NULL,
    metadata_json jsonb NOT NULL DEFAULT '{}',
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    PRIMARY KEY (universe_id, secret_id)
);
```

Implementation rules:

- encrypt values before writing them to Postgres;
- use envelope encryption when a KMS is available;
- for local development, allow a configured local master key;
- include `universe_id`, `secret_id`, and `secret_kind` as authenticated data;
- never store plaintext in logs, CAS, session events, provider request blobs, or
  API responses;
- rotate by writing a new encrypted value under the same logical `secret_id` or
  by adding versioned secret ids if audit requirements demand it later.

Recommended secret kinds:

- `mcp.oauth.access_token`;
- `mcp.oauth.refresh_token`;
- `mcp.oauth.client_secret`;
- `mcp.oauth.registration_access_token`;
- `mcp.oauth.pkce_verifier`;
- `mcp.static_bearer`.

## OAuth Flow

P68 should implement the authorization-code + PKCE path first.

### 1. Register Server

User or admin adds a server:

```bash
forge mcp server add https://crm.example.com/mcp --id crm
```

The gateway/control plane should:

- normalize and validate the server URL;
- reject credentials embedded in URLs;
- discover protected resource metadata when available;
- discover authorization server metadata;
- record the server config in the universe catalog;
- record whether auth appears required, optional, or absent.

If discovery fails, allow a manual record only with explicit flags and a clear
status such as `needs_auth_config` or `unverified`.

### 2. Register OAuth Client

For OAuth servers that support dynamic client registration, Forge can register
itself and store the resulting client metadata.

For servers that do not support dynamic registration, admins must configure:

- `client_id`;
- optional `client_secret`;
- allowed redirect URI;
- required scopes.

The client secret is stored in the secret store. The client id is not secret.

### 3. Start Authorization

CLI/API starts a user or service-account authorization:

```bash
forge mcp auth login crm --scope contacts.read
```

The gateway creates an `McpOAuthFlowRecord` with:

- state;
- PKCE verifier stored as a secret;
- PKCE challenge;
- redirect URI;
- resource parameter for the canonical MCP server URI;
- requested scopes.

It returns or opens an authorization URL. For local CLI usage, support:

- loopback redirect URI on `localhost`; or
- hosted HTTPS callback that the CLI polls.

For hosted UI/API usage, use a gateway HTTPS callback.

### 4. Complete Authorization

The callback validates:

- state;
- flow expiry;
- principal/session intent;
- authorization server identity.

Then it exchanges the code for tokens using the stored PKCE verifier and stores:

- access token encrypted;
- refresh token encrypted, if issued;
- expiry time;
- granted scopes;
- provider subject hints when available.

Refresh tokens are optional. Forge must not assume one will be issued.

### 5. Refresh And Reauth

Before provider request send, `llm-runtime` should ask a runtime token provider
for a current token:

```rust
#[async_trait]
pub trait McpTokenProvider: Send + Sync {
    async fn access_token(&self, grant_id: &str) -> Result<Option<ResolvedSecret>, McpAuthError>;
}
```

The token provider may:

- return a non-expired access token;
- refresh with the refresh token and rotate stored token values;
- mark the grant `NeedsReauth` if refresh fails or no refresh token exists;
- return a clear error before provider I/O.

`llm-runtime` then injects only the access token into the provider send request.
P67 owns redaction of persisted provider request blobs.

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
        "toolId": "mcp.crm",
        "serverLabel": "crm",
        "allowedTools": ["lookup_customer"],
        "approval": "never",
        "auth": {
          "grantId": "mcpgrant_123"
        }
      }
    ]
  }
}
```

The gateway resolves `serverId` and `grantId`, validates that both belong to
the same universe, and commits a `ToolRegistry` update containing:

```rust
ToolKind::RemoteMcp(RemoteMcpToolSpec {
    server_label: "crm".to_owned(),
    server_url: "https://crm.example.com/mcp".to_owned(),
    allowed_tools: Some(vec!["lookup_customer".to_owned()]),
    approval: RemoteMcpApprovalPolicy::Never,
    defer_loading: Some(false),
    auth_ref: Some(SecretRef {
        namespace: "mcp_grant".to_owned(),
        id: "mcpgrant_123".to_owned(),
    }),
    ..
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

mcp/auth/start
mcp/auth/complete
mcp/auth/status
mcp/auth/revoke

session/mcp/link
session/mcp/unlink
session/mcp/list
```

The API should return secret refs and status, never token values.

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

forge mcp auth login crm --scope contacts.read
forge mcp auth status crm
forge mcp auth revoke crm

forge mcp link --session session_1 crm --tool-id mcp.crm
forge chat --new --mcp crm
```

For `auth login`, the CLI should print the authorization URL and try to open the
browser when appropriate. It should support a local loopback callback for local
development and a hosted callback/poll flow for remote environments.

## Crate And Module Shape

Suggested first-cut changes:

```text
crates/store-pg/src/mcp/
  universe-scoped MCP server catalog tables and queries
  OAuth client/grant/flow records
  encrypted secret record storage

crates/api/src/mcp.rs
  public request/response DTOs for server registry, auth, and session linking
  secret refs and grant status values, never plaintext tokens

crates/temporal-server/src/mcp/
  JSON-RPC handlers for catalog, auth, and linking operations
  OAuth metadata discovery and callback handling
  token refresh orchestration

crates/cli/src/mcp.rs
  mcp server/auth/link commands
  local loopback callback and hosted callback/poll support

crates/llm-runtime/src/secrets.rs
  SecretResolver / McpTokenProvider boundary used by P67 request lowering
```

If a new shared crate becomes necessary, keep it narrow, for example
`mcp-registry` or `mcp-auth` for DTOs and traits that are used by both the
gateway and CLI. Do not create a general Forge MCP client/runtime crate in P68;
Forge is still not executing MCP tool calls in this milestone.

## Security And Policy

Minimum rules:

- all OAuth authorization and token endpoints must use HTTPS except loopback
  redirect URIs;
- authorization-code flows must use PKCE;
- token requests must include the MCP resource indicator when required by the
  server/spec;
- access tokens are scoped to one MCP resource and must not be reused for other
  servers;
- a grant must belong to the same universe as the server and session using it;
- deleting a grant must make future token resolution fail clearly;
- removing a server should not delete secrets until grants are revoked or
  explicitly purged;
- raw provider request logging must be redacted;
- provider errors indicating invalid/expired/insufficient scopes should update
  grant status when observable.

Longer-term policy can add:

- allowed MCP domains;
- admin approval for new servers;
- egress review for server descriptions and tool outputs;
- per-session or per-run restrictions on which grants can be used.

## G1: Static Registry And No-Auth Servers

Implement the universe-scoped MCP server catalog and session linking for public
or no-auth MCP servers.

Acceptance criteria:

- server records are scoped by `universe_id`;
- CLI/API can add, list, read, and delete server records;
- sessions can link a catalog server into `ToolKind::RemoteMcp`;
- linked session state contains a sanitized snapshot, not a live catalog pointer;
- P67 can lower linked no-auth servers to provider requests.

## G2: Secret Store And Static Bearer

Add encrypted secret storage and support static bearer credentials.

Acceptance criteria:

- secrets are encrypted before insertion into Postgres;
- API/CLI never returns plaintext secret values;
- static bearer auth can be attached to a server or grant;
- `llm-runtime` resolves the bearer token through the secret resolver;
- provider request blobs redact injected auth.

## G3: OAuth Authorization Code With PKCE

Implement OAuth discovery, authorization start/complete, and encrypted token
storage.

Acceptance criteria:

- protected resource metadata and authorization server metadata are discovered;
- dynamic client registration is used when supported and configured;
- manually configured OAuth clients are supported when dynamic registration is
  unavailable;
- CLI login opens or prints an authorization URL;
- callback completes the flow and stores encrypted token material;
- grants expose status, scopes, expiry, and subject hints without plaintext
  tokens.

## G4: Refresh And Runtime Token Provider

Add refresh-token handling and runtime token resolution.

Acceptance criteria:

- `McpTokenProvider` returns current access tokens to `llm-runtime`;
- expiring tokens refresh before provider calls when a refresh token exists;
- refresh-token rotation updates encrypted stored secrets atomically;
- failed refresh marks the grant `NeedsReauth`;
- provider calls fail clearly before I/O when no valid token can be resolved.

## Future Work

- OAuth client credentials extension for non-human automation.
- Device authorization grant if useful for headless CLI environments.
- Admin approval flows for new MCP servers and requested scopes.
- MCP server health checks and preflight tool listing outside `engine`.
- Hosted UI for managing servers, grants, scopes, and revocation.
- External KMS integration and secret version audit history.
- Sharing and delegation rules for user grants versus service-account grants.
