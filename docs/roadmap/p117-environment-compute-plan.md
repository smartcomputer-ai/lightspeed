# P117: Environment Compute Target Architecture

> **Superseding transport decision (2026-08-13):** Both external and
> provider-managed envd instances are passive and reachable from Lightspeed.
> Connections are opened on demand and closed when unused. Direct enrollment,
> daemon-initiated transport, daemon keys/tokens, admission rows, and the
> in-memory route registry have been removed. Provider and external daemon
> application authentication is deferred; deployment network/transport
> controls protect those endpoints. See P118-P120 for the normative design;
> older enrollment discussion below is retained only as design history.

Status: target design agreed 2026-08-10; scope trimmed 2026-08-11 after review
(trusted-tenant v1 and cursory backend sketches); split into delivery
milestones and moved provider policy out of Lightspeed 2026-08-11. Public
ingress changed from tunnel-first to provider-managed node-edge ingress on
2026-08-13.

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
2. Bind an approved provider to a universe while leaving templates, allocation,
   quotas, capacity, and ingress policy authoritative in the provider.
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
- raw image names and unrestricted provider options become provider-owned,
  curated template IDs;
- inbound public per-host WebSocket addresses become gateway routes backed by
  either direct outbound daemon connections or Lightspeed-initiated passive
  provider connections; and
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

A provider is global infrastructure. It is registered once with a stable
identity and authenticated controller connection, and is never owned by a
universe or session. Lightspeed does not persist provider heartbeat, lease, or
availability status; controller reachability is observed when it connects.

### Universe provider binding

A universe's admitted routing relationship with a provider. The Lightspeed
binding supplies stable universe/provider context and an enabled/disabled
switch. The provider owns template entitlement, resource allocation, aggregate
quota, capacity, networking, and public-ingress policy for that context.

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
- the environment gateway owns transport authentication and routing;
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
8. An environment is ready only when its direct daemon or provider-mediated
   data plane has negotiated current capabilities.
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

- global physical provider identity and authenticated controller connection;
- universe provider bindings and enabled/disabled admission;
- stable environment identities and source;
- desired lifecycle and observed readiness;
- provisioning request IDs;
- environment incarnations and fencing generation;
- direct-enrollment token hashes and expiry;
- directly enrolled daemon public-key identity and revocation;
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
- template catalogs, per-binding entitlement, allocation, aggregate quota,
  and public-ingress policy;
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
- which authenticated direct daemon currently owns an outbound route;
- immediate ping/disconnect observations.

These facts remain in gateway memory. Lightspeed may persist a bounded
availability observation, but a stored observation never makes a dead socket
live and is not used to reconstruct transport state after restart.

The route abstraction is independent of transport ownership:

- directly enrolled hosts normally keep one outbound envd connection because
  NAT/firewalls leave no other reachability channel;
- provider-managed environments expose a passive, target-specific provider
  data endpoint; Lightspeed opens it only when a worker needs the environment;
  and
- the provider opens a private-network connection to envd for that request and
  closes the paired connection after a configured idle period. It retains the
  provider-local dial/reachability path; envd does not connect to Lightspeed.

Multiplexing never merges authorization. Every logical stream carries and is
fenced by universe, environment, and incarnation. A direct stream is
additionally authenticated by its enrolled daemon identity; a mediated stream
is authenticated by the provider identity and checked against the enabled
binding and environment ownership.

### ls.bot Postgres

ls.bot continues to own humans, universe membership, and platform-level
commercial or administrative policy. It calls Lightspeed operator and universe
APIs. It does not mirror provider inventory, environment status, template
catalogs, daemon identity, or session selection.

Future billing plans may determine which provider offering a universe may
consume, while the effective compute and ingress policy remains provider-owned.

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

Lightspeed may register multiple providers. Provider records are managed
through the operator API; there is no provider self-registration requirement
and no operator admin UI in v1. Deployment configuration owns the provider
services themselves, but it is not the authoritative Lightspeed provider
registry.

The record contains stable provider identity, authenticated controller
connection, and operator metadata. Protocol version, implementation details,
and optional controller operations are negotiated transiently with
`controller/initialize` whenever Lightspeed opens a connection. They are not
seeded into or mirrored in Postgres. Provider heartbeat, last-seen, lease, and
availability status are likewise not durable provider fields.

The first Incus provider is an internal service on the trusted application
plane. Authenticate controller traffic with deployment identity over the
private network. Do not retain the current universe bearer key as provider
identity.

A third-party service may implement the same controller contract. A
Lightspeed operator registers its endpoint and authentication material, after
which Lightspeed connects to it. The provider never self-registers, calls the
Lightspeed operator API, or needs a Lightspeed API credential. Operator
admission and Lightspeed-to-provider reachability are the only registration
requirements.

### Universe bindings

Target universe/operator API:

```text
operator/environment-providers/put
operator/environment-providers/list
operator/environment-providers/read
operator/environment-providers/delete
operator/environment-providers/bindings/put
operator/environment-providers/bindings/delete

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
revision
createdAtMs
updatedAtMs
metadata
```

There is at most one binding for a `(universeId, providerId)` pair. Binding
document is replaced whole with `expectedRevision`; field-level update
vocabulary is not introduced. Disabling a binding prevents new provisioning
but does not strand existing environments. Deletion is rejected while a
non-closed environment references the binding.

Bindings do not copy provider template allowlists, CPU/memory/disk fields,
environment-count quotas, or public-ingress permission. Lightspeed therefore
has no resource reservation ledger to synchronize with backend inventory. The
provider filters template discovery by binding and atomically enforces its
current entitlement, allocation, quota, capacity, and ingress policy when it
performs the corresponding operation.

Every tenant-affecting target lifecycle request includes an authenticated
binding context derived by Lightspeed from its stored environment and binding,
never from opaque user options. `controller/initialize` uses only the
authenticated controller connection and has no tenant context. The provider
authenticates Lightspeed and treats the binding context as the key for its own
isolation and policy decisions.

Provider target listing is filtered by binding. A caller for binding A cannot
discover, inspect, or close binding B's targets.

## Environment sources and lifecycle

### Provisioned environment flow

```text
1. Universe caller generates a stable requestId and calls
   environments/create(requestId, bindingId, templateId).
2. Lightspeed validates that the binding is admitted and enabled.
3. In one transaction Lightspeed allocates/persists:
     environmentId
     incarnationId
     provisionRequestId = requestId
4. Repeating the same requestId returns the same environment.
5. The Lightspeed lifecycle reconciler calls provider createTarget with stable
   ids, binding context, and provider-owned template ID.
6. Provider validates template entitlement, allocation policy, aggregate quota,
   and physical capacity, then idempotently creates or finds the target.
7. Provider injects its private envd bootstrap configuration through
   cloud-init, metadata, config drive, Kubernetes Secret, or an equivalent
   backend channel.
8. envd boots and listens only on the provider-private network.
9. Provider reports ready after it can authenticate and reach envd.
10. On use, Lightspeed validates provider/binding/environment/incarnation/target,
    opens the provider data endpoint, and the provider dials envd.
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

A provider policy or capacity rejection occurs after Lightspeed has accepted
the logical intent. The reconciler records a structured terminal failure on the
environment. Lightspeed does not perform a racy provider preflight or pretend
that its logical environment rows are physical resource inventory.

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
- Provider VM reboot: normally same environment/incarnation; the provider may
  expose a new private envd connection behind the same logical route.
- VM rebuild or restore as replacement: same environment, new incarnation and
  a freshly fenced provider-mediated route.
- Clone into a distinct environment: new environment, incarnation, and daemon
  identity.
- Two simultaneous connections for one daemon/incarnation: gateway admits one
  current connection and fences the superseded connection.

Never publish or snapshot an already directly enrolled golden machine. Base
snapshots for provider-managed targets contain neither direct enrollment nor a
reusable provider credential; each instance receives fresh provider-private
bootstrap configuration.

## Environment gateway

The gateway is a dedicated Lightspeed deployment role, not a Temporal worker
and not part of the Incus provider.

Responsibilities:

- accept authenticated outbound direct-daemon connections and initiate
  target-specific provider connections;
- redeem one-time tickets and verify challenge signatures for direct daemons;
- authenticate provider-mediated routes through provider identity and binding;
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

The gateway transport is authenticated over TLS, using WebSocket or HTTP/2
according to the simplest implementation that satisfies streaming and
backpressure. Direct hosts open it from envd. For provider-managed targets,
Lightspeed opens a target-specific provider connection on demand; the provider
then opens its private envd connection.

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

`controller/initialize` is a transient handshake performed immediately after
opening a controller connection. It verifies protocol compatibility and
returns implementation information plus supported optional operations for that
connection. It is not provider registration or a heartbeat, and Lightspeed
does not persist the response as provider configuration or status.

`createTarget` must include:

```text
requestId
environmentId
incarnationId
providerBindingContext
templateId
opaque daemon bootstrap ticket
```

The provider owns any future template parameters and validates them against a
versioned provider schema. P118 sends only the template ID; do not introduce a
universal CPU/memory/disk override model in Lightspeed before a concrete
cross-provider requirement exists.

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

The provider may return while the target is `Starting`. Explicit target
observations advance status; this target lifecycle observation is distinct from
a global provider heartbeat, which Lightspeed does not persist. Repeating the
same `requestId` returns the same target or the same terminal failure class; it
never creates a second target.

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
catalog and filters it using the binding context supplied by Lightspeed.

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
    bootstrapRevision: envd-v1
    networkPolicy: development
    lifecycle: durable
    publicIngress: allowed
    status: active
```

Template fields exposed to universes include ID/version, display name,
description, provider-selected resource shape, durable/ephemeral lifecycle,
architecture, public-ingress eligibility, capabilities, and active/deprecated
status. Resource information is descriptive provider output, not a Lightspeed
quota or reservation schema.

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
- publish a binding-filtered template catalog;
- own and enforce binding entitlement, allocation, aggregate quota, capacity,
  and ingress policy;
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
resource fit, architecture/template constraints, provider-owned binding quota,
node health, and simple free-capacity preference.

Support node cordon and drain before automatic rebalance. A failed node makes
its environments unavailable; automatic destructive recreation is a separate
policy, not an implicit scheduler behavior.

Use an Incus cluster only when at least three sufficiently homogeneous nodes in
one datacenter need its unified scheduling, shared storage, or migration. A
cross-region Incus quorum is not the default pool design.

## Lightspeed API access from environments

The environment's control connection and the software it builds have separate
trust boundaries.

- envd/provider application authentication is currently deferred; deployment
  network/transport controls protect those endpoints and Lightspeed fences
  routes with current environment/incarnation ownership.
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

First implementation: a shared provider-managed node-edge reverse proxy.

- Wildcard DNS and deployment-managed TLS terminate at the edge proxy.
- When provider binding and template policy authorize ingress, the provider
  allocates one opaque hostname and routes it to one provider-defined guest
  port on the current owned target.
- Lightspeed stores requested ingress plus the resulting hostname and bounded
  observation, not a duplicate binding permission flag or proxy configuration.
- Universe callers cannot choose arbitrary VM addresses or ports. A VM-local
  application proxy may serve multiple application components through the one
  approved entrypoint.
- The VM contains no tunnel agent, ingress token, TLS key, or platform ingress
  credential.
- Baseline provider networking permits the edge proxy to reach only approved
  application routes and separately prevents access to sibling bindings, the
  node, and the trusted control plane.

Tailscale Funnel is not used for environment ingress: the tailnet is the
trusted application plane and agent-controlled VMs must not hold tailnet
identity.

Cloudflare Tunnel or another outbound tunnel remains a deployment alternative
when running public inbound edge infrastructure is impractical. It is not part
of the first implementation.

Never expose publicly:

- environment gateway or envd;
- SSH;
- Incus API;
- Docker socket/daemon;
- arbitrary application ports; or
- raw Lightspeed RPC endpoints.

For the first implementation, an environment exposes one application
entrypoint behind its single hostname. A generic deployment DSL, per-service
platform API, arbitrary ports, and multi-region global ingress are deferred.

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
node-local provider endpoint
  ⇄ Lightspeed-initiated target connection
environment gateway
```

The microVM need not have general network access for control traffic.
Lightspeed and the provider independently validate the environment's current
incarnation and target; no direct-enrollment token enters the guest.

Base snapshots contain no reusable daemon or provider credential. Restore
recreates the vsock backing channel, supplies a fresh incarnation, and exposes
a freshly fenced route. Snapshot state never authorizes two clones as the same
live environment.

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
- Transient controller initialization and protocol-version validation on each
  new connection; no persisted provider heartbeat or lease.
- One-time, expiring direct-enrollment tickets stored hashed at rest.
- Daemon proof-of-possession identity after direct enrollment.
- Provider identity plus enabled binding/environment/current-incarnation
  validation for mediated routes, with no daemon-enrollment row.
- Incarnation fencing on every routed call.
- Single current connection per daemon/incarnation.
- Short enrollment-ticket lifetimes and data-plane connection leases.
- Explicit filesystem roots and path traversal protection.
- Frame, file, output, process, job, and concurrency bounds.
- Secret redaction in errors, debug output, traces, and audit records.
- Restricted backend API credentials and tailnet ACLs.
- Full VMs for less-trusted dynamic development workloads.
- No raw provider options or cloud-init from universe callers.
- No Docker socket mounted into public application containers.
- No trusted-header Lightspeed gateway access from environment networks.
- Per-binding entitlement, quota, allocation, capacity, and ingress policy
  enforced by the provider at the physical operation boundary.
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

- controller connection/initialization failures and latency;
- create/close operations by status and template;
- node capacity, allocation, and unavailable targets;
- image cache/fetch latency;
- gateway active connections and reconnects;
- enrollment success/failure/expiry;
- host-protocol latency, bytes, errors, and backpressure by method class;
- environment readiness transition duration;
- public ingress health; and
- provider policy, quota, and capacity rejection counts.

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
- implement direct-enrollment records and daemon identity;
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

- Identifier parsing, lifecycle transitions, source invariants, binding
  revisions, provider-side policy rejection, template validation, token expiry,
  signature verification, and fencing.
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
- Provider-owned quota, template, and ingress policy are isolated by binding.
- Provider inventory is operator-visible but not leaked through universe APIs.

### Live infrastructure tests

- hz02 VM create, cloud-init, envd enrollment, execution, reboot, close.
- Provider restart during create and after ready.
- Gateway restart with daemon reconnect.
- Node loss and recovery.
- Image update affects new VMs only.
- Public ingress resolves only through the environment's allocated edge
  hostname and current provider-owned target route.
- Snapshot/clone attempt cannot create two accepted copies of one incarnation.

## First complete definition of done

1. One operator-scoped Incus provider is registered once.
2. Two universes have distinct bindings to that provider with different
   provider-owned template/quota policy.
3. Each universe can provision a full VM with no cross-universe visibility or
   control.
4. Lightspeed persists the environment before provisioning and repeating the
   same request ID cannot create a duplicate VM.
5. The VM boots an image without direct daemon enrollment; Lightspeed opens a
   target-specific provider route on demand and the provider dials
   `lightspeed-envd` privately.
6. Environment readiness requires provider target and envd availability;
   selection probes the authenticated current data path.
7. A plain session can select the environment and use filesystem, process,
   PTY, credential, and durable-job capabilities.
8. The environment survives session closure and ordinary VM/daemon restart.
9. A manually managed host can enroll as an environment without any provider
   record and uses the same envd/gateway data plane.
10. Stale incarnation, wrong provider/target, duplicate direct daemon, and
    superseded connection attempts are fenced.
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
| Provider registration | Operator supplies an authenticated reachable endpoint; provider never self-registers or calls the operator API |
| Provider initialization | Transient version/capability handshake per controller connection; not persisted and not a heartbeat |
| Provider presence | No provider heartbeat, lease, last-seen, or status in Lightspeed Postgres |
| Universe access | Explicit Lightspeed binding for routing and enabled/disabled admission |
| Tenancy assumption | v1 universes are trusted; baseline binding-network isolation is required, finer untrusted-workload policy is deferred |
| Resource and ingress policy | Provider-owned, including templates, allocation, aggregate quota, capacity, and public ingress |
| Lightspeed resource ledger | None; environment lifecycle rows are not physical capacity reservations |
| Existing machine | Direct enrolled environment, not singleton provider |
| Canonical data plane | `lightspeed-envd` through an environment-gateway route: direct outbound or Lightspeed-initiated through a passive provider |
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
| Public ingress | Provider-managed node-edge HTTPS proxy with wildcard DNS/TLS and one approved guest port per environment |
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
- General environment TTL/reaping policy beyond the required P120 relay
  connection idle timeout.
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
