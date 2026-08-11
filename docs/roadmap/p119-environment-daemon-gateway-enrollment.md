# P119: Environment Daemon, Gateway, And Direct Enrollment

**Status**

- Proposed 2026-08-11.
- Builds on [P118](p118-environment-domain-and-lifecycle.md).
- Implements the canonical data plane from
  [P117](p117-environment-compute-plan.md).

## Goal

Replace the attach-only, inbound `host-bridge` provider with one canonical
`lightspeed-envd` daemon that connects outbound through an authenticated
environment gateway.

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

- authenticated outbound daemon sockets;
- route ownership for the current incarnation;
- multiplexing, correlation, streaming, cancellation, and backpressure;
- connection leases and immediate availability observations; and
- per-environment transport limits.

Lightspeed Postgres owns environment/incarnation identity, enrollment ticket
hashes and expiry, admitted daemon public keys, revocation, and bounded
availability observations. Ephemeral sockets and `connectionId` values remain
in gateway memory.

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
the previous daemon identity. Only one connection owns a daemon/incarnation
route; admission of a replacement fences the old connection.

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
One gateway replica is sufficient for this milestone. Gateway HA and relay-bus
design remain deferred.

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
- [ ] Implement one-time enrollment storage and APIs.
- [ ] Implement the environment gateway deployment role and worker routing.
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
- token expiry/replay, signature failure, revoked daemon, stale incarnation,
  duplicate connection, and superseded-route rejection; and
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
