# Multitenancy

One Lightspeed deployment can serve many universes. Each universe contains
its own sessions, profiles, workspaces, bots, credentials, channels, and
environment records, while the deployment supplies shared runtime processes
and infrastructure. This allows several teams or customers to use the same
installation without placing their agent data in the same logical space.

A universe is the tenant boundary. It is separate from a person signing into
the Platform, a VFS workspace containing files, or an execution environment
providing a machine. One person can belong to several universes, and one
universe can contain many workspaces and machines.

This page describes the current isolation model and its limits. Follow
[Authentication and access](authentication-and-tenancy.md) to configure the
gateway, create accounts, and issue keys.

## Follow a request into its universe

Consider two teams, Acorn and Cedar. Both can have a profile called
`release-editor`, a workspace called `release-notes`, and a session with the
same session ID. Those identifiers are resolved within the selected universe,
so the records remain distinct.

When someone opens Acorn in the Platform, the Platform checks their access
and looks up Acorn's runtime UUID. It sends that UUID on its private request
to the runtime. The gateway resolves a universe-bound service before
dispatching the operation. A workspace read through that service therefore
looks for Acorn's workspace, even if Cedar has an identically named one.

An API-key gateway reaches the same boundary differently: it resolves the
key to its stored universe and principal. Ordinary resource methods do not
accept a caller-selected universe field that can override that resolution.
Operator methods sit outside this rule because they manage deployment
resources and may explicitly address a universe UUID.

In the multitenant modes, unknown universes are rejected. A misspelled UUID
does not create a new tenant. `single` mode has a different purpose: it pins
ordinary requests to `LIGHTSPEED_PG_UNIVERSE_ID` and ensures that universe
exists.

## How the boundary is represented

Isolation follows the universe through storage and execution:

| Layer | Current mechanism |
| --- | --- |
| Runtime API | Request authentication resolves a universe-bound service before ordinary method dispatch. |
| PostgreSQL | Universe-bound queries and composite keys/foreign keys associate tenant records with `universe_id`. |
| Content-addressed storage | The blob catalog includes the universe in its key. External objects live under `<prefix>/universes/<uuid>/cas/…`. |
| Blob cache | Shared process caches include both universe UUID and blob reference in their lookup key. |
| Temporal | Workflow identities include the universe, and workers resolve the corresponding universe service. |
| Encrypted secrets | Authenticated encryption binds ciphertext to the universe UUID, secret ID, and secret kind. |

For a session, the Temporal workflow ID is `<universe-uuid>/<session-id>`.
Bot controllers and channel conversations use their own universe-prefixed
workflow identities. Sub-agents retain the parent's universe; delegation
does not create an escape into another tenant.

Identical content in two universes is stored separately. A digest identifying
Acorn's blob does not by itself make that content readable through Cedar's
service. There is no cross-universe physical blob deduplication.

The PostgreSQL boundary is implemented by application queries and schema
constraints. The migrations do not define PostgreSQL row-level security
policies. A database administrator, storage administrator, or operator with
deployment access therefore remains outside the tenant boundary. Tenant
isolation does not limit the authority of the installation's own operators.

## What the deployment shares

The gateway and workers share a PostgreSQL pool, optional object-store
connection, provider clients, process memory, and other runtime resources.
Universe services are created as needed over those shared resources; they do
not allocate a separate database or worker fleet for every tenant.

Several records also have deployment-wide meaning. The universe registry and
runtime API-key registry support request routing. Environment providers are
registered at deployment scope, then bound to individual universes. Daemon
identities and provider-native channel account identities have deployment-wide
uniqueness so the same external identity cannot be assigned independently to
two tenants.

Within a deployment, universes share the session, bot, and channel task queues.
This supplies common execution capacity, but it does not reserve CPU, memory,
queue throughput, or provider quota for each universe. Feature-specific
limits, such as a bot's admission policy, do not establish tenant-wide fairness
or resource quotas.

If two deployments use one Temporal namespace, give all three runtime queues
distinct names: `LIGHTSPEED_TASK_QUEUE`, `LIGHTSPEED_TASK_QUEUE_BOTS`, and
`LIGHTSPEED_TASK_QUEUE_CHANNELS`. Changing the session queue does not rename
the others. Separate namespaces also separate workflow identity space and
namespace settings. A universe does not automatically get its own namespace.

### Credentials and billing

A universe can configure its own model providers and credentials. Those
records take precedence over deployment fallbacks. When a built-in provider
record is absent, a deployment-level key can supply the request, so several
universes may consume the same provider account and quota. A disabled or
unusable universe record blocks fallback instead of silently using the shared
key. [Models and credentials](../using-lightspeed/models-and-credentials.md)
explains that distinction.

Encrypted grants and secrets use one `LIGHTSPEED_SECRETS_MASTER_KEY` for the
deployment. The universe binding prevents ciphertext from being moved to
another universe and decrypted there, but there is no separately managed
encryption key per tenant. This setting encrypts secret material; it does not
encrypt every database row or every CAS object. Storage encryption and access
to deployment keys belong to the infrastructure configuration.

### Machines and networks

An environment record belongs to a universe. Its machine has the filesystem,
network access, and operating-system permissions provided by the operator or
environment provider. Registering a machine in a universe does not establish
a new operating-system sandbox around it.

Choose provider templates, host permissions, network policy, and credential
bindings to match the work the tenant should perform. The
[environment overview](../environments/overview.md),
[Incus guide](../environments/incus-vms.md), and
[networking guide](../environments/networking-and-ingress.md) explain these
boundaries. Files in a universe's VFS and files on a machine remain separate.

## Access inside a universe

The core identity directory owns deployment-wide principals and groups, with
explicit universe role assignments. Platform owns login and external mappings;
every interactive runtime call asserts the mapped user through an authenticated
service. DeploymentAdmin supplies no universe membership or session ownership.
Universe administrators can assign Viewer, Contributor, Operator and Admin to
people or groups. The directory is deployment-wide, not a separate identity realm
inside each universe.

Gateway and shared-service enforcement evaluates current roles, explicit service
capabilities and immutable ownership. Contributor control requires ownership;
Operator/Admin may stop another person's session but cannot steer it. Existing
content is readable by universe members. Private-session policies and individual
resource isolation remain follow-ups, so principal attribution alone is not a
privacy boundary. Lists and subsequent requests see committed revocations;
response-time reauthorization of in-flight reads remains separate work.

## Keep Platform and runtime records aligned

Creating a universe through the Platform creates the runtime universe and
records the association in the Platform database. Because these are separate
systems, an interrupted operation or a partial restore can leave one side
without the other.

**Admin → Universes** reconciles the configured default runtime inventory.
**Adopt** connects an existing runtime universe to the Platform without
replacing its contents. **Create in engine** creates an empty missing runtime
universe. It cannot restore deleted sessions or files. Universes using custom
gateway URLs can appear as `unchecked` in this inventory.

Preserve runtime UUIDs and their Platform mappings during recovery. Secret
encryption and workflow identities depend on those UUIDs, so copying records
under a newly invented UUID is not a supported tenant migration. See
[Upgrades and recovery](upgrades-and-recovery.md) for the full recovery set.

## Archive and delete a universe

Archiving changes the universe's Platform status and hides it from the
ordinary switcher. Existing URLs and API requests remain callable, and the
runtime has no corresponding archived state. Bots, schedules, channels, and
previously admitted work can continue. Archiving is therefore an organization
step, not an access-revocation or shutdown mechanism.

Retiring a tenant requires deliberate cleanup before permanent deletion:

1. Stop new submissions, disable or remove triggers, and stop external clients
   and connector ingress for the universe. Revoke runtime keys and remove
   unwanted Platform access.
2. Inspect active sessions, bot activity, channel deliveries, and environment
   jobs. Let required work finish or cancel it deliberately, accounting for
   effects it already performed. Close retired bots to disable their triggers,
   remove their schedules, and request controller teardown.
3. Close the universe's environments and confirm the intended machine cleanup.
   Provider-owned VMs require provider destruction; bring-your-own machines
   remain the operator's responsibility. Retain any files that must survive.
4. Archive the universe in the Platform. A platform administrator can then
   permanently delete it after the required data has been retained elsewhere.
5. Verify remaining infrastructure artifacts, including Temporal schedules
   and histories, provider inventory, connector-local state, and object storage.

The runtime purge terminates the session workflows it enumerates, deletes
catalogued external blobs, and removes runtime rows through database cascades.
It also attempts a best-effort cleanup of the universe's CAS prefix and evicts
the handling process's cached universe service. The Platform then removes its
organization and associated records.

That purge does not enumerate every bot, channel, or environment-job workflow
or every schedule; it does not destroy provider VMs, erase retained Temporal
histories, remove connector-local authentication files, or broadcast cache
eviction to other runtime replicas. Do not treat the delete button as a
complete infrastructure erasure operation. Use the cleanup and verification
steps above, and retain the operator records needed to investigate partial
failure.

## Evaluate the boundary for your deployment

Universes provide logical separation of agent data and execution identity
within one installation. Separate deployments are the appropriate unit when
tenants require independent operator access, database credentials, encryption
keys, infrastructure capacity, or failure domains. Within either arrangement,
review machine permissions and shared provider credentials separately.

To check an installation, create two disposable universes with different
content under the same workspace name. Read each through its own authorized
client, and verify that missing or invalid tenant credentials fail. Inspect
the corresponding universe-prefixed workflow identities. These checks
exercise the configured routing; they complement the underlying storage and
authorization design rather than establishing every isolation property.
