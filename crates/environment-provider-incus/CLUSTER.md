# Incus cluster operations

The provider consumes an existing native Incus cluster. It does not form,
upgrade, heal, or remove cluster members.

## Deployment prerequisites

Before starting the provider:

1. Form one Incus cluster and configure trusted HTTPS access on every endpoint
   listed in `incus.endpoints`.
2. Use the same cluster certificate authority for those endpoints and mount a
   restricted provider client certificate and key.
3. Create the configured storage-pool name on every member eligible for VM
   placement. Local storage is supported but does not imply failover.
4. Configure the OVN control plane and create the physical or bridge uplink
   named by `incus.clusterNetworkUplink` on every member.
5. Ensure the provider and P121 edge can route to the resulting guest address
   ranges.
6. Assign members to any cluster groups referenced by template
   `clusterGroup` values.
7. Keep `cluster.healing_threshold=0` and automatic rebalance disabled unless
   a later recovery design explicitly changes that policy.

Start with `config.cluster.example.json`. `GET /health` should report every
member and its scheduling state.

## Cordon and drain

Cordon a member with native Incus scheduling state:

```bash
incus cluster set member-a scheduler.instance manual
```

List its environments before deciding how to drain it:

```bash
incus list location=member-a
```

Close, rebuild, or explicitly move each environment according to operator
policy. Do not enable automatic evacuation as a substitute for that decision.
When maintenance is complete:

```bash
incus cluster unset member-a scheduler.instance
```

## Verification

The non-destructive live topology/reconciliation test is opt-in:

```bash
LIGHTSPEED_INCUS_PROVIDER_CONFIG=/absolute/path/provider.json \
  cargo test -p environment-provider-incus --test incus_live -- --ignored
```

Before production rollout, also provision an expendable environment, verify
its `incusMember` target metadata, disable the serving API endpoint, exercise
filesystem/process/PTY/job and public-ingress traffic through another API
endpoint, and close the environment. Repeat with a `clusterGroup`-constrained
template and a cordoned member.
