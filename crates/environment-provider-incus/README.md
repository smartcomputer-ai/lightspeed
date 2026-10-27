# Incus environment provider

`lightspeed-provider-incus` is a stateless controller, data-route relay,
optional application edge, and single-node/cluster Incus adapter. It serves
the controller plane of the environment protocol and depends on the public
protocol crate, not Lightspeed's database, API implementation, harness, or
Temporal runtime.

Start from `config.example.json`. The listener serves the controller on
`/control` and the data routes under `/routes/...`. Both require the provider
credential: `credentialFile` names a file holding a secret of at least 32
bytes without whitespace (a trailing newline is ignored), which Lightspeed
presents as `Authorization: Bearer` and the provider compares in constant
time. A missing or wrong credential is refused with HTTP 401 before the
WebSocket upgrade. `GET /health` needs no credential. Register the same
credential with the provider in Lightspeed. The guest envd listener has no
authentication of its own; keep guest networks private. The Incus client
certificate and key remain deployment files mounted into the provider
process.

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

Templates are provider-wide. They can set `clusterGroup` to target native Incus
placement at that group. Omitting it lets Incus choose from all schedulable
members. The provider does not duplicate Incus capacity accounting or
scheduling.

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

Register its controller as `ws://127.0.0.1:19090/control` with the credential.
`dataRouteBase` is advertised as `routes.data` in the controller's
`initialize`: it must reach this listener's `/routes` from the environment
gateway, and it must use `wss://` when Lightspeed reaches the controller over
TLS. Lightspeed connects to a data route only while an environment is being
used; the provider then dials private envd and relays frames unchanged. The
relay forwards WebSocket pings, counts every frame as activity, and closes a
connection only after `relayIdleSeconds` without frames, which cannot be set
below 60.
Registering the provider and creating an enabled binding in Lightspeed is
sufficient to use it. Provider configuration contains no universe or binding
allowlist. Every request carries a binding context, and the provider lazily
creates a deterministic restricted Incus project, managed network, ACL, and
profile for that ownership namespace. Incus chooses and persists an unused
subnet, so adding a universe does not require editing or restarting the
provider.

The provider configuration owns only provider-wide offerings and physical
policy: templates, blocked egress destinations, Incus topology, storage, and
optional ingress. A blocked CIDR must not overlap a subnet Incus assigns to a
binding network; the provider rejects that conflict during lazy reconciliation.
The provider also derives sibling-network ACL rules from live Incus inventory.
Lightspeed owns whether a universe may route to this provider. Per-universe
commercial quotas or specialized offerings would need a future provider
administration mechanism; they are intentionally not modeled as deployment
JSON entries.

Optional public ingress uses one provider-approved guest port per template. Set
`ingress.publicBaseUrl` to the wildcard HTTPS base and `ingress.listen` to the
provider edge listener behind deployment-managed wildcard TLS. For a template,
`publicIngress: true` requires `ingressPort`. Lightspeed callers can only
enable or disable the resulting endpoint; they cannot select a port, private
address, hostname, or proxy configuration.

The stateless edge discovers provider-managed Incus projects and resolves each
request hostname from current target metadata. It proxies HTTP, streaming
bodies, and WebSockets to the approved private guest port. The provider records
the realized hostname and port in `user.lightspeed.*` Incus target metadata so
route operations remain ownership-fenced and reconstructible without a
provider database or configured universe list.

## Controller behavior

Every controller method answers within about five seconds with the target's
current observation. Slow Incus work (an image pull, a boot, a graceful stop)
runs as one background job per target, serialized per target; later calls
report `provisioning` or `closing` while it runs, and a failed create reports
`failed` with the Incus error as the target's `message`. Job state is in
memory; Incus itself, read through the deterministic owner name, is the record
of every target, so a provider restart loses nothing.

- `target/create` looks up the owner's instance before applying any creation
  policy, so deprecating a template never affects existing targets.
- `target/get` only reads. A missing instance is `notFound`.
- `target/close` resolves the instance by owner when no target id is given,
  stops it gracefully for up to 30 seconds, then forces the stop and deletes
  the instance, its disk, and its ingress metadata.
- Templates report typed `resources`. `memory` and `disk` must be whole Incus
  sizes such as `4GiB`; the provider converts them to bytes at startup.
- Cloud-init sets `LIGHTSPEED_ENVD_STATE_DIR=/var/lib/lightspeed-envd`, so the
  daemon key and job records live outside the agent's `/workspace`.
- Incus API failures answer `unavailable`, which the runtime retries.
- The provider advertises `adoptTarget`, and `ingress` when an `ingress` block
  is configured.

## Adoption

`target/adopt` takes over an existing Incus VM as a managed environment. The
`sourceTarget` is `instance` or `project/instance`; the project defaults to
`default`. Project and instance names are 1 to 63 ASCII letters, digits, and
hyphens, without a leading or trailing hyphen. Only persistent virtual
machines that are not already Lightspeed-managed can be adopted; containers
and ephemeral instances are refused with `notAdoptable`.

Adoption moves the VM into the binding's project under the owner's
deterministic name and records ownership metadata. It replaces the VM's
profiles with the binding baseline, rewrites `eth0` onto the binding's managed
network, and disables every other local NIC. A running source is stopped
first. A later close destroys the adopted VM like any other target, so adopt
only machines whose disk Lightspeed may delete.

## Conformance

The ignored live test `the_controller_conforms_on_live_incus` runs the
environment client's controller conformance suite against this provider on a
live Incus server or cluster. It creates and deletes one VM from the first
non-deprecated template, using the fixed universe `lightspeed-conformance` and
binding `incus-live`:

```bash
LIGHTSPEED_INCUS_PROVIDER_CONFIG=/absolute/path/provider.json \
  cargo test -p environment-provider-incus --test incus_live \
  the_controller_conforms_on_live_incus -- --ignored
```
