# P205 — Environment gateway replicas

**Status:** Implemented and focused validation passed, 2026-11-14.
The environment change feed from [P202](p202-session-summaries-and-change-signals.md)
continues to accelerate environment revocation; ownership revalidation runs
on each lease renewal.

Addresses
[#164](https://github.com/smartcomputer-ai/lightspeed/issues/164).

## Goal

Several `environment-gateway` processes behind one address. Losing one
interrupts the control connections and data routes passing through it. Its
daemons reconnect to another without operator action, and new worker routes
can use surviving replicas. Workers keep one gateway URL. The daemon protocol
does not change; existing streams are not migrated between processes.

## Starting point

Before this change, a deployment had to run exactly one `environment-gateway` process, because
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
renews its lease every 5 s; a lease lasts 20 s. PostgreSQL supplies lease
timestamps; the process also keeps a conservative monotonic deadline. An
expired instance cannot renew its identity and must restart with a new ID.

The peer URL is the address other replicas use to reach this process, from
`LIGHTSPEED_ENVIRONMENT_GATEWAY_PEER_URL`, for example built automatically
from the pod IP. This is the process's own advertised HTTP(S) origin. All
replicas share an explicitly configured gateway token. A single-replica
deployment does not need a peer URL.

A replica that cannot renew its lease for a full lease period closes its
control connections and data routes, fails readiness, and exits. Its daemons
reconnect elsewhere. Ownership comparisons discard superseded handles on
every renewal.

### Ownership

`environments` gains `owner_instance_id` and `control_connection_id`.

- **Admission** writes both, with `connected` and daemon build metadata,
  conditional on a live lease and the expected bound daemon identity.
  In-flight local admissions are excluded from background ownership scans.
- **Disconnect** clears them only where both still match, so a replica
  cannot clear a newer connection made on another replica. Heartbeat writes
  use the same fence and cannot restore a stale connection. An ephemeral
  daemon's deliberate departure closes its environment under that same fence.
- **Supersede across replicas.** A daemon that reconnects to another
  replica overwrites the owner. The old replica drops its stale handle when
  it next compares its handles with the rows it owns, on every lease
  renewal.
- **A dead owner.** The sweep marks rows disconnected whose owner's lease
  has expired. This replaces the startup repair. The existing reconnect
  grace in `use_gate` applies unchanged.

### Opening a route

1. A worker dials the gateway URL and reaches the acceptor.
2. The acceptor reads the live owner, its peer URL, and the expected control
   connection ID. No live owner means a `disconnected` refusal.
3. The acceptor creates a token naming itself and records the pending stream
   with both ownership IDs before asking the owner to open the stream.
4. The owner verifies that the expected connection is still current and sends
   `registration/openStream`. A local owner uses the same control queue;
   a remote owner is reached through `POST /environment-gateway/peer/streams`.
5. The daemon dials `/environment-gateway/data` through the shared address.
   The receiver verifies the token MAC and expiry before looking up a peer.
6. If the receiver is the acceptor, it pairs locally. Otherwise it connects
   to the acceptor's `peer/data` endpoint and relays the socket once. The
   acceptor consumes the pending token and verifies its issuing connection
   against the database before upgrading the socket.
7. The daemon acknowledges `openStream` after its data upgrade succeeds.
   The owner then completes the peer request. The acceptor joins the worker
   to the paired data socket. This ordering handles a dial-back arriving
   before the owner's HTTP response without waiting on an unknown ID.

The token remains opaque to the daemon (`OpenStreamParams.token`). Its form
is `<acceptor instance id>.<expiry>.<nonce>.<mac>`, with a domain-separated
HMAC keyed from the deployment gateway token. It is one-time and short-lived.
Only the acceptor consumes it; the internal data endpoint cannot relay again.
Timeout and cancellation cleanup remove pending tokens and route watches.

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

- The acceptor pins the control connection ID before dispatching the stream
  request. Its batched ownership check on each lease renewal ends routes
  whose owner or connection ID no longer matches.
- `data_upgrade` rejects a dial whose issuing control connection is no
  longer current. With an acceptor on another replica, that check moves to
  the acceptor and compares against the row.
- Environment change signals accelerate revocation when an environment
  closes. Ownership changes are checked every 5 s. Cancellation interrupts
  proxy writes even when a peer has stopped reading.

### Background loops

The lifecycle reconciler, provider display sweep, and SQL part of the
registered sweep each run under a session-level advisory lock, following
`CasSweepLeader` in `crates/store-pg/src/cas_sweep.rs`. Each replica keeps
running the part of the sweep that ends its own local routes. Losing the
lock session cancels that leader's work; provider controllers already own
power policy, so there is no separate runtime power reaper.

### Start and shutdown

- **Start:** register the instance and begin renewing the lease. No repair.
- **Graceful shutdown:** fail `/ready`, refuse new registrations and worker
  routes, and drain existing streams for up to 10 s while renewing the lease.
  Then close control sockets normally, mark only still-owned rows disconnected,
  and delete the instance row. Daemons reconnect with their existing backoff.
  `registration/closed` is terminal and is never used for a rolling handover.
  `/health` remains process liveness. Control sockets close before database
  cleanup, so a stalled database cannot delay daemon reconnection.

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
   Implemented by migration 024 and `gateway/replica.rs`.
2. Ownership columns; conditional connect and disconnect; lease-expiry
   sweep in place of the startup repair; supersede across replicas.
3. Tokens that name the acceptor; the two peer endpoints; relayed
   dial-backs.
4. Bounded graceful shutdown followed by reconnect, preserving the existing
   daemon protocol.
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
- **Shutdown:** ordinary socket closure makes the same daemon reconnect to
  another replica; no terminal notification is sent.
- **Live:** the existing environment suites pass with two replicas behind
  one address.

## Out of scope

- Changes to the daemon's wire protocol.
- Scaling the provider controllers.
- Replicating the public `gateway` role, which already holds no connection
  state.

## Validation and remaining limits

Implementation lives in `crates/store-pg/src/runtime_instances.rs`,
`crates/temporal-runtime/src/gateway/{replica,peers,registration}.rs`, and the
runtime startup and background-loop wiring. Ownership is private store
metadata; public API DTOs and the daemon wire protocol remain unchanged.
Migration 024 advances the required schema revision to 24. Deployment,
networking, architecture, and configuration documentation was updated with
user sign-off.

Focused coverage includes store concurrency tests in
`crates/store-pg/tests/runtime_instances_live.rs` and an actual-daemon,
three-replica scenario in
`crates/temporal-runtime/tests/environment_gateway_replicas_live.rs`.
Validation passed: scoped all-target Cargo checks, 26 runtime unit tests,
five PostgreSQL concurrency tests, the three-replica live scenario, formatting,
and release metadata checks. The full workspace and legacy live suites were
not run.

Both suites use isolated PostgreSQL schemas. The latter exercises distinct
owner, acceptor, and receiver instances, local pairing, authentication,
startup noninterference, and daemon reconnection after draining an owner.

- **Worker recovery:** a process wait interrupted by transport failure returns
  its handle and already received output, allowing a later `continue_process`
  call. A failed start acknowledgement is reported without blindly repeating
  the command. Durable job activities retry with stable request IDs and daemon
  deduplication. Filesystem transport failures surface as tool errors;
  interrupted mutations can have uncertain outcomes. Gateway reconnection
  alone promises no transparent replay of arbitrary operations.
- **Concurrent admission:** the registration-key row lock serializes capacity
  decisions. A concurrent registration of the same daemon is rechecked under
  that lock before charging capacity. Ownership claims, disconnects,
  heartbeats, and departures are fenced by connection identity.
- **Provider routes:** their existing direct provider connection works from
  any replica. They participate in local draining without daemon ownership.
- **Direct dialling:** workers still use one URL. Direct owner routing remains
  an optional future optimization and is not required for correctness.
- **Failure timing:** owner validity is checked when opening and pairing a
  route; existing route ownership is rechecked every 5 s. Expired-owner rows
  are repaired by the 30-second registered sweep. A crashed gateway does not
  preserve sockets or migrate streams to a surviving replica.
