# PNNN — Declarative universe configuration

**Status:** Proposed, 2026-11-10. Nothing implemented.

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

### Why the existing API is enough

The resources in scope already have declarative write methods and
client-chosen identifiers:

| Resource | Method | Stable key |
| --- | --- | --- |
| Universe | `deployment/universes/create` (idempotent) | handle |
| Token credential | `auth/grants/import`, `auth/grants/replace` | `grantId` |
| Model or GitHub provider | `auth/providers/create`, `auth/providers/replace` | `providerId` |
| Model defaults | `models/defaults/put` | purpose slot |
| Profile | `profiles/put` | `profileId` |
| MCP server | `mcp/servers/put` | `serverId` |
| Environment provider enablement | `deployment/environment-providers/bindings/put` | provider id |
| Environment | `environments/create`, `environments/external/create` | `requestId` |
| Environment credential binding | `environments/credentials/bind` | environment + variable name |
| API key | `deployment/api-keys/put` (supplied secret) | scope + `keyId` |
| Environment registration key | `environments/registration-keys/put` (supplied secret) | `registrationKeyId` |

`apply` reads current state, compares it with the document, and calls the
write method where they differ, passing `expectedRevision` where the method
accepts one.

### Identity

A universe is declared by its **handle** and a display name. The handle is
the universe's deployment-unique name: every universe has one, the runtime
resolves it wherever it accepts a UUID, and it is the same in every deployment
a document is applied to. The runtime UUID differs between deployments and
never appears in a document.

- `apply` resolves the handle through `deployment/universes/read`. If no
  universe has it, `apply` generates a UUID and creates the universe with that
  handle.
- A changed handle is never treated as a rename. Renames stay an explicit
  `universe set-handle` followed by a document update. A rename releases the
  previous handle, so a document still naming it would create a new universe.
- Secret encryption and workflow identities depend on the UUID, so a document
  cannot rebuild a lost universe. Recovery still restores the database;
  `apply` then finds the universe by handle again.

Resources inside a universe are keyed by the identifiers in the table above,
and cross-references use those identifiers (an MCP header names a `grantId`).

### Document shape

`apply -f` takes a directory. One file per universe keeps diffs readable and
lets `apply` report results per universe. YAML and JSON are both accepted.
Resource bodies are the existing API input documents, so the schema is derived
from the API contract and an exported profile drops in unchanged.

```
config/
  deployment.yaml        # deployment-level resources
  universes/
    research.yaml
  profiles/
    reviewer.json        # existing profile export format
```

```yaml
schemaVersion: 1
handle: research
name: Research
credentials:
  - grantId: tracker-token
    tokenFrom: { env: TRACKER_TOKEN }
modelProviders:
  - providerId: primary-models
    config: { ... }
    credentialFrom: { env: MODEL_PROVIDER_KEY }
modelDefaults:
  chat: { ... }
profiles:
  - file: ../profiles/reviewer.json
mcpServers:
  - serverId: tracker
    serverUrl: https://tracker.example.com/mcp
    headers:
      x-upstream-token: { type: credential, grantId: tracker-token }
environmentProviders: [pods]
environments:
  - requestId: build-box
    providerId: pods
    templateId: default
    credentials:
      TRACKER_TOKEN: { type: grant, grantId: tracker-token }
```

Every document carries `schemaVersion`; these files outlive releases.

### Secrets

A document never contains secret material. It names a source (`env` or
`file`), which the process running `apply` resolves. In a cluster the values
come from the platform's secret store, mounted into the job.

The runtime never returns token material, so `apply` cannot compare values. It
stores a fingerprint of the applied value in the credential's `metadata` and
calls `auth/grants/replace` only when the fingerprint changes.

Keys that Lightspeed itself accepts work the same way, with the direction
reversed from minting: the secret is generated outside, in the secret store,
and declared to the runtime. `deployment/api-keys/put` and
`environments/registration-keys/put` take the key's id, its settings and the
secret. A missing key is created, changed settings are applied, a different
secret replaces the previous one at once with no overlap, and repeating the
call changes nothing. Scope, actor assertion and a registration key's
identity mode never change, and a revoked key is never restored. The service
that uses the key mounts the same secret, so no step reads a secret out of
Lightspeed and `apply` needs no outputs.

These need no fingerprint: the runtime keeps the secret's hash and compares
it itself, and each key's listed prefix (the first characters of its secret)
lets `diff` report a pending replacement without sending the secret.

```yaml
apiKeys:
  - keyId: release-bot
    displayName: Release bot
    groups: [session, models]
    secretFrom: { env: RELEASE_BOT_KEY }
registrationKeys:
  - registrationKeyId: build-pool
    displayName: Build pool
    identityMode: persistent
    maxActiveEnvironments: 5
    secretFrom: { env: BUILD_POOL_REGISTRATION_KEY }
```

Deployment-scope keys, such as the Platform's and a connector's, are declared
in `deployment.yaml`. The first key cannot be: nothing can authenticate
before it exists, so `lightspeed-runtime api-key provision` declares it on
the server host from `LIGHTSPEED_BOOTSTRAP_API_KEY`, with the same semantics.

A registration key must be declared before the daemons holding its secret
start, because a daemon treats an unknown key as a permanent refusal and
exits. An orchestrator that restarts the pod converges anyway.

`export` writes each secret as a reference with a placeholder source name. An
exported document is structurally complete but needs its secrets bound in the
target deployment.

### Ownership and pruning

`apply` touches only resources named in the document. Resources that users
created alongside them are left alone. This partial ownership is the default,
because a central team commonly manages providers and credentials while teams
add their own profiles.

Deletion is opt-in: `apply --prune` removes resources that carry the managed
marker and are absent from the document. Universes are never pruned.
Retiring a universe remains the manual procedure in the deployment guide,
because the runtime purge does not reach every workflow, schedule and machine.

Each applied resource records that it is managed (in `metadata` where the
resource has it). A UI indication, and possibly read-only editing, for managed
resources is a later step.

### Configuration, not state

Documents carry configuration only. Sessions, runs, workspace contents and
bot state are excluded. Environments are declared but treated as stateful:
`apply` creates a missing environment and converges its credential bindings,
and never replaces an existing machine.

### Round trip

`export` followed by `apply` against the same universe must change nothing.
This requires `export` to emit a normalized form: stable ordering, and
defaults either always written or always omitted. The round trip is the main
property to test.

### Environment differences and templates

The format has no templating or interpolation. Values that differ between
deployments are handled by the layer above: chart values in a GitOps
pipeline, or separate directories.

A template is a document applied with a different handle and name
(`apply --handle … --name …`). It is a starting point, not a link: a universe
created from a template does not follow later changes to it.

## Required changes

### Client

- `export`, `apply` and `diff` in `crates/cli`, with the document schema
  generated from the API contract.

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

1. **Format and `export`.** Read-only, and it shows where the resource model
   lacks stable names. Covers universes, credentials (as references), model
   providers, model defaults, profiles, MCP servers, environment provider
   enablement, environments and their credential bindings, API keys and
   registration keys (as references).
2. **`diff` and `apply`** for the same set, with the credential fingerprint,
   managed marker and round-trip tests.
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
- "Create universe from template" in the Platform, using stored documents.
- Managed-resource indication in the web UI.

## Open questions

- Should the Platform step be a Platform endpoint that accepts a service key,
  or should the runtime own a universe display name so `apply` needs no
  Platform call for naming?
- Should a fully managed mode exist, where `apply` prunes everything not in
  the document, or is `--prune` over marked resources sufficient?
- Model defaults require the current `expectedRevision` on every write. Is a
  read-then-put per slot acceptable, or does `apply` want an unconditional
  form?
- Where does the managed marker live for resources without `metadata`
  (model defaults, provider enablement)?
