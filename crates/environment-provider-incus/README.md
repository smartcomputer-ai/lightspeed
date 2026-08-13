# Incus environment provider

`lightspeed-provider-incus` is a stateless P118 controller, passive P119 data
endpoint, optional P121 application edge, and P122 single-node/cluster Incus
adapter. It depends on the public host
protocol crate, not Lightspeed's database, API implementation, engine, or
Temporal runtime.

Start from `config.example.json`. Application-level authentication for both
the provider endpoint and private envd endpoint is intentionally deferred.
Protect those listeners with the deployment network/transport boundary for
now. The Incus client certificate and key remain deployment files mounted into
the provider process.

## Incus topology

The provider has two explicit modes:

- `single` connects to exactly one standalone Incus server; and
- `cluster` connects through one or more failover API endpoints belonging to
  the same native Incus cluster.

`config.example.json` demonstrates single mode and
`config.cluster.example.json` demonstrates cluster mode. The provider checks
the topology at startup. In cluster mode, every reachable configured endpoint
must report identical member names and addresses. A two-member cluster is
accepted with a quorum warning; three or more members are recommended for
production.

The provider does not form the cluster, join or remove members, initialize
member-local storage/uplinks, evacuate instances, or recover quorum. Perform
those operations with deployment tooling and the Incus CLI.

Cluster mode uses one provider-managed OVN network per binding over the
existing `incus.clusterNetworkUplink`. The provider and optional public edge
must be able to route to addresses on those networks. Standalone mode retains
provider-managed bridge networks.

Templates can set `clusterGroup` to target native Incus placement at that
group. Omitting it lets Incus choose from all schedulable members. The provider
does not duplicate Incus capacity accounting or scheduling.

Use native Incus controls for maintenance:

```bash
# Prevent automatic placement on one member while existing VMs keep running.
incus cluster set member-a scheduler.instance manual

# Inspect environments before an explicit drain decision.
incus list location=member-a

# Re-admit the member after maintenance.
incus cluster unset member-a scheduler.instance
```

Do not enable automatic healing or rebalance for the initial Lightspeed
deployment. An evacuation or move is an explicit operator action. With local
member storage, an offline member's environments remain unavailable rather
than being destructively recreated.

`GET /health` checks the active Incus topology and returns only bounded
diagnostics: mode and each member's name, status, and schedulability.

There is intentionally no provider ID in this configuration. The operator
assigns that identity when registering the reachable controller endpoint.

Build an image with `image/build-image.sh`, then replace the example template's
image fingerprint with the immutable fingerprint. The provider deliberately
rejects aliases and arbitrary universe-supplied images or cloud-init.

```bash
cargo run -p environment-provider-incus -- \
  --config crates/environment-provider-incus/config.example.json
```

Register its controller as `ws://127.0.0.1:19090/control`. Lightspeed derives
the sibling `/routes/...` endpoint and connects only while an environment is
being used; the provider then dials private envd and reaps idle connections.
Bindings are
configured twice for different purposes: Lightspeed stores only admission and
routing identity, while this file is the provider's authoritative entitlement,
quota, template, and physical-network policy.

Optional P121 ingress uses one provider-approved guest port per template. Set
`ingress.publicBaseUrl` to the wildcard HTTPS base and `ingress.listen` to the
provider edge listener behind deployment-managed wildcard TLS. For a template,
`publicIngress: true` requires `ingressPort`. Lightspeed callers can only
enable or disable the resulting endpoint; they cannot select a port, private
address, hostname, or proxy configuration.

The stateless edge resolves each request hostname from current Incus target
metadata and proxies HTTP, streaming bodies, and WebSockets to the approved
private guest port. The provider records the realized hostname and port in
`user.lightspeed.*` Incus target metadata so route operations remain
ownership-fenced and reconstructible without a provider database.
