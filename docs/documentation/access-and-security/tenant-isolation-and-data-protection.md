# Tenant isolation and data protection

A universe separates one team's Lightspeed records from another team's within
the same deployment. Sessions, profiles, workspaces, bots, credentials,
channels, and environment records belong to a universe. The runtime processes
and infrastructure serving those universes can be shared.

Consider Acorn and Cedar, each with a workspace named `release-notes`. A
request authenticated for Acorn resolves that name inside Acorn's universe.
It does not reach Cedar's workspace, even when the two names are identical.
The same boundary follows requests into storage and workflows.

## How the universe follows a request

A universe API key selects its one stored universe. A deployment key can
address a universe by supplying its UUID in `x-lightspeed-universe`, subject
to the key's method groups. The Platform uses this second path after checking
the person's membership and permissions. The runtime resolves a service bound
to that universe before dispatching ordinary resource operations.

| Layer | How resources remain separate |
| --- | --- |
| Runtime API | Key authentication resolves the request's universe and allowed method groups. |
| PostgreSQL | Universe-bound queries and composite keys/foreign keys associate tenant records with `universe_id`. |
| Content-addressed storage | The blob catalog includes the universe in its key. External objects live under `<prefix>/universes/<uuid>/cas/…`. |
| Blob cache | Cache lookups include both the universe UUID and blob reference. |
| Temporal | Workflow identities include the universe, and workers resolve the corresponding universe service. |
| Encrypted secrets | Authenticated encryption binds ciphertext to the universe UUID, secret ID, and secret kind. |

A session's Temporal workflow ID is `<universe-uuid>/<session-id>`. Bots and
channel conversations also use universe-prefixed workflow IDs. A delegated
child stays in its parent's universe.

Identical blobs in two universes are stored separately; there is no physical
blob deduplication across universes. Knowing Acorn's content digest does not
let a caller read those bytes through Cedar's service. Inside Acorn, however,
`blobs/read` accepts a digest without a session visibility check. A caller
allowed to read blobs can retrieve known digests in that universe. This is
one reason [private session visibility](private-and-shared-work.md) is a
narrower property than isolation between tenants.

PostgreSQL isolation uses application queries and schema constraints, not
PostgreSQL row-level security policies. Database and storage administrators
remain able to access the infrastructure they operate. The Platform also gives
its administrators Admin access in every universe. These are trusted
administrative boundaries, not accounts constrained by an ordinary member's
session visibility.

## What a deployment shares

Universes share runtime processes, a PostgreSQL pool, optional object storage,
provider clients, and other process resources. Session, bot, and channel task
queues provide common worker capacity. Creating a universe does not reserve
CPU, memory, queue throughput, or provider quota for it. A bot's own limits
can bound its work without establishing tenant-wide resource quotas.

Some registries have deployment-wide meaning: universes and API keys support
routing, and environment providers are registered once before being bound to
universes. Daemon identities and provider-native channel account identities
have deployment-wide uniqueness.

Separate deployments are the appropriate unit when teams need independent
operator access, database credentials, encryption keys, infrastructure
capacity, or failure domains. If deployments share a Temporal namespace, give
all three runtime task queues distinct names. A separate namespace also
separates workflow identity space and namespace settings. See
[deployment configuration](../deployment/configuration.md).

## Credentials and encryption

Secret values and grant tokens use the deployment's
`LIGHTSPEED_SECRETS_MASTER_KEY`. Binding ciphertext to its universe and secret
identity prevents moving it to a different identity and decrypting it there.
There is no independently managed encryption key per universe. This mechanism
encrypts secret material; encryption of database storage, backups, and CAS
objects belongs to the infrastructure configuration. Preserve the master key
as part of the [recovery set](../deployment/upgrades-and-recovery.md).

A universe can configure its own model credentials. When a built-in provider
record is absent, a deployment fallback key can supply the request, so several
universes may consume the same provider account and quota. A disabled or
unusable universe record blocks fallback rather than silently using that
shared key. [Models and credentials](../using-lightspeed/models-and-credentials.md)
explains credential selection.

Credentials used by tools deserve the same attention as credentials used to
enter Lightspeed. Sharing an environment also shares access to credentials
injected into processes on that environment. Keeping a session private does
not narrow the external permissions of those credentials.

Bot trigger metadata also carries access material. Current trigger read and
list responses include bearer webhook ingest paths and chat pairing codes,
and the Platform permits those methods from Viewer upward. Treat those values
as available to universe members; they are not restricted to bot managers.
See [Bots and triggers](../using-lightspeed/bots-and-triggers.md) for trigger
setup and the separate HMAC and pairing mechanisms.

## Machines, files, and external services

An environment record belongs to a universe, while its machine has the
filesystem, network access, and operating-system permissions supplied by its
operator or provider. Registering a machine does not create an additional
operating-system sandbox around it. Choose templates, host permissions,
network rules, and credential bindings for the work that should run there.

The VFS and an environment's filesystem are separate domains. Files transfer
only through explicit operations; session visibility does not make a shared
workspace or machine private. See [using environments](../environments/using-environments.md),
[environment credentials](../environments/credentials.md), and
[networking and ingress](../environments/networking-and-ingress.md).

Model providers and external tools receive the inputs sent to them. Universe
scoping inside Lightspeed does not impose a retention policy or access model
on those services. Their credentials and deployment configuration determine
that part of the boundary.

## Check an installation

Create two disposable universes with different content under the same
workspace name. Read each through its own universe key, and verify that the
other universe's content is unavailable. Inspect the corresponding
universe-prefixed workflow IDs. These checks exercise the installation's
routing; they do not establish every isolation property on their own.

For adopting existing universes, preserving their UUIDs during recovery, and
retiring a tenant, follow [Managing universes](../deployment/multi-tenancy.md).
