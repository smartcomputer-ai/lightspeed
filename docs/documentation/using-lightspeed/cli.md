# Use Lightspeed from the terminal

The CLI connects directly to the runtime. Platform can be added to the same
runtime for accounts, team permissions and the web interface; neither CLI
configuration nor runtime-token authentication requires it. The runtime still uses
PostgreSQL, Temporal and its configured content storage.

## Connect to local development

From a source checkout, start either the runtime alone or the full product:

```bash
./dev.sh runtime --no-envd
```

Use `./dev.sh --no-envd` to include Platform. Both profiles default to
`authenticated` mode. After migrations, the launcher creates the initial
universe and persistent CLI and Platform runtime tokens. The universe is created
with handle `development`; an existing universe keeps its handle. In another
terminal, from the same checkout:

```bash
cargo build --locked -p cli
target/debug/lightspeed connect dev
target/debug/lightspeed connect status
```

The launcher writes an owner-only handoff and token files under
`.lightspeed/cli/`, ignored by Git. `connect dev` verifies and selects a saved
connection for this checkout, preserving its universe selection on later imports.
Ordinary restarts reuse the tokens. Tokens are configuration, so resetting the
runtime database does not revoke them. A successful `./dev.sh reset` also clears
the local token files; the next startup generates replacements. Import the new
handoff with `connect dev` afterward.

Supply `LIGHTSPEED_RPC_TOKEN_CLI` and `LIGHTSPEED_PLATFORM_RPC_TOKEN`, or their
`_FILE` variants, to choose tokens yourself. The launcher installs these in the
runtime configuration and writes protected local files. When the CLI and Platform
use the same secret, the launcher keeps one runtime token identity (`default` or
`cli`) and gives Platform a file containing that token. `--no-token-bootstrap`
requires configured or saved tokens and prevents generation. A runtime token
has full authority over every universe and every method; keep it with operators
and trusted services.

For explicitly unauthenticated local development:

```bash
LIGHTSPEED_AUTH_MODE=single ./dev.sh runtime --no-envd
target/debug/lightspeed connect dev
```

Single mode uses the server's configured universe and sends no identity
headers. Authentication failures never cause an automatic switch to this mode.

## Configure an authenticated runtime without Platform

On the server host, configure stores and secrets as described in
[Self-hosting](../deployment/self-hosting.md). Generate an RPC token offline:

```bash
umask 077
lightspeed token generate-secret > runtime.token
lightspeed-runtime migrate
LIGHTSPEED_AUTH_MODE=authenticated \
  LIGHTSPEED_RPC_TOKEN_FILE="$PWD/runtime.token" lightspeed-runtime
```

`generate-secret` needs no runtime connection. A configured token contains at
least 32 printable ASCII characters without whitespace. Token files may end with
one LF or CRLF; inline values are used exactly. Optional token names use
environment suffixes, for example `LIGHTSPEED_RPC_TOKEN_OPERATOR_FILE` and
`LIGHTSPEED_RPC_TOKEN_PLATFORM_FILE`. Every token has full authority. A token's
name is recorded when a request names no actor; an unnamed token is `default`.
For rotation, configure a second token, move clients to it, then remove the old
one and restart the runtime. There is no key table or key-management API.

From source, replace `lightspeed-runtime` with
`cargo run -p temporal-runtime --`. On the client:

```bash
lightspeed connect add production --url https://runtime.example/rpc
lightspeed connect use production
lightspeed universe create --handle personal
lightspeed universe use personal
```

`connect add` prompts for the token without echoing it. For headless use, choose
`--token-file /protected/token`, `--token-env MY_RUNTIME_TOKEN`, or `--token-stdin`.
Environment, stdin and prompt input is stored in a protected local credential
file; an explicitly supplied file remains externally owned. Use `--single` only
for a server explicitly configured for single mode.

The CLI address is the private RPC listener, normally port `18080`. The runtime's
OAuth callback and client metadata use the separate public listener, normally
port `18082`, under `/credentials/callback` and
`/credentials/client-metadata.json`. The public listener does not accept CLI RPC
calls. Credential management methods use the `credentials/*` API family; the CLI
keeps its `credential`/`credentials` resource command.

## Manage connections and universes

```bash
lightspeed connect list
lightspeed connect status
lightspeed connect use production
lightspeed universe list
lightspeed universe use personal
lightspeed universe status
lightspeed universe set-handle personal my-personal
lightspeed --connection production --universe staging session list
lightspeed connect remove production
```

`add` saves without selecting; `use` selects an existing connection. `remove`
forgets local configuration and owned local credential material without
removing the token from runtime configuration or deleting external credential files. Removing the
active connection clears the selection.

The global `--connection` selects a saved endpoint and credential together,
ignoring ambient connection environment variables. Explicit `--api-url` on
commands that accept it can override the endpoint, but never forwards a saved
token to a different endpoint. Without `--connection`, `LIGHTSPEED_API_URL`,
`LIGHTSPEED_RPC_TOKEN` and `LIGHTSPEED_UNIVERSE` override saved settings.
`--universe` overrides universe selection for one invocation; it does not
change the saved default. The CLI loads a root `.env` when present, so check
for old overrides there if the effective connection is unexpected.

Saved connections live in `$XDG_CONFIG_HOME/lightspeed` or
`$HOME/.config/lightspeed`. `LIGHTSPEED_CONFIG_DIR` overrides that directory.
`connect status` shows the effective connection, authentication mode, token name,
and the selected universe's current handle and UUID. `universe status` shows just
the active universe. A direct connection has full runtime authority.
`universe list` marks the selected universe with `*`. These commands support
`--json`, as does `connect list`.

Authenticated connections select a universe by UUID or handle. `universe use`
saves the UUID, so a rename does not move the saved selection. `--universe` and
`LIGHTSPEED_UNIVERSE` are sent as given. Single mode uses the runtime's configured
universe and sends no identity headers.

Every universe has a handle: a name that is unique within the deployment and
addresses the universe wherever its UUID does. A handle is 1 to 63 lowercase
letters, digits and hyphens, starts and ends with a letter or digit, and cannot
have the form of a UUID. `universe create` without `--handle` derives one from
the UUID, such as `u-41d07ab355e1`.

`universe set-handle` renames a universe without changing its UUID, resources,
or saved CLI selection. Renaming releases the old handle: Platform URLs
and API clients that use it stop reaching the universe, and reach another one
if that handle is later reused. Automation that addresses a universe by handle
should treat a rename as a coordinated change.

Deployment operations omit the universe header even when a universe is
selected. A direct runtime token has full authority, independent of Platform
membership and session visibility.
See [Runtime access](../access-and-security/api-keys-and-service-access.md).

## Configure a provider and chat

Provider credentials are separate from the Lightspeed runtime token. Store a
built-in provider key from an environment variable:

```bash
lightspeed model provider put openai --api-key-env OPENAI_API_KEY
lightspeed model list
```

For a compatible endpoint, supply its full configuration:

```bash
lightspeed model provider put my-provider \
  --base-url https://provider.example/v1 \
  --api-kind openai:completions \
  --api-key-env MY_PROVIDER_KEY
```

Repeat `--api-kind` for multiple supported APIs. An extra header is either
`--header NAME=VALUE`, a plain value that every reader of the provider sees, or
`--header-credential NAME=GRANT_ID`, which sends the current token of a saved
token credential and never returns it. `api-key`, `x-api-key`, and
`proxy-authorization` are accepted only as `--header-credential`.
`lightspeed mcp server put` takes the same two flags for an MCP server's
request headers, with the same rule for `api-key` and `x-api-key`:

```bash
lightspeed credential put gateway-key --token-env GATEWAY_KEY
lightspeed model provider put my-provider \
  --base-url https://provider.example/v1 \
  --api-kind openai:completions \
  --no-auth \
  --header-credential api-key=gateway-key \
  --header X-Title=Lightspeed
```

For a credentialless endpoint, use `--no-auth` instead of
a key source. `put` declares the provider: repeating it changes nothing. The
runtime validates endpoint configuration; its loopback URLs
refer to the runtime host. Provider setup and model discovery use the direct runtime token.

Choose a discovered route explicitly when starting a new session:

```bash
lightspeed chat --provider my-provider --api-kind openai:completions --model MODEL_ID
```

Without those flags, a new session uses its profile's model, if supplied, then
the universe's agent-run default. Configure that default below or select a
route explicitly. Provider identity and API kind are fixed for each session;
`/model` can choose any other model within that route. The TUI footer shows the connection name and universe handle
for saved or explicit runtime connections; generated local-development
connections omit this label. The footer also shows the current session ID,
shortening UUIDs while preserving readable IDs. If the universe handle is
unavailable, the footer uses a short universe UUID. `/status` shows the full connection name, endpoint,
authentication mode, universe handle/UUID and current session ID. Connection
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

These shortcuts change the session's configuration with a revision check.
They replace an attachment at the specified path
and preserve other attachments and configuration. Repeating `--upload` creates
another workspace. Resuming with `chat -s SESSION_ID` alone retains the session's
existing attachments. `--session` also accepts `-s`.

## Configure universe model defaults

Model defaults belong to the selected universe, separately from provider
credentials and the CLI's saved connection:

```bash
lightspeed model defaults read
lightspeed model defaults set agent-run --provider my-provider --api-kind openai:completions --model MODEL_ID
lightspeed model defaults set speech-to-text --provider openai --api-kind openai:audio-transcriptions --model TRANSCRIPTION_MODEL_ID
lightspeed model defaults clear speech-to-text
```

Use a model supported by the configured provider. These commands require the
`models` method group. `set` and `clear` write whatever the current defaults
are; pass `--expected-revision REVISION` to have the write refused if the
defaults changed since the revision you read. All three commands support
`--json`.

An explicit session model takes precedence over a profile model, followed by
the `agent-run` default. Without any of these, creation fails with
`modelDefaultUnset`. Changing or clearing defaults leaves existing sessions
and admitted transcription jobs unchanged. A resumed chat uses its stored
model. See [Models and credentials](models-and-credentials.md#choose-universe-defaults).

`LIGHTSPEED_CHAT_PROVIDER` and `LIGHTSPEED_CHAT_MODEL` are retired and prevent
runtime startup. Remove them from deployment configuration and save the
intended routes with these commands.

## Runtime tokens and Platform

Runtime tokens are loaded from configuration. Generate a secret with
`lightspeed token generate-secret`; configure and rotate it on the runtime host.
Never give a runtime token to an agent or store one in its workspace.

To add Platform, configure a named runtime token for it and set
`LIGHTSPEED_PLATFORM_RPC_TOKEN` or `LIGHTSPEED_PLATFORM_RPC_TOKEN_FILE` on the
Platform backend to the same value. Use Platform's administrator adoption flow
to link an existing runtime universe. Platform uses the universe's handle for
its URLs and caches it for lookups. Creation sends the requested handle to
runtime; adoption keeps the handle the universe already has. Platform does not
invent a different handle on collision.
Ordinary universe lists and reads use Platform's database without contacting
runtime. Creation, adoption and renaming through Platform's general settings
immediately cache the handle returned by runtime. After a CLI or direct API rename,
a Platform administrator must use **Sync from runtime** on the **Universes**
page to update the cached URLs. There is no periodic or background handle refresh.
A failed sync reports an error and preserves the cache; universes missing from
the runtime are reported as skipped for review. During runtime outages,
the cached universe list remains available for membership administration.
Linked universes across different runtime deployments must have distinct handles
within one Platform.

IDs, profiles and sessions stay in the runtime; Platform creates only its
organization and membership records. This
association does not automatically share private sessions. Direct CLI access
continues to work while Platform is stopped.


## Command layout

Resource commands use singular names and accept plural aliases: `universe(s)`,
`session(s)`, `profile(s)`, `model(s)`, `workspace(s)`, `credential(s)`,
`environment(s)`, and `mcp`/`mcps`. `env` is also an alias for `environment`.
`connect` (`connections`) manages local connection settings; `chat` (`chats`)
starts or continues a conversation. `workspace snapshot` (`snapshots`) uploads
and downloads immutable snapshots. `--connection`, `--universe`, and `--api-url` can appear
before or after subcommands. The endpoint override follows the same credential
isolation rules as saved connections.

Workspaces, MCP registrations, credentials and environments belong to the
selected universe and have independent lifecycles. Session attachments declare
which resources an agent may use. They are part of the session's
configuration; configuration changes check its revision and require an idle
session. Attaching an environment does not provision it.

```bash
lightspeed workspace list
lightspeed workspace create --display-name project
lightspeed mcp list
lightspeed mcp tools my-server
lightspeed mcp login my-server
lightspeed credential list
lightspeed credential put my-service-token --token-env SERVICE_TOKEN --display-name "My service"
lightspeed credential replace my-service-token --token-stdin
lightspeed model provider list
lightspeed environment list
lightspeed profile list

lightspeed session read SESSION_ID
lightspeed session config read SESSION_ID --json > session-config.json
lightspeed session config put SESSION_ID --file session-config.json
lightspeed session profile apply SESSION_ID --profile reviewer
```

`credential put` declares a token credential by id: it is created when missing, its settings
follow the flags given (unset ones are cleared), its token is replaced only
when it differs, and repeating the command changes nothing. `--dry-run`
reports what would change. `credential replace` swaps only the token and
keeps the settings. Both read the token from `--token-file`, `--token-env` or
`--token-stdin`, never from the command line.

A credential is brokered unless it is created with `--retrievable`: its token
stays inside the runtime, which attaches it to its own requests. Pass
`--retrievable` to `credential put`, `credential login` or
`credential github installation grant` for a credential that will be
[bound into an environment](../environments/credentials.md) or leased by a
trusted service. The choice is fixed when the credential is created;
declaring the same id with the other exposure is refused.

A session's attached workspaces, MCP servers and environments are part of its
configuration. To change them, read the configuration with
`session config read --json`, edit the document and write it back with
`session config put`, or apply a profile that attaches them with
`session profile apply`. Removing an attachment does not delete the resource.
`session read` shows the active environment. In the chat TUI, `/skills` lists
discovered skills and `/skill` selects one. Skill selection submits ordinary
agent input (or steers an existing run); there is no separate skill execution
or installation registry.

`credential` manages external credentials, not authentication to Lightspeed.
Its `oauth-client` and `github` subcommands configure reusable OAuth clients and
GitHub Apps. Model endpoint credentials belong under `model provider`; MCP
credential binding and OAuth login belong under `mcp`. `token generate-secret`
generates a runtime token offline; runtime access is configured on the host.
API family names such as `credentials` and `models` remain contract metadata.

Human output uses labeled details, table headers and explicit empty states.
Resource operations accept `--json` for scripts. OAuth authorization instructions
are written to stderr so JSON stdout remains parseable. Profile export always
emits a reusable JSON document; profile list/read/import/check use human output
unless `--json` is requested. Configuration `put` replaces a complete document;
omitted fields return to their defaults, except an omitted model preserves the
existing session model. It checks the revision read immediately
before the write, or the explicit `--expected-revision` supplied by the caller.

## Keep configuration in files

The `config` commands treat a universe's configuration as a document:

```bash
lightspeed --universe research config export -o research.yaml
lightspeed config diff -f research.yaml
lightspeed config apply -f research.yaml
```

`export` writes credentials, providers, model defaults, MCP servers,
registration keys, environments and profiles in the shape of the
API's own inputs, with each secret named by where it comes from. `apply`
makes the universe named by the document's `handle` match it, creating the
universe when it does not exist, and changes nothing when it already
matches. `diff` shows what `apply` would do and exits with status 2 when that
is anything. See
[Configuration as code](../deployment/configuration-as-code.md).

## Repair rejected context

If a provider repeatedly rejects a particular user message or tool result,
inspect the active context after the run has stopped:

```bash
lightspeed session context list SESSION_ID
lightspeed session context list SESSION_ID --json
```

The list follows model order and includes entry IDs such as `item_12`, kinds,
media handles when present, and bounded previews. Use the IDs actually
returned for that session. Replace an entry with useful text, or redact it
with a standard removed-by-operator placeholder:

```bash
lightspeed session context replace SESSION_ID item_12 "The command failed; its oversized diagnostic output was removed."
lightspeed session context redact SESSION_ID item_15 item_16
```

Only active user messages and tool results can be replaced. A tool result
keeps its call identity and position, so the provider still sees the call
answered. Assistant messages, tool calls, and provider-native state cannot
be edited this way. Replacement is refused while a run is active.

Inspect each reported result: `replaced`, `unchanged`, `absent`, or `failed`.
An entry that has already left active context is absent; a batch can report
different outcomes for different entries. List the context again before
continuing the conversation. These commands also support `--json`; API
clients use `session/context/replace`.

Redaction changes what future requests receive. Original events and blobs
remain in retained history, so this is not a data-erasure operation. For a
full context window rather than a specific rejected entry, use the
[compaction controls](sessions-and-runs.md#manage-long-conversations).

## Provision environments from the CLI

A deployment administrator registers a provider controller and binds it to a
universe. `environment provider put` takes the provider ID, its
`--controller-endpoint`, and optionally `--display-name`, `--data-route-base`
(required for a `unix://` controller), and a credential from
`--credential-file`, `--credential-env` or `--credential-stdin`; omitting the
credential keeps the stored one, and `--clear-credential` removes it.
`environment binding put PROVIDER_ID --enable` (or `--disable`) binds the
selected universe. These operations use the runtime token; provider
list/read/delete and binding delete do as well. Normal environment and
template operations operate within the selected universe.

```bash
lightspeed environment provider put incus-local --controller-endpoint ws://127.0.0.1:19090/control --credential-file ./provider-credential
lightspeed environment binding put incus-local --enable
lightspeed environment template list --provider incus-local
lightspeed environment create --provider incus-local --template linux-v1 --request-id provision-my-machine
lightspeed environment register wss://machine.example/envd --request-id register-my-machine
lightspeed environment list --include-closed --limit 50
lightspeed environment read ENVIRONMENT_ID
lightspeed environment update ENVIRONMENT_ID --display-name "Build machine" --metadata team=release
lightspeed environment power ENVIRONMENT_ID paused
lightspeed environment ingress ENVIRONMENT_ID --enable
```

Reuse the same request ID when retrying an uncertain provisioning or external
registration request. Creation and closure of a provisioned environment are
asynchronous: inspect the returned state with `environment read`. `power` and
`ingress` wait for the provider's answer and print what it reported; the
provider keeps owning power afterwards. `environment list` omits closed
environments unless `--include-closed` is given, and returns one page at a
time; when more remain, it prints the `--after` value for the next page. `environment update`
replaces the display name and metadata, so omitted flags clear them.

Outbound daemons use `environment registration-key create|put|list|read|update|rotate|revoke`.
`create --id` chooses the key's id, and `put` declares a key with a secret you
supply (`environment registration-key generate-secret`), for pipelines that
keep the secret in a secret store.
`update` replaces a key's group name and policy, including
`--offline-close-after` and `--declared-only` for a persistent key; `rotate`
prints a new secret once and retires the old one. Durations such as
`--grace 5m`, `--offline-close-after 12h` and `--expires-in 24h` take the
suffixes `ms`, `s`, `m`, `h`, `d` and `w`.

A fixed machine can be declared before its daemon registers, under an
environment ID you choose:

```bash
lightspeed environment declare build-box --key ci-runners --registration-name lightspeed-runner-0
lightspeed environment release build-box
```

`declare` makes the environment exist, disconnected, until the first
daemon that key admits under that registration name binds to it; running it
again changes nothing. `release` forgets the bound daemon so that a
machine with a new identity can take its place. `environment create` and
`environment register` take `--id` to choose their environment's ID as well.
See [Run registered daemons](../environments/registered-daemons.md#declare-fixed-machines-before-their-daemons-register).

Credentials bound with `environment credential
bind` supply the environment's configured secret references; attachment
access controls the agent's permitted use of the machine.
