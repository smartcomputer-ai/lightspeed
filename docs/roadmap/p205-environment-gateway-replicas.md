# P205 — Environment gateway replicas

**Status:** Proposed, 2026-11-13. Not started. Independent of
[P202](p202-session-summaries-and-change-signals.md), which only makes two
steps here faster.

Addresses
[#164](https://github.com/smartcomputer-ai/lightspeed/issues/164).

## Goal

Several `environment-gateway` processes behind one address. Losing one
affects only the daemons connected to it, and those reconnect to another
without operator action. Workers keep one gateway URL. The daemon protocol
does not change.

## Current state

A deployment must run exactly one `environment-gateway` process, because
this state lives in its memory (`RegisteredConnections` in
`crates/temporal-runtime/src/gateway/registration.rs`):

1. **Control connections.** A registered daemon holds one control
   connection. `open_registered_route` finds it in `controls`; a worker
   route that lands on another process is refused as disconnected.
2. **Dial-back pairing.** For each worker route the gateway sends
   `registration/openStream` with a one-time token. The daemon dials
   `/environment-gateway/data` with it, and `data_upgrade` pairs the two
   sockets through `pending`. Behind a load balancer the dial can land on a
   different process.
3. **Startup repair.** `repair_registered_connections` marks every
   connected row disconnected at start. A second process starting would
   disconnect the first one's daemons.
4. **Background loops.** The lifecycle reconciler, the registered sweep and
   the power reaper run in this role with no leader.
5. **Signals.** `EnvironmentSignals` revokes routes and nudges the
   reconciler only inside one process.
6. **Failed-registration limiter.** Counted per process.

PostgreSQL records that a daemon is connected (`environments.connected`,
`connection_changed_at_ms`, `last_seen_at_ms`) but not which process holds
it. No table identifies runtime processes.

Workers build route URLs from the single
`LIGHTSPEED_ENVIRONMENT_GATEWAY_URL`
(`crates/temporal-runtime/src/environments/gateway.rs`) and open a
WebSocket to it for each route, authenticated with
`LIGHTSPEED_ENVIRONMENT_GATEWAY_TOKEN`.

## Design

This is a routing problem, not a broadcast: bytes must reach the one
process that holds a socket. It needs to know who holds what, and a way for
replicas to reach each other.

### Three roles per route

- **Owner:** the replica holding the daemon's control connection.
- **Acceptor:** the replica that accepted the worker's route and waits for
  the data socket. Any replica.
- **Receiver:** the replica on which the daemon's dial-back landed. Any
  replica.

Control requests go to the owner. The data socket goes to the acceptor. A
route therefore crosses one gateway process when the dial-back lands on the
acceptor, and two otherwise. It never crosses three.

### Instance registry

A new table, `runtime_instances`: instance id (generated at start), roles,
peer URL, start time and lease expiry. Each `environment-gateway` process
renews its lease every 5 s; a lease lasts 20 s (proposed values).

The peer URL is the address other replicas use to reach this process, from
a new setting (proposed: `LIGHTSPEED_ENVIRONMENT_GATEWAY_PEER_URL`, for
example built from the pod IP). A single-replica deployment does not need
it.

A replica that cannot renew its lease for a full lease period closes its
control connections. Its daemons reconnect elsewhere, and no two replicas
believe they own the same daemon for long.

### Ownership

`environments` gains `owner_instance_id` and `control_connection_id`.

- **Admission** writes both, with `connected`.
- **Disconnect** clears them only where both still match, so a replica
  cannot clear a newer connection made on another replica.
- **Supersede across replicas.** A daemon that reconnects to another
  replica overwrites the owner. The old replica drops its stale handle when
  it next compares its handles with the rows it owns, on every lease
  renewal.
- **A dead owner.** The sweep marks rows disconnected whose owner's lease
  has expired. This replaces the startup repair. The existing reconnect
  grace in `use_gate` applies unchanged.

### Opening a route

1. A worker dials the gateway URL as today and reaches the acceptor.
2. The acceptor reads the environment row, as it already does for the use
   gate, and with it the owner and the owner's peer URL. No live owner
   means the `disconnected` refusal of today.
3. The acceptor creates a token that names itself, registers the pending
   stream locally, and asks the owner to open a stream: a local call when
   it is the owner, otherwise a peer request.
4. The owner sends `registration/openStream` with that token on the control
   connection and answers with its control connection id.
5. The daemon dials `/environment-gateway/data` through the public address
   and reaches the receiver.
6. The receiver reads the acceptor from the token. If it is the acceptor it
   pairs locally, as today. Otherwise it connects to the acceptor's peer
   URL and relays the socket.
7. The acceptor pipes the worker socket to the data socket.

The token is opaque to the daemon (`OpenStreamParams.token`), so the
protocol is unchanged. Its proposed form is
`<acceptor instance id>.<nonce>.<mac>`, where the MAC is keyed from the
deployment gateway token. A receiver verifies the MAC before it connects to
a peer, so an unauthenticated dial cannot make replicas open connections to
each other. The token stays one-time and short-lived.

Client-IP affinity at the load balancer is a useful optimization where it
is available. It is not required for correctness.

### Peer endpoints

Two internal endpoints on the existing listener, authenticated with
`LIGHTSPEED_ENVIRONMENT_GATEWAY_TOKEN`, and rejected at the public edge
like `/environment-gateway/routes/`:

```text
POST /environment-gateway/peer/streams   ask the owner to open a stream
GET  /environment-gateway/peer/data      hand a dialed socket to the acceptor
```

The first answers that the stream was requested, or that this replica no
longer holds the connection; the acceptor then refuses the route as
disconnected and the worker retries as today.

### Ending a route

Today a route ends when its environment is revoked or its control
connection ends. Both are in-process signals.

- The acceptor records the control connection id from step 4. The periodic
  `revoke_ended_routes` check also ends a route whose row no longer shows
  that connection id.
- `data_upgrade` rejects a dial whose issuing control connection is no
  longer current. With an acceptor on another replica, that check moves to
  the acceptor and compares against the row.
- With P202, an `environment` change signal ends such routes at once. The
  periodic check remains the fallback.

### Background loops

The lifecycle reconciler, the SQL part of the registered sweep and the
power reaper each run under a session-level advisory lock, following
`CasSweepLeader` in `crates/store-pg/src/cas_sweep.rs`. Each replica keeps
running the part of the sweep that ends its own local routes.

### Start and shutdown

- **Start:** register the instance and begin renewing the lease. No repair.
- **Graceful shutdown:** stop accepting connections, send
  `registration/closed` to each daemon so it reconnects to another replica
  at once, keep open data streams until the shutdown deadline, mark owned
  rows disconnected, delete the instance row.

### Failed-registration limiter

It stays per process. With N replicas the effective limit is N times the
configured one. Accepted; the limiter protects handshake capacity, not a
security boundary.

## Failure cases

| Event | Effect |
| --- | --- |
| The owner dies | Its daemons reconnect to another replica with their usual backoff. Routes through it close; the worker's existing retry handling applies. Rows are marked disconnected when the lease expires. |
| The acceptor dies | Its routes close. Other routes to the same daemon are unaffected. |
| The receiver dies while relaying | Those routes close. |
| A replica loses PostgreSQL | It closes its control connections after one lease period. |
| A rolling update | Each replica hands its daemons over on shutdown; routes open during the handover are refused as disconnected and retried. |

## Rejected

- **Workers look up the owner and dial it directly, as the baseline.**
  Every worker would need to reach every gateway pod, and the pod addresses
  would become worker configuration. It can be added later as an
  optimization: an acceptor that is also the owner is already the fast
  path.
- **The acceptor forwards the whole route to the owner.** A route could
  cross three processes when the dial-back also lands elsewhere.
- **Multiplex data streams over the control connection.** No dial-back and
  no pairing, but it is a daemon protocol change with its own flow control,
  and large transfers would compete with interactive output on one
  connection.
- **A Temporal task queue per replica.** Workable for requests, not for
  byte streams.

## Implementation order

1. `runtime_instances`, the lease, and leader locks for the three loops.
2. Ownership columns; conditional connect and disconnect; lease-expiry
   sweep in place of the startup repair; supersede across replicas.
3. Tokens that name the acceptor; the two peer endpoints; relayed
   dial-backs.
4. Graceful shutdown with handover.
5. Documentation that states the one-process rule, after sign-off:
   `docs/documentation/deployment/operations.md`,
   `docs/documentation/deployment/overview.md`,
   `docs/documentation/environments/networking-and-ingress.md`,
   `docs/documentation/reference/environment-variables.md` and
   `docs/documentation/how-it-works/architecture.md`.

Steps 1 and 2 are safe with one replica and can ship before step 3.

## Tests

- **Two replicas in one test process:** a route accepted by the replica
  that does not own the daemon works; a dial-back that lands on the other
  replica is relayed; a token with a bad MAC is refused before any peer
  connection.
- **Ownership:** a disconnect from a superseded connection does not clear a
  newer owner; a replica drops a handle it no longer owns; rows of an
  expired lease become disconnected; a starting replica disconnects
  nothing.
- **Leadership:** each loop runs on one replica and moves when that replica
  stops.
- **Shutdown:** daemons receive `registration/closed` and reconnect to the
  other replica.
- **Live:** the existing environment suites pass with two replicas behind
  one address.

## Out of scope

- Changes to the daemon's wire protocol.
- Scaling the provider controllers.
- Replicating the public `gateway` role, which already holds no connection
  state.

## Open questions

- **Worker retries.** When a route closes mid-stream because a replica
  died, confirm what each tool execution class does today and whether a
  running process's output can be resumed.
- **Concurrent admission.** Confirm that the capacity limit
  (`max_active_environments`) and the ownership write hold when the same
  key admits daemons on two replicas at once.
- **Provider routes.** They hold no daemon connection and should already
  work from any replica. Confirm.
- **Direct dialling.** Is a later optimization where workers dial the owner
  worth the extra addressing?
