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
universe and provisions separate CLI and Platform keys as needed. The initial
universe receives the slug `development` if it has no slug; an existing slug
is preserved. Universe listings show `(no slug)` for unnamed universes. In another
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
lightspeed universe status
lightspeed universe set-slug personal my-personal
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
`connect status` groups the effective connection and key authority, followed by
the selected universe's current slug and UUID. `universe status` shows just the
active universe. Both resolve the effective selection, including environment
and command-line overrides or a universe bound by the server. Status reports
when no universe is selected, or when its slug cannot be read with the current
key. Slug lookup requires deployment authority and the `deployment/universes`
group (or single mode); a universe key still shows its bound UUID.
`universe list` marks the active universe with `*`; it marks none if there is no
selection. These commands support `--json`, as does `connect list`. JSON status
includes the slug and UUID; JSON universe lists include each row's `active`
flag and the effective `activeUniverseId`.

| Authority | Universe selection |
| --- | --- |
| Deployment key | Select by core UUID or slug. Listing/slugs require `deployment/universes`; explicit UUIDs work without listing permission. |
| Universe key | The key selects its universe. Conflicting overrides fail; no universe header is sent. |
| Single mode | The server selects its configured universe; no identity headers are sent. |

Runtime slugs are optional and unique within a deployment. `universe set-slug`
assigns or changes one without changing the UUID, resources, keys or saved CLI
selection. Renaming releases the old slug; old Platform URLs can stop working
or refer to another universe if that slug is reused.

Deployment operations omit the universe header even when a universe is
selected. Keys remain constrained by their allowed groups. A direct runtime
key's authority is independent of Platform membership and session visibility.
See [API keys and service access](../access-and-security/api-keys-and-service-access.md).

## Configure a provider and chat

Provider credentials are separate from the Lightspeed gateway key. Store a
built-in provider key from an environment variable:

```bash
lightspeed model provider add openai --api-key-env OPENAI_API_KEY
lightspeed model list
```

For a compatible endpoint, supply its full configuration:

```bash
lightspeed model provider add my-provider \
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
within that route. The TUI footer shows the connection name and universe slug
for saved or explicit runtime connections; generated local-development
connections omit this label. The footer also shows the current session ID,
shortening UUIDs while preserving readable IDs. If the universe slug is
unavailable, the footer uses a short universe UUID. `/status` shows the full connection name, endpoint,
authentication scope, universe slug/UUID and current session ID. Connection
details are captured when chat opens, with no background polling. Each process
keeps its resolved connection; switching saved defaults elsewhere does not
retarget a running chat. See [Sessions and runs](sessions-and-runs.md) for
history, session switching and run controls.

For a quick view of recent conversations:

```bash
lightspeed chat --list
lightspeed chat --list --json
lightspeed chat --resume
lightspeed chat --continue
```

`--list` shows up to 10 unmanaged root sessions in the selected universe,
most recently updated first, with names, lifecycle status, current activity
and time since the last update. Managed sessions and delegated subagents are
excluded; closed sessions remain visible in the list. JSON output includes
the full summaries and timestamps.

`--resume` and its alias `--continue` open the most recently updated unmanaged
root session that is not closed, preserving its configuration and attachments.
If none exists, the command explains how to start a new chat. It never creates
a replacement for a session deleted between listing and opening it. Use
`chat -s SESSION_ID` to choose a specific session instead. Listing and resuming
use the effective connection and universe, including command-line overrides.

To give the session files, upload a local directory or attach an existing runtime
workspace:

```bash
lightspeed chat --upload .
lightspeed chat --workspace WORKSPACE_ID --workspace-path /repo --workspace-access read
lightspeed chat -s SESSION_ID
```

`--upload` takes a one-time snapshot and creates a new runtime workspace. Local
files are not kept in sync, and agent edits are not written back to your machine.
`--workspace` reuses an existing workspace. Choose one of these options; both
default to `/workspace` with `edit` access. `--workspace-path` and
`--workspace-access read|edit` require one of the two sources.

These shortcuts use the same revision-checked attachment operation as
`session workspace attach`. They replace an attachment at the specified path
and preserve other attachments and configuration. Repeating `--upload` creates
another workspace. Resuming with `chat -s SESSION_ID` alone retains the session's
existing attachments. Every `--session` option also accepts `-s`.

## Administer keys and add Platform later

A deployment key with `deployment/api-keys` can issue narrower keys:

```bash
lightspeed api-key create --name "Agent client" --universe-id UNIVERSE_UUID \
  --group session --group models --group vfs --group blobs/put
lightspeed api-key list
lightspeed api-key revoke KEY_PREFIX
```

Create prints the secret once; use `--json` for machine-readable output. Add `--group auth` if the client needs
to configure provider credentials, and other groups for the resources it
manages. Omitting groups grants every group allowed by the chosen scope.
These are integration credentials, not personal Platform login tokens.

To add Platform, give it its own deployment service key with the required
method groups and actor assertion. Use Platform's administrator adoption flow
to link an existing runtime universe. Platform uses the runtime slug for its
URLs and caches it for lookups. Creation sends the requested slug to runtime;
adoption preserves an existing slug or requires one to be assigned to an unnamed
universe before linking. Platform does not invent a different slug on collision.
Ordinary universe lists and reads use Platform's database without contacting
runtime. Creation, adoption and renaming through Platform's general settings
immediately cache the slug returned by runtime. After a CLI or direct API rename,
a Platform administrator must use **Sync from runtime** on the **Universes**
page to update the cached URLs. There is no periodic or background slug refresh.
A failed sync reports an error and preserves the cache; missing or unnamed
runtime universes are reported as skipped for review. During runtime outages,
the cached universe list remains available for membership administration.
Linked universes across different runtime deployments must have distinct slugs
within one Platform.

IDs, profiles and sessions stay in the runtime; Platform creates only its
organization and membership records. This
association does not automatically share private sessions. Direct CLI access
continues to work while Platform is stopped.


## Command layout

Resource commands use singular names and accept plural aliases: `universe(s)`,
`api-key(s)`, `session(s)`, `profile(s)`, `model(s)`, `workspace(s)`, `credential(s)`,
`environment(s)`, and `mcp`/`mcps`. `env` is also an alias for `environment`.
`connect` (`connections`) manages local connection settings; `chat` (`chats`)
starts or continues a conversation. `vfs` (`vfses`) uploads and downloads
immutable snapshots. `--connection`, `--universe`, and `--api-url` can appear
before or after subcommands. The endpoint override follows the same credential
isolation rules as saved connections.

Workspaces, MCP registrations, credentials and environments belong to the
selected universe and have independent lifecycles. Session attachments declare
which resources an agent may use. Changing an attachment preserves unrelated
configuration and checks its revision; configuration changes require an idle
session. Attaching an environment does not activate or provision it.

```bash
lightspeed workspace list
lightspeed workspace create --display-name project
lightspeed mcp list
lightspeed mcp tools my-server
lightspeed mcp login my-server
lightspeed credential list
lightspeed credential import --provider-id my-service --token-env SERVICE_TOKEN
lightspeed model provider list
lightspeed environment list
lightspeed profile list

lightspeed session read SESSION_ID
lightspeed session config read SESSION_ID --json > session-config.json
lightspeed session config put SESSION_ID --file session-config.json
lightspeed session profile apply SESSION_ID --profile reviewer
lightspeed session workspace attach --session SESSION_ID --workspace WORKSPACE_ID --path /repo --access read
lightspeed session mcp attach SERVER_ID --session SESSION_ID --tools search,fetch
lightspeed session environment attach ENVIRONMENT_ID --session SESSION_ID --access exec --working-directory /repo
lightspeed session environment activate ENVIRONMENT_ID --session SESSION_ID
lightspeed session skill list --session SESSION_ID
lightspeed session skill use SKILL_ID --session SESSION_ID
```

The workspace, MCP and environment groups under `session` each provide
`attach`, `detach` and `list`. Lists show declared attachments rather than
inferring them from materialized tools. Detaching does not delete the resource.
Environment lists mark the active attachment. Skill selection submits ordinary
agent input (or steers an existing run); there is no separate skill execution
or installation registry.

`credential` manages external credentials, not authentication to Lightspeed.
Its `oauth-client` and `github` subcommands configure reusable OAuth clients and
GitHub Apps. Model endpoint credentials belong under `model provider`; MCP
credential binding and OAuth login belong under `mcp`. `api-key` manages keys
that authenticate callers to Lightspeed itself. Runtime API method-group names
such as `auth` and `models` are unchanged.

Human output uses labeled details, table headers and explicit empty states.
Resource operations accept `--json` for scripts. OAuth authorization instructions
are written to stderr so JSON stdout remains parseable. Profile export always
emits a reusable JSON document; profile list/read/import/check use human output
unless `--json` is requested. Configuration `put` replaces a complete document;
omitted fields return to their defaults. It checks the revision read immediately
before the write, or the explicit `--expected-revision` supplied by the caller.

## Provision environments from the CLI

A deployment administrator registers a provider controller and binds it to a
universe. `environment provider put --file provider.json` takes a
`DeploymentEnvironmentProviderPutParams` document; `environment binding put
--file binding.json` takes `DeploymentProviderBindingPutParams`, including its
explicit `universeId`. The public API reference documents their fields. These
operations require a deployment key; provider list/read/delete do as well.
Binding deletion acts on the selected universe. Normal environment and template
operations operate within the selected universe.

```bash
lightspeed environment provider list
lightspeed environment binding list
lightspeed environment template list --binding local
lightspeed environment create --binding local --template linux-v1 --request-id provision-my-machine
lightspeed environment register wss://machine.example/envd --request-id register-my-machine
lightspeed environment read ENVIRONMENT_ID
lightspeed environment power ENVIRONMENT_ID paused
lightspeed environment ingress ENVIRONMENT_ID --enable
```

Reuse the same request ID when retrying an uncertain provisioning or external
registration request. Creation and closure can be asynchronous: inspect the
returned lifecycle state with `environment read`. Outbound daemons continue to
use `environment registration-key create|list|read|revoke`. Credentials bound
with `environment credential bind` supply the environment's configured secret
references; attachment access controls the agent's permitted use of the machine.
