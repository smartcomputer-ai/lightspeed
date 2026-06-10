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
  with typed `AuthBrokerError` kinds instead of `Option`.
- `memory` — in-memory grant/secret stores for tests.

## How it works

Sessions and the engine only ever record `SecretRef { namespace:
"auth_grant", id }`. At LLM-call time the runtime's `SecretResolver` asks the
broker for the token for a specific resource URL; the token is injected into
the outgoing provider request at the last moment, while the persisted request
blob keeps `"authorization": "<redacted>"`. Plaintext tokens never enter
engine events, CAS blobs, Temporal history, API responses, or logs. The one
deliberate inbound-plaintext path is `auth/grants/import`.

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
