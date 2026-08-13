# P120: Incus Environment Provider

> **Update (2026-08-13):** Lightspeed initiates application-protocol
> connections to both provider endpoints, and the provider initiates the
> connection to private envd. Application-level authentication is currently
> absent; deployment network/transport controls are the trust boundary.
> Controller bearer and daemon-token configuration have been removed and richer
> authentication is deferred.

**Status**

- Core implementation completed 2026-08-12; hz01/hz02 deployment and live
  acceptance verification remain.
- Builds on [P118](p118-environment-domain-and-lifecycle.md) and
  [P119](p119-environment-daemon-gateway-enrollment.md).
- Delivers the first managed backend from
  [P117](p117-environment-compute-plan.md).

## Goal

Provision durable full-VM development environments on hz02 through one
standalone Rust Incus provider running on hz01.

The provider is intentionally stateless. It has no database and does not read
or write Lightspeed Postgres. Lightspeed owns lifecycle intent; Incus owns
physical truth; the provider reconstructs its view from deployment
configuration, Incus inventory, and immutable target metadata.

The provider is a passive controller and data endpoint. Lightspeed opens both
kinds of connection. It performs the transient `controller/initialize`
handshake on controller connections and opens a target-specific data
connection only when an environment is used. The provider does not
self-register, call Lightspeed's operator API, or need a Lightspeed API
credential. Its endpoints are currently protected by the deployment network
or transport boundary rather than application credentials. The same contract
may be implemented by a third-party provider reachable from Lightspeed.

Keep the provider contract as the extraction seam. The implementation starts
in this workspace as `crates/environment-provider-incus`, but it must not
depend on the engine, Temporal server, session runtime, CLI, or store
implementation so it can move to a separate repository later.

## Deployment and trust

- The provider runs on hz01's trusted application plane.
- It talks to the hz02 Incus HTTPS API over the tailnet with a restricted
  deployment credential.
- Controller and data traffic rely on deployment network/transport isolation;
  application authentication is deferred.
- Guests receive no Incus, node-root, tailnet, or general universe credential.
- Dynamically provisioned development environments use full KVM/QEMU VMs.

P120 configures one Incus node. Names, IDs, and metadata are also valid in the
native cluster-wide namespace introduced by P122, so clustered operation does
not require another identity migration.

## Projects and networking

Create one restricted Incus project and one managed bridge/network per provider
binding on first use. Apply:

- per-instance CPU, memory, and disk limits;
- no Incus API access from guests;
- rules blocking sibling binding networks;
- rules blocking Incus hosts and the trusted control-plane/tailnet; and
- ordinary internet egress required for development.

Fine-grained egress policy and workload-specific ACLs remain deferred. The
baseline is sufficient to support the cross-universe control claim and the
public-ingress milestone without pretending the first release has a complete
untrusted-tenant network product.

## Templates and images

Start with one canonical versioned image and at most:

- `dev-small-v1`;
- `dev-large-v1`.

Both are durable full VMs with Git, Docker, common toolchains, and an
passive `lightspeed-envd`. Resource size does not create a separate image.

Images are built from a versioned CI recipe and addressed by immutable
fingerprint. They contain no daemon identity, enrollment token, universe API
key, environment credential, or populated envd state directory.

The provider:

- publishes provider-wide typed immutable template versions;
- applies provider-wide resource, network, and public-ingress policy;
- ensures the exact image fingerprint is cached on hz02;
- records template version and image fingerprint on every target; and
- never accepts arbitrary images, cloud-init, backend JSON, privileged devices,
  or host paths from universe callers.

## Provisioning

For `createTarget`, the provider:

1. validates the provider-wide template and uses the request binding context as
   an ownership/isolation namespace;
2. lets Incus enforce physical-capacity admission;
3. finds an existing target with the same provision request ID or selects hz02;
4. creates the project/network/profile resources idempotently;
5. creates the VM with deterministic naming and immutable ownership metadata;
6. configures the passive envd listener through cloud-init;
7. starts the VM and returns while it may still be starting; and
8. reports `Ready` only after it can reach envd over the
   private guest network.

Required backend metadata includes environment, incarnation, request, binding,
template-version, image-fingerprint, and creation-time facts. It contains no
bearer secrets.

Environment readiness requires both an available Incus target and reachable
envd. VM running alone is not ready. Selection still probes the full
Lightspeed-to-provider-to-envd path rather than trusting stored status.

The Incus provider exposes a passive target-specific WebSocket beside its
controller endpoint. Lightspeed derives that endpoint from the registered
controller URL and opens it on demand. Before upgrade, both sides independently
validate provider, enabled binding, universe, environment, current incarnation,
and target ownership. The provider then dials envd over the private guest
network, proxies the host protocol unchanged, and closes the pair after a
configurable idle timeout. The next use creates fresh connections.

Neither the VM nor provider maintains a permanent connection to Lightspeed.
The provider's private dial path is the reachability mechanism.

`closeTarget` verifies immutable ownership metadata and is idempotent. A
missing object is success only after ownership/adoption facts show that it was
the requested target.

## Recovery

After provider restart:

- reload node, template, and credential configuration;
- list owned Incus targets by immutable metadata;
- reconstruct request/target mappings;
- resume observations; and
- adopt repeated create requests rather than creating duplicates.

Partial create remains represented by the Lightspeed environment intent.
Unknown or mismatched targets are reported for conservative operator cleanup;
they are never deleted merely because the provider does not recognize them.

The provider has no Postgres, local durable operation ledger, or third copy of
environment state.

## Implementation

- [x] Add the standalone `environment-provider-incus` crate and binary.
- [x] Implement the P118 controller contract over the Incus
      remote HTTPS API.
- [ ] Configure restricted tailnet-only Incus access from hz01 to hz02.
- [x] Reconcile per-binding projects, managed networks, baseline rules, and VM
      profiles.
- [x] Add a versioned immutable development-image build recipe and systemd
      unit. Publishing its production fingerprint on hz02 remains deployment
      work.
- [x] Implement provider-wide typed template discovery for every
      Lightspeed-admitted binding, with no provider-side universe allowlist.
- [x] Implement exact image-fingerprint resolution and optional lazy caching
      from a configured trusted Incus image server.
- [x] Implement deterministic naming and immutable ownership metadata.
- [x] Implement the passive provider data endpoint; Lightspeed initiates every
      target-specific route and does not create direct-enrollment rows.
- [x] Fence every route by provider ownership, enabled binding, environment,
      current incarnation, and target on admission and while connected.
- [x] Implement bounded on-demand envd dialing and idle connection reaping.
- [x] Reconcile Incus/envd observations into Lightspeed readiness and probe the
      complete data path during selection.
- [x] Implement ownership-verified idempotent close.
- [ ] Update ls.bot template-aware environment creation and status views.

The provider has no database or durable local operation ledger. Its JSON
configuration is authoritative for provider-wide templates, physical
networking, and Incus client credentials; it contains no universe or binding
records. Target/request recovery is derived
from Incus inventory and `user.lightspeed.*` ownership metadata. No provider
or envd application credential enters Lightspeed.

Binding networks use Incus-assigned persistent subnets. Sibling-network ACL
rules are derived from current Incus inventory whenever a binding is lazily
reconciled, so network isolation does not require a configured universe list or
provider database. Provider-wide blocked egress CIDRs must not overlap an
Incus-assigned binding subnet.

## Verification

- Bind one provider to two test universes solely through Lightspeed and prove
  both immediately discover the provider-wide templates without provider
  configuration changes or restart.
- Prove neither universe can list, inspect, route to, credential-bind, or close
  the other's targets.
- Prove repeated create adoption, provider restart during create, partial
  create recovery, gateway restart, VM reboot, image-update behavior, and
  verified close.
- Prove network access cannot cross binding networks or reach Incus/control
  endpoints.
- Provision through the universe API, select from a plain session, and exercise
  repository development, Docker Compose, filesystem/process/PTY operations,
  credentials, durable jobs, and restart survival.
- Prove a snapshot/clone cannot be adopted or routed as the current target
  without matching provider-owned target metadata.

## Done

P120 is complete when a plain session can use a dynamically provisioned hz02
VM as a durable development environment, two universes remain isolated, and
Lightspeed/provider/gateway restarts converge without any provider database.

## Deferred

- Public application ingress: P121.
- Additional Incus nodes and scheduling: P122.
- Stop/start without close, TTL reaping, user images, snapshots as a product,
  automatic recovery, GPUs, and Windows guests.
- Firecracker and KubeVirt implementations.
