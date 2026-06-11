# P69: Generic Auth, Secret Store, And Token Broker

**Status**
- In progress; design review decisions folded in on 2026-06-10.
- G1 implemented on 2026-06-10: `auth-registry` crate (grant/secret records,
  store traits, `SecretValue` redacted wrapper, `TokenAudience`,
  `RegistryTokenBroker` with typed `AuthBrokerError` kinds, in-memory test
  adapters); `store-pg` `auth_secrets`/`auth_grants` tables with AES-256-GCM
  encryption (AAD = universe/secret id/kind, `FORGE_SECRETS_MASTER_KEY`
  config; `dev/local/` exports a well-known dev-only default key);
  `auth/grants/import|list|read|revoke` JSON-RPC + `forge auth grant` CLI; `llm-runtime` `SecretResolver` with OpenAI Responses `authorization`
  injection and redacted persisted request blobs; worker
  `BrokerSecretResolver` wiring; and P68 link-time grant validation
  (status/kind/audience).
- G2 + G3 implemented in one cut on 2026-06-10: `auth-registry` gained
  `OAuthClientRecord`/`AuthFlowRecord` + store traits, PKCE/state helpers
  (RFC 7636 S256, state stored only as SHA-256 hash), the `OAuthTokenClient`
  trait with a reqwest implementation shared by code exchange and refresh,
  the `OAuthFlowService` (start/callback/status with atomic one-time-use
  consume semantics), and broker refresh: single-flight per grant via the
  `GrantRefreshLock` trait (Postgres transaction-scoped advisory lock in
  `store-pg`, in-memory locks for tests), 60s expiry margin, refresh-token
  rotation persisted under new secret ids before the grant row swap,
  `invalid_grant` -> `NeedsReauth`, and fallback to a still-valid stored
  token on transient refresh failures. `store-pg` migration 004 adds
  `auth_clients`, `auth_flows`, and `auth_grants.oauth_client_id`. The
  gateway hosts `GET /auth/callback` (public base URL via
  `FORGE_PUBLIC_BASE_URL`, default `http://{bind}`) plus
  `auth/clients/create|list|read|delete` and `auth/flows/start|status`;
  the CLI gained `forge auth client add|list|read|remove` and
  `forge auth login <client>` with status polling.
- G4 implemented on 2026-06-10: `auth-registry::mcp_oauth` driver with PRM
  (RFC 9728) and AS metadata (RFC 8414/OIDC) discovery, CIMD + DCR + manual
  client identification, lazy `mcp:<server_id>` client upsert triggered by
  `auth/flows/start` (`forge auth login mcp:<server>`), and the
  gateway-served CIMD document at `/auth/client-metadata.json`.
- G5 mint slice implemented on 2026-06-10: generic `auth_providers` table
  (typed `AuthProviderConfig` enum over `config_json`, credential secret FK
  into `auth_secrets`), `auth_grants.metadata_json`, GitHub App driver
  (app JWT signing, on-demand installation token minting with in-process
  caching, live installation listing/verification),
  `auth/providers/*` + `auth/github/installations/*` API, and
  `forge auth github` CLI. Token leases remain deferred until a VM/sandbox
  consumer exists.
- G1 deliberately deferred `AuthProviderRecord`, `AuthFlowRecord`, and token
  leases; G2 landed `AuthFlowRecord` and OAuth client records; G5 landed
  provider records as the generic `auth_providers` table. Static bearer and
  OAuth grants still carry `provider_kind` + `provider_id` inline without a
  provider row.
- Split out of the original P68 remote MCP registry/auth plan.
- Provides generic auth and credential infrastructure for MCP, GitHub, future
  hosted tools, VMs, sandboxes, and provider runtimes.
- Does not implement MCP server catalog/session linking; P68 owns that.
- Does not implement provider MCP request lowering; P67 owns that.

## Goal

Add a universe-scoped auth substrate that can securely store credentials,
complete provider-specific auth flows, refresh grants, and issue short-lived
runtime tokens to consumers such as:

- P68 remote MCP session links;
- P67 provider MCP request lowering;
- future GitHub repository tools;
- VMs and sandboxes that need temporary repository/API access;
- future hosted connectors.

The core product rule is: durable secrets stay in Forge's encrypted store, and
runtimes receive only the shortest-lived token or lease needed for the current
operation.

## Design Position

Build a generic auth substrate, not an MCP-only OAuth subsystem.

The shared layer owns durable state and security boundaries:

- encrypted secret records;
- provider/client registration records;
- grants and grant status;
- short-lived auth flows;
- refresh and revocation state;
- runtime token broker and token leases;
- redaction and audit semantics.

Provider drivers own protocol details:

- MCP OAuth protected resource metadata, resource indicators, and dynamic client
  registration;
- GitHub App JWT signing, installation token minting, and user tokens;
- GitHub OAuth App scopes and device/web flows;
- future provider quirks.

Do not pretend every provider is the same OAuth shape. The generic layer should
provide records, storage, lifecycle, and token-broker interfaces; provider
drivers should own request details.

## Runtime Placement

The broker is library code, not a network service. The gateway and the Temporal
worker instantiate it in-process over the universe-bound store plus key config,
following the existing `PgStore` pattern.

Placement rules:

- token resolution happens only inside activity execution (worker-side) or
  inside gateway request handling; never in workflow code, which is
  deterministic and replayed;
- resolved tokens must never appear in anything serialized into Temporal event
  history: workflow state, activity inputs, activity results, or heartbeat
  payloads. The LLM activity input stays the sanitized planned request
  (`engine::LlmGenerationRequest` with `SecretRef`s); the activity body
  resolves refs immediately before the provider call;
- `llm-runtime` owns a narrow `SecretResolver` trait and stays free of auth
  and store dependencies. `temporal-server` adapts the broker to that trait
  and dispatches on `SecretRef.namespace` (`auth_grant` -> broker, `env` ->
  env resolver for development).

## Reference Points

MCP auth:

- https://modelcontextprotocol.io/specification/2025-06-18/basic/authorization
- https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization
- https://modelcontextprotocol.io/extensions/auth/oauth-client-credentials

GitHub auth:

- https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps
- https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-user-access-token-for-a-github-app
- https://docs.github.com/en/authentication/connecting-to-github-with-ssh/managing-deploy-keys#github-app-installation-access-tokens
- https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/differences-between-github-apps-and-oauth-apps

## Core Concepts

Suggested principal and auth references:

```rust
pub struct PrincipalRef {
    pub kind: PrincipalKind,
    pub id: String,
}

pub enum PrincipalKind {
    User,
    ServiceAccount,
    UniverseDefault,
}
```

Forge has no user identity system yet: the gateway is unauthenticated and
universe-bound. Until identity exists, flows and grants default to
`PrincipalKind::UniverseDefault` (or a CLI-supplied id), and principal-policy
enforcement in P68 linking is deferred. The record shape is kept now so adding
identity later is a data migration, not a schema redesign.

```rust

pub struct AuthProviderRef {
    pub universe_id: Uuid,
    pub provider_id: String,
}

pub struct AuthGrantRef {
    pub universe_id: Uuid,
    pub grant_id: String,
}

pub struct AuthCredentialRef {
    pub namespace: String,
    pub id: String,
}
```

`AuthCredentialRef` can be lowered into engine `SecretRef` when deterministic
session state needs to point at runtime auth:

```rust
SecretRef {
    namespace: "auth_grant".to_owned(),
    id: "authgrant_123".to_owned(),
}
```

`engine` never resolves that reference. Runtime adapters and gateways ask P69's
token broker for a current token.

## Provider Kinds

First provider kinds:

```rust
pub enum AuthProviderKind {
    StaticBearer,
    McpOAuth,
    GitHubApp,
    GitHubAppUser,
    GitHubOAuthApp,
    CustomOAuth,
}
```

`McpOAuth` supports remote MCP servers that require OAuth bearer tokens.

`GitHubApp` is the preferred repository automation path. Forge stores the app
private key as a secret, records installation ids, and mints short-lived
installation access tokens on demand.

`GitHubAppUser` is for actions that need user attribution or the intersection of
app permissions and user access.

`GitHubOAuthApp` is a fallback for traditional OAuth scopes such as `repo`, but
should not be the default for repository automation because it is broader and
less precise than GitHub App installation access.

`StaticBearer` supports imported tokens and MCP static bearer credentials.

## Data Model

### Secret Store

Add a generic encrypted secret store instead of provider-specific token columns.

Candidate table shape:

```sql
CREATE TABLE auth_secrets (
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
- use secret wrapper types in memory so accidental `Debug` output does not leak
  token values;
- rotate by writing a new encrypted value under the same logical `secret_id` or
  by adding versioned secret ids if audit requirements demand it later.

Initial secret kinds:

- `auth.static_bearer`;
- `auth.oauth.access_token`;
- `auth.oauth.refresh_token`;
- `auth.oauth.client_secret`;
- `auth.oauth.pkce_verifier`;
- `auth.oauth.registration_access_token`;
- `auth.github_app.private_key`;
- `auth.github_app.webhook_secret`;
- `auth.token_lease.bearer`.

### Providers

```rust
pub struct AuthProviderRecord {
    pub universe_id: Uuid,
    pub provider_id: String,
    pub provider_kind: AuthProviderKind,
    pub display_name: Option<String>,
    pub config_json: serde_json::Value,
    pub status: AuthProviderStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

pub enum AuthProviderStatus {
    Active,
    NeedsConfiguration,
    Disabled,
}
```

Provider-specific config stays non-secret. Secret material is referenced by
`SecretRef`/`AuthCredentialRef`.

### OAuth Clients

```rust
pub struct OAuthClientRecord {
    pub universe_id: Uuid,
    pub provider_id: String,
    pub authorization_server: String,
    pub client_id: String,
    pub client_secret_ref: Option<AuthCredentialRef>,
    pub registration_access_token_ref: Option<AuthCredentialRef>,
    pub registration_client_uri: Option<String>,
    pub metadata_json: serde_json::Value,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}
```

### Grants

```rust
pub struct AuthGrantRecord {
    pub universe_id: Uuid,
    pub grant_id: String,
    pub provider_id: String,
    pub provider_kind: AuthProviderKind,
    pub principal: PrincipalRef,
    pub subject_hint: Option<String>,
    pub scopes: Vec<String>,
    pub permissions_json: serde_json::Value,
    pub audience: Option<String>,
    pub access_token_ref: Option<AuthCredentialRef>,
    pub refresh_token_ref: Option<AuthCredentialRef>,
    pub expires_at_ms: Option<i64>,
    pub status: AuthGrantStatus,
    pub metadata_json: serde_json::Value,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

pub enum AuthGrantStatus {
    Active,
    NeedsReauth,
    Revoked,
    Failed,
}
```

`audience` is a normalized resource identifier, not free-form JSON, because
audience enforcement is load-bearing: for MCP it is the canonical server
resource URL the token was minted for (RFC 8707 resource), and the broker
refuses to hand a token to a non-covered audience. Provider-specific extras can
live in `metadata_json`.

Stored status covers only durable facts. "Needs refresh" is derivable from
`expires_at_ms` plus refresh-token presence and is computed by the broker, not
stored, so it cannot go stale.

For GitHub App installation access, `AuthGrantRecord` may represent the
installation rather than a stored bearer token. The access token is minted on
demand and may be stored only as a short-lived lease if it is handed to a VM or
sandbox.

### Auth Flows

```rust
pub struct AuthFlowRecord {
    pub universe_id: Uuid,
    pub flow_id: String,
    pub provider_id: String,
    pub provider_kind: AuthProviderKind,
    pub principal: PrincipalRef,
    pub state_hash: String,
    pub pkce_verifier_ref: Option<AuthCredentialRef>,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub audience_json: serde_json::Value,
    pub expires_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub metadata_json: serde_json::Value,
}
```

### Token Leases

Token leases are for handing short-lived credentials to external runtimes such
as VMs, sandboxes, or tool workers.

```rust
pub struct AuthTokenLeaseRecord {
    pub universe_id: Uuid,
    pub lease_id: String,
    pub grant_id: String,
    pub issued_to: TokenLeaseSubject,
    pub audience_json: serde_json::Value,
    pub token_ref: AuthCredentialRef,
    pub expires_at_ms: i64,
    pub revoked_at_ms: Option<i64>,
    pub created_at_ms: i64,
}

pub enum TokenLeaseSubject {
    LlmRuntime,
    ToolWorker,
    Vm { vm_id: String },
    Sandbox { sandbox_id: String },
}
```

Provider request lowering can ask for an in-memory resolved token and skip lease
storage. External runtimes should get leases for audit and expiry tracking.

## Token Broker

Runtime consumers use a generic token broker, not direct database reads:

```rust
#[async_trait]
pub trait AuthTokenBroker: Send + Sync {
    async fn bearer_token(
        &self,
        grant_id: &AuthGrantId,
        audience: &TokenAudience,
    ) -> Result<ResolvedSecret, AuthError>;

    async fn lease_bearer_token(
        &self,
        grant_id: &AuthGrantId,
        audience: &TokenAudience,
        issued_to: TokenLeaseSubject,
        ttl: TokenLeaseTtl,
    ) -> Result<AuthTokenLease, AuthError>;
}
```

`bearer_token` does not return `Option`: a named grant either resolves or fails
with a typed error (`GrantNotFound`, `GrantRevoked`, `NeedsReauth`,
`AudienceMismatch`, `SecretMissing`, `Store`, ...). Optional-auth-absent is a
link-policy concern that P68 expresses by omitting `auth_ref` entirely, never
by silent `None` from the broker. Tests assert error kinds, not message
strings.

`TokenAudience` is the enforcement point for "tokens must not cross
incompatible audiences" and is a typed value, not a bare string:

```rust
pub enum TokenAudience {
    McpResource(String),
    // GitHubApi { ... } and others arrive with their drivers.
}
```

A grant with `audience: None` is unrestricted (static bearer imports may omit
it); a grant with `audience: Some(aud)` only resolves for resources covered by
`aud` (exact match or path-prefix on the same origin). The universe is bound by
construction: the broker is instantiated over a universe-scoped store, the same
way `PgStore` binds `universe_id` today, so `AuthGrantId` does not carry a
universe.

The broker may:

- return an existing non-expired access token;
- refresh an OAuth access token;
- mint a GitHub App installation token;
- create a static bearer lease;
- mark a grant `NeedsReauth`;
- fail before provider/tool I/O when no valid token can be resolved.

Refresh must be single-flight per grant. Concurrent activities will hit the
broker for the same grant, and with refresh-token rotation a double refresh is
destructive: several authorization servers treat refresh-token reuse as theft
and revoke the whole grant chain. Serialize the refresh itself (Postgres
`SELECT ... FOR UPDATE` or an advisory lock keyed by grant), refresh with an
expiry margin (for example 60s before `expires_at_ms`), and only then update
the stored secrets atomically.

P67 and P68 should depend on this boundary, not on OAuth tables directly.

## OAuth Flow

P69 should implement authorization-code + PKCE first, plus device flow where a
provider supports it and CLI usage benefits.

Generic flow:

1. Register provider/client config.
2. Start auth flow for a principal and audience.
3. Store state and PKCE verifier as encrypted/sealed data.
4. Open or print an authorization URL.
5. Complete callback or device flow.
6. Exchange code for tokens using provider driver.
7. Store encrypted tokens and grant metadata.
8. Refresh or require reauth as needed.

OAuth helper crates are appropriate for protocol mechanics, but all persisted
state and redaction rules remain Forge-owned.

Callback topology: the default is a gateway-hosted callback endpoint
(`/auth/callback` on the existing HTTP server) with the CLI polling
`auth/flows/status`. This works identically for local development and hosted
deployments and is the redirect URI that dynamic client registration or client
metadata documents advertise. It requires one new deployment config value: the
gateway's public base URL. Loopback-redirect and device-authorization flows are
later additions for environments where the gateway is not reachable from the
user's browser.

Flows are one-time-use: completing a flow consumes it, a second callback with
the same state must fail, and expired flows must not be completable.

## GitHub Design

Use GitHub Apps as the default for repository automation.

### GitHub App Installation Access

Store:

- GitHub App id/client id;
- app private key secret ref;
- installation id;
- selected account/repository metadata;
- requested app permissions;
- optional enterprise/base URL config.

At runtime:

1. Sign a GitHub App JWT using the private key.
2. Exchange it for an installation access token.
3. Return an in-memory token to an API/tool call or issue a short-lived lease to
   a VM/sandbox.

Do not store installation tokens durably unless needed for a lease. They are
short-lived and can be minted on demand.

### GitHub App User Access

Use when actions need user attribution. These tokens use OAuth-style web/device
flows but are limited by both the app permissions and user access. Store access
and refresh tokens only through the generic grant/secret store.

### GitHub OAuth App

Support as a fallback for user-scoped flows. Treat broad scopes such as `repo`
as higher risk than GitHub App installation permissions.

### PATs And Deploy Keys

Support import only as explicit static credentials if needed. They should not be
the default product path.

## API Surface

Candidate JSON-RPC methods:

```text
auth/providers/list
auth/providers/create
auth/providers/read
auth/providers/update
auth/providers/delete

auth/flows/start
auth/flows/complete
auth/flows/status

auth/grants/import
auth/grants/list
auth/grants/read
auth/grants/revoke

auth/token/lease
auth/token/revoke_lease
```

Implemented so far (G2): `auth/clients/create|list|read|delete` covers the
manual-configuration slice of `auth/providers/*` (full provider records stay
deferred), and `auth/flows/start` + `auth/flows/status` are live. There is no
`auth/flows/complete` RPC: authorization-code flows complete via the
gateway-hosted HTTP callback `GET /auth/callback`; a complete RPC appears with
device flow if needed.

`auth/grants/import` and the `client_secret` param of `auth/clients/create`
are the two deliberate inbound-plaintext paths: values are encrypted on
receipt and never returned by any method. Their params are concrete redaction
surfaces — any gateway request logging or error reporting must never echo
them. No other method accepts or returns secret values.

Internal runtime APIs may expose token resolution, but public APIs should not
return plaintext token values except when explicitly issuing a short-lived lease
to a trusted runtime boundary.

## CLI Surface

Candidate commands:

```bash
forge auth provider list
forge auth provider add static-bearer --id mcp-crm
forge auth login mcp:crm --scope contacts.read
forge auth status authgrant_123
forge auth revoke authgrant_123

forge auth github app add --id forge-github
forge auth github app installation list
forge auth github app installation grant --installation-id 12345
forge auth github login
```

MCP-specific convenience wrappers may live in P68 later, but generic auth should
be usable without going through MCP commands.

## Crate And Module Shape

Suggested first-cut changes:

```text
crates/auth-registry/
  grant/secret records, statuses, validation
  SecretStore and AuthGrantStore traits
  AuthTokenBroker trait, TokenAudience, typed AuthError kinds
  generic broker implementation over the store traits
  in-memory adapters for tests

crates/store-pg/src/auth.rs (+ migration)
  auth_secrets and auth_grants tables
  AEAD encryption with configured master key (KMS envelope later)
  PgStore impls of the auth-registry store traits

crates/api/src/lib.rs
  public auth grant DTOs and methods
  secret refs and statuses, never durable plaintext tokens

crates/temporal-server/src/gateway + worker
  auth/grants JSON-RPC handlers
  broker -> llm-runtime SecretResolver adapter in the worker
  later: callback handling, refresh, lease orchestration

crates/cli/src/auth_cli.rs
  auth grant import/list/revoke commands; login/status arrive with OAuth

crates/llm-runtime/src/secrets.rs
  SecretResolver trait owned by llm-runtime, no auth/store dependencies
```

The narrow shared crate exists from the start (`auth-registry`, mirroring the
`mcp-registry` precedent) rather than waiting for shared code to grow.
Dependency direction is deliberate: `llm-runtime` defines its own resolver
boundary and never depends on `auth-registry` or `store-pg`; `temporal-server`
adapts the broker to that boundary. Keep provider drivers behind traits so
`oauth2`, GitHub client helpers, or MCP SDK helpers can be swapped without
schema churn.

## Security And Policy

Minimum rules:

- all OAuth authorization and token endpoints must use HTTPS except loopback
  redirect URIs;
- authorization-code flows must use PKCE when supported or required;
- token requests must include provider-specific audience/resource indicators
  where required;
- access tokens must not be reused across incompatible audiences;
- grants, secrets, leases, sessions, and runtime consumers must belong to the
  same universe unless an explicit cross-universe sharing policy exists;
- deleting or revoking a grant must make future token resolution fail clearly;
- leases must expire and be revocable;
- raw provider/tool/runtime logs must be redacted;
- resolved tokens must never enter Temporal event history (workflow state,
  activity inputs/results, heartbeats), engine events, CAS blobs, persisted
  provider request blobs, or API responses;
- in-memory token values use wrapper types that redact `Debug`/`Display`
  output (`secrecy`/`zeroize`-style; a minimal Forge-owned wrapper is fine
  first);
- provider errors indicating invalid, expired, or insufficient scopes should
  update grant status when observable.

Two boundaries deserve explicit naming:

- **Token egress to model providers.** In direct remote MCP (P67), the
  resolved token is injected into the provider request and handed to
  OpenAI/Anthropic, who connect to the MCP server on Forge's behalf. The
  user's grant transits a third party. This is inherent to provider-hosted
  MCP; policy may later add a per-grant or per-link consent bit ("allowed to
  be sent to model providers").
- **Lease revocation is honest, not magical.** Revoking a lease prevents
  re-issue and re-read; it cannot invalidate a bearer token already held in a
  VM's memory unless the upstream supports revocation (GitHub installation
  tokens do not). Short TTLs are the real mitigation.

Longer-term policy can add:

- admin approval for new providers, scopes, permissions, and GitHub
  installations;
- per-session and per-run grant allowlists;
- sandbox/VM egress policy tied to token audience;
- external KMS integration and secret version audit history.

## G1: Secret Store And Static Bearer

Implement encrypted secret storage and static bearer grants as one vertical
slice: `auth-registry` crate, Postgres-backed encrypted storage,
`auth/grants/import|list|read|revoke`, broker resolution, `llm-runtime`
`SecretResolver` with OpenAI Responses `authorization` injection and redacted
persisted requests, and P68 link-time grant validation. Provider records, auth
flows, and token leases are deferred to G2+.

Acceptance criteria:

- [x] secrets are encrypted before insertion into Postgres;
- [x] API/CLI never returns durable plaintext secret values;
- [x] static bearer credentials can be stored as generic grants via
  `auth/grants/import`;
- [x] `AuthTokenBroker` can resolve static bearer grants for runtime consumers,
  enforcing status and audience with typed error kinds;
- [x] P68 `session/mcp/link` validates grant existence, status, policy
  compatibility, and audience before committing session state;
- [x] OpenAI Responses materialization injects `authorization` for resolved
  `auth_ref` and the persisted provider request blob redacts it;
- [x] provider request/runtime logs redact injected auth (redacted persisted
  blobs; `Debug`-redacted wrappers for values and import params; gateway does
  not log request params).

## G2: Generic OAuth Authorization Code With PKCE

Implement generic OAuth client records, auth flow start/complete, and encrypted
token storage.

Acceptance criteria:

- [x] provider drivers can supply authorization/token endpoint metadata
  (manually configured `OAuthClientRecord`s; discovery arrives with G4);
- [x] CLI/API can start an authorization flow (`auth/flows/start`,
  `forge auth login`);
- [x] callback completes the flow and stores encrypted token material
  (gateway `GET /auth/callback`; flows are one-time-use, a second callback
  with the same state fails, expired flows cannot complete);
- [x] grants expose status, scopes, expiry, and subject hints without
  plaintext tokens;
- [x] refresh tokens are optional and stored only when issued.

Notes beyond the original sketch: the OAuth `state` is never persisted (only
its SHA-256 hash, which is also the callback lookup key); the PKCE verifier is
an encrypted secret deleted after use; `auth/clients/create` is the second
deliberate inbound-plaintext path (client secret, `Debug`-redacted params);
device flow and loopback redirects remain future work.

## G3: Refresh And Runtime Token Broker

Add refresh handling and runtime token resolution.

Acceptance criteria:

- [x] `AuthTokenBroker` returns current access tokens to runtime consumers;
- [x] expiring OAuth tokens refresh before provider calls when a refresh token
  exists (60s margin; transient refresh failures fall back to the stored
  token while it is still valid);
- [x] refresh-token rotation updates encrypted stored secrets atomically (new
  secret ids written first, single-row grant pointer swap, old secrets
  deleted best-effort; refresh is single-flight per grant via a Postgres
  advisory lock);
- [x] failed refresh marks the grant `NeedsReauth` (`invalid_grant` only;
  network errors do not poison the grant);
- [x] provider calls fail clearly before I/O when no valid token can be
  resolved (typed `GrantExpired`/`RefreshFailed`/`GrantNotActive` kinds).

## G4: MCP OAuth Driver

Add the MCP-specific OAuth driver consumed by P68 links.

Driver notes:

- protected resource metadata (RFC 9728) lists `authorization_servers` as an
  array; the driver must select among multiple, and P68's single
  `authorization_server` field becomes a list when its G3 discovery lands;
- the 2025-11-25 MCP auth revision adds Client ID Metadata Documents as the
  preferred alternative to dynamic client registration. For a hosted product
  CIMD is the easier path (one static client-metadata JSON at a stable public
  URL, no per-AS registration state); the driver should support CIMD alongside
  DCR and manual client config;
- `McpOAuth` provider records use the `mcp:<server_id>` provider-id convention
  and are upserted lazily by `forge auth login mcp:<server_id>` from the P68
  catalog record plus discovered metadata; P68 registration does not create
  auth providers.

Grant compatibility, as validated by P68 linking (the gateway is universe-bound
on both stores, so universe equality holds by construction):

- provider-kind class matches the server auth policy: `StaticBearer` for
  bearer policies, `McpOAuth` for OAuth policies;
- the grant audience covers the server's canonical resource;
- grant status is `Active`.

Acceptance criteria:

- [x] protected resource metadata and authorization server metadata are
  discovered (RFC 9728 path-inserted PRM URL with root fallback and an
  explicit catalog URL override; RFC 8414 OAuth + OIDC metadata forms;
  `resource` and `issuer` echoes are verified; PKCE `S256` support is
  required when the AS advertises `code_challenge_methods_supported`);
- [x] MCP resource/audience binding (RFC 8707) is included in authorization
  and token requests where required (since G2; the grant audience is the
  PRM's canonical `resource`);
- [x] client identification supports CIMD, dynamic client registration, and
  manual configuration (CIMD when the AS advertises
  `client_id_metadata_document_supported` and the gateway has a public
  https base URL — the gateway serves the document at
  `/auth/client-metadata.json`; DCR requests a public PKCE client and
  encrypts any issued secret; existing `mcp:<server_id>` records are reused
  without network traffic, so `forge auth client add --id mcp:<server_id>`
  is the manual path);
- [x] P68 can validate and link MCP-compatible grants by auth handle (since
  G1; OAuth-minted grants carry kind `mcp_oauth`, provider id
  `mcp:<server_id>`, and the canonical resource as audience).

Implemented 2026-06-10: `auth-registry::mcp_oauth` (`McpOAuthDriver`,
`McpOAuthTarget`, `OAuthMetadataClient` trait + reqwest impl, typed
`McpOAuthError` kinds), gateway lazy upsert in `auth/flows/start` when the
client id is `mcp:<server_id>` (so `forge auth login mcp:<server>` needs no
new CLI surface), and the CIMD document route. Deferred: persisting DCR
`registration_access_token`/`registration_client_uri` (registration
management is not implemented, so re-registration is the recovery path) and
P68's `authorization_server` field becoming a list (tracked for P68 G3; the
driver already selects among the PRM's `authorization_servers` array, using
the catalog's single field as a hint).

## G5: GitHub App Driver

Add GitHub App installation access support.

Acceptance criteria:

- [x] GitHub App private key is stored in the secret store
  (`auth.github_app.private_key`; the PEM is validated at registration, and
  `auth_providers.credential_secret_id` carries a foreign key into
  `auth_secrets` with ON DELETE RESTRICT);
- [x] installation records can be represented as grants (kind `github_app`,
  no stored tokens, audience = the API base URL, installation facts in the
  new `auth_grants.metadata_json`);
- [x] installation access tokens are minted on demand (broker mint path:
  RS256 app JWT -> `POST /app/installations/{id}/access_tokens`, process-local
  cache with the expiry margin behind the per-grant lock, nothing persisted;
  401 -> grant `Failed`, 404 -> `NeedsReauth`, transient errors do not poison
  the grant);
- [x] repository permissions and installation repository selection are
  visible as non-secret grant metadata (captured live at
  `auth/github/installations/grant` time and shown on grant views);
- [ ] token leases can be issued to VMs/sandboxes for Git-over-HTTPS or API
  access — DEFERRED until a VM/sandbox consumer exists; the lease layer is a
  thin wrapper over the mint path once the consuming boundary is concrete.

Implemented 2026-06-10 (mint slice): generic `auth_providers` table in
migration 004 (one table for all provider kinds — typed `AuthProviderConfig`
enum decoded from `config_json` at the store boundary, GitHub Apps first),
`auth-registry::github` (JWT signing via `jsonwebtoken`, `GitHubApiClient`
trait + reqwest impl, typed `GitHubAppError` kinds, RFC 3339 parsing),
`TokenAudience::GitHubApi`, broker `GitHubAppRuntime`,
`auth/providers/create|list|read|delete` +
`auth/github/installations/list|grant` JSON-RPC (the private key is the third
deliberate inbound-plaintext path), and `forge auth github app
add|list|read|remove` + `forge auth github installation list|grant` CLI.
Installation listing/verification calls GitHub live with the app JWT, so
grants are created against verified installations. No Forge consumer spends
GitHub tokens yet (no repo tools); the broker path is unit-tested against a
mocked GitHub API.

Refactored 2026-06-11: provider-kind dispatch moved behind the
`GrantTokenSource` trait. `RegistryTokenBroker` now owns only grant loading,
status/audience enforcement, and the per-grant single-flight lock around a
generic fast-path/renew skeleton; stored/OAuth-refreshable tokens are served
by a built-in source, and `GitHubAppRuntime` lives in
`auth-registry::github` as a registered source (`with_token_source`), so
`broker.rs` carries no GitHub imports and the next provider kind
(`github_app_user`) is a new source impl, not a new broker branch.

## Future Work

- GitHub App user access token flow.
- GitHub OAuth App fallback flow.
- OAuth client credentials extension for non-human automation.
- Device authorization grant where useful for headless CLI environments.
- Hosted UI for managing providers, grants, scopes, permissions, leases, and
  revocation.
- External KMS integration and secret version audit history.
