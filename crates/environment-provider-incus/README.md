# Incus environment provider

`lightspeed-provider-incus` is a stateless controller, data-route relay that
wakes powered-down targets, idle power manager, optional application edge,
and single-node/cluster Incus adapter. It serves the controller plane of the
environment protocol and depends on the public protocol crate, not
Lightspeed's database, API implementation, harness, or Temporal runtime.

Start from `config.example.json`. The listener serves the controller on
`/control` and the data routes under
`/routes/{universeId}/{environmentId}/{targetId}`. With `credentialFile`
set, both require the provider credential: the file holds a secret of at
least 32 bytes without whitespace (a trailing newline is ignored), which
Lightspeed presents as `Authorization: Bearer` and the provider compares in
constant time. A missing or wrong credential is refused with HTTP 401 before
the WebSocket upgrade. Register the same credential with the provider in
Lightspeed. Without `credentialFile` the listener checks no credential and
Lightspeed sends none; only run it that way on a network where nothing but
Lightspeed can reach the listener. `GET /health` needs no credential. The
guest envd listener has no authentication of its own; keep guest networks
private. The Incus client certificate and key remain deployment files
mounted into the provider process.

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

Cluster mode uses one provider-managed OVN network per universe over the
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

Register its controller as `ws://127.0.0.1:19090/control`, with the credential
when one is configured. Lightspeed derives the data route base from the
controller URL (`ws://127.0.0.1:19090/routes`), which is where this listener
serves its data routes. When the environment gateway reaches the listener
under another address, set the data route base on the provider record in
Lightspeed instead; it must use `wss://` when Lightspeed reaches the
controller over TLS. Lightspeed connects to a data route only while an
environment is being used; the provider then dials private envd and relays
frames unchanged. The relay forwards WebSocket pings, counts every frame as
activity, and closes a connection only after `relayIdleSeconds` without
frames, which cannot be set below 60.

Registering the provider and enabling it for a universe in Lightspeed is
sufficient to use it. Provider configuration contains no universe allowlist.
Every request carries its universe, and the provider lazily creates a
deterministic restricted Incus project, managed network, ACL, and profile
for that universe; every environment of the universe lives in it. Incus
chooses and persists an unused subnet, so adding a universe does not
require editing or restarting the provider.

The provider configuration owns only provider-wide offerings and physical
policy: templates, blocked egress destinations, Incus topology, storage, and
optional ingress. A blocked CIDR must not overlap a subnet Incus assigns to a
universe network; the provider rejects that conflict during lazy reconciliation.
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
of every target, so a provider restart loses nothing but the idle clock of
powered-down targets (see below).

- A target belongs to `{universeId, environmentId}`, and its instance name,
  which is its `targetId`, is derived from that owner. `target/create` looks
  up the owner's instance before applying any creation policy, so
  deprecating a template never affects existing targets.
- `target/get` only reads. A missing instance is `notFound`. Whether envd
  answers is not part of an observation: Lightspeed proves it by dialing the
  data route.
- `target/close` resolves the instance by owner when no target id is given,
  stops it gracefully for up to 30 seconds, then forces the stop and deletes
  the instance, its disk, and its ingress metadata.
- `target/setPower` moves a target to `running`, `paused` (an Incus freeze)
  or `stopped`; Incus targets cannot be `suspended`. Lightspeed calls it only
  for a manual power change, and the provider stays the owner of power
  afterwards.
- Templates report typed `resources`, their `powerStates`, and their default
  `idle` policy. `memory` and `disk` must be whole Incus sizes such as
  `4GiB`; the provider converts them to bytes at startup.
- Cloud-init sets `LIGHTSPEED_ENVD_STATE_DIR=/var/lib/lightspeed-envd`, so the
  daemon key and job records live outside the agent's `/workspace`.
- Incus API failures answer `unavailable`, which the runtime retries.
- The provider advertises `power`, and `ingress` when an `ingress` block is
  configured.

## Waking on dial

A dial of a paused or stopped target's data route wakes it: the provider
unfreezes or starts the instance and then tries envd until it answers. The
dial is held for at most five seconds, and a dial that arrives while a pause
or stop is still running waits for that power-down and the wake after it
before it tries envd. When envd answers in time the
connection upgrades and relays as usual; otherwise the provider refuses with
a typed JSON body and lets Lightspeed retry:

| Situation | Answer |
| --- | --- |
| The target was powered down and is waking | 503 `{"state":"waking"}`, `Retry-After: 2` |
| The target runs but envd does not answer yet | 503 `{"state":"booting"}`, `Retry-After: 2` |
| No instance exists for the owner, or it is being closed | 410 `{"state":"gone"}` |
| Incus reports the instance in its `Error` state | 409 `{"state":"failed","message":…}` |

An Incus API failure during the dial answers 502 without a body.

## Idle policy

The provider powers idle targets down on its own clock. A template may set a
default `idle` policy, and Lightspeed may pass another one on
`target/create`; the target keeps the policy it was created with, recorded
as the `user.lightspeed.idle` instance key. A policy names thresholds in
milliseconds, non-decreasing in the order `pauseAfterMs`, `stopAfterMs`,
`closeAfterMs`; `suspendAfterMs` is not accepted in a template here and
never fires, because Incus targets cannot be suspended.

Every `idleCheckSeconds` (default 60) the provider visits each target with a
policy:

- A running target's idle time is envd's `env/getIdle` report, read over
  the guest network. Running processes, jobs and processes a command left
  running hold it at zero, so they keep the target awake.
- A paused or stopped target has no envd to ask. The provider continues
  from the idle time it measured when it powered the target down, kept in
  memory; after a provider restart, or for a target someone else powered
  down, it starts from zero.
- A target with a data connection in progress, or with a dial waking it, is
  never powered down or closed.
- A target a dial woke is left alone for a grace of `idleCheckSeconds`, at
  least 30 seconds, after that dial's connection ends. Unpausing does not
  reset envd's idle time, so the grace keeps the next pass from pausing the
  target again before the caller's first call arrives.

The most escalated threshold the idle time has reached is applied when it
goes beyond the target's current power: pause, then stop, then close. An
idle close deletes the instance like `target/close`; Lightspeed then sees the
target gone and records the environment as ended by the provider. The next
data route dial wakes a paused or stopped target.

## Conformance

The ignored live tests in `tests/incus_live.rs` run against a live Incus
server or cluster, each creating and deleting one VM from the first
non-deprecated template in the fixed universe `lightspeed-conformance`:

- `the_controller_conforms_on_live_incus` runs the environment client's
  controller conformance suite;
- `the_guest_envd_conforms_on_live_incus` runs the data conformance suite
  against the guest's envd through the provider's data route;
- `a_paused_target_wakes_on_dial_on_live_incus` pauses a target and dials it
  awake;
- `the_idle_loop_pauses_an_idle_target_on_live_incus` creates a target with
  a short idle override and waits for the idle loop to pause it.

```bash
LIGHTSPEED_INCUS_PROVIDER_CONFIG=/absolute/path/provider.json \
  cargo test -p environment-provider-incus --test incus_live \
  the_controller_conforms_on_live_incus -- --ignored
```
