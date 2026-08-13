# P119: Environment Daemon And On-Demand Gateway Routing

**Status**

- Core implementation completed 2026-08-12.
- Simplified to reachable passive daemons on 2026-08-13.
- Builds on [P118](p118-environment-domain-and-lifecycle.md).

## Goal

Provide one canonical `lightspeed-envd` data-plane daemon for filesystem,
process, PTY, and durable-job operations. Lightspeed reaches it only when an
operation needs a connection.

There are two routes:

- An external environment stores a typed `daemonConnection` and Lightspeed
  dials envd directly.
- A provisioned environment stores provider/binding/target linkage.
  Lightspeed dials the provider's target-specific route; the provider then
  dials envd on its private network.

There is no daemon-initiated transport, enrollment token, daemon identity,
daemon-admission table, persistent route registry, or permanent data-plane
connection.

## Durable model

External environments are created through `environments/external/create`.
The request contains a retry ID and a typed connection document:

```json
{
  "requestId": "external-dev-1",
  "connection": {
    "endpoint": "ws://devbox.internal:19091",
    "transport": "webSocket"
  }
}
```

Postgres stores this as `environments.daemon_connection_json`. It has the same
connection shape as `environment_providers.controller_connection_json` but a
different semantic role. No connection document contains application secrets.

Lightspeed creates one incarnation when it creates the external environment.
The current incarnation and logical lifecycle state fence all subsequent
routes. Closing the environment prevents new routes; an active gateway proxy
periodically rechecks the current incarnation and state and closes when they no
longer match.

## Transport and routing

Temporal workers connect to a stable, deployment-authenticated Lightspeed route:

```text
/environment-gateway/routes/{universe}/{environment}/{incarnation}
```

The gateway reads current durable state on each connection. It then opens one
outbound WebSocket to either the external envd connection or the registered
provider's target route and proxies host-protocol frames unchanged.

This works with multiple Temporal workers because workers do not own daemon
connections or in-memory route entries. Idle environments use no open
data-plane connection. Provider relays additionally apply an idle timeout.

Application-level authentication for provider and envd connections is
deliberately deferred. Deployments must currently protect those endpoints with
network or transport controls. Worker-to-Lightspeed gateway authentication is
still deployment-internal and unchanged.

## Daemon runtime

`lightspeed-envd` is a passive WebSocket listener:

```text
lightspeed-envd --listen 0.0.0.0:19091
```

It owns rooted filesystem access, process/PTY execution, retained durable jobs,
capability negotiation, concurrency limits, and job state under its configured
state directory. It has no Lightspeed URL, enrollment token, daemon key, or
reconnect loop.

## Implementation

- [x] Rename/refactor `host-bridge` to `environment-daemon` / `lightspeed-envd`.
- [x] Implement filesystem, process, PTY, and durable-job host protocol methods.
- [x] Implement a passive, on-demand WebSocket listener.
- [x] Route workers through a stable Lightspeed gateway path.
- [x] Dial external envd endpoints on demand.
- [x] Dial provider target routes on demand and fence them by durable ownership.
- [x] Remove `attachTarget`, `AttachedHost`, inbound host endpoints, direct
      enrollment, daemon keys, leases, and the in-memory route registry.
- [ ] Package envd for systemd and launchd.
- [ ] Preserve a local/stdio test transport if still useful.

## Verification

- filesystem, process, PTY, and durable-job conformance;
- external and provider route reachability;
- universe, binding, environment, incarnation, and target fencing;
- multi-worker use through the stable gateway route; and
- proof that idle environments hold no permanent connection.

## Deferred

- Application-level or mTLS provider/envd authentication.
- Public application ingress (P121).
- Additional Incus nodes and scheduling (P122).
- Windows daemon packaging.
