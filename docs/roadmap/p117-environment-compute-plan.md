# P117: Environment Compute Target Architecture

Status: target design agreed 2026-08-10; scope trimmed 2026-08-11 after review
(trusted-tenant v1, tunnel-first ingress, cursory backend sketches); split into
delivery milestones 2026-08-11.

This is the umbrella target architecture for dynamic and directly enrolled
Lightspeed environments across the `lightspeed` and `ls.bot` repositories.
It is not one implementation completion unit. Delivery is split across
[P118](p118-environment-domain-and-lifecycle.md),
[P119](p119-environment-daemon-gateway-enrollment.md),
[P120](p120-incus-environment-provider.md),
[P121](p121-environment-public-ingress.md), and
[P122](p122-incus-multi-node-pool.md).

The subsystem already has useful implemented boundaries. Preserve
universe-owned environments, event-sourced active session selection, explicit
VFS/environment tool domains, environment-owned credential bindings, and
provider-owned jobs from P104/P108/P113. Replace the universe-scoped physical
provider registry, attach-only `host-bridge`, synchronous create-before-record,
and direct per-host connection model. Breaking changes remain acceptable; do
not add dual writes, legacy aliases, migration layers, or permanent adapters
for the provider-as-singleton design.

## Product definition

**Environment compute lets a universe acquire, enroll, select, and operate a
durable execution environment without making sessions or higher-level products
own machines.**

A capable environment gives an ordinary Lightspeed session enough authority to
build substantial software:

- a real filesystem and repository checkout;
- interactive processes, PTYs, and durable jobs;
- Docker, systemd, language toolchains, and build caches;
- environment-scoped credentials resolved at execution time;
- authenticated access to its own Lightspeed universe;
- optional public ingress for APIs and frontends; and
- a lifetime independent of any one session.

Foundry is one potential consumer of this substrate. It may add durable events,
manager policy, release history, and operational supervision, but it must not
be required to provision a machine or deploy software. A plain session with an
appropriately configured environment remains a first-class product path.

## Goals

1. Register a physical compute provider once at operator scope and safely share
   it across many universes.
2. Bind an approved provider to a universe with explicit templates, quotas,
   and ingress policy.
3. Provision universe-owned VMs dynamically through the existing environment
   lifecycle surface.
4. Enroll an existing laptop, workstation, server, or manually managed VM
   directly as an environment without pretending it is a provider.
5. Use one canonical in-environment daemon, `lightspeed-envd`, for enrolled
   hosts and managed VMs.
6. Keep the environment and host-protocol model neutral to Incus, Firecracker,
   KubeVirt, and provider-native execution adapters.
7. Give every environment a stable logical identity while fencing stale boots,
   restored snapshots, duplicate daemons, and superseded connections.
8. Make provisioning and close idempotent, observable, and recoverable across
   controller, gateway, provider, and node restarts.
9. Allow an environment to expose software on the internet without exposing
   its host-protocol endpoint, SSH, Docker daemon, or Lightspeed gateway.
10. Maintain one authoritative owner for each durable fact and avoid a third
    database until coordination state proves necessary.

## Non-goals

- Building a hypervisor, VMM, VM image format, SDN, or distributed consensus
  system.
- Making Kubernetes a prerequisite for environment compute.
- Treating a session as the owner of an environment.
- Giving a universe permission to register arbitrary infrastructure endpoints.
- Giving an AI agent direct access to Incus, Kubernetes, Firecracker, or root
  node credentials.
- Exposing arbitrary Incus images or raw provider configuration to users.
- Routing interactive filesystem and process calls through Temporal workflows.
- Making Foundry the deployment or provisioning primitive.
- Providing live migration, cross-region HA, billing, GPUs, Windows guests, or
  arbitrary user-supplied images in the first implementation.

## Greenfield replacement

Replace the current model directly:

- universe-scoped physical provider records become one operator-scoped
  provider plus universe-scoped bindings;
- `host-bridge` provider registration becomes direct environment enrollment;
- `host-bridge` becomes `lightspeed-envd` and connects outbound through the
  environment gateway;
- provider-managed VMs run the same envd implementation used by enrolled
  hosts;
- required `providerId`/`providerTargetId` fields on every environment become
  an explicit `Provisioned | Enrolled` source plus incarnation-scoped target
  facts;
- synchronous create-before-record becomes persist-intent-first asynchronous
  provisioning;
- provider `attachTarget` and `AttachedHost` creation disappear; enrollment is
  the only path for an existing independently owned machine;
- session-scoped host targets disappear; environments are universe resources
  selected by sessions;
- raw image names and unrestricted provider options become curated template IDs
  plus bounded overrides;
- inbound per-host WebSocket addresses become gateway-routed outbound daemon
  connections; and
- the current per-universe provider API key is removed from infrastructure
  identity.

Delete obsolete API methods, DTO variants, tables/columns, UI assumptions,
tests, and deployment assets as the target replacements land. Do not preserve
the old shapes behind deprecated names.

## Terminology

### Environment provider

An operator-approved physical compute backend. It can discover templates and
create, inspect, and close provider targets. Examples are one shared Incus
pool, a future Firecracker sandbox pool, or a future KubeVirt cluster.

A provider is global infrastructure. It is registered once, has one physical
identity and liveness record, and is never owned by a universe or session.

### Universe provider binding

A universe's entitlement to use a provider. The binding supplies tenant policy:

- allowed templates;
- environment, CPU, memory, and disk quotas; and
- public-ingress permission.

Several universes can bind the same provider without creating another provider
process or physical registration.

### Environment

A universe-owned logical execution resource. It has a stable `environmentId`
and can outlive sessions. A session only selects an environment; it never owns
or copies it.

An environment comes from exactly one source:

```text
EnvironmentSource
  Provisioned
    providerId
    providerBindingId

  Enrolled
```

Provisioned environments have provider-owned machine lifecycle. Enrolled
environments have externally owned machine lifecycle.

Target and admission facts have a shorter lifetime than the logical
environment and therefore belong to its current incarnation:

```text
EnvironmentIncarnation
  incarnationId
  provisionRequestId?
  providerTargetId?
  templateVersion?
  imageFingerprint?
  admittedDaemonId?
```

Enrollment tickets and daemon-identity history are separate records. Rebuild,
replacement, and reenrollment may replace those facts without changing the
stable environment or its source kind.

### Environment daemon

`lightspeed-envd`, shortened operationally to `envd`, is the canonical daemon
that implements host-protocol filesystem, process, PTY, and job operations on
the environment's own operating system.

It replaces the `host-bridge` project and name. “Agent” remains reserved for AI
agents.

### Environment gateway

The authenticated data-plane broker. Environment daemons connect outbound to
it. Lightspeed workers route host-protocol calls through it to the current
connection for an environment incarnation.

The gateway is independent of any compute provider. It serves provisioned VMs
and directly enrolled hosts identically.

### Environment identity

Four identities describe different lifetimes:

```text
environmentId
  Stable universe-owned logical environment.

incarnationId
  Current admitted provisioning/boot generation. Rebuild, replacement, or
  snapshot cloning creates a new incarnation.

daemonId
  Public-key fingerprint for one envd installation. It survives ordinary
  daemon and machine restarts but can be rotated or revoked.

connectionId
  One ephemeral WebSocket, HTTP/2, or relay connection. It changes on every
  reconnect.
```

The `daemonId` is not a hardware fingerprint. `envd` generates its key locally
and proves possession on reconnect. Disposable VMs may use one daemon identity
for only one incarnation.

### Provider target

The provider's physical object, such as an Incus VM name, Firecracker microVM,
or KubeVirt `VirtualMachine`. A provider target is not a Lightspeed environment
until it is associated with a universe-owned `environmentId`.

### Environment template

A supported environment contract, not merely a boot image. A template selects
an immutable image revision plus instance type, resource policy, bootstrap,
networking, capabilities, lifecycle, and ingress eligibility.

## Core architecture

```text
                              trusted application plane

 users / AI agents
        │
        ▼
 Lightspeed API and sessions
        │
        ├──────── logical environment state ───────► Lightspeed Postgres
        │
        ├──────── provider lifecycle calls ────────► Environment Provider
        │                                                │
        │                                                ▼
        │                                     Incus / Firecracker / KubeVirt
        │                                                │
        │                                                ▼
        │                                           VM or sandbox
        │                                                │
        │                                       lightspeed-envd
        │                                                │ outbound
        ▼                                                ▼
 Environment Gateway ◄──────────────────────── authenticated connection
        │
        └──────── direct host-protocol data plane ─────► filesystem/process/jobs
```

Control and data planes remain separate:

- providers own target creation and destruction;
- Lightspeed owns environment identity, authorization, credentials, and
  selection;
- the environment gateway owns connection admission and routing;
- `envd` or a conforming provider-native adapter owns execution semantics; and
- Temporal owns durable environment-job supervision where already defined, not
  individual file or process calls.

## Design invariants

1. A provider is global; a provider binding is universe-scoped.
2. A singleton host is an environment, not a one-target provider.
3. Every environment belongs to exactly one universe.
4. Sessions select environments but do not own their lifecycle.
5. One provider target maps to at most one live environment incarnation.
6. Environment creation and provider creation are idempotent by one stable
   caller-supplied request ID recorded by Lightspeed before provider I/O.
7. VM running and environment ready are different states.
8. An environment is ready only when its data plane has negotiated current
   capabilities.
9. Every data-plane request is fenced by `environmentId + incarnationId`.
10. A stale daemon, restored snapshot, or superseded connection cannot serve
    calls after a new incarnation is admitted.
11. Base images and snapshots contain no live enrollment token, daemon private
    identity, universe API key, or environment credential.
12. The provider never receives general universe credentials.
13. Environment credentials are resolved by Lightspeed at process or job start
    and sent only to the selected environment data plane.
14. Provider type is not part of model-facing tool behavior. Tools consume
    generic filesystem, process, and job traits backed by `HostConnectionSpec`.
15. A provider-native adapter and `envd` must pass the same host data-plane
    conformance suite.
16. Public application ingress never exposes the environment gateway or host
    control protocol.

## Data ownership

### Lightspeed Postgres

Lightspeed is authoritative for:

- global physical provider records and liveness;
- universe provider bindings and policy;
- stable environment identities and source;
- desired lifecycle and observed readiness;
- provisioning request IDs;
- environment incarnations and fencing generation;
- enrollment token hashes and expiry;
- admitted daemon public-key identity and revocation;
- stable gateway routing identity and last observed data-plane availability,
  never the ephemeral socket or `connectionId`;
- environment credential bindings;
- session environment selection; and
- provider/environment audit events required by the product.

### Incus or another compute backend

The provider backend is authoritative for:

- actual VM/container existence and power state;
- disks, snapshots, backend networks, and backend image cache;
- physical CPU, memory, disk, and network consumption;
- node-local target placement; and
- backend-specific failure information.

Targets carry enough immutable Lightspeed metadata to reconcile after a
provider restart:

```text
lightspeed.environment-id
lightspeed.incarnation-id
lightspeed.provision-request-id
lightspeed.provider-binding-id
lightspeed.template-version
lightspeed.image-fingerprint
lightspeed.created-at
```

Do not place bearer credentials or raw universe secrets in backend metadata.

### Environment gateway runtime

The environment gateway is authoritative only for live transport facts:

- active sockets and their ephemeral `connectionId` values;
- current stream correlation, backpressure, and cancellation state;
- which admitted daemon/incarnation currently owns a route; and
- immediate ping/disconnect observations.

These facts remain in gateway memory. Lightspeed may persist a bounded
availability observation, but a stored observation never makes a dead socket
live and is not used to reconstruct transport state after restart.

### ls.bot Postgres

ls.bot continues to own humans, universe membership, and platform-level
commercial or administrative policy. It calls Lightspeed operator and universe
APIs. It does not mirror provider inventory, environment status, template
catalogs, daemon identity, or session selection.

Future billing plans may determine which provider binding policy the platform
is allowed to create, but the effective binding remains in Lightspeed.

### Deployment configuration and secrets

Versioned deployment configuration owns:

- compute node endpoints and labels;
- provider template catalog;
- global image sources;
- ingress base domains;
- default resource classes; and
- provider/gateway service settings.

SOPS-managed deployment secrets own:

- Incus remote client credentials;
- provider service identity;
- gateway signing/encryption keys; and
- image registry or DNS provider credentials.

### No provider database in v1

The provider is a mostly stateless scheduler and reconciler. Durable intent is
in Lightspeed; physical truth is in the backend; active operations and node
metrics can initially remain in provider memory.

Add provider persistence only when non-derivable coordination state is needed,
such as:

- multi-replica provisioning operation leases;
- strong concurrent capacity reservations;
- durable image rollout status;
- non-derived public hostname allocation;
- historical billing usage; or
- durable guest-enrollment audit beyond Lightspeed's environment record.

If introduced, persist only the coordination ledger, not a third copy of every
environment and VM field. Use a dedicated schema in the existing Postgres
backbone rather than a new database server.

## Multi-tenant provider model

### Physical provider admission

Creating or deleting a physical provider is operator-only. Registering a
provider grants trusted infrastructure code the ability to allocate compute
and causes Lightspeed to communicate with its controller. A universe must not
be able to register an arbitrary endpoint.

v1 has exactly one provider and no operator CRUD API or admin UI. The provider
record is seeded from versioned deployment configuration at deploy time;
changing it is a configuration change. A programmatic operator API is deferred
until there is more than one provider or an external operator.

The record contains stable provider identity, authenticated controller
connection, implementation/protocol version, lifecycle and template
capabilities, global health, and operator metadata.

The first Incus provider is an internal service on the trusted application
plane. Authenticate controller traffic with deployment identity over the
private network. Do not retain the current universe bearer key as provider
identity.

An externally operated provider may later establish an authenticated outbound
control connection, but it still requires operator admission.

### Universe bindings

Target universe/operator API:

```text
operator/universes/provider-bindings/put
operator/universes/provider-bindings/delete

environments/provider-bindings/list
environments/provider-bindings/read
```

Initially only the platform operator creates bindings, through the operator
API or CLI; there is no operator admin UI in v1. Later a universe admin
may enable a provider offering that the platform has explicitly made
self-service. Physical provider registration remains operator-only.

One binding contains:

```text
bindingId
universeId
providerId
status
allowedTemplateIds
maxEnvironments
maxCpu
maxMemoryBytes
maxDiskBytes
publicIngressAllowed
revision
createdAtMs
updatedAtMs
```

There is at most one binding for a `(universeId, providerId)` pair. Binding
policy is replaced whole with `expectedRevision`; field-level update
vocabulary is not introduced. Disabling a binding prevents new provisioning
but does not strand existing environments. Deletion is rejected while a
non-closed environment references the binding.

Bindings store immutable template-version IDs, not moving aliases. Lightspeed
enforces `maxEnvironments` and aggregate CPU/memory/disk quotas
transactionally when it persists the environment intent. Provisioning,
failed, ready, offline, and closing environments retain their reservation;
only `Closed` releases it. The provider enforces per-instance template limits
and performs no aggregate quota accounting.

Every tenant-affecting target lifecycle request includes an authenticated
binding context. Global initialize, health, and operator inventory calls use
operator context instead. The provider must never authorize a raw universe ID
supplied in opaque user options.

Provider target listing is filtered by binding. A caller for binding A cannot
discover, inspect, or close binding B's targets.

## Environment sources and lifecycle

### Provisioned environment flow

```text
1. Universe caller generates a stable requestId and calls
   environments/create(requestId, bindingId, templateId, options).
2. Lightspeed validates binding, immutable template entitlement, and quota.
3. In one transaction Lightspeed reserves quota and allocates/persists:
     environmentId
     incarnationId
     provisionRequestId = requestId
     pending daemon-enrollment ticket
4. Repeating the same requestId returns the same environment.
5. The Lightspeed lifecycle reconciler calls provider createTarget with stable
   ids, bounded template parameters, binding context, and the opaque bootstrap
   ticket.
6. Provider idempotently creates or finds the target.
7. Provider injects bootstrap material through cloud-init, metadata, config
   drive, Kubernetes Secret, or an equivalent backend channel.
8. envd boots and connects to the environment gateway.
9. Gateway validates the ticket, admits the daemon identity, negotiates
   capabilities, and opens the incarnation lease.
10. Provider/backend readiness and gateway data-plane readiness converge.
11. Lightspeed marks the environment Ready and it becomes selectable.
```

`environments/create` returns the environment resource even when it is still
provisioning. Clients poll/read or subscribe to status; they do not hold an HTTP
request open for an entire VM boot.

One Lightspeed-owned reconciliation loop drives pending create and close
intents from Postgres. It retries the idempotent controller operations and
reconciles provider/gateway observations after process restarts. v1 runs one
active reconciler; multi-replica operation leasing is deferred. The provider
does not need its own database: it reconstructs targets from backend inventory
and immutable Lightspeed metadata.

Target lifecycle:

```text
Provisioning
Booting
WaitingForDaemon
Ready
Offline
Closing
Closed
Failed
Unknown
```

Stop/start of a provisioned VM without closing it is deferred; a v1
environment is running, unavailable, or closed. The provider reports backend
state. The gateway reports connection state. Lightspeed derives user-visible
readiness from both without fabricating a single owner's observation.

### Direct enrollment flow

Target universe API:

```text
environments/enrollments/create
environments/enrollments/read
environments/enrollments/revoke
```

Flow:

```text
1. An authenticated universe caller creates a pending enrolled environment.
2. Lightspeed allocates environmentId and incarnationId and returns a
   short-lived, one-time enrollment token.
3. User installs/runs lightspeed-envd on the existing machine.
4. envd generates a local key pair and connects outbound to the gateway.
5. Gateway consumes the token and records its public-key fingerprint as
   daemonId.
6. envd negotiates bounded roots and capabilities.
7. Environment becomes Ready.
```

The token authorizes only one daemon enrollment for one pending environment.
It is not a general universe API key.

Closing an enrolled environment revokes its daemon/incarnation and removes it
from selection. It does not shut down the underlying laptop or server.

### Reconnect, rebuild, and clone semantics

- Ordinary reconnect: same `environmentId`, `incarnationId`, and `daemonId`;
  new `connectionId`.
- Daemon reinstall with approval: same environment/incarnation, new daemon
  identity, previous identity revoked.
- Provider VM reboot: normally same environment/incarnation/daemon identity;
  new connection.
- VM rebuild or restore as replacement: same environment, new incarnation and
  fresh daemon enrollment.
- Clone into a distinct environment: new environment, incarnation, and daemon
  identity.
- Two simultaneous connections for one daemon/incarnation: gateway admits one
  current connection and fences the superseded connection.

Never publish or snapshot an already enrolled golden machine. Base snapshots
must predate identity generation and receive fresh enrollment material when
instantiated.

## Environment gateway

The gateway is a dedicated Lightspeed deployment role, not a Temporal worker
and not part of the Incus provider.

Responsibilities:

- accept authenticated outbound daemon connections;
- redeem one-time enrollment tickets;
- verify daemon challenge signatures on reconnect;
- enforce environment incarnation fencing;
- negotiate protocol version and capabilities;
- allocate ephemeral connection IDs;
- route host-protocol calls to the current connection;
- propagate disconnect and health observations;
- apply per-environment concurrency, frame, and bandwidth limits;
- redact secrets and sensitive request data from logs; and
- expose health and bounded metrics.

The gateway does not:

- own environment lifecycle;
- create VMs;
- store session state;
- execute Temporal workflows;
- make authorization decisions without a resolved Lightspeed environment;
- grant universe API access; or
- expose public application ingress.

### Transport

The default transport is an outbound authenticated, multiplexed connection
over TLS, using WebSocket or HTTP/2 according to the simplest implementation
that satisfies streaming and backpressure.

The protocol must support:

- request/response correlation;
- server-to-daemon calls;
- stdout/stderr and PTY streaming;
- daemon-to-server output notifications;
- cancellation;
- bounded frame sizes;
- ping/lease detection;
- graceful reconnect; and
- explicit environment/incarnation/connection identity.

Do not route high-volume filesystem or process streams through the Kubernetes
API server, Incus REST API, or Temporal history.

### Horizontal scaling

One gateway replica is sufficient initially. Before multiple replicas:

- separate durable daemon identity from live connection ownership;
- choose connection-aware routing or a small internal relay bus;
- guarantee that one incarnation has one admitted current connection; and
- ensure a worker resolves the current gateway replica without persisting a
  stale address in deterministic session state.

Do not add Redis, NATS, or another broker before gateway replication requires
it.

## `lightspeed-envd`

### Project shape

Target Lightspeed workspace structure:

```text
crates/environment-daemon/
  src/
    config.rs
    identity.rs
    enrollment.rs
    runtime/
      mod.rs
      unix.rs
      macos.rs
    transport/
      mod.rs
      websocket.rs
      vsock.rs
      stdio.rs
    server.rs
    main.rs
```

The binary supports modes such as:

```text
lightspeed-envd enroll --gateway ... --token ...
lightspeed-envd run --config /etc/lightspeed-envd/config.toml
lightspeed-envd run --transport vsock --port ...
```

The exact CLI may change during implementation; the semantic modes should not.

### Runtime policy

envd runs as a dedicated unprivileged user by default. Its configuration
defines:

- allowed filesystem roots;
- default working directory;
- read-only versus read-write policy;
- process executable/environment restrictions when required;
- maximum concurrent processes and jobs;
- output and file-transfer bounds;
- whether PTY, search, glob, ranged reads, durable jobs, and network are
  advertised; and
- durable state directory.

Managed development VMs may deliberately grant the envd user passwordless sudo
and Docker access. That is a template policy and makes the whole VM the trusted
environment boundary; it is not a default for less-trusted sandboxes.

### Identity storage

Persist the daemon private key in a root-owned or daemon-owned state directory,
never in the repository or command-line arguments. On Linux use a location such
as:

```text
/var/lib/lightspeed-envd/identity.key
/var/lib/lightspeed-envd/state/
```

Provider base images must not contain either path after image sealing.

Provider-owned job semantics from P104 continue to apply through envd. The
daemon persists enough job identity, status, and retained-output state to
reconcile after restart. An in-flight job must either be safely rediscovered or
become a typed terminal interruption; it must never leave a Temporal waiter
parked indefinitely. The implementation may use an OS supervisor or its own
state directory, and the conformance suite tests the observable behavior
rather than prescribing that mechanism.

### Provider-native execution remains valid

The environment model does not require envd. A specialized provider may return
a provider-native `HostConnectionSpec` and implement the same filesystem,
process, and job traits outside the guest.

Use provider-native execution only when it can faithfully represent the live
environment. Hypervisor disk access alone is not sufficient for a general
development VM because it misses live mounts, overlays, permissions, tmpfs,
process state, PTYs, and job supervision.

Every native adapter advertises only supported capabilities and passes the
shared host data-plane conformance suite.

## Host protocol target contract

Keep `host-protocol` as the backend-neutral controller and data-plane wire
contract. Do not rename it to environment protocol; an environment is the
semantic resource and host protocol is one concrete execution substrate.

Refactor the controller plane around global providers and binding-aware calls.
Conceptual target operations:

```text
controller/initialize
controller/listTemplates
controller/createTarget
controller/getTarget
controller/listTargets
controller/closeTarget
```

`createTarget` must include:

```text
requestId
environmentId
incarnationId
providerBindingContext
templateId
bounded template parameters
opaque daemon bootstrap ticket
```

It must not accept arbitrary cloud-init, raw Incus/Kubernetes objects, host
paths, privileged device mappings, or unrestricted provider JSON from a
universe caller.

The provider returns:

```text
providerTargetId
observed target status
backend metadata safe for the universe
data-plane connection description or preallocated gateway route
```

The provider may return while the target is `Starting`. Heartbeats or explicit
observations advance status. Repeating the same `requestId` returns the same
target or the same terminal failure class; it never creates a second target.

`closeTarget` is also idempotent. A missing already-closed backend object is
success after ownership metadata has been verified.

## Capabilities and conformance

Capabilities are negotiated for each incarnation, not inferred from provider
or template name. Preserve and extend the existing bounded capabilities:

- filesystem read/write;
- search, glob, and ranged reads;
- process start/stdin/terminate/output;
- output notifications;
- PTY;
- durable job start/list/read/cancel;
- wait hints, dependencies, and queue keys; and
- network availability.

Future browser, computer-use, port-publication, or snapshot capabilities
require explicit additions. Do not overload `network` to mean public ingress.

One reusable conformance suite must run against:

- `lightspeed-envd` over local/stdio transport;
- `lightspeed-envd` through the real environment gateway;
- the Incus VM path;
- the Firecracker vsock relay when implemented;
- the KubeVirt Service path when implemented; and
- any provider-native adapter.

Test path traversal, symlinks, permissions, cwd, large/ranged files, bounded
search/glob, PTY behavior, output backpressure, cancellation, reconnect,
credential redaction, job terminality, and stable error mapping.

## Templates and images

### Template catalog

Users choose templates, not raw backend images. The provider publishes a typed
catalog filtered through the universe binding.

Example deployment configuration:

```yaml
templates:
  dev-v1:
    displayName: Linux development
    description: Durable development VM with Docker and common toolchains
    backendKind: virtual-machine
    image:
      source: ls-images
      fingerprint: sha256:...
    resources:
      cpu: 8
      memory: 16GiB
      rootDisk: 120GiB
    limits:
      maxCpu: 16
      maxMemory: 32GiB
      maxRootDisk: 300GiB
    bootstrapRevision: envd-v1
    networkPolicy: development
    lifecycle: durable
    publicIngress: allowed
    status: active
```

Template fields exposed to universes include ID/version, display name,
description, resource defaults and allowed overrides, durable/ephemeral
lifecycle, architecture, public-ingress eligibility, capabilities, and
active/deprecated status.

Backend image references, bootstrap internals, node constraints, and privileged
configuration remain operator-only.

### Initial catalog

Start with one canonical image and at most two product templates:

- `dev-small-v1`: durable full VM with Docker, Git, common toolchains, envd,
  moderate resources, and optional ingress;
- `dev-large-v1`: the same image with larger CPU, memory, and disk limits.

Add a minimal or ephemeral image only when a real sandbox workload requires
it. Resource size and ingress policy should not create unnecessary image
variants.

### Image lifecycle

- Build images in CI from a versioned recipe.
- Bake packages and `lightspeed-envd`; inject no environment identity.
- Resolve a template version to an immutable image fingerprint.
- Record the fingerprint on every target.
- Let a friendly alias such as `dev` point to the current template version,
  but never mutate an existing version in place.
- New environments use the newly active version; existing environments remain
  on their recorded image.
- Mark versions deprecated before removal.
- Refuse image deletion while a retained target or supported rollback depends
  on it.

For independent Incus nodes, the provider checks the selected node's cache and
copies/imports the exact fingerprint lazily. Add a dedicated image distribution
service only when node count or rollout latency justifies it.

Arbitrary user images, Dockerfiles as VM definitions, and raw image aliases are
deferred.

## Incus provider

### Deployment

The first provider is a Rust deployable on hz01's trusted application plane. It
reuses Lightspeed host-protocol types but does not depend on the engine,
Temporal worker, session runtime, or ls.bot app.

It talks over the tailnet to the Incus remote HTTPS API on hz02 using a
restricted deployment credential. The hz02 root host continues to run Incus
and host administration only; agent-controlled code runs in guests.

Target crate:

```text
crates/environment-provider-incus/
```

The provider crate stays in the Lightspeed workspace while the controller
contract is still churning; the contract is the seam, so extracting it into a
separate repository later is cheap if deployment-specific logic or release
cadence demands it. Extract the backend seam from what Incus actually needs
rather than designing an unused generic cloud SDK.

Responsibilities:

- expose the provider controller contract;
- publish the configured template catalog;
- validate binding policy and bounded overrides;
- select an eligible node;
- ensure the exact image exists;
- create VM, disk, network, and bootstrap configuration idempotently;
- observe VM power/health state;
- reconcile ownership metadata;
- close targets safely; and
- report capacity and bounded metrics.

### Single-node first

The initial provider has one configured node, hz02. Scheduling is therefore
trivial, but target names and metadata must already be globally unique and
multi-node-safe.

Do not create a one-node abstraction that assumes every target lives on hz02
or stores an unqualified local VM name as its global identity.

### VM defaults

Use full KVM/QEMU VMs for dynamically provisioned agent environments. They have
a dedicated kernel and provide a stronger boundary for arbitrary development
workloads and nested Docker.

Keep system containers for manually trusted, durable machines such as the
current `ls-dev` recipe where efficiency is more valuable than the VM boundary.

### Incus projects and networks

First cut:

- one restricted Incus project per provider binding, created on first use;
- full VMs;
- per-instance resource limits;
- one managed bridge/network per provider binding;
- baseline rules preventing access to sibling binding networks, Incus hosts,
  and the trusted control-plane/tailnet; and
- no Incus API access inside guests.

Gated on the first untrusted tenant (v1 assumes trusted universes):

- fine-grained egress policy and workload-specific network ACLs;
- project CPU, memory, disk, and instance limits beyond defaults; and
- an end-to-end public-application-compromise threat model.

### Independent node pool

Joining another root node to the provider pool means:

1. install and initialize Incus reproducibly;
2. configure storage, managed network, firewall, and remote API;
3. establish tailnet identity and provider trust;
4. register node ID, failure domain, architecture, labels, and capacity;
5. reconcile projects, profiles, and template images; and
6. admit the node for scheduling.

The provider schedules over independent Incus servers. Nodes do not need a
shared consensus database or storage system. Initial placement uses hard
resource fit, architecture/template constraints, binding quota, node health,
and simple free-capacity preference.

Support node cordon and drain before automatic rebalance. A failed node makes
its environments unavailable; automatic destructive recreation is a separate
policy, not an implicit scheduler behavior.

Use an Incus cluster only when at least three sufficiently homogeneous nodes in
one datacenter need its unified scheduling, shared storage, or migration. A
cross-region Incus quorum is not the default pool design.

## Lightspeed API access from environments

The environment's control connection and the software it builds have separate
credentials.

- envd authenticates with daemon identity and incarnation fencing.
- A session process receives environment-bound secrets through the existing
  environment credential mechanism.
- Software that must call Lightspeed persistently receives a dedicated,
  revocable, universe-scoped API key explicitly passed to that backend service.
- A frontend never receives a Lightspeed bearer key.
- The provider and base image never receive a general universe key.

Provide a stable non-secret API URL and a small CLI/client wrapper in the
development template. Add an ls.bot convenience action that creates, stores,
and binds an environment Lightspeed API credential after the underlying API is
solid; the manual create-secret-bind flow is sufficient for the first proof.

## Public application ingress

Public ingress is independent of environment control transport.

First implementation: one Cloudflare Tunnel per environment.

- The development template image includes `cloudflared`.
- When the binding allows public ingress, a tunnel and a hostname under the
  platform ingress domain are allocated for the environment. The hostname is
  recorded in environment metadata.
- The tunnel credential is installed only for the system-managed connector
  using a protected service-secret path. It is not a generic environment
  credential and is not injected into arbitrary Lightspeed-started processes
  or jobs.
- The connector runs inside the environment and routes to local application
  ports. Cloudflare terminates DNS and TLS; no inbound port opens on any node
  and no per-node edge service exists.
- A tunnel credential authorizes routing for its own hostname only. Baseline
  provider networking separately prevents access to sibling bindings, the
  node, and the trusted control plane.

Tunnel and hostname allocation may be a manual, CLI-assisted operator step for
the first proof; automate through the Cloudflare API once the flow is stable.

Tailscale Funnel is not used for environment ingress: the tailnet is the
trusted application plane and agent-controlled VMs must not hold tailnet
identity.

The node edge guest (wildcard DNS, host-terminated TLS, approved-route
proxying) remains the fallback if tunnel limits, bandwidth costs, or
third-party dependence become a problem. It is deferred, not abandoned.

Never expose publicly:

- environment gateway or envd;
- SSH;
- Incus API;
- Docker socket/daemon;
- arbitrary application ports; or
- raw Lightspeed RPC endpoints.

For the first implementation, an environment routes multiple internal services
behind its single tunnel hostname. A generic deployment DSL, per-service
platform API, and multi-region global ingress are deferred.

## Firecracker backend compatibility

Cursory sketch — a sanity check that the semantic model ports, not an
implementation commitment. No Firecracker work is scheduled.

The semantic model requires no Firecracker-specific changes.

Mapping:

```text
EnvironmentProvider       Firecracker pool controller
ProviderTarget            one microVM
Template                  kernel + rootfs + base snapshot + resource policy
EnvironmentDaemon         envd baked into rootfs
Daemon transport          AF_VSOCK through node-local relay
EnvironmentGateway        unchanged
Incarnation               one restored/booted microVM generation
```

Preferred data path:

```text
envd in microVM
  ⇄ AF_VSOCK
node-local provider relay
  ⇄ authenticated outbound stream
environment gateway
```

The microVM need not have general network access for control traffic. The
provider transports a fresh enrollment ticket into each new incarnation.

Base snapshots are taken before daemon enrollment. Restore recreates the vsock
backing channel, supplies a fresh incarnation, and establishes a new gateway
connection. Snapshot state never authorizes two clones as the same live
environment.

A Firecracker provider may instead implement host protocol outside the guest
when it owns a deliberately shared workspace and has a faithful execution
mechanism. That adapter is capability-limited and must pass conformance. Do not
make provider-native execution the general VM default.

## KubeVirt backend compatibility

Cursory sketch — a sanity check that the semantic model ports, not an
implementation commitment. No KubeVirt work is scheduled.

The semantic model also remains unchanged on Kubernetes/KubeVirt.

Mapping:

```text
EnvironmentProvider       one KubeVirt cluster/provider controller
Provider binding          namespace/RBAC/quota/network policy entitlement
ProviderTarget            VirtualMachine
Incarnation               VirtualMachineInstance generation
Template                  VM template + DataVolume/containerDisk + policy
Bootstrap                 Secret + cloud-init
EnvironmentDaemon         envd inside VM
Daemon transport          outbound to Environment Gateway Service
Public ingress            Service + Gateway/Ingress
Backend state             VM/VMI conditions
```

Do not route normal host-protocol traffic through `kubectl`, `virtctl`, console,
SSH port forwarding, or the Kubernetes API server. Those are operator/debug
paths. envd uses an ordinary Service/data-network connection so filesystem and
PTY traffic does not load the control plane.

Kubernetes namespace, RBAC, ResourceQuota, NetworkPolicy, and admission policy
enforce each provider binding. The provider watches VM/VMI state and translates
it into the same target lifecycle used by Incus.

VM restart or migration changes the incarnation/connection as policy requires,
but not the logical environment ID. The daemon reconnects and the gateway
fences stale connections.

Kubernetes becomes an implementation detail behind the provider interface. No
session, environment tool, credential binding, or Foundry behavior branches on
KubeVirt.

## Security model

### Trust boundaries

- Lightspeed operator control plane may register providers and create provider
  bindings.
- An authenticated universe caller may consume an admitted binding or enroll a
  direct host. v1 universe API keys authorize the whole universe; finer
  universe roles are deferred.
- A session may use and select environments only when its effective profile
  grants the corresponding features. Model-driven provisioning is not part of
  P118-P122 and requires a future, separate default-off grant.
- The provider is trusted to allocate compute but does not receive universe
  bearer credentials.
- envd is trusted within its configured environment boundary but has no
  provider/root-node credentials.
- Internet-facing application containers are not trusted with envd identity or
  environment secrets unless explicitly injected.

### Required controls

- Authenticated provider controller connection.
- One-time, expiring enrollment tickets stored hashed at rest.
- Daemon proof-of-possession identity after enrollment.
- Incarnation fencing on every routed call.
- Single current connection per daemon/incarnation.
- Short provider and connection leases.
- Explicit filesystem roots and path traversal protection.
- Frame, file, output, process, job, and concurrency bounds.
- Secret redaction in errors, debug output, traces, and audit records.
- Restricted backend API credentials and tailnet ACLs.
- Full VMs for less-trusted dynamic development workloads.
- No raw provider options or cloud-init from universe callers.
- No Docker socket mounted into public application containers.
- No trusted-header Lightspeed gateway access from environment networks.
- Per-binding quotas enforced transactionally by Lightspeed at create.
- Destructive close verifies target ownership metadata before deletion.

## Reconciliation and failure semantics

Lightspeed reconciles its logical environment intent against provider
observations and gateway availability. The provider reconciles accepted,
idempotent target requests and Lightspeed ownership metadata against backend
targets; it does not read or write Lightspeed's database. The gateway
independently reports live data-plane connections.

### Provider restart

- Reload provider and node configuration.
- List owned targets by immutable metadata.
- Reconstruct target-to-node mapping.
- Resume observations.
- Repeated create request IDs adopt their existing targets.
- Do not delete an unrecognized target automatically.

### Gateway restart

- Daemons reconnect with their persistent identity.
- New connection IDs are allocated.
- Existing incarnations remain valid if their lease/policy permits.
- Calls during the gap fail as unavailable and retry only according to existing
  tool/job semantics.

### Node loss

- Provider marks targets on the node Unknown/Unavailable.
- Environment IDs and selection remain; tools return typed availability errors.
- Do not silently select another environment.
- Do not recreate durable disks elsewhere without an explicit recovery policy.

### Partial create

- Lightspeed retains the stable request/environment/incarnation IDs.
- Provider retries adopt an existing matching target.
- A terminal create failure remains inspectable and explicitly retryable or
  closeable.
- Orphan reconciliation reports mismatches; automatic deletion begins only
  after ownership and age thresholds are proven.

### Close

- Lightspeed atomically rejects new attachment/job starts and marks Closing.
- Provisioned source calls provider close idempotently.
- Enrolled source revokes daemon identity without touching the external host.
- Gateway fences current connections.
- Credentials are deleted or revoked according to environment ownership rules.
- Final Closed is retained long enough for audit; physical cleanup is verified.

## Observability

Expose bounded metrics for:

- global provider health and controller latency;
- create/close operations by status and template;
- node capacity, allocation, and unavailable targets;
- image cache/fetch latency;
- gateway active connections and reconnects;
- enrollment success/failure/expiry;
- host-protocol latency, bytes, errors, and backpressure by method class;
- environment readiness transition duration;
- public ingress health; and
- quota rejection counts.

Do not put universe IDs, paths, command lines, environment variables, file
contents, tokens, or process output into metric labels.

Structured logs include stable request, environment, incarnation, provider,
binding, target, node, daemon, and connection IDs where appropriate, with
secrets redacted.

Uptime Kuma remains sufficient for initial external health. Prometheus/Grafana
can consume Incus/provider/gateway metrics when fleet-level diagnosis becomes
necessary.

## Repository boundaries

### Lightspeed

Target work:

- replace universe-scoped physical provider records with operator-scoped
  providers plus universe bindings;
- make environment source explicit (`Provisioned | Enrolled`);
- accept the stable create request ID and allocate environment/incarnation
  identity before provider calls;
- add pending/asynchronous lifecycle and reconciliation observations;
- implement enrollment records and daemon identity;
- add the environment gateway deployment role;
- rename/refactor `host-bridge` into `environment-daemon`;
- harden host-protocol control context and connection fencing;
- add template discovery;
- preserve transport-neutral filesystem/process/job adapters;
- implement the Incus provider as a standalone deployable crate; and
- extend generated API clients and conformance tests.

Provider and daemon crates may reuse protocol/API crates but must not depend on
the engine, Temporal server, session runtime, CLI, or store implementation.

### ls.bot

Target work:

- replace the current read-only universe provider view with a binding view;
- add template-aware environment creation;
- add direct environment enrollment UX with one-time token display;
- show source, provider/binding, template, lifecycle, readiness, incarnation,
  public endpoint, and bounded diagnostics;
- keep environment credential assignment on the environment resource;
- deploy environment gateway and Incus provider roles on hz01;
- expose only required private/tailnet ports;
- add hz02 Incus API, project, network, and template assets;
- version provider/node/image configuration under `infra/`;
- store provider/node/ingress secrets through SOPS; and
- update `ls-dev` from a self-registering provider into a directly enrolled
  environment using `lightspeed-envd`.

## Implementation plan

Implementation is tracked in five independently completable plans:

1. [P118 — Environment Domain, Bindings, And Durable Lifecycle](p118-environment-domain-and-lifecycle.md)
   replaces the provider/domain model and establishes the Postgres-backed
   lifecycle reconciler.
2. [P119 — Environment Daemon, Gateway, And Direct Enrollment](p119-environment-daemon-gateway-enrollment.md)
   delivers the canonical outbound data plane and enrolled-host path.
3. [P120 — Incus Environment Provider](p120-incus-environment-provider.md)
   delivers the first dynamically provisioned VM path on hz02.
4. [P121 — Environment Public Ingress](p121-environment-public-ingress.md)
   adds controlled application ingress and its credential boundary.
5. [P122 — Independent Incus Multi-Node Pool](p122-incus-multi-node-pool.md)
   adds scheduling, image distribution, cordon/drain, and node-loss behavior.

Security, negative testing, reconciliation, and observability are acceptance
work in the milestone that introduces each boundary, not a separate late
hardening phase. Firecracker and KubeVirt remain portability checks in this
umbrella document and have no scheduled implementation plan.

## Test strategy

### Unit and contract tests

- Identifier parsing, lifecycle transitions, source invariants, binding policy,
  quota arithmetic, template validation, token expiry, signature verification,
  and fencing.
- Host-protocol serde fixtures and version negotiation.
- Provider create/close idempotency and ownership checks.
- Environment readiness derivation from independent provider and gateway
  observations.

### Data-plane conformance

- Shared filesystem/process/job suite against local envd and real gateway.
- Stream backpressure, cancellation, disconnect, reconnect, and bounded output.
- Credential injection without logging or persistence leakage.
- Duplicate daemon and stale incarnation rejection.

### Multi-tenant tests

- One global fake/Incus provider bound to at least two universes.
- Same provider/template names allowed without target collision.
- Universe A cannot list, route to, close, credential-bind, or claim ingress for
  universe B's environment.
- Quota and template policy are independently enforced.
- Provider inventory is operator-visible but not leaked through universe APIs.

### Live infrastructure tests

- hz02 VM create, cloud-init, envd enrollment, execution, reboot, close.
- Provider restart during create and after ready.
- Gateway restart with daemon reconnect.
- Node loss and recovery.
- Image update affects new VMs only.
- Public ingress resolves only through the environment's allocated tunnel
  hostname.
- Snapshot/clone attempt cannot create two accepted copies of one incarnation.

## First complete definition of done

1. One operator-scoped Incus provider is registered once.
2. Two universes have distinct bindings to that provider with different
   template/quota policy.
3. Each universe can provision a full VM with no cross-universe visibility or
   control.
4. Lightspeed persists the environment before provisioning and repeating the
   same request ID cannot create a duplicate VM.
5. The VM boots an unenrolled image, receives fresh bootstrap material, and
   connects outbound through `lightspeed-envd` to the environment gateway.
6. Environment readiness requires both provider target availability and an
   admitted data-plane incarnation.
7. A plain session can select the environment and use filesystem, process,
   PTY, credential, and durable-job capabilities.
8. The environment survives session closure and ordinary VM/daemon restart.
9. A manually managed host can enroll as an environment without any provider
   record and uses the same envd/gateway data plane.
10. Stale incarnation, duplicate daemon, and superseded connection attempts are
    fenced.
11. Closing a provisioned environment destroys its verified provider target;
    closing an enrolled environment only revokes access.
12. A session can deploy an API/frontend behind controlled public ingress and a
    backend can call only its own Lightspeed universe using a dedicated key.
13. Provider and gateway restarts recover from Lightspeed and backend truth
    without a provider database.
14. The UI shows provider binding, template, source, lifecycle, public endpoint,
    and bounded diagnostics without mirroring environment state in ls.bot.

## Decisions

| Area | Decision |
|------|----------|
| Physical provider scope | Global and operator-managed |
| Universe access | Explicit provider binding with policy and quota |
| Tenancy assumption | v1 universes are trusted; baseline binding-network isolation is required, finer untrusted-workload policy is deferred |
| Aggregate quota | Lightspeed-enforced at create; provider enforces per-instance limits only |
| Existing machine | Direct enrolled environment, not singleton provider |
| Canonical data plane | `lightspeed-envd` through environment gateway |
| AI terminology | Reserve “agent” for AI agents; infrastructure process is daemon/envd |
| Provider-native execution | Allowed behind the same host contract, not the general VM default |
| Dynamic isolation | Full VM by default; trusted system containers remain possible |
| First backend | Incus/KVM on hz02 |
| Initial pool | Independent Incus node(s), centrally scheduled |
| Incus clustering | Only for 3+ suitable same-datacenter nodes when justified |
| Kubernetes | Future backend detail, not current substrate requirement |
| Firecracker | Cursory future sketch; prefer envd over vsock; no scheduled work |
| Provider persistence | None initially; derive from Lightspeed + backend |
| User choice | Curated versioned templates, not raw images |
| Image identity | Immutable fingerprint recorded per target |
| Public ingress | Per-environment Cloudflare Tunnel first; node edge gateway deferred |
| Interactive data plane | Direct transport; never per-call Temporal workflows |
| Session relationship | Session selects universe environment; no lifecycle ownership |
| Foundry relationship | Ordinary consumer of environment compute |

## Open questions

- Does the first gateway transport use WebSocket or HTTP/2 after testing PTY,
  notifications, cancellation, and backpressure?
- Which environment state, if any, receives backup guarantees beyond repository
  source and explicit provider snapshots?
- When gateway replication is required, is connection-aware RPC routing enough
  or is a dedicated relay bus justified?
- What finer universe role model should gate direct enrollment once a universe
  API key no longer represents full-universe administrative authority?

## Deferred

- Environment stop/start without close.
- TTL and idle reaping policy.
- Operator provider/binding admin UI and provider CRUD API.
- Node edge-gateway ingress (wildcard DNS, host-terminated TLS, approved-route
  proxying).
- Fine-grained per-binding egress policy and hostname scoping beyond the
  baseline isolation required by P120.
- User-supplied VM images and arbitrary cloud-init.
- Provider marketplace or unreviewed third-party providers.
- Automatic cross-node recovery of durable VMs.
- Shared cross-node storage and live migration.
- Firecracker and KubeVirt production backends.
- GPU/device scheduling and passthrough.
- Windows/macOS guest templates.
- Per-service deployment APIs or a generic deployment DSL.
- Multi-region global ingress.
- Provider billing and metered historical usage.
- Gateway HA before connection volume requires it.
- Provider-owned Postgres before non-derivable coordination state requires it.
