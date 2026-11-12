# Configuration as code

A universe's configuration can be kept in a file and applied by a pipeline,
with the review and promotion flow you already use for the deployment
itself. The `lightspeed config` commands do this:

| Command | Effect |
| --- | --- |
| `lightspeed config export` | Writes the configuration of a universe as a document. |
| `lightspeed config apply -f FILE` | Makes a universe match a document. |
| `lightspeed config diff -f FILE` | Reports what `apply` would change, and writes nothing. |
| `lightspeed config schema` | Prints the JSON Schema of a document, for editors. |

They use the public API. A document holds what you would otherwise pass
to the API one resource at a time, so anything `apply` does can also be done
by hand.

Service settings are not part of a document. Roles, addresses, storage and
Temporal connections stay with the chart or Compose file that starts the
services; see [Configure a deployment](configuration.md).

## Before you start

- Use a deployment API key. It needs the `deployment/universes` group and the
  groups of the resources in your documents: `auth`, `models`, `mcp`,
  `environments`, `profiles`, and `deployment/api-keys` or
  `deployment/environment-providers` if you declare keys or enable providers.
  See [API keys and service access](../access-and-security/api-keys-and-service-access.md).
- Connect the CLI to the runtime with `LIGHTSPEED_API_URL` and
  `LIGHTSPEED_API_KEY`, or a saved connection.

## Write a universe document

A document is YAML or JSON; both hold the same fields. This one declares a
credential, an MCP server that uses it, a registration key, a fixed machine
and a profile that runs on that machine:

```yaml
schemaVersion: 1
kind: universe
handle: research

credentials:
  - grantId: tracker-token
    displayName: Issue tracker
    tokenFrom: { env: TRACKER_TOKEN }

mcpServers:
  - serverId: tracker
    serverUrl: https://tracker.example.com/mcp
    defaultServerLabel: tracker
    headers:
      x-upstream-token: { type: credential, grantId: tracker-token }

registrationKeys:
  - registrationKeyId: runners
    displayName: Runners
    identityMode: persistent
    declaredOnly: true
    secretFrom: { file: /var/run/secrets/runners/registration-key }

environments:
  - environmentId: build-box
    displayName: Build box
    source: { type: registered, registrationKeyId: runners, name: lightspeed-runner-0 }
    credentials:
      - envName: TRACKER_TOKEN
        source: { type: authGrant, grantId: tracker-token }

profiles:
  - profileId: reviewer
    displayName: Reviewer
    instructions: { type: text, text: "Review the change and report risks." }
    config:
      features:
        environments:
          version: 1
          environments:
            - { environmentId: build-box, default: true, access: read }
```

`handle` names the universe. `apply` finds the universe by its handle and
creates it when no universe has that handle, so the same file works in every
deployment. The universe's UUID never appears in a document.

`schemaVersion` identifies the layout of the file. It is not a revision of
your configuration, and it plays no part in deciding what changed.

Every section is optional. A section you leave out is not managed by the
document: `apply` does not read or change it.

### Sections

Each entry is the input of the API method that writes the resource, with the
same field names. The
[JSON-RPC reference](../../../crates/api/contract/api-reference.md) describes
the fields.

| Section | Method an entry follows | Identified by |
| --- | --- | --- |
| `credentials` | `auth/grants/put` | `grantId` |
| `providers` | `auth/providers/put` | `providerId` |
| `modelDefaults` | `models/defaults/put`; a map of slot (`agentRun`, `speechToText`) to model, where `null` clears a slot | slot |
| `mcpServers` | `mcp/servers/put` (the `server` object) | `serverId` |
| `environmentProviders` | `deployment/environment-providers/bindings/put`: `providerId` and `enabled` | `providerId` |
| `registrationKeys` | `environments/registration-keys/put` | `registrationKeyId` |
| `environments` | see below | `environmentId` |
| `profiles` | `profiles/put` (the `profile` object) | `profileId` |
| `apiKeys` | `deployment/api-keys/put`, scoped to this universe | `keyId` |

An environment entry has an `environmentId`, an optional `displayName` and
`metadata`, and a `source` that says what kind of environment it is:

| `source.type` | Other fields | Created with |
| --- | --- | --- |
| `registered` | `registrationKeyId`, `name` | `environments/registered/put`. A fixed machine declared before its daemon registers; see [Run registered daemons](../environments/registered-daemons.md#declare-fixed-machines-before-their-daemons-register). |
| `provisioned` | `providerId`, `templateId`, optional `idle` | `environments/create` |
| `external` | `endpoint` | `environments/external/create` |

An entry differs from the API input in these ways only:

- A secret is never written in a document. In place of `token`, `credential`
  or `secret`, name where it comes from with `tokenFrom`, `credentialFrom` or
  `secretFrom`; see [Supply secrets](#supply-secrets).
- `expectedRevision`, `dryRun` and `requestId` are not accepted. `apply`
  supplies them.
- The universe and a key's `scope` are implied by the document.
- A profile's instructions are written as text, not as a blob reference.
- An entry can live in a file of its own: `- file: profiles/reviewer.json`,
  relative to the document. A profile written by `lightspeed profile export`
  can be included unchanged.

An unknown field is an error, at every level, so a misspelled field never
silently does nothing.

### Validate a document as you write it

The CLI prints the JSON Schema of a document, without contacting a runtime:

```bash
lightspeed config schema > lightspeed-configuration.schema.json
```

Point your editor at it. With the YAML language server, add this line at the
top of a document:

```yaml
# yaml-language-server: $schema=./lightspeed-configuration.schema.json
```

The schema is generated from the same API inputs `apply` checks entries
against, so it describes every field and refuses the same unknown fields.
It belongs to the CLI version that printed it; regenerate it when you
upgrade. An included file holds a single entry without the envelope, so the
schema does not apply to it directly.

### The deployment's own resources

API keys with deployment scope go in a document of their own:

```yaml
schemaVersion: 1
kind: deployment
apiKeys:
  - keyId: platform
    displayName: Platform
    assertActor: true
    secretFrom: { env: PLATFORM_API_KEY }
```

The first key cannot be declared this way, because nothing can authenticate
before it exists. Declare it on the server host with
`lightspeed-runtime api-key provision`; see
[Self-hosting](self-hosting.md).

## Supply secrets

A secret source is `{ env: NAME }` or `{ file: PATH }`. The process that
runs `apply` reads it; in a cluster, mount the values from your secret store
into the job. A relative file path is relative to the document. One trailing
line break in a file is dropped.

Every source is read before anything is sent. If one is missing, `apply`
stops without having read or written anything in the runtime.

Secrets flow one way, into Lightspeed. For keys that Lightspeed itself
accepts (`apiKeys`, `registrationKeys`), generate the secret in your secret
store, or with `lightspeed api-key generate-secret` and
`lightspeed environment registration-key generate-secret`, and give the same
value to the
service that uses the key.

When the value behind a source changes, the next `apply` replaces the stored
secret at once. The old secret stops working at that moment, so update the
consumers of a key together with it.

## Check and apply

```bash
lightspeed config diff -f config/
lightspeed config apply -f config/
```

`-f` takes files or directories. A directory is read recursively; every YAML
or JSON file in it that has a `kind` is a document.

`apply` works in four steps and writes only in the last one:

1. It checks every entry and every reference between entries, for example a
   profile that attaches an environment.
2. It reads the secret sources, finds each universe, and reads the current
   state of the sections the document contains.
3. It compares each entry with the current state.
4. It writes the entries that differ, in dependency order.

```
universe research
  created     credentials/tracker-token
  created     mcpServers/tracker
  created     registrationKeys/runners
  created     environments/build-box
  created     environments.credentials/build-box/TRACKER_TOKEN
  created     profiles/reviewer
  6 changed, 0 unchanged
```

Applying the same document again changes nothing and makes no write. An
update names the fields that differ, and `(secret)` when the stored secret
differs from the supplied one. Secret values never appear in the output.
`--json` prints one object per resource.

| Command | Exit status |
| --- | --- |
| `config apply` | 0 when the universe matches the document afterwards, 1 otherwise |
| `config diff` | 0 when nothing would change, 2 when something would, 1 on an error |

If a write is refused, `apply` stops there and reports what it completed and
what remains. Every write is safe to repeat, so run it again once the cause
is fixed.

### What apply changes, and what it leaves alone

The document is the desired state of the resources it names:

- A resource you edited in the web app or with another command is set back
  to what the document says by the next `apply`, including fields the
  document leaves at their defaults. `diff` shows such an edit as an update.
- A resource the document does not name is left alone, and so is every
  section the document does not contain.
- **Nothing is deleted.** Removing an entry from a document leaves the
  resource in the universe; remove it by hand. The one exception is the
  `credentials` list of an environment entry: a binding that is not listed is
  unbound. Leave the list out to keep an environment's bindings unmanaged.

Some settings cannot change once a resource exists. An entry that differs in
one of them is reported as a conflict, and a document with a conflict writes
nothing at all:

| Resource | Fixed |
| --- | --- |
| Registration key | `identityMode` |
| API key | `assertActor` |
| Environment | `source` |
| A revoked key | Stays revoked; declare the key under a new id. |
| A closed environment | Its id stays taken; declare the environment under a new id. |

`apply` never releases the daemon bound to a declared environment and never
renames a universe. Both stay explicit commands:
`lightspeed environment release` and
`lightspeed universe set-handle`. After a rename, update the document's
`handle` before the next `apply`, or it creates a new universe under the old
handle.

A document does not carry the name people see. That name belongs to the
Platform organization. A universe created by `apply` has no Platform
organization until an administrator adopts it in the web app, and names it
there; see [Managing universes](multi-tenancy.md).

## Export a universe

```bash
lightspeed --universe research config export -o config/universes/research.yaml
lightspeed --universe research config export -o research.json --profiles-dir profiles
lightspeed config export --deployment
```

The exported document is in a normal form: entries ordered by id, fields the
runtime derives removed, and fields left out when they hold their default.
Applying it to the universe it came from changes nothing.

Each secret is written as a placeholder source named after the resource, for
example `tokenFrom: { env: TRACKER_TOKEN }`. Bind those variables, or edit
the sources, before applying the document elsewhere.

An export leaves out what is not configuration: closed environments,
environments whose daemon registered without a declaration, revoked keys, and
credentials created through a sign-in flow.

To promote a universe from one deployment to another, export it, review the
file, and apply it against the other deployment with that deployment's
secrets. To start a new universe from an existing one, change `handle` in the
exported file.

## Run it from a pipeline

`apply` is safe to run on every deployment and on a schedule. A typical
arrangement in Kubernetes:

- Render the documents into a ConfigMap and the secrets into a Secret.
- Run `lightspeed config apply -f /config` as a Job whose name includes a hash of
  the documents, so a changed document triggers a new run.
- Run `lightspeed config diff -f config/` in merge requests against the target
  deployment to show reviewers what would change.

Declare a registration key before the daemons that hold its secret start: a
daemon whose key the runtime does not know exits. An orchestrator that
restarts the pod converges anyway. A declared environment and its machine can
be deployed in either order.

## Limits

- Members and roles, bots and triggers, chat channel accounts, workspaces and
  session retention defaults cannot be declared yet.
- Deployment environment providers
  (`deployment/environment-providers/put`) cannot be declared yet; a universe
  document can only enable an existing one.
- Credentials created through an OAuth sign-in cannot be declared. A document
  can refer to one by its `grantId`.
- An environment credential that stores its secret directly on the binding
  cannot be declared. Bind a credential or a provider credential by id.
