# auth-registry

Generic auth grant, secret, and token-broker substrate for Forge (P69).

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
  with typed `AuthBrokerError` kinds instead of `Option`. OAuth grants
  refresh automatically: single-flight per grant (`GrantRefreshLock`; a
  Postgres advisory lock in `store-pg`), a 60s expiry margin, rotation-safe
  secret swaps, `invalid_grant` → `needs_reauth`, and fallback to the stored
  token when a refresh fails transiently but the token is still valid.
- `oauth` — `OAuthClientRecord` (manually configured authorization/token
  endpoints + AS-issued client id), `AuthFlowRecord` (one-time
  authorization-code flows storing only the SHA-256 of `state`), PKCE S256
  helpers, and the `OAuthTokenClient` trait with a reqwest implementation.
- `flow` — `OAuthFlowService`: start builds the authorization URL and
  persists the encrypted PKCE verifier; the callback atomically consumes the
  flow, exchanges the code, stores encrypted tokens, and mints the grant.
- `memory` — in-memory grant/secret/client/flow stores for tests.

## How it works

Sessions and the engine only ever record `SecretRef { namespace:
"auth_grant", id }`. At LLM-call time the runtime's `SecretResolver` asks the
broker for the token for a specific resource URL; the token is injected into
the outgoing provider request at the last moment, while the persisted request
blob keeps `"authorization": "<redacted>"`. Plaintext tokens never enter
engine events, CAS blobs, Temporal history, API responses, or logs. The two
deliberate inbound-plaintext paths are `auth/grants/import` (bearer token)
and `auth/clients/create` (client secret); both encrypt on receipt and
redact `Debug` output.

## Build & test

```bash
cargo test -p auth-registry
# encrypted store impl (needs dev/local infra):
cargo test -p store-pg --test store_pg_live -- --ignored
```

## Testing auth grants end to end

Uses the public authenticated test server at
<https://mcpplaygroundonline.com/mcp-auth-server>, which issues bearer tokens
for testing.

```bash
# 0. once: infra + env, gateway in its own terminal
dev/local/up.sh && source dev/local/env.sh
cargo run -p temporal-server        

# separate terminal, env.sh sourced
# 1. register the authenticated MCP server
source dev/local/env.sh
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
`"authorization": "<redacted>"` and never the token, and `secret_records`
holds only ciphertext.

## Testing an OAuth login end to end

Works against any standard authorization server with a manually configured
client (a GitHub OAuth app is the cheapest real one; set its callback URL to
`http://127.0.0.1:18080/auth/callback` for local dev). MCP-server discovery
(`forge auth login mcp:<server>`) arrives with P69 G4.

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

For an OAuth-protected MCP server, register the client with
`--kind mcp-oauth --audience <server URL>`; the resulting grant is
audience-bound and linkable via `forge mcp link --auth-grant-id ...` against
servers with an OAuth auth policy. The broker refreshes the grant's access
token automatically when it expires, as long as the authorization server
issued a refresh token.
