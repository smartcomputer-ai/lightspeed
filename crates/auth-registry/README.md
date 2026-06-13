# auth-registry

Generic auth grant, secret, and token-broker substrate for Lightspeed (P69).

The crate defines the provider-independent records and traits; persistence and
encryption live in store adapters (`store-pg` stores secrets AES-256-GCM
encrypted at rest), and resolution happens only inside runtime activity
execution — never in the engine or the session log.

## What it provides

- `grants` — `AuthGrantRecord` / `CreateAuthGrantRecord` and the
  `AuthGrantStore` trait: a grant binds a provider kind (`static_bearer`,
  `mcp_oauth`, GitHub kinds, `custom_oauth`), a principal, a status lifecycle
  (`active`, `needs_reauth`, `revoked`, `failed`), an optional `audience`
  (the resource URL the credential may be sent to), and references to stored
  secrets — never secret values.
- `secrets` — `SecretValue` (debug-redacted, not serializable),
  `PutSecretRecord`, and the `SecretStore` trait for opaque secret payloads.
- `broker` — the `AuthTokenBroker` trait and `RegistryTokenBroker`: resolves
  a grant id plus a `TokenAudience` to a bearer `SecretValue`, enforcing
  status, expiry, and `audience_covers` (exact match or path-boundary prefix),
  with typed `AuthBrokerError` kinds instead of `Option`. The broker owns
  grant loading, enforcement, and single-flight renewal per grant
  (`GrantRefreshLock`; a Postgres advisory lock in `store-pg`); how a token
  is obtained per provider kind sits behind the `GrantTokenSource` trait —
  stored/OAuth-refreshable tokens are built in, on-demand minters (the
  GitHub App runtime) register via `with_token_source`. OAuth grants refresh
  automatically: a 60s expiry margin, rotation-safe secret swaps,
  `invalid_grant` → `needs_reauth`, and fallback to the stored token when a
  refresh fails transiently but the token is still valid.
- `oauth` — `OAuthClientRecord` (manually configured authorization/token
  endpoints + AS-issued client id), `AuthFlowRecord` (one-time
  authorization-code flows storing only the SHA-256 of `state`), PKCE S256
  helpers, and the `OAuthTokenClient` trait with a reqwest implementation.
- `flow` — `OAuthFlowService`: start builds the authorization URL and
  persists the encrypted PKCE verifier; the callback atomically consumes the
  flow, exchanges the code, stores encrypted tokens, and mints the grant.
- `mcp_oauth` — the MCP OAuth driver (P69 G4): discovers protected resource
  metadata (RFC 9728, path-inserted URL with root fallback) and
  authorization server metadata (RFC 8414/OIDC), requires PKCE S256,
  identifies the client via CIMD (when the AS supports client-id metadata
  documents and the deployment has a public https URL) or dynamic client
  registration (RFC 7591), and lazily upserts the result as an
  `mcp:<server_id>` client record. Existing records are reused without
  network traffic; manual `lightspeed auth client add --id mcp:<server_id>` always
  wins.
- `providers` — the generic `AuthProviderRecord`: one record shape for every
  provider kind, with non-secret config decoded into the typed
  `AuthProviderConfig` enum (GitHub Apps first) and the credential reference
  as a typed field (`store-pg` backs it with a foreign key into
  `auth_secrets`).
- `github` — the GitHub App driver (P69 G5): RS256 app JWT signing,
  installation listing, and the `GitHubAppRuntime` token source (on-demand
  installation token minting via the `GitHubApiClient` trait). Installation
  grants store no tokens; minting happens per call with a process-local
  cache, and `401`/`404` from GitHub mark the grant `failed`/`needs_reauth`
  respectively.
- `memory` — in-memory grant/secret/client/flow/provider stores for tests.

## How it works

Sessions and the engine only ever record `SecretRef { namespace:
"auth_grant", id }`. At LLM-call time the runtime's `SecretResolver` asks the
broker for the token for a specific resource URL; the token is injected into
the outgoing provider request at the last moment, while the persisted request
blob keeps `"authorization": "<redacted>"`. Plaintext tokens never enter
engine events, CAS blobs, Temporal history, API responses, or logs. The
three deliberate inbound-plaintext paths are `auth/grants/import` (bearer
token), `auth/clients/create` (client secret), and `auth/providers/create`
(GitHub App private key); all encrypt on receipt and redact `Debug` output.

## Build & test

```bash
cargo test -p auth-registry
# encrypted store impl (needs local infra):
cargo test -p store-pg --test store_pg_live -- --ignored
```

## Testing auth grants end to end

Uses the public authenticated test server at
<https://mcpplaygroundonline.com/mcp-auth-server>, which issues bearer tokens
for testing.

```bash
# 0. once: infra + env, gateway in its own terminal
local/up.sh && source local/env.sh
cargo run -p temporal-server        

# separate terminal, env.sh sourced
# 1. register the authenticated MCP server
source local/env.sh
cargo run -q -p cli -- mcp server add https://mcpplaygroundonline.com/mcp-auth-server \
  --id playground --label playground --auth-policy required-bearer

# 2. import the bearer token as an encrypted grant
export PLAYGROUND_TOKEN=<token from mcpplaygroundonline.com>
cargo run -q -p cli -- auth grant import --id playground \
  --token-env PLAYGROUND_TOKEN \
  --audience https://mcpplaygroundonline.com/mcp-auth-server

# 3. create the session (one-shot message creates it and exits)
cargo run -q -p cli -- chat --session mcp_test "hello"

# 4. link the server into the session with the grant
cargo run -q -p cli -- mcp link --session mcp_test --auth-grant-id playground playground

# 5. open the TUI on that session and ask it to use the tools
cargo run -q -p cli -- chat --session mcp_test
```

Steps 3 and 5 can also collapse: open the TUI directly with `--session
mcp_test` (which creates the session), run the link from a second terminal
while the TUI sits idle at the prompt, and the next message will see the MCP
tools — linking patches the live session's tool set.

Verify redaction afterwards: the `cas_blobs` request blobs must contain
`"authorization": "<redacted>"` and never the token, and `auth_secrets`
holds only ciphertext.

## Testing an OAuth login end to end

Works against any standard authorization server with a manually configured
client (a GitHub OAuth app is the cheapest real one; set its callback URL to
`http://127.0.0.1:18080/auth/callback` for local dev). For OAuth-protected
MCP servers, prefer `lightspeed auth login mcp:<server>` below — it discovers and
registers the client automatically where the AS allows it.

```bash
# 0. infra + env + gateway as above (env.sh sourced everywhere)

# 1. register the OAuth client (secret read from env, encrypted at rest)
export MY_OAUTH_CLIENT_SECRET=<client secret>
cargo run -q -p cli -- auth client add --id github \
  --kind github-oauth-app \
  --authorization-endpoint https://github.com/login/oauth/authorize \
  --token-endpoint https://github.com/login/oauth/access_token \
  --client-id <AS-issued client id> \
  --client-secret-env MY_OAUTH_CLIENT_SECRET \
  --scope read:user

# 2. run the flow: prints the authorization URL, then polls until the
#    browser hits the gateway callback and the grant is stored
cargo run -q -p cli -- auth login github
# -> open the URL, approve, see "login complete" + the grant id

# 3. inspect the grant (no token values, only hasAccessToken/hasRefreshToken)
cargo run -q -p cli -- auth grant list
```

## Testing an MCP OAuth login end to end

For a catalogued MCP server with an OAuth auth policy, no manual client setup
is needed — the gateway discovers and registers the client on first login:

```bash
# 1. register the server with an OAuth policy (resource = canonical server URL)
cargo run -q -p cli -- mcp server add https://crm.example.com/mcp \
  --id crm --label crm --auth-policy required-oauth \
  --oauth-resource https://crm.example.com/mcp

# 2. login by server id: discovers PRM + AS metadata, registers a client
#    (CIMD if supported and the gateway is public https, else DCR),
#    then runs the normal browser flow
cargo run -q -p cli -- auth login mcp:crm

# 3. the discovered client and the audience-bound grant are inspectable
cargo run -q -p cli -- auth client read mcp:crm
cargo run -q -p cli -- auth grant list

# 4. link with the grant id printed by login
cargo run -q -p cli -- mcp link --session s1 --auth-grant-id <grant id> crm
```

To force re-discovery (for example after the server changes authorization
servers), remove the client: `lightspeed auth client remove mcp:crm`. If the AS
supports neither CIMD nor dynamic registration, login fails with instructions
to register manually — see the verified walkthrough below.

## Walkthrough: GitHub Copilot MCP (verified end to end)

The GitHub Copilot MCP server (`https://api.githubcopilot.com/mcp/`) is a
real OAuth-protected server whose authorization server (`github.com`)
supports **neither dynamic client registration nor CIMD**, so it exercises
the manual-client fallback: discovery finds the AS, then login tells you to
register a client by hand. A manually registered `mcp:<server_id>` client
always wins — login reuses it without touching the catalog or the network.

One-time GitHub setup: create a GitHub App (or OAuth app) with the
authorization callback URL set to `http://127.0.0.1:18080/auth/callback`.
GitHub App client ids look like `Iv23...`; their tokens are scoped by the
app's permissions (OAuth scopes are ignored), and with "user token
expiration" enabled they come with refresh tokens, which exercises the
broker's automatic refresh path.

```bash
# 0. infra + schema, env in every terminal; gateway in its own terminal
local/up.sh && source local/env.sh
local/pg-migrate.sh
cargo run -p temporal-server        # separate terminal, env.sh sourced

# 1. register the MCP server under id gh
cargo run -q -p cli -- mcp server add https://api.githubcopilot.com/mcp/ \
  --id gh --label github --auth-policy required-oauth

# 2. register the GitHub app as the manual OAuth client for it.
#    The id mcp:gh is what `auth login mcp:gh` looks for.
export GH_OAUTH_SECRET=<client secret from GitHub>
cargo run -q -p cli -- auth client add --id mcp:gh \
  --kind mcp-oauth \
  --audience https://api.githubcopilot.com/mcp/ \
  --authorization-endpoint https://github.com/login/oauth/authorize \
  --token-endpoint https://github.com/login/oauth/access_token \
  --client-id <GitHub app client id> \
  --client-secret-env GH_OAUTH_SECRET

# 3. log in: prints the GitHub consent URL, polls until the callback lands
cargo run -q -p cli -- auth login mcp:gh
# -> "login complete" + grantId authgrant_...

# 4. create a session, link the server with the grant, open the TUI
cargo run -q -p cli -- chat --session gh_test "hello"
cargo run -q -p cli -- mcp link --session gh_test --auth-grant-id <grantId> gh
cargo run -q -p cli -- chat --session gh_test
# then ask it to use the GitHub tools

# sanity checks along the way
cargo run -q -p cli -- mcp server read gh        # authPolicy required-oauth
cargo run -q -p cli -- auth client read mcp:gh   # endpoints, hasClientSecret true
cargo run -q -p cli -- auth grant list           # grant status, never token values
```

The link passes validation because the grant's kind is `mcp_oauth`, its
status is `active`, and its audience (`https://api.githubcopilot.com/mcp/`)
covers the server URL. The broker refreshes the grant's access token
automatically when it expires, as long as the authorization server issued a
refresh token.

## GitHub App installation access (G5)

Unlike OAuth there is no flow and no stored access token: Lightspeed holds the
app's private key encrypted and the broker mints ~1 hour installation tokens
on demand (app JWT -> token exchange), caching them only in process memory.
A grant with kind `github_app` represents the installation itself.

```bash
# 1. register the app: key validated (must parse as RSA PEM), stored encrypted
cargo run -q -p cli -- auth github app add --id lightspeed-github \
  --app-id 12345 --private-key-file lightspeed-github.pem

# 2. see where the app is installed (live, signed with the app JWT)
cargo run -q -p cli -- auth github installation list --app lightspeed-github

# 3. record an installation as a grant (verified live; captures account,
#    permissions, and repository selection as non-secret grant metadata)
cargo run -q -p cli -- auth github installation grant \
  --app lightspeed-github --installation-id 678

# 4. inspect: no token values, metadata shows the installation facts
cargo run -q -p cli -- auth grant list
```

Runtime consumers resolve the grant through the broker
(`TokenAudience::GitHubApi`), which signs the JWT and mints per call. If
GitHub rejects the app credentials (key revoked) the grant turns `failed`;
if the app was uninstalled it turns `needs_reauth`. There is no Lightspeed
consumer of GitHub tokens yet (repo tools arrive later); token leases for
VMs/sandboxes are deferred until that boundary exists.
