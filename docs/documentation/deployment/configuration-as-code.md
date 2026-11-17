# Configuration as code

Keep universe and shared organization configuration in YAML or JSON. Review
changes with `lightspeed config diff`, then apply the same files to each
deployment with its own secrets. For process settings such as storage and
Temporal, see [Configure a deployment](configuration.md).

## Before you start

Use a [Platform CLI connection](../using-lightspeed/cli.md#connect-to-platform)
for organizations, memberships, and universe grants. For a pipeline, use a
Platform service-user key. A direct runtime connection can apply universe
resources. Keep the runtime token in your infrastructure secret store.

### Platform permissions

| Work | Who can do it |
| --- | --- |
| Create a shared organization or set its universe quota | Platform Admin |
| Set organization policy or manage service users and their keys | Organization Admin |
| Create a universe in an organization | Organization Creator or Admin |
| Change universe settings or access | Universe Owner |
| Write ordinary universe resources such as profiles, credentials, or MCP servers | Universe Operator or Owner |
| Bind a deployment provider limited to selected universes | Platform Admin |

When a Platform Admin creates a universe outside their own organizations, pass
`--owner-user USER_ID` or `--owner-service-user SERVICE_USER_ID` to name its
first Owner. The principal must already belong to the target organization.
An Organization Admin can also use these flags to appoint someone else.

A key's restrictions apply alongside the principal's role. Allow discovery,
the reads needed for planning, and the writes needed for applying. A key
restricted to existing universe UUIDs cannot create another universe. See
[API keys and service access](../access-and-security/api-keys-and-service-access.md).

## Write a universe document

Save this as `config/release-review.yaml`. It defines a review profile in a
universe named `release-review`.

```yaml
schemaVersion: 1
kind: universe
handle: release-review
displayName: Release Review

profiles:
  - profileId: reviewer
    displayName: Reviewer
    instructions: { type: text, text: "Review the change and report risks." }
```

Lightspeed finds the universe by its deployment-wide handle and creates it
when absent. With a Platform connection, set `organization` to the
organization's handle or select an organization with `--organization
ORGANIZATION_ID` when creating the universe. The selected organization is
only a creation default. To check that an existing universe belongs to the
expected organization, put `organization` in the document.

### Sections

Add sections for the resources you want to manage. Each entry uses the
corresponding [HTTP API](../../../crates/api/contract/api-reference.md) fields.
Use [the generated configuration schema](#validate-a-document-as-you-write-it)
to see exact types for each section.

| Section | What it configures |
| --- | --- |
| `credentials`, `providers`, `modelDefaults` | External credentials, providers, and models for agent runs and speech-to-text. |
| `mcpServers` | MCP connections. |
| `profiles` | Agent profiles. Write instructions as text or include an entry with `file: profiles/reviewer.json`, relative to the document. |
| `registrationKeys`, `environments` | Registration keys and registered, provisioned, or external environments. |
| `environmentProviders` | Bindings to deployment providers that are limited to selected universes. |

### Environment provider allocation

A deployment provider serves every universe by default. To use one,
declare a provisioned environment with its `providerId` and `templateId`.
When a Platform Admin restricts a provider to selected universes, the
universe needs a binding. A Platform Admin can declare that binding as an
`environmentProviders` entry with the `providerId`. Leaving one out keeps
its existing binding.

### Environment entries

An environment entry names its `environmentId` and its `source`.

| Source type | Fields |
| --- | --- |
| `registered` | `registrationKeyId` and daemon `name`. See [Declare fixed machines](../environments/registered-daemons.md#declare-fixed-machines-before-their-daemons-register). |
| `provisioned` | `providerId`, `templateId`, and optional `idle` settings. |
| `external` | `endpoint`. |

A profile can refer to one of these environments by ID. To pass an external
token to processes there, create the credential with `exposure: retrievable`
before binding it. See [Environment credentials](../environments/credentials.md).

### Give a model endpoint or an MCP server a header

Headers can be a `literal` visible to readers of the endpoint or a
`credential` whose token the runtime attaches to its requests. Use a
credential for secret values.

```yaml
credentials:
  - grantId: tracker-token
    displayName: Issue tracker
    tokenFrom: { env: TRACKER_TOKEN }

mcpServers:
  - serverId: tracker
    serverUrl: https://tracker.example.com/mcp
    defaultServerLabel: tracker
    headers:
      x-api-key: { type: credential, grantId: tracker-token }
```

The default `brokered` exposure keeps the token available to runtime
requests. For model endpoints, put the same header form under
`providers[].config.endpoint.headers`.

### Validate a document as you write it

Generate the schema with the CLI version you use to apply the files:

```bash
lightspeed config schema > lightspeed-configuration.schema.json
```

For the YAML language server, add
`# yaml-language-server: $schema=./lightspeed-configuration.schema.json`
and adjust the path relative to the YAML file. Unknown fields are errors.

## Configure organizations and access

With a Platform connection, declare a shared organization alongside its
universes. Save this example as `config/research-team.yaml`.

```yaml
schemaVersion: 1
kind: organization
handle: research-team
name: Research
baseUniverseRole: viewer
defaultProtected: true
membershipPolicy:
  directoryGroupMappings:
    - { issuer: https://login.example.com, group: research, role: member }
serviceUsers:
  - name: deployer
    role: member
    keys:
      - name: ci
        secretFrom: { file: /run/secrets/deployer-key }
        expiresAt: "2027-12-31T00:00:00Z"
```

An organization document can set the default universe role for people
(`baseUniverseRole: null` means grants only), deletion protection defaults
(`defaultProtected`, `requireProtected`), and an organization quota
(`universeQuota: null` follows the deployment default). Its
`membershipPolicy` can name verified emails, company issuer and subject
pairs, or company groups. See [People and roles](../access-and-security/people-and-roles.md).

Add an `access` section to the universe document to give the deployer access.

```yaml
organization: research-team
access:
  serviceUsers:
    - { name: deployer, role: operator }
```

Service users get their universe access from explicit grants or organization
Admin authority. A universe can also declare `access.members` and
`access.directoryGroupMappings` in the shape of `membershipPolicy`, using
universe roles. The people referenced there need organization membership.

`apply` processes organization documents before universe documents, so both
can live in one directory. It replaces the declared organization
`membershipPolicy` when supplied, or each part of the universe's people or
group policy when supplied. Other manually assigned grants and memberships
stay in place. Leaving out a service user, its key, or its universe grant
keeps it.

### Manage a declared service-user key

Generate the secret with `lightspeed platform api-key generate-secret` and
save it in your secret store. Refer to it with `secretFrom`. The key's
`restrictions` and `expiresAt` describe its full desired state. Omit
`restrictions` for an unrestricted key acting within the service user's
authority. Omit `expiresAt` for a key that does not expire, where deployment
policy permits it.

Changing the secret in the store and applying replaces the key in place.
Its ID stays the same and the old secret stops working immediately. Use a
second key when consumers must switch without interruption. Exports omit key
entries, so keep declared keys in the source documents you maintain.

## Supply secrets

Use `{ env: NAME }` or `{ file: PATH }` in a secret source. The CLI reads
the value, resolving relative paths from the document. For example, a
credential uses `tokenFrom` and a registration key uses `secretFrom`.
When a key is shared with a daemon, supply the same value on both sides.

Check that the pipeline can read each mounted secret. A missing source stops
planning before writes. Changing a supplied secret appears as `(secret)`
in the diff. The report never prints the value.

## Check and apply

```bash
lightspeed config diff -f config/
lightspeed config apply -f config/
```

`-f` accepts files or directories. The CLI looks in subdirectories for
YAML and JSON files with a `kind`, skipping hidden files and directories.
`diff` exits with 0 for no changes, 2 for planned changes, and 1 for errors
or conflicts. Run it again after applying to check that the deployment
matches your files.

The CLI checks all documents for conflicts before making any writes, then
applies changes in dependency order. If a write fails midway, fix the
cause and rerun. Earlier successful changes are already in place. If universe
creation was interrupted, reuse the provisioning UUID in the report with
`config apply --universe-id UUID -f FILE`.

### What apply changes, and what it leaves alone

`apply` reconciles the resources it names, including changes someone made
in the UI to those resources. Unlisted resources stay in place. When you
supply an environment's `credentials` list, it replaces that environment's
bindings. Omit the list to leave those bindings unmanaged.

Some values are fixed after creation, including an environment's source, a
registration key's identity mode, and a credential's exposure. Use a new ID
when changing one of those. To rename a universe, use
`lightspeed universe set-handle`, then update the document's handle before
applying again.

New universes created by a document are protected by default. For existing
universes, leaving out `protected` preserves their current setting. To
clear it through Platform, an Organization Admin or Platform Admin must set
`protected: false` and organization policy must permit it. See
[Managing universes](multi-tenancy.md#archive-and-delete-a-universe).

## Export a universe

```bash
lightspeed --universe release-review config export -o config/release-review.yaml
lightspeed --organization research-team config export --kind organization -o config/research-team.yaml
```

Review the export before applying it to another deployment. It records
secret sources as placeholders, which you must bind in the target. A
Platform export can include declared access and service-user grants when
the caller can read them. Manually granted roles for people, issued keys,
and credentials created through sign-in stay out of the export. Use
`--profiles-dir` to write profiles as separate files.

## Run it from a pipeline

Use the matching runtime image and invoke `/usr/local/bin/lightspeed`
explicitly as the job command. Set `LIGHTSPEED_API_URL` to the Platform's
`/api/v1` base and inject the service-user key as `LIGHTSPEED_API_TOKEN`.
Mount the configuration and its secret sources so the container user can
read them. A scheduled apply can detect and reconcile drift, or run apply
only when the configuration changes.

The credential that runs the pipeline must already exist. To bootstrap
the example organization and its first service-user key, apply the documents
as a Platform Admin, then use the new key to manage ordinary resources in
that universe. Use the admin connection later for access changes.

For registered daemons, apply their registration keys before starting the
daemon processes. See [Run registered daemons](../environments/registered-daemons.md).

## Limits

Shared organization and universe documents cover configuration and access
policy. Manage personal organizations, deployment settings, provider
registration, bots, channel accounts, workspaces, and session retention
through their own tools or APIs.
