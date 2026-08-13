# P122: Single-Node and Clustered Incus Provider

**Status**

- Revised 2026-08-13 around native Incus clustering.
- Builds on [P120](p120-incus-environment-provider.md) and
  [P121](p121-environment-public-ingress.md).
- Implementation in progress.

## Goal

Run the same stateless Incus provider in either of two explicit modes:

1. `single`: one standalone Incus server; or
2. `cluster`: one native Incus cluster containing any Incus-supported number
   of members.

The provider does not pool unrelated Incus servers and does not implement a
second node scheduler. Lightspeed selects the provider and binding. In cluster
mode, Incus owns member inventory, health, placement, and cluster-wide target
location.

Both modes expose the same Lightspeed provider protocol. Lightspeed remains
unaware of Incus members and persists only the provider's opaque target ID.

## Configuration and topology

The operating mode is explicit rather than inferred:

```json
{
  "incus": {
    "mode": "single",
    "endpoints": ["https://node-a.example:8443"]
  }
}
```

```json
{
  "incus": {
    "mode": "cluster",
    "endpoints": [
      "https://node-a.example:8443",
      "https://node-b.example:8443",
      "https://node-c.example:8443"
    ]
  }
}
```

Cluster endpoints are failover addresses for the same Incus cluster, not
independently scheduled nodes. The provider validates reachable endpoints and
rejects a configuration whose endpoints expose different cluster membership.
An unavailable configured endpoint does not prevent startup when another
endpoint proves the topology.

`single` requires exactly one endpoint. `cluster` supports any cluster size
accepted by Incus. A two-member cluster is allowed but emits a warning because
loss of one voter can remove quorum. Three or more members are the recommended
production topology.

Cluster formation, join tokens, member removal, storage initialization,
network prerequisites, TLS trust, upgrades, and quorum recovery remain
deployment responsibilities. The provider never bootstraps or mutates cluster
membership.

## Placement

Target names remain deterministic from binding, environment, and incarnation.
Incus guarantees a cluster-wide instance namespace, so the same opaque target
ID works in both modes and remains stable if an operator explicitly moves an
instance between members.

In `single`, creation runs on the only server. In `cluster`, creation either:

- lets the native Incus scheduler choose from all schedulable members; or
- targets an Incus cluster group declared by the selected provider template.

Cluster groups express hard compatibility sets such as architecture, CPU
baseline, GPU availability, or failure-domain policy. The provider validates
that a configured group exists before creation. CPU, memory, disk, project,
and storage constraints remain part of the Incus instance request. Incus is
the final capacity and placement authority.

The provider does not mirror member capacity or calculate free capacity. If
the native scheduler later proves insufficient, use an Incus placement
scriptlet before adding a scheduler to the provider.

Templates and physical network policy are provider-wide. New Lightspeed
bindings are not copied into provider configuration: the first create lazily
reconciles a deterministic restricted project, ACL, managed network, and
profile from the request binding context. Incus chooses and persists the
binding network subnet (`ipv4.address=auto`). The provider derives sibling
network denies from realized Incus inventory and rejects provider-wide denied
CIDRs that overlap an assigned binding subnet.

## Idempotency and ownership

The provider remains database-free. Durable truth consists of Lightspeed
environment intent plus Incus instances and their `user.lightspeed.*`
metadata.

Before creation, the provider adopts either:

- the deterministic target name with exactly matching ownership metadata; or
- a cluster-wide target carrying the same stable request ID.

Create uncertainty is resolved by reading the deterministic target again. A
retry never creates a second environment merely because the serving Incus API
endpoint changed.

Owned target observations include the current Incus member as diagnostic
metadata in cluster mode. Member location is not part of target identity.

## Reconciliation and images

Projects, profiles, managed networks, ACLs, and ownership metadata are
reconciled through the cluster API. The configured storage pool must already
exist with the same logical name on eligible members; member-specific pool
initialization remains deployment work.

The provider imports an immutable image fingerprint when absent. Incus owns
cluster image replication through `cluster.images_minimal_replica`; the
provider does not copy images member by member or run a separate distribution
service.

## Data plane and public ingress

The provider continues to accept Lightspeed-initiated, target-specific data
connections. Incus cluster requests return target state and current member
location through the cluster API. The provider reaches the selected guest over
the deployment's provider-routable managed network.

Cluster networking must therefore make guest addresses unambiguous and
reachable from the provider and P121 edge listener. A shared Incus managed
network such as OVN is appropriate. Independently overlapping per-member
bridges are not supported by clustered mode.

Public ingress remains reconstructed from cluster-wide Incus instance
metadata. Moving an instance does not change its public hostname or target ID.

## Cordon, drain, and failure

Incus owns member scheduling state:

- `scheduler.instance=manual` cordons a member for automatic placement;
- cluster groups limit eligible placement sets;
- Incus exposes offline and evacuated member state; and
- explicit operator evacuation is available for planned maintenance.

The provider observes these facts but does not run background evacuation,
healing, or rebalance. Initial production settings keep automatic healing and
rebalance disabled. A failed member makes its local-storage environments
unavailable until the member recovers or an operator applies an explicit
recovery policy.

Drain is an operator workflow: inspect the instances located on a member, then
explicitly close, move, or rebuild them. The provider never silently migrates
or destroys an environment.

## Availability and safety

The provider rotates across configured cluster API endpoints for transport
failover. A request is successful only after a valid Incus response and any
reported operation completes. Configuration or authorization errors are not
hidden as endpoint failures.

The provider reports cluster mode and current member location only as bounded,
non-sensitive target metadata. Logs and metrics must not label universe IDs,
environment IDs, addresses, commands, or credentials.

Recommended conservative cluster settings:

- `cluster.healing_threshold=0`;
- automatic rebalance disabled;
- local member storage permitted, with no implied failover;
- explicit evacuation only; and
- at least three members for production quorum tolerance.

## Implementation

- [x] Add explicit `single` and `cluster` provider configuration.
- [x] Add ordered Incus API endpoint failover.
- [x] Validate single-server versus shared-cluster topology.
- [x] Preserve one deterministic cluster-wide target identity.
- [x] Expose current cluster member as diagnostic target metadata.
- [x] Add optional template-to-cluster-group placement.
- [x] Reconcile provider resources through the cluster API.
- [x] Delegate immutable image replication to Incus.
- [x] Keep envd relay and public ingress cluster-location-safe through a
      provider-routable OVN binding network.
- [x] Add single-node and cluster configuration/deployment guidance.
- [x] Add unit coverage and an opt-in non-destructive live topology and
      reconciliation test.
- [x] Keep the provider stateless across restart and endpoint changes.

The code and runbook are complete. Live multi-member provisioning, endpoint
failure, cluster-group placement, cordon, move, envd, and ingress acceptance
remain deployment verification because this workspace has no configured Incus
cluster.

## Verification

- Run all provider lifecycle operations in `single` mode.
- Provision concurrently across at least two members in `cluster` mode.
- Prove configured API endpoint loss does not interrupt operations while
  another cluster endpoint is healthy.
- Prove different-cluster endpoints are rejected.
- Prove request retries adopt one target across provider restart and endpoint
  failover.
- Prove cluster-group placement and Incus capacity rejection.
- Prove a cordoned member receives no automatic placement.
- Prove one member's location appears diagnostically without entering target
  identity or Lightspeed's schema.
- Prove envd and public ingress continue after an explicit supported move.
- Prove provider restart reconstructs all target and route state without a
  provider database.

## Done

P122 is complete when the same provider works against a standalone Incus
server and a native Incus cluster, endpoint failover and placement constraints
are verified, node loss remains non-destructive, and all durable provider state
is reconstructible from Lightspeed plus Incus.

## Deferred

- Pooling unrelated standalone Incus servers in one provider.
- Provider-owned capacity scheduling or reservation ledgers.
- Automatic healing, rebalance, evacuation, or cross-member recovery.
- Shared-storage deployment and live-migration guarantees.
- Provider-managed cluster formation, upgrades, and quorum recovery.
- Multi-region Incus clusters, billing history, and general device scheduling.
