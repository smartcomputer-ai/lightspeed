# P120: Incus Environment Provider

**Status**

- Proposed 2026-08-11.
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

Keep the provider contract as the extraction seam. The implementation starts
in this workspace as `crates/environment-provider-incus`, but it must not
depend on the engine, Temporal server, session runtime, CLI, or store
implementation so it can move to a separate repository later.

## Deployment and trust

- The provider runs on hz01's trusted application plane.
- It talks to the hz02 Incus HTTPS API over the tailnet with a restricted
  deployment credential.
- Controller traffic is authenticated with deployment identity, not a universe
  bearer key.
- Guests receive no Incus, node-root, tailnet, or general universe credential.
- Dynamically provisioned development environments use full KVM/QEMU VMs.

v1 configures one Incus node. Names, IDs, and metadata must still be
multi-node-safe so P122 does not require another identity migration.

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
unenrolled `lightspeed-envd`. Resource size does not create a separate image.

Images are built from a versioned CI recipe and addressed by immutable
fingerprint. They contain no daemon identity, enrollment token, universe API
key, environment credential, or populated envd state directory.

The provider:

- publishes typed immutable template versions;
- validates binding entitlement and bounded overrides;
- ensures the exact image fingerprint is cached on hz02;
- records template version and image fingerprint on every target; and
- never accepts arbitrary images, cloud-init, backend JSON, privileged devices,
  or host paths from universe callers.

## Provisioning

For `createTarget`, the provider:

1. validates authenticated binding and template context;
2. finds an existing target with the same provision request ID or selects hz02;
3. creates the project/network/profile resources idempotently;
4. creates the VM with deterministic naming and immutable ownership metadata;
5. injects the opaque fresh envd bootstrap ticket through cloud-init or config
   drive;
6. starts the VM and returns while it may still be starting; and
7. reports backend observations while envd independently connects to the
   environment gateway.

Required backend metadata includes environment, incarnation, request, binding,
template-version, image-fingerprint, and creation-time facts. It contains no
bearer secrets.

Environment readiness requires both an available Incus target and the current
admitted gateway incarnation. VM running alone is not ready.

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

- [ ] Add the standalone `environment-provider-incus` crate and binary.
- [ ] Implement the authenticated P118 controller contract over the Incus
      remote HTTPS API.
- [ ] Configure restricted tailnet-only Incus access from hz01 to hz02.
- [ ] Reconcile per-binding projects, managed networks, baseline rules, and VM
      profiles.
- [ ] Build and publish the first immutable development image.
- [ ] Implement typed template discovery and bounded overrides.
- [ ] Implement image fingerprint resolution and lazy hz02 caching.
- [ ] Implement deterministic naming and immutable ownership metadata.
- [ ] Bootstrap a fresh envd enrollment into each new incarnation.
- [ ] Reconcile Incus and gateway observations into Lightspeed readiness.
- [ ] Implement ownership-verified idempotent close.
- [ ] Update ls.bot template-aware environment creation and status views.

## Verification

- Bind one provider to two test universes with distinct template/quota policy.
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
- Prove a snapshot/clone cannot authenticate two copies as one incarnation.

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
