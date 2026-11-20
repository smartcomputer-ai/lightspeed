# Use Lightspeed from the terminal

Use `lightspeed` to open an agent conversation, inspect resources, or manage
configuration from a terminal. The same CLI connects to Platform or directly
to the runtime. Use Platform for your own account and for scripts that need
limited access. A direct runtime connection is for operators and trusted
services.

## Connect to Platform

```bash
lightspeed connect login --url https://lightspeed.example --name work
lightspeed connect status
lightspeed universe list
lightspeed universe use my-universe
lightspeed session list
```

The CLI opens the browser for sign-in and displays a short code. Verify the
code in the browser and approve the CLI. With `--no-browser`, open the printed
URL on a device with a browser. `lightspeed connect logout` revokes that CLI
session without signing out your other devices.

For a script, give the connection an existing Platform API key. The key
uses the current permissions of its owner and any restrictions set on it.

```bash
lightspeed connect add automation --url https://lightspeed.example --token-env LIGHTSPEED_KEY
lightspeed connect use automation
```

You can also read the secret from `--token-file` or `--token-stdin`. Use HTTPS
for credentialed remote connections. An operation allowlist must include
`initialize` so the CLI can verify the connection. See
[API keys and runtime tokens](../access-and-security/api-keys-and-service-access.md)
for choosing a principal and restricting its key.

### Platform administration

Organization commands and universe creation use the selected organization.
Select it once for this connection, or pass `--organization` on an individual
command.

```bash
lightspeed platform organization list
lightspeed platform organization use ORGANIZATION_ID
lightspeed platform organization member list
lightspeed universe create --name Development --handle development
```

A Creator can provision a universe and becomes its first Owner. An
Organization Admin can specify another Owner. A Platform Admin provisioning
in an organization they do not belong to supplies `--owner-user` or
`--owner-service-user`.
See [Organizations, members, and roles](../access-and-security/people-and-roles.md)
for the other ways access can be granted.

Use `platform service-user` for organization service users, `platform api-key`
for personal or service-user keys, and `universe access` for individual grants.
For example, an Organization Admin can issue a key restricted to a universe
where the service user has a role.

```bash
lightspeed platform api-key create build --service-user SERVICE_USER_ID \
  --scope-universe UNIVERSE_UUID --output-secret ./build.key
```

Replace the secret to keep the key ID, restrictions, and expiry. For a
service-user key, include the service user again.

```bash
lightspeed platform api-key replace KEY_ID --service-user SERVICE_USER_ID \
  --output-secret ./build-next.key
```

Specify a new `--output-secret` file or explicitly choose `--show-secret`.
API key management requires a human sign-in.
Use [Configuration as code](../deployment/configuration-as-code.md) to declare
organization policy, service users and their keys, or universe access.

## Connect to local development

From a checkout running `./dev.sh --no-envd` or `./dev.sh runtime --no-envd`,
build the CLI and import the launcher's connection.

```bash
cargo build --locked -p cli
target/debug/lightspeed connect dev
target/debug/lightspeed connect status
```

This is a direct runtime connection to the local stack, with full runtime
access. To exercise Platform permissions locally, use `connect login` against
`http://localhost:5173` with the full profile running. Ordinary launcher
restarts reuse the direct token. After a successful `./dev.sh reset`, import
the new handoff with `connect dev`. See [Local development](../development/local-development.md)
for launcher options and test accounts.

<a id="runtime-tokens-and-platform"></a>

## Configure an authenticated runtime without Platform

A direct client uses a configured runtime token. Every token has full
deployment authority. Follow [Self-host Lightspeed](../deployment/self-hosting.md)
to configure the runtime, generate a token with `lightspeed token generate-secret`,
and keep the private API listener reachable only by trusted clients.

```bash
lightspeed connect add runtime --url https://runtime.example/api/v1 --token-file /protected/runtime.token
lightspeed connect use runtime
lightspeed universe list
lightspeed universe use my-universe
```

If you omit the token source, `connect add` prompts for it. Use `--single`
only for a private development runtime configured with `LIGHTSPEED_AUTH_MODE=single`.
For token configuration and rotation, see [Runtime tokens](../reference/environment-variables.md#runtime-api-tokens).

## Manage connections and universes

`connect add` saves a connection and `connect use` selects it. `connect remove`
forgets the saved connection and any secret stored by the CLI. Revoke Platform
keys or rotate runtime tokens with the system that issued them.

```bash
lightspeed connect list
lightspeed connect status
lightspeed universe status
lightspeed --connection work --universe staging session list
```

`universe use` saves the UUID for the selected connection. You can supply a
UUID or a handle. A saved UUID survives a handle rename, while URLs and scripts
that use the old handle need updating. `--universe` overrides the selection
for one invocation. `--connection` selects a saved endpoint and credential
and takes precedence over ambient connection settings. Without it,
`LIGHTSPEED_API_URL`, `LIGHTSPEED_API_TOKEN`, and `LIGHTSPEED_UNIVERSE` can
override saved settings. The CLI also reads a checkout's root `.env` file.

Connections are stored under `LIGHTSPEED_CONFIG_DIR`, or under
`$XDG_CONFIG_HOME/lightspeed` or `$HOME/.config/lightspeed`. The
[SDKs can reuse a saved connection](../integrating-and-extending/api-and-typescript.md#reuse-a-saved-cli-connection).

## Configure a provider and chat

With Platform, use Operator or Owner access to configure providers and start
new CLI chats. The chat client sends session and run settings of its own.
If you have Contributor access, start and continue prepared sessions in the
web app, or use the Platform API without configuration overrides.

A provider needs its own credential. This OpenAI example reads it from your
environment. Use `model list` to find the available routes.

```bash
lightspeed model provider put openai --api-key-env OPENAI_API_KEY
lightspeed model list
lightspeed chat --provider openai --api-kind openai:responses --model MODEL_ID
```

For a compatible endpoint, also give `model provider put` its `--base-url`
and one or more `--api-kind` values. Use `--no-auth` for a service that needs
no primary credential. See [Models and credentials](models-and-credentials.md)
for choosing providers and configuring headers.

`lightspeed chat` uses the selected profile's model, if supplied, or the
universe's agent-run default. Pass a message to send one turn and exit. Omit
it to open the interactive terminal.

```bash
lightspeed chat --list
lightspeed chat --resume
lightspeed chat --session SESSION_ID
```

`--list` shows up to ten recent standalone root sessions. `--resume` opens
the most recent open one. Use `--session` to select a specific ID from the web
app or the list. See [Sessions and runs](sessions-and-runs.md) for conversation
controls and slash commands.

To bring files to a session, upload a local directory or attach an existing
workspace.

```bash
lightspeed chat --upload .
lightspeed chat --workspace WORKSPACE_ID --workspace-path /repo --workspace-access read
```

`--upload` creates a workspace from a one-time snapshot of the local files.
Later changes are made in Lightspeed. Both shortcuts default to `/workspace`
with edit access, and replace an existing attachment at the same path. On
Platform, uploading also requires workspace creation permission.

## Configure universe model defaults

Set an agent-run default so new sessions can start without having to choose
a route. The speech-to-text default is separate.

```bash
lightspeed model defaults read
lightspeed model defaults set agent-run --provider openai --api-kind openai:responses --model MODEL_ID
lightspeed model defaults set speech-to-text --provider openai --api-kind openai:audio-transcriptions --model TRANSCRIPTION_MODEL_ID
lightspeed model defaults clear speech-to-text
```

Through Platform, setting or clearing a default requires Operator or Owner
access. Pass `--expected-revision` to reject a write if the defaults changed
since you read them. Existing sessions keep their configured model. See
[Choose universe defaults](models-and-credentials.md#choose-universe-defaults).

## Universe protection, metadata, and expiry

```bash
lightspeed universe create --handle experiment --expires-in 2h --metadata purpose=development
lightspeed universe list --metadata purpose=development
lightspeed universe set-expiry experiment never
lightspeed universe protect experiment
```

This example creates a temporary universe through a direct runtime connection,
then clears its expiry and protects it. An expiry triggers deletion. Clear
protection before scheduling expiry or deleting. Through Platform, creation
uses the selected organization, whose policy may require protection. See
[Manage universes](../deployment/multi-tenancy.md) for adoption, protection,
archiving, and deletion.

## Command layout

Commands are grouped by resource. Use `--help` on the command you need for
its flags. Many commands support `--json` for scripts.

| Command group | Purpose |
| --- | --- |
| `session`, `chat`, `profile` | Inspect and run sessions, configure their setup, and manage reusable profiles. |
| `workspace` | Work with persistent VFS files and snapshots. |
| `model`, `credential`, `mcp` | Connect model providers and MCP servers, and manage credentials for external services. |
| `environment` | Provision or register compute and manage its lifecycle. |
| `universe`, `platform`, `config` | Select and administer universes and organizations and reconcile configuration documents. |
| `api` | Call a generated operation with `api OPERATION --params params.json`. Use dedicated commands for streams and files. |

For example, to update an idle session's configuration, read the current
document, edit it, and put the complete configuration back.

```bash
lightspeed session config read SESSION_ID --json > session-config.json
lightspeed session config put SESSION_ID --file session-config.json
```

To apply an existing setup instead, use `session profile apply SESSION_ID
--profile PROFILE_ID`. See [Profiles and instructions](profiles-and-instructions.md).

## Keep configuration in files

```bash
lightspeed --universe research config export -o research.yaml
lightspeed config diff -f research.yaml
lightspeed config apply -f research.yaml
```

`diff` shows what `apply` would change and exits with status 2 if there is a
difference. Apply is safe to repeat. Use `config export --kind organization`
for an organization's configuration. See [Configuration as code](../deployment/configuration-as-code.md)
for schemas, secret sources, and adoption of existing resources.

## Repair rejected context

If a provider repeatedly rejects a user message or tool result, inspect the
active context after the run has stopped.

```bash
lightspeed session context list SESSION_ID
lightspeed session context replace SESSION_ID item_12 "The command failed. Its diagnostic output was removed."
lightspeed session context redact SESSION_ID item_16
```

Use entry IDs from the list. `replace` changes the text, and `redact` uses a
standard placeholder. Only active user messages and tool results can be
replaced. List the context again before continuing. Original events and blobs
remain in retained history. If the problem is the size of the full context
window, see [Manage long conversations](sessions-and-runs.md#manage-long-conversations).

## Provision environments from the CLI

Register a provider through a direct runtime connection. By default every
universe can use it. To limit the provider, register it with
`--selected-universes` and bind the eligible universes individually using
`environment binding put PROVIDER_ID`.

```bash
lightspeed environment provider available
lightspeed environment template list --provider incus-local
lightspeed environment create --provider incus-local --template linux-v1 --request-id build-machine
lightspeed environment list
lightspeed environment read ENVIRONMENT_ID
```

Reuse the request ID when retrying after an uncertain result. Provisioning
and closing are asynchronous, so inspect status with `environment read`.
For your own machines, see [Bring your own compute](../environments/bring-your-own-compute.md)
and [Run registered daemons](../environments/registered-daemons.md), including
how to declare fixed machines before their daemons connect.
