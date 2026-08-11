# P122: Independent Incus Multi-Node Pool

**Status**

- Proposed 2026-08-11.
- Builds on [P120](p120-incus-environment-provider.md).
- Deferred until a second Incus root node is required.

## Goal

Extend the stateless Incus provider from one node to a centrally scheduled pool
of independent Incus servers without introducing a provider database, shared
storage, or a cross-region consensus cluster.

Each node remains independently operable. Lightspeed owns environment intent,
each Incus server owns its physical targets, and the provider derives pool
state from declarative configuration plus live backend inventory.

## Node admission

Node inventory is versioned deployment configuration:

```text
nodeId
endpoint
architecture
labels
failureDomain
capacity
admissionStatus
```

Joining a node means:

1. reproducibly install and initialize Incus;
2. configure storage, managed networking, firewall, and remote API;
3. establish tailnet identity and provider trust;
4. declare stable ID, architecture, labels, failure domain, and capacity;
5. reconcile projects, profiles, networks, and template images; and
6. explicitly admit the node for placement.

No node is selected merely because it is reachable.

## Placement

Initial placement uses:

- architecture and immutable template compatibility;
- hard resource fit;
- binding policy and quota already admitted by Lightspeed;
- node health and admission state;
- required labels/failure-domain constraints; and
- simple free-capacity preference.

The provider records the selected node in provider-safe target metadata and
returns a globally qualified target identity. It never relies on an
unqualified local Incus instance name.

Concurrent placement does not require a durable provider reservation ledger in
this milestone. Backend create plus stable request IDs remains the final
idempotency boundary. If real contention later demonstrates a need for strong
capacity reservations or multi-replica operation leases, that coordination
ledger is a separate design change.

## Images

The provider observes each node's immutable image fingerprints and lazily
copies/imports the selected version before target creation. It exposes bounded
cache/fetch status and latency.

Do not introduce a dedicated image distribution service until node count or
rollout latency justifies it.

## Cordon, drain, and failure

- Cordon prevents new placement while existing environments continue.
- Drain reports environments that must be explicitly moved, rebuilt, or
  closed; it does not silently migrate or destroy them.
- Node loss marks its environments unavailable while preserving environment
  identity and active session selection.
- Unaffected nodes continue serving their environments.
- Recovery does not recreate durable disks elsewhere without explicit policy.

Use an Incus cluster only if at least three sufficiently homogeneous nodes in
one datacenter need unified scheduling, shared storage, or migration. A
cross-region Incus quorum is not the default pool model.

## Implementation

- [ ] Define declarative node inventory, validation, health, and admission.
- [ ] Add reproducible node bootstrap and reconciliation assets.
- [ ] Make target identity and metadata globally node-qualified.
- [ ] Add hard-constraint and free-capacity placement.
- [ ] Add per-node immutable image cache observation and lazy distribution.
- [ ] Add cordon and explicit drain operations.
- [ ] Add bounded node/provider metrics without sensitive labels.
- [ ] Add conservative orphan and unknown-target reporting.
- [ ] Keep the provider stateless and reconstruct pool state after restart.

## Verification

- Provision concurrently across at least two independent Incus servers.
- Prove stable request retries adopt one target even across provider restart.
- Prove node constraints, capacity fit, binding isolation, and image-version
  selection.
- Prove cordoned nodes receive no new work.
- Prove drain never silently destroys or moves a durable environment.
- Prove one-node loss leaves its environments unavailable while environments
  on other nodes continue operating.
- Prove provider restart reconstructs node/target mappings without a database.

## Done

P122 is complete when independent Incus nodes form one safely scheduled
provider pool, operational controls and node-loss behavior are explicit, and
the provider still derives all durable truth from Lightspeed plus Incus.

## Deferred

- Automatic cross-node recovery of durable environments.
- Shared storage, live migration, and automatic rebalance.
- Incus clustering.
- Strong provider-side capacity reservations or a coordination ledger.
- Multi-region HA, billing usage history, GPUs, and device passthrough.
