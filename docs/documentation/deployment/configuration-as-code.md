# Configuration as code

A universe's configuration, and through Platform an organization's, can be
kept in files and applied by a pipeline, with the review and promotion flow
you already use for the deployment itself. The `lightspeed config` commands
do this:

| Command | Effect |
| --- | --- |
| `lightspeed config export` | Writes the configuration of a universe, or with `--kind organization` of an organization, as a document. |
| `lightspeed config apply -f FILE` | Makes universes and organizations match documents. |
| `lightspeed config diff -f FILE` | Reports what `apply` would change, and writes nothing. |
| `lightspeed config schema` | Prints the JSON Schema of a document, for editors. |

They use the public API. A document holds what you would otherwise pass
to the API one resource at a time, so anything `apply` does can also be done
by hand.

Service settings are not part of a document. Process roles, addresses, storage and
Temporal connections stay with the chart or Compose file that starts the
services; see [Configure a deployment](configuration.md).

## Before you start

- Through Platform, use a service-user API key for a pipeline, a personal API
  key, or a saved browser-authorized CLI connection. A key acts as its user or
  service user; it does not carry a separate universe role.
- Connect with `LIGHTSPEED_API_URL` and `LIGHTSPEED_API_TOKEN`, or a saved CLI
  connection. Select the organization for new universes with `--organization ORGANIZATION_ID`
  or `lightspeed platform organization use ORGANIZATION_ID`. See
  [CLI connections](../using-lightspeed/cli.md#connect-to-platform).
- Direct runtime connections also work. Use a configured runtime token when
  authentication is enabled, or an explicitly selected unauthenticated single
  runtime connection. Runtime tokens have full deployment authority and belong
  in infrastructure secret stores, outside agent workspaces. See
  [Runtime access](../access-and-security/api-keys-and-service-access.md).
- Documents configure universes and, through Platform, organizations and who
  may use a universe. API keys are issued, never declared, and deployment
  settings belong to the deployment's environment; `apiKeys` sections and
  `kind: deployment` documents are not supported.

### Platform permissions

The principal's current permissions and the key's restrictions both apply to
every request. Choose authority for the work the document will perform:

| Work | Required authority |
| --- | --- |
| Create missing universes in an organization | Organization Creator or Organization Admin; Platform Admin also has organization authority. |
| Reconcile ordinary resources in an existing universe | Universe Operator or Owner; Organization Admin has Owner access throughout its organization. |
| Change universe `displayName`, `metadata`, or enable `protected` | Universe Owner. |
| Clear `protected` | Organization Admin or Platform Admin, and organization policy must permit unprotected universes. |
| Change `environmentProviders` bindings | Platform Admin; see [Environment provider allocation](#environment-provider-allocation). |
| Create an organization | Platform Admin. |
| Change an organization's settings, `membershipPolicy` and `serviceUsers` | Organization Admin. |
| Set an organization's `universeQuota` | Platform Admin. |
| Change a universe's `access` | Universe Owner. |

A Creator becomes the first Owner of each universe it creates. Creator alone
does not grant access to other existing universes; give the principal a
universe grant when needed. Platform Admin creation requires an explicit
`--owner-user USER_ID` or `--owner-service-user SERVICE_USER_ID`. Organization Admins
can also use these flags to assign another initial Owner in the organization.

For automation that creates universes, use an organization-scoped key with creation
allowed. A key restricted to existing universe UUIDs cannot create another
universe, even when its principal is an Admin or Creator. Such a key can
reconcile the universes in its scope, within the principal's granted role.
Operation restrictions must allow discovery, universe reads, the resource
reads used by `diff`, and the writes needed by `apply`; creation also needs
`platform/universes/create`. A read-only key cannot apply changes.

For example, save a Platform service-user key from a mounted secret and select
the organization used for provisioning:

```bash
lightspeed connect add automation --url https://lightspeed.example --token-file /run/secrets/lightspeed-platform-key
lightspeed --connection automation platform organization use ORGANIZATION_ID
lightspeed --connection automation config diff -f config/
lightspeed --connection automation config apply -f config/
```

Use `--organization ORGANIZATION_ID` on an individual command to override that saved
selection.

The organization selection and initial Owner are connection/command context, so the
same universe document can be promoted across organizations and deployments.
Organization selection chooses where missing universes are created; it does not
restrict existing-handle lookup to that organization. Handles identify universes
across the deployment. Restrict the key to the intended organization when the
pipeline must not reconcile a universe in another organization.

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
    # Bound into build-box below, whose commands receive the plain token.
    exposure: retrievable
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

Creation plans print a provisioning UUID. To retry an interrupted creation,
pass the printed UUID with `config apply --universe-id UUID -f FILE`; use the
same flag with `diff` to inspect that retry. This flag requires exactly one
document and refuses a handle that already resolves to a different UUID.

`schemaVersion` identifies the layout of the file. It is not a revision of
your configuration, and it plays no part in deciding what changed.

Every section is optional. A section you leave out is not managed by the
document: `apply` does not read or change it.

### Sections

Each entry is the input of the API method that writes the resource, with the
same field names. The
[HTTP API reference](../../../crates/api/contract/api-reference.md) describes
the fields.

| Section | Method an entry follows | Identified by |
| --- | --- | --- |
| `credentials` | `credentials/grants/put` | `grantId` |
| `providers` | `credentials/providers/put` | `providerId` |
| `modelDefaults` | `models/defaults/put`; a map of slot (`agentRun`, `speechToText`) to model, where `null` clears a slot | slot |
| `mcpServers` | `mcp/servers/put` (the `server` object) | `serverId` |
| `environmentProviders` | `deployment/environment-providers/bindings/put`: `providerId` | `providerId` |
| `registrationKeys` | `environments/registration-keys/put` | `registrationKeyId` |
| `environments` | see below | `environmentId` |
| `profiles` | `profiles/put` (the `profile` object) | `profileId` |

### Environment provider allocation

Most universe documents need no `environmentProviders` section. A deployment
environment provider is available to every universe unless a deployment
administrator limited it to selected universes when registering it. Declare
`source.type: provisioned` environments with such a provider directly, with
Universe Operator authority.

For a provider limited to selected universes, `environmentProviders` lists the
providers this universe is bound to, by `providerId`. Binding grants access to
deployment infrastructure, so Platform keeps it under Platform Admin
authority; Organization Admin and universe Owner do not imply it. A provider
available to every universe already counts as present, so listing one changes
nothing. A provider left out keeps its binding. Provider registration and
availability remain deployment administration and are not a document section.

When a Platform principal is not a Platform Admin, `diff` and `apply` report a
binding the document would add as a conflict before writing any document. An
unchanged binding is allowed. This checks the principal's allocation authority;
the server still checks current credential restrictions and policy on each
request, and access can change between planning and applying.

### Environment entries

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
- The universe is named by the document's `handle`.
- A profile's instructions are written as text, not as a blob reference.
- An entry can live in a file of its own: `- file: profiles/reviewer.json`,
  relative to the document. A profile written by `lightspeed profile export`
  can be included unchanged.

An unknown field is an error, at every level, so a misspelled field never
silently does nothing.

### Give a model endpoint or an MCP server a header

A model provider's endpoint and an MCP server can each send extra headers,
written the same way under `headers`. Each is a tagged value: a `literal`,
which every reader of the provider or server sees, or a `credential`, which
names a token credential whose current token the runtime sends and never
returns. A header that carries a secret by convention (`api-key`, `x-api-key`,
and on a model endpoint `proxy-authorization`) must be a `credential`; `diff`
reports a literal under one of these names before anything is written:

```yaml
credentials:
  - grantId: gateway-key
    displayName: Model gateway
    tokenFrom: { env: GATEWAY_KEY }

providers:
  - providerId: model:gateway
    config:
      type: modelEndpoint
      endpoint:
        baseUrl: https://models.example.com/v1
        apiKinds: ["openai:completions"]
        headers:
          api-key: { type: credential, grantId: gateway-key }
          x-title: { type: literal, value: Lightspeed }
```

The credential a header names is a reference like any other: it must be in the
document or already in the universe, and `apply` writes credentials before the
providers that use them. It can stay `brokered`, because the runtime attaches
it to its own request. Header names are compared in lowercase, which is how the
runtime stores them, so one header written in two spellings is an error.

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

## Configure organizations and access

Through Platform, a document can also declare an organization, and a
universe document can say which organization holds the universe and who may
use it beyond the organization's default access. Organizations, people and
roles are Platform's, so these documents and sections need a Platform
connection.

```yaml
schemaVersion: 1
kind: organization
handle: research
name: Research
baseUniverseRole: viewer
defaultProtected: true
membershipPolicy:
  directoryGroupMappings:
    - { issuer: https://login.example.com, group: research, role: member }
    - { issuer: https://login.example.com, group: research-leads, role: admin }
serviceUsers:
  - { name: deployer, role: creator }
```

`handle` names the organization. `apply` finds the organization by it and
creates it when none has it; creating one needs `name` and a Platform Admin.
The other fields follow `platform/organizations/update`:

| Field | Meaning |
| --- | --- |
| `name` | The organization's name. |
| `baseUniverseRole` | The default universe access: the role every member person holds in every universe. `null` gives members only what they are granted. |
| `defaultProtected`, `requireProtected` | Deletion protection for new universes, and whether every universe must stay protected. |
| `universeQuota` | The organization's own universe limit; `null` follows the deployment default. Only a Platform Admin changes it. |
| `membershipPolicy` | People, by verified email or by company sign-in issuer and subject, and company directory groups that hold organization roles. When present, it replaces the organization's policy. |
| `serviceUsers` | Service users by name, with their organization role, optionally `enabled: false`, and optionally `keys`. A name is unique in its organization. |

A service user's `keys` are declared with secrets from your secret store, so
the secret flows into Lightspeed and never out:

```yaml
serviceUsers:
  - name: deployer
    role: creator
    keys:
      - name: ci
        secretFrom: { file: /run/secrets/deployer-key }
        restrictions: { readOnly: false }
        expiresAt: 2027-12-31T00:00:00Z
```

A key is identified by its `name` among the service user's keys. Its secret is
`lsa_` followed by at least 64 letters, digits, `-` or `_`;
`lightspeed platform api-key generate-secret` makes one. The entry is the
key's desired state: restrictions left out mean none, and an expiry left out
means the key never expires, which a deployment maximum refuses. Platform
compares the secret itself, so `diff` reports a changed secret as
`(secret)` without either side revealing it. `apply` then replaces it in
place: the key keeps its ID, and the old secret stops working at once. Two keys
cannot share a secret. Keys issued in the web app or with
`platform api-key create` are not listed by `export`; a key left out of a
document is kept.

A universe document names its organization with `organization` and declares
access with `access`:

```yaml
schemaVersion: 1
kind: universe
handle: research
organization: research
access:
  directoryGroupMappings:
    - { issuer: https://login.example.com, group: research-ops, role: operator }
  serviceUsers:
    - { name: deployer, role: owner }
```

- `organization` is the handle of the organization holding the universe.
  `apply` creates a missing universe there, so the document needs no
  `--organization`. An existing universe must already be there; universes do
  not move between organizations. Left out, creation uses the selected
  organization.
- `access.members` and `access.directoryGroupMappings` are the universe's
  declared access policy, in the shape of `membershipPolicy` with universe
  roles. Each one present replaces its part of the policy.
- `access.serviceUsers` gives service users of the universe's organization a
  universe role by name. Service users get nothing from the organization's
  default access, so a pipeline's service user needs a role here.

People are given roles one at a time in the web app; documents declare roles
for groups and service users, whose names are the same in every deployment.
Nothing is removed: a service user or grant a document leaves out is kept.

`apply` applies organization documents before universe documents, and finds
organizations and service users by handle and name when each step runs. One
directory can therefore declare an organization, its service users, and the
universes and grants that need them. A grant to a service user that neither
exists nor is declared in the run is reported as a conflict, and nothing is
written. Personal organizations are not configured by documents.

## Supply secrets

A secret source is `{ env: NAME }` or `{ file: PATH }`. The process that
runs `apply` reads it; in a cluster, mount the values from your secret store
into the job. A relative file path is relative to the document. One trailing
line break in a file is dropped.

Every source is read before anything is sent. If one is missing, `apply`
stops without having read or written anything in the runtime.

Secrets flow one way, into Lightspeed. For environment registration keys,
generate the secret in your secret store or with
`lightspeed environment registration-key generate-secret`, and give the same
value to the daemon that uses the key.

When the value behind a source changes, the next `apply` replaces the stored
secret at once. The old secret stops working at that moment, so update the
consumers of a key together with it.

## Check and apply

```bash
lightspeed config diff -f config/
lightspeed config apply -f config/
```

`-f` takes files or directories. A directory is read recursively; every YAML
or JSON file in it that has a `kind` is a document. Files and directories
whose name starts with a dot are skipped, which is also what makes a mounted
ConfigMap or Secret read correctly.

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
| Credential | `exposure`; declare the credential under a new id. |
| Registration key | `identityMode` |
| Environment | `source` |
| A revoked key | Stays revoked; declare the key under a new id. |
| A closed environment | Its id stays taken; declare the environment under a new id. |

A credential's `exposure` says where its plain token may go. `brokered`, the
default, keeps it inside the runtime, which attaches it to its own requests:
an MCP server's credential or header, or a model endpoint's header, needs
nothing more. `retrievable` also
lets the token be bound into an environment and leased by trusted services.
A document that binds a credential into an environment (`source: { type:
authGrant }` in an environment's `credentials` list) must declare that
credential `exposure: retrievable`, or name one that already is. Otherwise
`diff` and `apply` stop before anything is written and name the binding and
the credential. A provider's credential (`authProviderCredential`) can be
bound only when the provider is a model API key provider; see
[Environment credentials](../environments/credentials.md#choose-a-credential-source).

`apply` never releases the daemon bound to a declared environment and never
renames a universe. Both stay explicit commands:
`lightspeed environment release` and
`lightspeed universe set-handle`. After a rename, update the document's
`handle` before the next `apply`, or it creates a new universe under the old
handle.

A universe document can carry `displayName`, bounded `metadata`, and
`protected`. Metadata allows 32 entries, keys up to 64 bytes and values up to
256 bytes; keys starting with `lightspeed.` are reserved. A supplied map
replaces the existing map.

New universes created by `apply` are protected when `protected` is omitted.
For an existing universe, omission preserves the current protection. An
explicit `false` requests clearing protection. Through Platform, only the
containing Organization Admin or a Platform Admin can do this, and mandatory
organization protection must first be removed. Runtime tokens remain full-authority
infrastructure credentials; the runtime still refuses deletion until
protection has been explicitly cleared.

A universe created directly with a runtime token has no Platform organization
association until an administrator adopts it into an organization with an
initial Owner. See [Managing universes](multi-tenancy.md).

## Export a universe

```bash
lightspeed --universe research config export -o config/universes/research.yaml
lightspeed --universe research config export -o research.json --profiles-dir profiles
lightspeed --organization ORGANIZATION_ID config export --kind organization -o config/organizations/research.yaml
```

Through Platform, a universe's export includes its `organization` and, for an
Owner, its `access`.

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
exported file. An export lists only real bindings, to providers limited to
selected universes; review them separately: the destination provider must
exist, and binding through Platform requires Platform Admin authority.

## Run it from a pipeline

`apply` is safe to run on every deployment and on a schedule. The runtime
image contains the `lightspeed` CLI of the same release at
`/usr/local/bin/lightspeed`, so a job needs no other image and always matches
the runtime it talks to. A typical arrangement in Kubernetes:

- Render the documents into a ConfigMap and the secrets into a Secret.
- Run `lightspeed config apply -f /config` as a Job whose name includes a hash of
  the documents, so a changed document triggers a new run.
- Run `lightspeed config diff -f config/` in merge requests against the target
  deployment to show reviewers what would change.

```yaml
apiVersion: batch/v1
kind: Job
metadata:
  name: lightspeed-config-3f9a1c   # include a hash of the documents
spec:
  backoffLimit: 3
  template:
    spec:
      restartPolicy: Never
      containers:
        - name: apply
          image: <the runtime image of your release>
          command: ["/usr/local/bin/lightspeed", "--organization", "ORGANIZATION_ID", "config", "apply", "-f", "/config"]
          env:
            - name: LIGHTSPEED_API_URL
              value: https://lightspeed.example/api/v1
            - name: LIGHTSPEED_API_TOKEN
              valueFrom:
                secretKeyRef: { name: lightspeed-config, key: api-token }
            - name: TRACKER_TOKEN
              valueFrom:
                secretKeyRef: { name: lightspeed-config, key: tracker-token }
          volumeMounts:
            - { name: config, mountPath: /config, readOnly: true }
      volumes:
        - name: config
          configMap: { name: lightspeed-config }
```

The job takes its connection from `LIGHTSPEED_API_URL` and
`LIGHTSPEED_API_TOKEN`. Store the Platform service-user key in the `api-token`
Secret entry and replace `ORGANIZATION_ID` with its organization. Give the service user
Creator authority to create its own universes, or the necessary grants to
reconcile existing ones. It needs no home directory and no saved connection, and
it writes nothing to the container's filesystem, so a read-only root
filesystem works.

Documents and secrets can come from mounted volumes. A secret source of the
form `{ file: /var/run/secrets/tracker/token }` reads a mounted Secret, as
`{ env: … }` reads one injected as a variable. The image runs as user and
group 65532: files mounted with the default mode are readable; if you
restrict a Secret's mode, set the pod's `fsGroup` to 65532 and use mode
`0440`. Keep the authentication key separate from the outbound service
credentials declared in the document, as described under
[Before you start](#before-you-start).
The image's default command is still the server, so name the CLI as the
job's `command`.

Declare a registration key before the daemons that hold its secret start: a
daemon whose key the runtime does not know exits. An orchestrator that
restarts the pod converges anyway. A declared environment and its machine can
be deployed in either order.

## Limits

- Members and roles, bots and triggers, chat channel accounts, workspaces and
  session retention defaults cannot be declared yet.
- Deployment environment providers
  (`deployment/environment-providers/put`) cannot be declared yet; a universe
  document can only enable or disable an existing allocation, with deployment
  administration authority.
- Credentials created through an OAuth sign-in cannot be declared. A document
  can refer to one by its `grantId`.
