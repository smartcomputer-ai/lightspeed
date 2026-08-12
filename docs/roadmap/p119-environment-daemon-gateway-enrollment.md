# P119: Environment Daemon, Gateway, And Direct Enrollment

**Status**

- Proposed 2026-08-11.
- Builds on [P118](p118-environment-domain-and-lifecycle.md).
- Implements the canonical data plane from
  [P117](p117-environment-compute-plan.md).

## Goal

Replace the attach-only, publicly inbound `host-bridge` provider with one
canonical `lightspeed-envd` daemon reached through an authenticated
environment gateway route. Directly enrolled hosts use an outbound daemon
connection; provider-managed environments may later use either that form or a
multiplexed provider relay.

After P119, an existing laptop, workstation, server, or manually managed VM can
be enrolled directly as a universe environment without creating a physical
provider record. Provisioned environments will use the same gateway path in
P120.

## Boundaries

`lightspeed-envd` owns execution semantics inside the environment:

- rooted filesystem access;
- process and PTY execution;
- bounded search, glob, and ranged reads;
- provider-owned durable jobs and retained output; and
- capability negotiation.

The environment gateway owns only live transport:

- authenticated direct-daemon or provider-relay sockets;
- route ownership for the current incarnation;
- multiplexing, correlation, streaming, cancellation, and backpressure;
- connection leases and immediate availability observations; and
- per-environment transport limits.

Lightspeed Postgres owns environment/incarnation identity and, only for
directly enrolled environments, bootstrap ticket hashes, daemon public keys,
and revocation. A provider-mediated route is authorized through the provider
identity, universe binding, environment ownership, and current incarnation; it
does not create a daemon-enrollment row. Ephemeral sockets, route presence,
`connectionId` values, and idle timers remain in gateway memory. Environment
status may carry a bounded availability observation, but an old observation
never reconstructs a live route.

The gateway does not own environment lifecycle, create machines, execute
Temporal workflows, grant universe API access, or provide public application
ingress.

## Identity and enrollment

```text
environmentId   stable universe environment
incarnationId   admitted boot/replacement generation
daemonId        local public-key fingerprint
connectionId    one ephemeral transport connection
```

Universe API:

```text
environments/enrollments/create
environments/enrollments/read
environments/enrollments/revoke
```

An authenticated universe caller creates a pending enrolled environment and
receives one short-lived token exactly once. Only its hash is stored.
`lightspeed-envd` generates its private key locally, redeems the token, and
proves possession on later reconnects.

Ordinary reconnect keeps environment, incarnation, and daemon IDs but creates
a new connection ID. Rebuild creates a new incarnation. Approved daemon-key
rotation retains the environment and may retain the incarnation while revoking
the previous daemon identity. Only one connection owns a directly enrolled
daemon/incarnation route; acceptance of a replacement fences the old
connection.

The durable enrollment record belongs only to the direct-daemon trust path. It
is not proof that a socket must stay open. Provider provisioning does not
create one: a provider-mediated route is instead accepted only from the
authenticated provider assigned to the environment, through an enabled
universe binding, and for the current incarnation.

```text
direct route:   daemon key -> daemon enrollment -> current incarnation
mediated route: provider identity -> binding -> environment -> current incarnation
```

End-to-end daemon identity behind a provider relay is not required by this
milestone. It may be added later as an additional proof without making direct
enrollment a prerequisite for provider-managed environments.

Base images and snapshots contain no live enrollment token or daemon private
key.

## Transport

Use the simplest TLS transport that passes the conformance suite; WebSocket and
HTTP/2 remain implementation choices. The protocol must support:

- server-to-daemon request/response calls;
- stdout, stderr, PTY, and file streaming;
- daemon output notifications;
- cancellation and bounded backpressure;
- frame and concurrency limits;
- ping/lease detection and graceful reconnect; and
- explicit environment, incarnation, daemon, and connection identity.

Workers resolve a stable gateway route outside deterministic session state.
Worker-to-gateway calls use deployment identity and a resolved
universe/environment context; a raw environment ID alone grants no route.
One gateway replica is sufficient for this milestone. P119 implements direct
outbound connections but defines route/authentication framing that permits a
provider relay to multiplex many environments. Gateway HA and a cross-replica
relay bus remain deferred.

For provider-managed environments, a relay may dial envd over the private
provider network only when a routed call arrives and close that connection
after an idle timeout. The provider owns that dial/wake behavior and idle
policy; the gateway owns logical route correlation and fencing. Directly
enrolled NAT-bound hosts normally remain connected because no separate wake or
inbound reachability channel exists.

Keep local/stdio transport for fast tests. Do not route interactive calls
through Temporal, provider APIs, or Kubernetes control APIs.

## Daemon runtime

Rename the crate and binary:

```text
crates/environment-daemon/
lightspeed-envd
```

envd runs as a dedicated unprivileged user by default. Local configuration
controls allowed roots, cwd, read/write policy, process and concurrency bounds,
advertised capabilities, and its durable state directory. Managed development
VM templates may deliberately grant sudo and Docker access; that makes the VM
the trusted boundary and is not a default for less-trusted hosts.

Provider-owned job semantics from P104 remain intact. After daemon restart, a
known job must either be safely rediscovered with retained status/output or
become a typed terminal interruption. It must never leave a Temporal waiter
parked indefinitely. The implementation may use an OS supervisor or daemon
state; the protocol tests only the observable guarantee.

Package Linux/systemd and macOS/launchd forms. Exact CLI flags and internal
module layout are implementation details.

## Implementation

- [ ] Rename/refactor `host-bridge` to `environment-daemon` /
      `lightspeed-envd`.
- [ ] Separate local filesystem/process/job execution from provider
      registration and inbound serving.
- [ ] Implement daemon key generation, protected storage, challenge proof,
      rotation, and revocation.
- [ ] Implement one-time direct-enrollment storage and APIs; provider-managed
      incarnations must not create enrollment rows.
- [ ] Implement the environment gateway deployment role and worker routing.
- [ ] Keep gateway routes transport-neutral across direct, multiplexed relay,
      and provider-local on-demand connections.
- [ ] Implement multiplexing, streaming, cancellation, limits, leases, and
      incarnation fencing.
- [ ] Route existing environment filesystem/process/job adapters through the
      gateway.
- [ ] Preserve local/stdio transport.
- [ ] Package envd for systemd and launchd with Linux and macOS builds.
- [ ] Convert `ls-dev` to direct enrollment and remove its fake provider.
- [ ] Add ls.bot direct-enrollment UX with one-time token display and bounded
      source/incarnation/availability diagnostics.
- [ ] Delete `attachTarget`, `AttachedHost`, and inbound per-host endpoint
      assumptions left after P118.

## Verification

Run one shared data-plane conformance suite against local/stdio envd and the
real gateway. Cover:

- traversal, symlinks, permissions, cwd, large/ranged files, search, and glob;
- process/PTY streaming, backpressure, cancellation, and bounded output;
- credential injection without log or state leakage;
- job terminality and restart reconciliation;
- direct token expiry/replay, signature failure, revoked daemon, stale
  incarnation, wrong-provider relay, disabled binding, duplicate connection,
  and superseded-route rejection; and
- daemon restart, gateway restart, reconnect, and identity rotation.

The live proof enrolls `ls-dev`, selects it from an ordinary session, and
uses filesystem, process, PTY, credential, and durable-job capabilities through
the gateway.

## Done

P119 is complete when independently managed hosts enroll without provider
records, all ordinary environment tools use the outbound gateway path, stale
or duplicate identities are fenced, and restarts produce either recovered
state or explicit terminal failures without hanging callers.

## Deferred

- Provider-managed Incus VMs: P120.
- Public application ingress: P121.
- Gateway replication and relay infrastructure.
- Windows daemon packaging.
