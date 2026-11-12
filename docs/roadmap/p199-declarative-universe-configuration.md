# P199 — Declarative universe configuration

**Status:** Proposed, 2026-11-10; revised 2026-11-12 with the document format
and the resource list. The API prerequisites are implemented as of
2026-11-11: universe handles, declared API and registration keys, declared
credentials and providers, optional `expectedRevision`, and
[declared registered environments](p198-declared-registered-environments.md)
with chosen environment ids. The document format, `export`, `diff` and
`apply` are not started, nor are the Platform step and the CLI image.

## Goal

Describe a universe's configuration as a portable document that Lightspeed can
export from a live universe and apply to another one.

Three uses share that document:

- **GitOps.** An operator keeps universe definitions in a repository. A
  deployment pipeline applies them, with the same review and promotion flow
  that already governs the runtime's own deployment. This is a current need:
  one self-hosted installation deploys Lightspeed through a pull-based Helm
  pipeline and wants its universes, credentials, environments and MCP servers
  governed the same way.
- **Promotion.** A universe configured interactively in a development
  deployment is exported and applied to a test or production deployment.
- **Templates.** A stored document is the starting point for a new universe.

Today each of these is a sequence of UI clicks or hand-written CLI calls, and
nothing records the intended result.

## Design

**The document format stands alone. Deployment machinery only transports it.**
Lightspeed defines the format and three client-side verbs. Helm charts, jobs
and pipelines stay outside the repository's core and call the verbs.

| Verb | Effect |
| --- | --- |
| `lightspeed export` | Reads a universe and writes its configuration document. |
| `lightspeed apply` | Makes a universe match a document. |
| `lightspeed diff` | Reports what `apply` would change. Also serves as drift detection and as a merge-request check. |

All three are public API clients in `crates/cli`. They add no runtime
transport and no server-side reconciler. A Kubernetes operator with custom
resources was considered and not chosen: it needs cluster-scoped installs
that locked-down clusters often refuse, and the existing pipeline already
provides the reconciliation loop once `apply` is idempotent.

**A document is the API's own input, grouped by resource.** There is no
separate configuration language. Each entry is the payload of the method
that writes the resource, with the same field names and nesting. The
document adds only an envelope: which universe, which section a resource
belongs to, and where each secret comes from.

## The document

### Files

`apply -f` takes a file or a directory. A directory is read recursively; each
`.yaml`, `.yml` or `.json` file holds one document. YAML and JSON are the
same format: a YAML document is read as plain JSON data (no custom tags),
and every example below can be written as JSON with the same field names.

```
config/
  deployment.yaml          # kind: deployment
  universes/
    research.yaml          # kind: universe
  profiles/
    reviewer.json          # a profile body, included from research.yaml
```

Every document starts with two fields:

| Field | Meaning |
| --- | --- |
| `schemaVersion` | The version of this format, `1`. It identifies the layout of the file and nothing else: it is not a revision of the content and takes no part in deciding what changed. A CLI refuses a version it does not know. |
| `kind` | `universe` or `deployment`. |

Unknown fields are an error, at every level, so a misspelled field never
silently does nothing.

### A universe document

```yaml
schemaVersion: 1
kind: universe

# The universe's deployment-unique name. apply finds the universe by it and
# creates the universe when no universe has it.
handle: research
# The name people see. The Platform organization owns it.
name: Research

# auth/grants/put — token credentials.
credentials:
  - grantId: tracker-token
    displayName: Issue tracker
    tokenFrom: { env: TRACKER_TOKEN }

# auth/providers/put — model providers and GitHub Apps.
providers:
  - providerId: model:primary
    displayName: Primary models
    config:
      type: modelEndpoint
      endpoint:
        baseUrl: https://models.example.com/v1
        apiKinds: ["openai:completions"]
    credentialFrom: { file: /var/run/secrets/models/api-key }

# models/defaults/put — one entry per slot.
modelDefaults:
  agentRun: { providerId: primary, apiKind: "openai:completions", model: large-1 }

# mcp/servers/put — each entry is the method's `server` object.
mcpServers:
  - serverId: tracker
    displayName: Issue tracker
    serverUrl: https://tracker.example.com/mcp
    defaultServerLabel: tracker
    headers:
      x-upstream-token: { type: credential, grantId: tracker-token }

# deployment/environment-providers/bindings/put — the providers this
# universe may provision from.
environmentProviders:
  - providerId: pods
    enabled: true

# environments/registration-keys/put
registrationKeys:
  - registrationKeyId: runners
    displayName: Runners
    identityMode: persistent
    declaredOnly: true
    maxActiveEnvironments: 2
    secretFrom: { env: RUNNERS_REGISTRATION_KEY }

# environments/registered/put, environments/create and
# environments/external/create, told apart by `source.type`.
environments:
  - environmentId: build-box
    displayName: Build box
    source: { type: registered, registrationKeyId: runners, name: lightspeed-runner-0 }
    # environments/credentials/bind, one entry per variable.
    credentials:
      - envName: TRACKER_TOKEN
        source: { type: authGrant, grantId: tracker-token }
  - environmentId: sandbox
    source: { type: provisioned, providerId: pods, templateId: default }
    idle: { stopAfterMs: 1800000 }
  - environmentId: lab
    source: { type: external, endpoint: wss://lab.example.com/envd }

# profiles/put — each entry is the method's `profile` object, inline or in
# a file of its own.
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
  - file: ../profiles/triage.json

# deployment/api-keys/put — keys scoped to this universe.
apiKeys:
  - keyId: release-bot
    displayName: Release bot
    groups: [session, profiles]
    secretFrom: { env: RELEASE_BOT_KEY }
```

The same document as JSON, shortened:

```json
{
  "schemaVersion": 1,
  "kind": "universe",
  "handle": "research",
  "name": "Research",
  "credentials": [
    { "grantId": "tracker-token", "displayName": "Issue tracker", "tokenFrom": { "env": "TRACKER_TOKEN" } }
  ],
  "environments": [
    {
      "environmentId": "build-box",
      "source": { "type": "registered", "registrationKeyId": "runners", "name": "lightspeed-runner-0" }
    }
  ]
}
```

Every section is optional. A section that is absent is not managed by the
document; see [Ownership and pruning](#ownership-and-pruning).

### Supported resources

| Section | Entry is | Written with | Read with | Id |
| --- | --- | --- | --- | --- |
| `handle`, `name` | the universe | `deployment/universes/create`; the Platform for `name` | `deployment/universes/read` | handle |
| `credentials` | `auth/grants/put` params | `auth/grants/put` | `auth/grants/list` | `grantId` |
| `providers` | `auth/providers/put` params | `auth/providers/put` | `auth/providers/list` | `providerId` |
| `modelDefaults` | a map of slot to `ModelConfig` | `models/defaults/put`, once per slot | `models/defaults/read` | slot (`agentRun`, `speechToText`) |
| `mcpServers` | `McpServerInput` | `mcp/servers/put` | `mcp/servers/list` | `serverId` |
| `environmentProviders` | binding params | `deployment/environment-providers/bindings/put` | `environments/provider-bindings/list` | `providerId` |
| `registrationKeys` | `environments/registration-keys/put` params | `environments/registration-keys/put` | `environments/registration-keys/list` | `registrationKeyId` |
| `environments` | create or put params, by `source.type` | `environments/registered/put`, `environments/create`, `environments/external/create`; `environments/update` for the name and metadata of the latter two | `environments/list` | `environmentId` |
| `environments[].credentials` | `environments/credentials/bind` params | `environments/credentials/bind`, `unbind` | `environments/credentials/list` | `environmentId` + `envName` |
| `profiles` | `AgentProfileInput` | `profiles/put` | `profiles/list`, `profiles/read` | `profileId` |
| `apiKeys` | `deployment/api-keys/put` params | `deployment/api-keys/put` | `deployment/api-keys/list` | scope + `keyId` |

Not in a document:

| Resource | Why |
| --- | --- |
| Members and role or directory-group mappings | Platform records. Later. |
| Bots, triggers, channel accounts | Have `put` methods and stable ids. Later; bot webhook tokens are still only generated by the runtime. |
| Workspaces and shared skills | Content, not settings. Later, as "create if missing from a snapshot". |
| OAuth clients, OAuth grants, GitHub installations | Created through interactive consent. A document can reference such a grant by id; the flow that creates it stays as it is. |
| Self-registered environments | A daemon that registered without a declaration is state. Declare the machine to manage it. |
| Sessions, runs, blobs, bot state, transcripts | State. Backups carry these. |
| Service settings (roles, addresses, storage and Temporal connections) | They configure the processes, before any API exists. The chart or Compose file that starts the services owns them. |

### How an entry differs from the wire format

An entry is the write method's input with these differences, and no others:

| Difference | On the wire | In a document |
| --- | --- | --- |
| Secrets | a value in `token`, `credential` or `secret` | the same field name with `From`: `tokenFrom`, `credentialFrom`, `secretFrom`, each `{ env: NAME }` or `{ file: PATH }` |
| Call mechanics | `expectedRevision`, `dryRun`, `requestId` | absent; `apply` supplies them. A created environment's `requestId` is its `environmentId`. |
| Universe | a `universe` parameter, a key `scope`, or the request's universe | implied by the document's `handle` |
| Wrapper objects | `{ "server": { … } }`, `{ "profile": { … } }` | the inner object is the entry |
| Model defaults | one call per slot | a map keyed by slot |
| Environments | three methods with different parameters | one list; `source` has the shape of the `source` object that `environments/read` returns, reduced to its settings |
| Environment credentials | a call per variable, naming the environment | a list under the environment |
| Includes | none | an entry may be `{ file: PATH }`, a file holding that one entry in JSON or YAML. Paths are relative to the including file. |

Field names stay camelCase, as in the API. An exported profile
(`lightspeed profile export`) is already an `AgentProfileInput` and is
included unchanged.

Two fields need care because their wire form is not portable:

- A profile's instructions may be stored as a blob reference
  (`{ type: textRef, blobRef }`). A blob reference means nothing in another
  universe, so `export` writes the text and a document carries
  `{ type: text, text }`.
- An environment credential bound to a secret stored directly on the binding
  (`directSecret`) cannot be expressed. A document binds credentials and
  provider credentials by id (`authGrant`, `authProviderCredential`).

### A deployment document

Resources that belong to the deployment and not to one universe:

```yaml
schemaVersion: 1
kind: deployment

# deployment/api-keys/put — keys with deployment scope.
apiKeys:
  - keyId: platform
    displayName: Platform
    groups: [session, profiles, deployment/universes]
    assertActor: true
    secretFrom: { env: PLATFORM_API_KEY }
```

The first key cannot be declared this way: nothing can authenticate before
it exists. `lightspeed-runtime api-key provision` declares it on the server
host from `LIGHTSPEED_BOOTSTRAP_API_KEY`, with the same semantics.
Deployment environment providers (`deployment/environment-providers/put`)
join this document later.

## How apply works

`apply` runs four phases. Nothing is written before the third.

1. **Load.** Parse every file, resolve includes, and check the documents
   against the schema. Check references inside the document set: a header
   that names a `grantId`, a profile that attaches an `environmentId`, an
   environment that names a `registrationKeyId`. A reference may also point
   at a resource the document does not manage; that is checked in phase 2.
2. **Read.** Resolve every secret source; a missing variable or file fails
   here, before any write. Resolve each universe by its handle and read the
   current state of every section the document contains. Confirm that
   references to unmanaged resources exist in the target.
3. **Plan.** Compare each entry with the current state and decide: create,
   update, replace a secret, or nothing. `diff` stops here and prints the
   plan.
4. **Write.** Call the write method for each entry that differs, in
   dependency order, and report the result per resource.

### Comparing

The client compares. An entry is parsed into the API's own input type, so
omitted fields take the defaults the server would give them. The resource the
runtime reports for the same id is converted from its read view into that
same input type, which drops the server-derived fields (timestamps, status,
counts, revisions, key prefixes); `export` needs this conversion anyway. The
two values are compared, and an entry that equals the current state causes no
call.

The comparison needs no table of defaults, because it uses the types the
server uses. What it can get wrong is a field the server fills in when the
input leaves it out. Such a field shows up in the round-trip test as a change
that should not be there, and is handled in the conversion.

Secrets cannot be compared by a client, and need not be:

- `auth/grants/put` and `auth/providers/put` take the supplied value, compare
  it with the stored one on the server, and write only what differs. Each
  reports whether it created the resource and whether the secret was
  replaced, and each accepts `dryRun`, which is how `diff` reports a pending
  secret change without making it.
- For keys, the runtime keeps the secret's hash and lists each key's prefix,
  the first characters of its secret. `diff` compares the prefix with the
  supplied secret and never sends it.

The document carries no revision. `apply` passes `expectedRevision` only
where it read one in phase 2, so that an edit made between its read and its
write is a conflict and not a silent overwrite.

`dryRun` on every write method was considered and not chosen. It would add a
parameter and a result to each method, and a dry-run path to prove free of
side effects, for the benefit of `diff` alone. The server compares only what
a client cannot read back, which is the two cases above. `dryRun` can be
added to further methods later without breaking anything, if a client that
does not share the API's types needs `diff`.

### Order

Writes follow references:

1. The universe (created when its handle is unknown), then its Platform
   organization and name.
2. Credentials and providers. A provider that authenticates with a grant
   follows that grant.
3. Model defaults, MCP servers, environment provider enablement.
4. Registration keys, then environments, then environment credential
   bindings.
5. Profiles, which reference models, MCP servers and environments.
6. API keys.

`apply` stops at the first failure and reports what it completed and what
remains. Every write is safe to repeat, so running it again continues from
there, including after a universe was created but its Platform step failed.

### What apply reports

One line per resource: `created`, `updated`, `secret replaced`, `unchanged`
or `failed`, with the field that differed for an update. Secret values never
appear in output. `--json` prints the same as a list.

| Command | Exit status |
| --- | --- |
| `apply` | 0 when the target matches the document afterwards, 1 otherwise |
| `diff` | 0 when nothing would change, 2 when something would, 1 on an error |

### What apply will not change

Some settings are fixed once a resource exists. An entry that differs in one
of them is a failure for that resource, with the manual step named, never a
replacement:

- A universe's handle. A changed handle is not a rename: it names another
  universe. Renames stay an explicit `universe set-handle` followed by a
  document update. A rename releases the previous handle, so a document still
  naming it would create a new universe.
- An API key's scope and actor assertion; a registration key's identity mode;
  a revoked key, which is never restored.
- An environment's source: its kind, its provider and template, its endpoint,
  or the key and name of a declared machine.
- The daemon bound to a declared environment. `apply` never releases one;
  replacing a machine that lost its identity stays an explicit
  `environment registered release`.

## Identity

A universe is declared by its **handle** and a display name. The handle is
the universe's deployment-unique name: every universe has one, the runtime
resolves it wherever it accepts a UUID, and it is the same in every deployment
a document is applied to. The runtime UUID differs between deployments and
never appears in a document.

- `apply` resolves the handle through `deployment/universes/read`. If no
  universe has it, `apply` generates a UUID and creates the universe with that
  handle.
- Secret encryption and workflow identities depend on the UUID, so a document
  cannot rebuild a lost universe. Recovery still restores the database;
  `apply` then finds the universe by handle again.

Resources inside a universe are keyed by the ids in the table above, and
cross-references use those ids. Every one of them is chosen by the document,
so a profile references `build-box` and an MCP header references
`tracker-token` identically in every deployment.

An environment for a fixed machine that runs outside Lightspeed, for example
a StatefulSet pod, is declared with `source.type: registered`. It exists as
soon as the document is applied, and the first daemon its key admits under
that name is bound to it. The universe and the machines can therefore be
deployed in either order.

## Secrets

A document never contains secret material. It names a source (`env` or
`file`), which the process running `apply` resolves. In a cluster the values
come from the platform's secret store, mounted into the job.

Keys that Lightspeed itself accepts work with the direction reversed from
minting: the secret is generated outside, in the secret store, and declared
to the runtime. A missing key is created, changed settings are applied, a
different secret replaces the previous one at once with no overlap, and
repeating the call changes nothing. The service that uses the key mounts the
same secret, so no step reads a secret out of Lightspeed and `apply` needs no
outputs.

A registration key must be declared before the daemons holding its secret
start, because a daemon treats an unknown key as a permanent refusal and
exits. An orchestrator that restarts the pod converges anyway.

`export` writes each secret as a reference with a placeholder source name
derived from the resource's id (`tokenFrom: { env: TRACKER_TOKEN }`). An
exported document is structurally complete but needs its secrets bound in the
target deployment.

## Ownership and pruning

`apply` touches only resources named in the document. Resources that users
created alongside them are left alone, and so is every resource of a section
the document does not contain. This partial ownership is the default, because
a central team commonly manages providers and credentials while teams add
their own profiles.

Deletion is opt-in: `apply --prune` removes resources that carry the managed
marker and are absent from the document. Universes are never pruned.
Retiring a universe remains the manual procedure in the deployment guide,
because the runtime purge does not reach every workflow, schedule and machine.

Each applied resource records that it is managed (in `metadata` where the
resource has it). A UI indication, and possibly read-only editing, for managed
resources is a later step.

## Configuration, not state

Documents carry configuration only. Sessions, runs, workspace contents and
bot state are excluded. Environments are declared but treated as stateful:
`apply` creates a missing environment and converges its name, metadata and
credential bindings, and never replaces an existing machine.

## Export and the round trip

`export` reads the sections in the table above and writes a document in
normalized form: entries ordered by id, server-derived fields removed, and
optional fields omitted when they hold their default. `--format yaml|json`
selects the syntax, and `--profiles-dir` writes each profile to a file of
its own and includes it.

It leaves out what is not configuration: closed environments, self-registered
environments, and revoked or expired keys.

`export` followed by `apply` against the same universe must change nothing.
The round trip is the main property to test, and it is what keeps the
client-side comparison honest: a field the server fills in and the
conversion does not account for shows up as a change that should not be
there.

## Environment differences and templates

The format has no templating or interpolation. Values that differ between
deployments are handled by the layer above: chart values in a GitOps
pipeline, or separate directories.

A template is a document applied with a different handle and name
(`apply --handle … --name …`). It is a starting point, not a link: a universe
created from a template does not follow later changes to it.

## Required changes

### Client

- `export`, `apply` and `diff` in `crates/cli`.
- The document schema, generated from the API contract: the entry schemas
  are the contract's own input schemas with the differences listed above
  applied. Published with the contract so editors can validate a file.

### Platform

The runtime has no universe display name; the Platform organization owns it,
and a runtime-only universe has no members until an administrator adopts it.
The Platform's universe routes (`POST /universes`, `/adopt`) authenticate a
browser session today. `apply` needs a way to create or adopt the
organization and set its name with a service credential. Without it a
declared universe exists but nobody can sign in to it.

### Runtime

- None outstanding. `lightspeed-runtime api-key provision` accepts `--key-id`
  and `--group`, so a pipeline can supply a narrowly scoped key from its
  secret store. `apply` needs `deployment/universes`,
  `deployment/environment-providers`, `deployment/api-keys`, `auth`, `models`,
  `profiles`, `mcp` and `environments`, and no actor assertion.

### Release

- The release builds the `lightspeed` binary but no container image contains
  it. Publish one so a cluster job can run `apply`.

## Deployment integration

This lives outside the core repository and is shown for orientation. A small
configuration chart renders the documents into a ConfigMap and runs
`lightspeed apply` as a job whose name includes a content hash, so a changed
document triggers a new run. The chart depends on the runtime's chart and is
versioned separately, so a profile edit does not roll the runtime. Between
runs, interactive edits to managed resources persist; a scheduled job running
the same command closes that gap where it matters.

A ConfigMap holds at most 1 MiB. Split per universe if profile or skill
content approaches that.

## Slices

1. **Format and `export`.** Read-only, and it shows where a read view does
   not map cleanly onto its write input. Covers every section in the
   resource table.
2. **`diff` and `apply`** for the same set, with the managed marker and
   round-trip tests.
3. **Platform organization** creation and naming with a service credential.
4. **The CLI image.**

## Later

- Members and role or directory-group mappings.
- Overlapping secrets during a key replacement, so consumers can be rolled
  without a gap.
- Bot webhook tokens, which the runtime still only generates.
- Bots and triggers (`bots/put`, `bots/triggers/put`), and channel accounts.
- Workspace seeds and shared skills, created if missing from a snapshot.
- Session retention defaults.
- Deployment-level environment providers
  (`deployment/environment-providers/put`).
- An offline `validate` that runs the load phase only.
- "Create universe from template" in the Platform, using stored documents.
- Managed-resource indication in the web UI.

## Open questions

- Should the Platform step be a Platform endpoint that accepts a service key,
  or should the runtime own a universe display name so `apply` needs no
  Platform call for naming?
- Should a fully managed mode exist, where `apply` prunes everything not in
  the document, or is `--prune` over marked resources sufficient?
- Where does the managed marker live for resources without `metadata`
  (model defaults, provider enablement)? Caller metadata also refuses the
  reserved `lightspeed.` prefix today, so the marker needs either an
  exception or a name outside it.
- Which provisioned environments are configuration? `export` cannot yet tell
  a long-lived machine an operator created from one a session or a bot
  created for its own use.
