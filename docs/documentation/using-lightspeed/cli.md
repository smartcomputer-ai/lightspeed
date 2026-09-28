# Use Lightspeed from the terminal

The CLI connects directly to the runtime. Platform can be added to the same
runtime for accounts, team permissions and the web interface; neither CLI
configuration nor API-key authentication requires it. The runtime still uses
PostgreSQL, Temporal and its configured content storage.

## Connect to local development

From a source checkout, start either the runtime alone or the full product:

```bash
./dev.sh runtime --no-envd
```

Use `./dev.sh --no-envd` to include Platform. Both profiles default to
`authenticated` mode. After migrations, the launcher creates the initial
universe and provisions separate CLI and Platform keys as needed. In another
terminal, from the same checkout:

```bash
cargo build --locked -p cli
target/debug/lightspeed connect dev
target/debug/lightspeed connect status
```

The launcher writes a handoff under `.lightspeed/cli/`. Its key files are
owner-only and ignored by Git. `connect dev` verifies and selects a saved
connection specific to this checkout. It preserves that connection's universe
selection on subsequent imports. Starting the launcher alone does not change
your selected CLI connection.

Ordinary restarts reuse the keys. Adding Platform does not rotate the CLI key.
Revoked keys, missing saved secrets or keys absent after a database reset
require explicit repair; startup does not silently issue another administrator
key. To replace a key, provision a new one with the server command described
below, put its secret into the corresponding protected `.lightspeed/cli/cli.key`
or `platform.key` file, then restart and run `connect dev` again for the CLI.
If replacing an active key, revoke the old one explicitly too.

Use `--no-api-key-bootstrap` on either launcher profile to manage keys yourself.
It preserves authenticated mode and normal startup preparation, including
migrations and universe setup. Platform then needs an existing service key.
Without a CLI handoff, use `connect add` with your own credential.

For explicitly unauthenticated local development:

```bash
LIGHTSPEED_AUTH_MODE=single ./dev.sh runtime --no-envd
target/debug/lightspeed connect dev
```

Single mode uses the server's configured universe and sends no identity
headers. Authentication failures never cause an automatic switch to this mode.

## Provision an authenticated runtime without Platform

On the server host, configure the runtime stores and secrets as described in
[Self-hosting](../deployment/self-hosting.md), then run:

```bash
lightspeed-server migrate
lightspeed-server api-key provision --name "Initial operator"
```

From source, replace `lightspeed-server` with
`cargo run -p temporal-server --`. The provision command prints JSON containing
the secret; capture it securely. It creates a deployment key with every method
group and no actor assertion. No universe is needed yet.

To supply a key from deployment secret configuration, set
`LIGHTSPEED_BOOTSTRAP_API_KEY` for the provision command. Its format is `lsk_`
followed by 32 random bytes encoded as unpadded base64url. The command stores
only its hash and metadata in the normal key store. Repeating it with the
same active key is idempotent; revoked keys and authority mismatches fail.
The runtime never authenticates directly against this environment variable,
and ordinary server startup does not provision keys.

Start the server in `LIGHTSPEED_AUTH_MODE=authenticated`. On the client:

```bash
lightspeed connect add production --url https://runtime.example/rpc
lightspeed connect use production
lightspeed universe create --slug personal
lightspeed universe use personal
```

`connect add` prompts for the key without echoing it. For headless use,
choose `--api-key-file /protected/key`, `--api-key-env MY_RUNTIME_KEY`, or
`--api-key-stdin`. Environment/stdin/prompt input is stored in a protected
local credential file; an explicitly supplied file remains externally owned.
Use `--single` only for a server explicitly configured for single mode.

## Manage connections and universes

```bash
lightspeed connect list
lightspeed connect status
lightspeed connect use production
lightspeed universe list
lightspeed universe use personal
lightspeed --connection production --universe staging session list
lightspeed connect remove production
```

`add` saves without selecting; `use` selects an existing connection. `remove`
forgets local configuration and owned local credential material without
revoking the runtime key or deleting external credential files. Removing the
active connection clears the selection.

The global `--connection` selects a saved endpoint and credential together,
ignoring ambient connection environment variables. Explicit `--api-url` on
commands that accept it can override the endpoint, but never forwards a saved
key to a different endpoint. Without `--connection`, `LIGHTSPEED_API_URL`,
`LIGHTSPEED_API_KEY` and `LIGHTSPEED_UNIVERSE` override saved settings.
`--universe` overrides universe selection for one invocation; it does not
change the saved default. The CLI loads a root `.env` when present, so check
for old overrides there if the effective connection is unexpected.

Saved connections live in `$XDG_CONFIG_HOME/lightspeed` or
`$HOME/.config/lightspeed`. `LIGHTSPEED_CONFIG_DIR` overrides that directory.
`connect status` shows the effective endpoint, key scope, allowed groups and
universe selection. `connect list` and `connect status` support `--json`.

| Authority | Universe selection |
| --- | --- |
| Deployment key | Select by core UUID or slug. Listing/slugs require `deployment/universes`; explicit UUIDs work without listing permission. |
| Universe key | The key selects its universe. Conflicting overrides fail; no universe header is sent. |
| Single mode | The server selects its configured universe; no identity headers are sent. |

Deployment operations omit the universe header even when a universe is
selected. Keys remain constrained by their allowed groups. A direct runtime
key's authority is independent of Platform membership and session visibility.
See [API keys and service access](../access-and-security/api-keys-and-service-access.md).

## Configure a provider and chat

Provider credentials are separate from the Lightspeed gateway key. Store a
built-in provider key from an environment variable:

```bash
lightspeed auth model add openai --api-key-env OPENAI_API_KEY
lightspeed models list
```

For a compatible endpoint, supply its full configuration:

```bash
lightspeed auth model add my-provider \
  --base-url https://provider.example/v1 \
  --api-kind openai:completions \
  --api-key-env MY_PROVIDER_KEY
```

Repeat `--api-kind` for multiple supported APIs and `--header NAME=VALUE` for
non-secret headers. For a credentialless endpoint, use `--no-auth` instead of
a key source. The runtime validates endpoint configuration; its loopback URLs
refer to the runtime host. Provider setup requires the `auth` group and model
discovery requires `models`.

Choose a discovered route explicitly when starting a new session:

```bash
lightspeed chat --provider my-provider --api-kind openai:completions --model MODEL_ID
```

Without those flags, a new session uses deployment defaults. Provider identity
and API kind are fixed for each session; `/model` can choose another model
within that route. The TUI displays its connection and universe. Each process
keeps its resolved connection; switching saved defaults elsewhere does not
retarget a running chat. See [Sessions and runs](sessions-and-runs.md) for
history, session switching and run controls.

## Administer keys and add Platform later

A deployment key with `deployment/api-keys` can issue narrower keys:

```bash
lightspeed api-key create --name "Agent client" --universe-id UNIVERSE_UUID \
  --group session --group models --group vfs --group blobs/put
lightspeed api-key list
lightspeed api-key revoke KEY_PREFIX
```

Create prints the secret once as JSON. Add `--group auth` if the client needs
to configure provider credentials, and other groups for the resources it
manages. Omitting groups grants every group allowed by the chosen scope.
These are integration credentials, not personal Platform login tokens.

To add Platform, give it its own deployment service key with the required
method groups and actor assertion. Use Platform's administrator adoption flow
to link an existing runtime universe. Its IDs, profiles and sessions stay in
the runtime; Platform creates its organization and membership records. This
association does not automatically share private sessions. Direct CLI access
continues to work while Platform is stopped.
