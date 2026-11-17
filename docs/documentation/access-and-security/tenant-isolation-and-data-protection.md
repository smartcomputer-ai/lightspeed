# Tenant isolation and data protection

A universe separates its Lightspeed records from other universes in the same
deployment. Sessions, profiles, workspaces, bots, credentials, and
environment records belong to a universe. An organization
groups universes for administration and access.

For example, Acorn and Cedar can each have a workspace named
`release-notes`. A request for that name in Acorn resolves to Acorn's
workspace. A person needs access to Cedar to open Cedar's workspace through
the Platform.

## Where the universe boundary applies

The Platform authorizes a request for a universe and forwards it to the
runtime. The runtime scopes database records, stored blobs and caches, and
Temporal workflows to that universe. A delegated child stays in its
parent's universe. Identical blobs in separate universes are stored
separately. A member with permission to read blobs in one universe can
retrieve a known digest there, independently of access to a particular
session. See [Private and shared work](private-and-shared-work.md).

Runtime tokens can select any universe. The runtime and its stores are
trusted infrastructure. Database isolation relies on universe-scoped
queries and schema constraints. Database and storage administrators can
access the infrastructure they operate. Platform Admins have Owner access
across the deployment, and Organization Admins have it within their
organizations.

## What a deployment shares

Universes share runtime processes, database and blob infrastructure, and
worker capacity. A universe does not reserve CPU, memory, or queue throughput.
An environment provider can serve all universes or only the ones it is bound
to. Universe-specific model credentials can use separate provider accounts.
Where a deployment fallback key supplies model requests, those universes
share that provider account and its quota. See [Models and
credentials](../using-lightspeed/models-and-credentials.md).

Use separate deployments when teams need independent infrastructure
administration, encryption keys, capacity, or failure domains. See
[Deployment configuration](../deployment/configuration.md) for configuring
database, storage, and Temporal connections.

## Credentials, files, and external services

Lightspeed encrypts secret material using the deployment's master key. The
ciphertext is bound to its universe and secret identity. Encryption for
database storage, backups, and other blob data is configured in the
infrastructure. Preserve the master key with the [recovery
set](../deployment/upgrades-and-recovery.md#preserve-a-complete-recovery-set).

A registered machine retains the filesystem, network access, and
operating-system permissions supplied by its operator or provider. Its
environment record belongs to a universe. Sessions allowed to use the
machine share its filesystem and may have access to its injected credentials.
VFS workspace files and machine files are separate, with [explicit
transfer](../environments/vfs-transfer.md) between them.

Model providers and external tools receive the inputs sent to them. Their
service permissions and retention policies govern those copies. See [Agent
and tool access](agent-and-tool-access.md) for how a run uses resources and
credentials.

To verify an installation, create a disposable workspace with the same name in
two universes and place different content in each. Sign in as a person whose
access reaches only one universe and confirm that only its workspace is
available. A trusted runtime operator can select either universe and confirm
each resolves to its own content.
