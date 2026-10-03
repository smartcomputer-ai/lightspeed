# P148 — Key-Based Outbound Environment Registration

**Status**

- Proposed 2026-09-01. Reviewed and revised 2026-09-02, before
  implementation.
- **Implemented 2026-09-02**, slices 1 through 3 plus the deployment
  documentation of slice 4, in four commits on `harbor`: domain, store, and
  API; the outbound transport; lifecycle, model-tool group, and CLI; the
  Platform key management and demo. Verified by the unit suites of every
  touched crate and by the live suite
  `crates/temporal-runtime/tests/environment_registration_live.rs`, which
  runs an in-process `envd` against the real gateway, Postgres, and Temporal:
  first registration, two concurrent reverse-dialed routes, offline and
  identity-only reconnect, a second identity under one key, refusal of a new
  daemon after revocation while reconnects continue, a closed environment
  spending its identity, and ephemeral close through the reconciler.
  Two defects surfaced only live and are fixed: a 64-bit decode of the
  integer active-limit column, and a read-modify-write on connection
  observations that could overwrite a concurrent close with `Offline` (now
  one conditional update). The `environment-gateway` role landed the same
  day: it owns the worker environment route, the public registration routes,
  the lifecycle reconciler, and the power reaper, so a `gateway`-only process
  answers the registration routes with 404 and a deployment pins the daemon
  connections to one process by role rather than by proxy convention. Still
  open from slice 4: multi-replica owner routing, deferred until a deployment
  needs a second `environment-gateway` process.
- Builds on P119's environment protocol, gateway route, incarnation fencing,
  and canonical `lightspeed-envd` runtime. It adds one new environment source
  whose daemon opens its connections outbound.
- Replaces the direct-enrollment sketch in P117 for this scope: the only
  outbound registration credential is a reusable, universe-scoped
  registration key. There are no one-time enrollment tickets or pre-created
  pending environments.
- Review decisions, in order:
  1. The outbound socket is a **control channel**. Each worker route is
     served by a fresh, reverse-dialed data socket that the existing gateway
     proxy handles unchanged. There is no frame multiplexing over one daemon
     socket.
  2. Identity mode is **registration-key policy only**. `envd` has no mode
     setting and always keeps its daemon key in its state directory; the
     key's policy decides what Lightspeed does while a daemon is
     disconnected.
  3. There is no daemon-identity table. The daemon public key and the
     registration-key id are columns on the environment row, with one unique
     index over all rows, closed included.
  4. There is no `Booting` phase and no new status. Registration creates the
     environment `Ready`; disconnect maps to the existing `Offline`.
  5. Ephemeral disconnect grace runs in the existing lifecycle reconciler
     from a `last_seen_at_ms` heartbeat stamp, not from gateway timers.
  6. The key record drops `max_registrations`, `allowed_identity_modes`,
     and every stored counter; capacity is counted from environment rows
     inside the admission transaction.
  7. The registration key is the grouping axis for registered environments.
     The session grant gains a `registrationKeys` allowlist beside
     `providers`, `environments/list` gains a key filter, and model and UI
     views show the key's display name as the group. Pool-claim intents are
     deferred.
- Harbor is an initial motivating use case, not part of this item.
  [P149](p149-harbor-end-to-end-agent-evaluation.md) defines the Harbor agent
  adapter, trial lifecycle, benchmark configuration, and result handling.

## Goal

Let an operator start arbitrary machines, VMs, containers, or pods with a
publicly reachable Lightspeed gateway URL and one registration key:

```text
LIGHTSPEED_ENVD_GATEWAY_URL=wss://lightspeed.example/environment-gateway/connect
LIGHTSPEED_ENVD_REGISTRATION_KEY=<secret>
```

`lightspeed-envd` connects outbound, authenticates with the key, and becomes a
new environment in the key's universe. The same key may register one daemon or
many unrelated daemons concurrently. Key policy bounds active environments,
lifetime, identity mode, and ephemeral cleanup without changing the image or
bootstrap command.

The operator does not manage a client certificate or pre-create one
environment per machine. `envd` generates its own daemon key pair, Lightspeed
assigns the logical environment and incarnation ids, and the gateway returns a
registration receipt. Persisting the daemon key reconnects the same logical
environment; losing it deliberately creates a new one.

This supports:

- a VM or physical host that remains the same environment across restarts;
- a Kubernetes StatefulSet whose `envd` identity lives on stable storage;
- replaceable pods or batch workers that become new environments per instance;
- externally provisioned compute pools whose members all receive the same
  registration key;
- locked-down enterprise footprints, such as one Kubernetes namespace or a few
  VMs, where installing a daemon is possible but installing a provisioning
  provider is not; and
- Harbor started on a developer machine while its task sandboxes run in local
  Docker or any supported remote compute backend and connect to hosted
  Lightspeed.

## Problem

P119 implements `envd` as a passive WebSocket listener. An external environment
stores a reachable daemon endpoint, and Lightspeed dials it on demand. That is
simple on a private network but is a poor fit for Docker Desktop, NATed VMs,
Kubernetes pods, and hosted sandbox providers:

- every environment needs an inbound address or provider-specific tunnel;
- port publication and reachability differ by compute backend;
- a public passive daemon currently relies on deployment network controls
  rather than application authentication; and
- a general pool cannot be bootstrapped from one image and one shared secret.

A Harbor-specific bridge could translate Lightspeed environment calls into
Harbor's environment interface, but that would not exercise the real `envd`
process/PTY/filesystem/job implementation and would create a second execution
adapter to maintain. Outbound registration makes the canonical daemon usable
directly instead.

## Decision

Add **registered environments** alongside the existing provisioned and passive
external environment sources:

```text
environment source
├── provisioned       provider lifecycle + provider-mediated envd route
├── external          stored endpoint; Lightspeed dials passive envd
└── registered        envd connects outbound with a registration key
```

The registered path has these fixed rules:

1. A registration key belongs to exactly one universe and authorizes creation
   of registered environments in that universe. A daemon never chooses or
   claims a universe.
2. A registration key is reusable unless its stored policy limits it. One key
   may admit many distinct daemon identities and environments.
3. `envd` generates a local asymmetric daemon key pair. Lightspeed derives the
   `daemonId` from the public key and proves possession during every initial
   registration or reconnect.
4. Lightspeed, not `envd`, allocates `environmentId` and `incarnationId`. A
   shared registration key holder cannot name an existing environment or take
   it over.
5. A first-seen daemon identity creates one environment atomically. A known,
   active daemon identity reconnects that same environment and does not touch
   the registration key.
6. The registration key admits a new daemon identity; it is not the daemon's
   permanent data-plane credential. After registration, the daemon private key
   authenticates reconnects without requiring an operator-managed certificate.
7. Identity mode is a property of the registration key and is copied onto
   each environment it admits. `envd` neither knows nor chooses a mode;
   persistent and ephemeral daemons run the same binary with the same
   configuration.
8. Client-supplied names and metadata are correlation aids only. They never
   select a universe, environment id, incarnation, authority, or routing
   destination.
9. A registered daemon keeps one outbound control connection while available.
   Every worker route is served by a separate data socket that `envd` opens on
   request, so filesystem, process, PTY, and job calls stay direct gateway
   traffic on their own connection and never become Temporal activities or
   workflows per call.
10. The registration key that admitted an environment is its group. Grants,
    list filters, and user interfaces scope registered environments by key;
    client metadata never does.

This reverses P119's “no daemon-initiated transport” decision only for the new
registered source. Existing passive external and provider-mediated routes stay
valid and unchanged.

## Identity Model

Five identities describe separate authorities and lifetimes:

| Identity | Assigned by | Lifetime and purpose |
|---|---|---|
| `registrationKeyId` | Lightspeed | Identifies one reusable universe-scoped admission policy and the group of environments it admitted; never sent as authority without its secret |
| `daemonId` | Lightspeed from the `envd` public-key fingerprint | Identifies one daemon installation or ephemeral instance |
| `environmentId` | Lightspeed | Stable universe-owned logical environment bound to one daemon identity |
| `incarnationId` | Lightspeed | Fences the current admitted generation of that environment |
| `connectionId` | Gateway | Identifies one live outbound control socket and changes on reconnect |

The registration secret is not an identity. Multiple daemons intentionally
present the same secret. The daemon public key distinguishes them.

### Why the server assigns the environment id

`environmentId` is a universe-owned resource id used by profiles, sessions,
credentials, close operations, and gateway routes. Allowing an untrusted
daemon to choose it would create collision and takeover semantics: a holder of
one shared key could try to register as an existing environment. The gateway
therefore accepts only a daemon public key and bounded descriptive metadata.
The server allocates the logical ids and returns them in the receipt.

The durable create request id is derived server-side from the daemon
fingerprint, following the `session:` convention used for profile-provisioned
environments, so retries of the same first registration converge on the same
environment through the existing `(universe, requestId)` idempotency path. The
exact id string is an implementation detail; it must not depend on a
client-supplied hostname or correlation value.

### One daemon identity, one environment

A daemon public key is bound to at most one registered environment in the
deployment, ever. The binding is a column on the environment row with one
unique index over all rows, including closed ones. Presenting another
registration key cannot move that identity to another universe or environment,
and the identity of a closed environment cannot register again. Moving or
re-registering a machine deliberately means closing the old environment and
resetting the local daemon identity, which is deleting the daemon key file,
before registering again.

Two simultaneous control connections proving the same daemon identity
represent reconnect races or a copied private key, not two environments. The
gateway admits one current connection, assigns a new `connectionId`, and
fences the superseded socket together with its data sockets. Base images and
snapshots must never contain an already generated daemon private key.

## Persistent and Ephemeral Identities

Identity mode is one immutable field on the registration key, copied onto
every environment the key admits. There is no allowed set, no daemon-side
override, and no daemon configuration for it. An operator who needs both
behaviors from one image creates two keys.

`envd` behaves identically in both modes. On start it loads the daemon key
from its state directory or generates one there with restrictive permissions,
connects, and reconnects with backoff until it is told to stop. Whether the
identity survives a restart is decided by where the state directory lives.
What Lightspeed does while the daemon is disconnected is decided by the key.

### Persistent

Persistent mode is for a machine identity expected to survive daemon and host
restarts:

- the state directory is on storage that survives the intended restarts; for a
  Kubernetes StatefulSet that is a PVC from `volumeClaimTemplates`, not the
  StatefulSet name or ordinal;
- a reconnect proves the same daemon key and receives the same
  `environmentId` and `incarnationId`;
- disconnect marks the environment `Offline` and never closes it merely
  because time passed; and
- closing the environment spends the identity. Registering the machine again
  requires deleting the daemon key file, which registers a new environment.

A persistent key whose daemons keep their state on volatile storage
accumulates one `Offline` environment per restart until the key's active
limit refuses more. That is visible in the key's group and is an operator
error, not a protocol case.

### Ephemeral

Ephemeral mode is for replaceable pods, batch workers, benchmark sandboxes, and
other instances that should disappear from Lightspeed after they disappear
from the compute backend:

- the state directory is instance-local, such as `emptyDir` or a directory
  inside the sandbox, so a daemon restart or brief network loss within the
  instance's lifetime reconnects the same environment;
- the first connection creates a new server-assigned environment;
- disconnect marks it `Offline` and starts the key's disconnect grace;
- reconnect with the same daemon key before expiry resumes the same
  environment; and
- expiry closes the environment. A replacement instance with new storage
  registers a new environment.

A Kubernetes Deployment or Job gets this behavior with default storage.
`emptyDir` preserves identity across a container restart inside one Pod; a
replacement Pod receives new storage and therefore registers a new
environment.

## Registration Keys

### Durable record

Add a universe-scoped registration-key record:

```text
registration_key_id
universe_id
display_name                    required; the group name
key_prefix
secret_hash
identity_mode                   persistent | ephemeral; immutable
max_active_environments         optional; non-closed environments admitted by this key
ephemeral_disconnect_grace_ms   deployment default unless set
expires_at_ms                   optional
created_at_ms
revoked_at_ms                   optional
```

The raw secret is generated with cryptographic randomness, returned once, and
never stored. Reuse the API-key mint, SHA-256 hash, and display-prefix code in
`crates/auth` with a distinct secret prefix; a 256-bit random secret needs no
key-derivation function. Status derives from `revoked_at_ms` and
`expires_at_ms`. Registration count, active count, and last use derive from
environment rows carrying the key id; the key row stores no counters.
`display_name` is required because it is the group name shown wherever
registered environments are listed.

Admission locks the key row, counts the key's non-closed environments, and
inserts the new environment in the same transaction. Concurrent registrations
against one key serialize on that lock; registrations beyond the limit receive
a typed capacity refusal and leave no rows.

Known-daemon reconnects do not touch the key. They authenticate with the
daemon key alone and remain allowed after non-cascading revocation.

### Management API

Add universe-scoped management methods:

```text
environments/registration-keys/create
environments/registration-keys/read
environments/registration-keys/list
environments/registration-keys/revoke
```

These are ordinary universe methods, like `environments/external/create`,
which already lets any universe caller add an arbitrary daemon endpoint. A
registration key admits environments into the same universe and nothing more,
so it needs no operator scope.

Create returns the raw secret once and uses a redacted `Debug` implementation.
Read and list return the display prefix, policy, derived counts, status, and
audit times, never the hash or secret. Rotation is create-new then revoke-old;
no method reveals or mutates secret material in place.

Revoke has explicit behavior:

- without cascade, reject new daemon identities while already registered
  daemon identities may reconnect; and
- with cascade, also close every non-closed environment admitted by the key
  through the ordinary close path.

These methods must never enter the Configurator MCP or any other model-facing
tool catalog. The Platform and CLI may expose them to authenticated human or
service administration surfaces. API responses, traces, audit records, and
errors must not contain the raw registration secret. Audit is structured log
events; no audit table is added.

Unlimited reuse is supported by absent limits; it is not a bypass around rate
limits. The gateway applies bounded per-key connection and failed-auth rates
in process memory, in addition to stored capacity limits.

## Daemon Configuration

Support a minimal image/bootstrap contract:

```text
LIGHTSPEED_ENVD_GATEWAY_URL
LIGHTSPEED_ENVD_REGISTRATION_KEY
LIGHTSPEED_ENVD_REGISTRATION_KEY_FILE     alternative to the direct form
LIGHTSPEED_ENVD_REGISTRATION_NAME         optional
LIGHTSPEED_ENVD_REGISTRATION_METADATA     optional bounded JSON object
LIGHTSPEED_ENVD_REGISTRATION_RECEIPT      optional output-file path
LIGHTSPEED_ENVD_CA_FILE                   optional additional trust anchors
LIGHTSPEED_ENVD_STATE_DIR                 existing; also holds the daemon key
```

There is no identity-mode setting; mode is key policy. Supplying both key
forms is an error. `LIGHTSPEED_ENVD_CA_FILE` exists because the daemon trusts
only the bundled WebPKI roots; an enterprise gateway behind a private CA
cannot be reached without it. Outbound HTTP proxy support is deferred.

The direct environment-variable form is a required convenience path: a
preconfigured image may need no input other than the public gateway URL and
shared key. `envd` reads the secret once, removes it from its own process
environment, never writes it into its state directory or receipt, and excludes
it from diagnostics. Today the daemon passes its whole environment to every
child process and job; registration configuration must be removed from the
daemon's environment before any child starts, and a test must prove children
cannot see it.

Removing the variable is not enough against code in the same security domain.
On Linux, `/proc/<pid>/environ` exposes the daemon's initial environment for
its whole lifetime, readable by any process with the same uid. The direct form
is therefore for images whose workloads are trusted. Where the workload is not
trusted, as in a benchmark sandbox, use the file form, delete the file after
the receipt appears, and constrain the key's capacity and lifetime. The
daemon-key exchange narrows a post-bootstrap compromise from “register
arbitrary new environments” to “impersonate this environment”. No in-guest
representation protects against root in the same security domain; prefer a
distinct service user, process namespace, sidecar, or VM boundary for such
workloads.

## Outbound Registration Handshake

`envd` connects to one public gateway route over WSS. Plain WS is accepted only
for loopback URLs. The registration secret never appears in a URL, query
string, close reason, or log field.

The control connection carries registration, heartbeat, and data-connection
requests. It never carries environment-protocol frames.

```text
envd                                      environment gateway
  │                                               │
  ├──────────── WebSocket connect ───────────────►│
  │◄──────── challenge { nonce } ─────────────────┤
  ├─ register {                                   │
  │    protocolVersion,                           │
  │    registrationKey?,                          │
  │    daemonPublicKey,                           │
  │    signature,                                 │
  │    displayName?, metadata?                    │
  │  } ──────────────────────────────────────────►│
  │                                               ├─ authenticate/admit
  │                                               ├─ create or reconnect
  │◄─ accepted { environmentId, incarnationId,    │
  │             daemonId, connectionId,           │
  │             identityMode } ───────────────────┤
  │◄════════ heartbeat / openData requests ═══════╡
```

Cryptography, fixed for the first version:

- the daemon key is an Ed25519 key pair; the seed lives in
  `<state dir>/daemon-key` with mode `0600` and comes from the operating
  system's random source;
- `daemonId` is `daemon_` followed by the hex SHA-256 of the 32-byte public
  key;
- the challenge nonce is 32 random bytes, single-use, bound to the socket, and
  expires with the handshake timeout. A nonce is used instead of a signed
  timestamp because sandbox clocks drift;
- the signature covers the fixed domain string
  `lightspeed-envd-registration/v1` followed by the nonce; and
- there are no certificates, no key rotation, and no attestation.

A symmetric daemon secret sent over TLS and hashed server-side would be equally
safe under TLS server authentication and one message shorter. The key pair is
kept because the secret never transits, a gateway logging bug cannot leak it,
and the server holds nothing that can impersonate a daemon. `ed25519-dalek` is
already a workspace dependency.

For a first-seen daemon public key, `registrationKey` is required and must be
active, unexpired, and within policy. For a known daemon identity, proof of its
private key is sufficient; a supplied registration key cannot change its
universe or binding. The gateway bounds unauthenticated connection count,
handshake duration, frame size, and authentication attempts before allocating
an environment.

Admission of a new daemon is one transaction:

1. resolve and verify the registration-key secret;
2. derive `daemonId` from the proved public key and confirm no environment
   row carries that key;
3. lock the key row and enforce the active limit;
4. allocate `environmentId` and `incarnationId`;
5. insert the environment as `Ready` with its key id, daemon public key,
   identity mode, and `last_seen_at_ms`; and
6. return the receipt.

There is no `Booting` phase. `environments/external/create` already creates
external environments `Ready`, and protocol version, roots, and capabilities
are negotiated on every data connection exactly as they are today, so
registration has nothing to wait for.

Rejections are typed and fall into two classes that `envd` treats
differently:

- **retryable**: gateway unavailable, handshake timeout, capacity refusal,
  rate limit. `envd` reconnects with exponential backoff and jitter; and
- **terminal**: unknown, revoked, or expired registration key for a new
  daemon; closed environment for a known daemon; invalid signature;
  unsupported protocol version. `envd` logs the reason, writes no receipt, and
  exits non-zero. It never retries a terminal rejection and never generates a
  new identity on its own.

## Data Connections

Every worker route is served by its own data socket, as today. Only the dial
direction changes:

```text
worker               gateway                            envd
  │ route request       │                                 │
  ├────────────────────►│ openData { token }  (control)   │
  │                     ├────────────────────────────────►│
  │                     │◄─── WSS /environment-gateway/data
  │                     │      Authorization: Bearer <token>
  │                     ├─ pair, then the existing proxy ─┤
  │◄════════ environment-protocol frames, unchanged ═════►│
```

- The gateway generates a random one-time token per route request, remembers
  it with the waiting worker socket for a short timeout, and sends it over
  the control connection.
- `envd` dials the data route with the token as a bearer header and runs its
  existing per-connection request loop on that socket. The daemon's method
  handler is not modified for client mode; the accept loop gains a dial-out
  sibling.
- The gateway pairs the sockets and runs the same proxy it runs for passive
  external environments, including the periodic re-authorization by universe,
  environment, incarnation, and status. That check also compares the daemon's
  current control connection, so a data socket from a superseded daemon
  connection is fenced.
- The worker sends `initialize` on the data socket exactly as it does for
  every other source.
- A route request while no control connection is current fails as
  unavailable and never falls back to another environment admitted by the
  same key.
- The activation probe in `EnvironmentResolver::selectable` works unchanged:
  it opens one data route, which costs one reverse dial.

This costs one control round trip and one TLS handshake per route, on top of
the route each tool activity already opens today. It avoids a frame
multiplexer, keeps JSON-RPC id spaces and process-output notifications per
connection, and keeps backpressure per route. If per-call latency from remote
daemons ever matters, `envd` can keep one or two pre-opened standby data
sockets so pairing is immediate; that is a later optimization, not part of
this item.

## Environment Record and Routing

Extend the environment source enum with a connection-free registered variant:

```text
Registered {
    registration_key_id,
    daemon_id,
    identity_mode,
}
```

In PostgreSQL, `environments` gains `registration_key_id` and
`daemon_public_key` columns, the source-kind check gains `registered`, the
source-fields check gains the third case, and `daemon_public_key` gets a
unique index over all rows. The in-memory store is per universe and can only
enforce that uniqueness per universe; the PostgreSQL index is the
deployment-wide guarantee.

Registered environments have no provider, provider binding, provider target,
template, stored daemon endpoint, power control, idle policy, or
provider-managed public ingress. Validation mirrors the external source:
desired power running, no idle policy, no ingress. The Environments page hides
power controls for them as it does for external environments.

Availability is projected from the control connection:

- connect sets status `Ready` and stamps `last_seen_at_ms`;
- the gateway re-stamps `last_seen_at_ms` on a fixed interval on the order of
  tens of seconds while the connection lives;
- disconnect sets status `Offline`. `Offline` already exists and the resolver
  already passes it through for non-provisioned sources; wake-on-use applies
  only to provisioned environments, so activating an `Offline` registered
  environment fails with the typed unavailable error; and
- a stale stamp under `Ready` means the gateway stopped without recording the
  disconnect. Consumers treat it as `Offline` and the reconciler repairs the
  row.

No new status is added and the gateway holds no durable lifecycle authority.
The stamp is also the future owner lease: record the gateway instance id
beside it when multi-instance routing arrives.

Temporal workers continue to connect to the stable internal route:

```text
/environment-gateway/routes/{universe}/{environment}/{incarnation}
```

For a registered source the gateway serves it by the reverse dial above. Every
routed request is fenced by universe, environment, incarnation, daemon, and
current control connection.

The gateway replica holding a control connection is the environment's owner.
Worker route requests and the daemon's data dials must both reach it. The
first implementation runs the process that accepts daemon connections as one
replica and states so in the hosted deployment configuration. Workers are
unaffected; they already dial the gateway over an internal URL. The JSON-RPC
gateway may scale separately once the environment gateway is its own role.

Multi-replica support is deferred to slice 4 and consists of:

- an owner lease: the environment row records the owning replica and its
  internal address beside the heartbeat stamp;
- direct worker dialing: `connection_for` reads the owner address from the row
  instead of one fixed base URL;
- a self-describing data token: the owner signs its replica id into the
  token. A replica that receives a data dial pairs locally when it is the
  owner and otherwise relays the socket to the owner over an internal route
  with the existing proxy loop. The relay is the only new code and runs only
  when a dial scattered; and
- optional sticky ingress by cookie or header, which keeps the relay from
  firing at all.

Do not route individual environment calls through Temporal or store socket
payloads in PostgreSQL.

Use WebSocket ping/pong, reconnect backoff with jitter, frame and output
limits, and explicit backpressure on the control connection. Network
disconnect is availability loss, not environment closure by itself.

## Correlation Metadata and Registration Receipt

An operator or orchestrator may supply a bounded display-name hint and bounded
string metadata when starting `envd`. Examples include:

```json
{
  "orchestrator": "harbor",
  "harbor.jobId": "job-...",
  "harbor.trialId": "trial-...",
  "kubernetes.namespace": "agents",
  "kubernetes.podUid": "...",
  "deployment.member": "worker-3"
}
```

The daemon may add bounded implementation facts such as hostname, OS,
architecture, and `envd` version. It must not scrape all environment variables,
cloud instance metadata endpoints, or mounted Kubernetes credentials.

Bounds apply at the handshake only: at most 32 entries, keys up to 64 bytes,
values up to 256 bytes, no control characters, and no key under the reserved
`lightspeed.` prefix. The general environment metadata validation is not
changed.

The gateway records accepted values in the environment's existing metadata
map at first registration. They are descriptive and queryable but never:

- authentication input;
- an idempotency or uniqueness key;
- a substitute for the daemon public key;
- authority to select an existing environment; or
- a routing or grouping key.

The accepted response is also written atomically to the configured receipt
path and emitted as one bounded structured log event:

```json
{
  "environmentId": "environment_...",
  "incarnationId": "incarnation_...",
  "daemonId": "daemon_...",
  "connectionId": "connection_...",
  "identityMode": "ephemeral"
}
```

It contains no registration secret or private-key material. Orchestrators use
the receipt to target the exact environment they started even when many
instances register concurrently with the same key. Harbor will use this
mechanism, but its adapter and cleanup contract are specified separately.

## Grouping and Selection

Registered environments arrive without a provider, binding, or template, and
a busy universe may hold dozens of them from several keys. Nothing today
distinguishes them: the model's `environment_list` tool renders id, provider,
display name, status, and active flag in an unfiltered id-ordered page; the
trusted `environments/list` filters only by provider, binding, status, and
origin session; the Environments page and the profile picker are flat lists;
and a session grant that names allowed providers excludes provider-less
environments entirely.

The registration key is the group. It is server-assigned, trustworthy, and
already the operator's unit of policy. Client metadata stays descriptive.

- **Grant.** `EnvironmentsFeature` gains `registrationKeys`, an optional list
  of key ids beside the existing `providers` list with the same semantics:
  absent means every key is allowed; present means only environments admitted
  by a listed key. An environment passes when its provider is allowed or its
  key is allowed. A profile can therefore scope a session, and a bot through
  its profile, to named pools without any new concept. Harbor's benchmark
  profile restricts sessions to the campaign key.
- **List and read.** `environments/list` gains a `registrationKeyId` filter.
  `environments/read` and list rows expose the key id, daemon id, identity
  mode, and last-seen time for registered environments.
- **Model view.** The `environment_list` tool output gains a `group` field
  holding the key's display name, and the tool accepts a `group` filter.
  Nothing about the registration key beyond its display name is
  model-visible.
- **User interfaces.** The Environments page and the profile environment
  picker group registered environments under the key's display name; the
  registration-keys page lists each key's environments.
- **Profiles and bots.** The existing intents are unchanged: `existing` binds
  one environment by id, `inherit` follows the parent, and `provision` stays
  provider-only. A bot's environment remains an `existing` environment, as
  decided in [P140](p140-bot-environments-and-bot-lifecycle.md). Profile
  validation warns when `existing` names an ephemeral registered environment,
  because the key's grace will close it under the bot.

Deferred, deliberately:

- a pool intent such as `{ type: "pool", registrationKeyId }` that claims any
  ready environment from a key exclusively for one session. That needs a claim
  and release model and is a separate item; and
- selection by name within a key, which would let a profile reference a
  machine before it registers. Until then, register first and read the id
  from the receipt or the key page.

## Lifecycle and Failure Semantics

- **New daemon + valid key:** create one `Ready` environment and return its
  receipt.
- **Known daemon reconnect:** return the same environment/incarnation with a
  new connection id, set `Ready`, stamp `last_seen_at_ms`; the key is not
  consulted.
- **Unknown daemon + revoked/expired/exhausted key:** terminal rejection
  before any durable environment state.
- **Known daemon after non-cascading key revocation:** allow reconnect based
  on daemon-key proof.
- **Known daemon after environment close:** terminal rejection. The identity
  is spent; a local identity reset plus an active registration key registers
  a new environment.
- **Persistent disconnect:** `Offline` until explicit close.
- **Ephemeral disconnect:** `Offline`; the lifecycle reconciler closes the
  environment once `last_seen_at_ms` is older than the key's grace and no
  control connection is current. A reconnect inside the grace refreshes the
  stamp; there is no timer to cancel.
- **Gateway restart:** all control connections drop and every registered
  environment is `Offline` until its daemon reconnects with backoff. Ephemeral
  environments keep their grace window from the last stamp. Durable identity
  binding prevents duplicate environments.
- **Duplicate live daemon identity:** one control connection becomes current;
  the old connection and its data sockets are fenced and closed.
- **Key limit race:** admissions serialize on the key row; losers receive a
  typed, retryable capacity refusal and leave no rows.
- **Data dial with an unknown or expired token:** rejected; the waiting worker
  route fails as unavailable.
- **Metadata collision:** display names need not be unique; metadata cannot
  affect identity or grouping. Server ids remain authoritative.

Closing a registered environment closes its control connection and data
sockets through the gateway, spends its daemon identity, and disables session
selection. It does not terminate the underlying VM, Pod, or host; those remain
owned by the external orchestrator.

## Security Boundary

A reusable registration key delegates the ability to add execution capacity to
one universe. A leaked unlimited key can create many attacker-controlled
environments that may later receive commands or injected credentials if a
profile or user selects them. Treat key creation and distribution like
cluster-join credentials, not like a harmless correlation value.

Required controls:

- TLS server authentication for every non-loopback gateway URL, with an
  optional operator-supplied trust anchor file;
- cryptographically random secrets, stored only as hashes and returned once;
- explicit universe binding, expiry, revocation, capacity, and rate limits;
- atomic limit enforcement under concurrent registrations;
- proof of daemon private-key possession before binding or reconnecting;
- server-assigned environment/incarnation ids and strict current-incarnation
  fencing;
- one-time data tokens bound to one route request and one control connection;
- no secret in URLs, metadata, receipts, traces, errors, command lines, child
  process environments, or persisted daemon state;
- registration configuration removed from the daemon's environment before any
  child starts, with a test, and the documented `/proc` limitation of the
  direct form;
- bounded pre-authentication work, frames, metadata, output, and connection
  counts;
- audit of key creation, successful new registrations, refusals, revocation,
  and cascade close without storing the secret;
- explicit cleanup of ephemeral environments and operator-visible stale
  persistent environments; and
- a test proving that an identity baked into two images cannot create two
  environments or serve two current connections.

Registration by itself grants no general Lightspeed API access and reveals no
universe resources beyond the assigned environment receipt and bounded
gateway protocol errors. The registration key is not accepted on the normal
JSON-RPC API, model-provider endpoints, provider controller endpoints, or
worker routes.

## Implementation Slices

### 1. Domain, store, and management API

- Add the registration-key record, secret mint/hash/redaction, create/read/
  list/revoke methods, policy validation, and row-locked capacity admission.
- Add the `Registered` environment source, the two environment columns, the
  amended check constraints, the unique daemon-key index, and memory/
  PostgreSQL parity.
- Add `registrationKeys` to the environments grant, the `registrationKeyId`
  list filter, and the registered fields on read/list rows.
- Keep registration-key methods out of Configurator MCP and generated
  model-facing catalogs while retaining normal generated Rust/TypeScript
  management clients.
- Regenerate the public API contract and TypeScript consumers.

### 2. Control channel and daemon connector

- Add the public WSS connect route, nonce challenge, Ed25519 proof,
  create-or-reconnect transaction, in-memory control-connection registry, and
  per-key rate limits.
- Add the `openData` request, the data route, the token pairing map, and the
  reuse of the existing external proxy with the extra control-connection
  fence.
- Give `lightspeed-envd` a dial-out sibling of its accept loop: daemon key
  file, registration handshake, receipt output, heartbeat, reconnect backoff,
  terminal-versus-retryable rejection handling, data-route dialing, and the
  trust-anchor file option.
- Scrub registration configuration from the daemon's own environment before
  any child or job starts, and from all diagnostics.

### 3. Lifecycle and administration

- Project connect/disconnect into `Ready`/`Offline` with `last_seen_at_ms`
  stamps, and repair stale `Ready` rows in the reconciler.
- Add ephemeral grace close in the lifecycle reconciler and cascading
  registration-key revoke.
- Add Platform and CLI key creation, listing, rotation/revocation, copy-once
  secret display, and mode/limit configuration.
- Group registered environments by key on the Environments page and in the
  profile picker, add the `group` field and filter to the model list tool,
  and add the ephemeral `existing` warning to profile validation.

### 4. Deployment and live acceptance

- Publish the public connect and data routes through the hosted ingress with
  TLS and bounded metrics, and pin one environment-gateway replica in the
  hosted deployment configuration.
- Document VM, plain Docker, Kubernetes Deployment/Job, and StatefulSet + PVC
  bootstrap examples using the same daemon image, including the private-CA
  case.
- Prove hosted Lightspeed with externally started compute, restart/reconnect,
  concurrent shared-key registration, capacity refusal, cleanup, and cascade
  revocation.
- Split the environment gateway into its own role so the JSON-RPC gateway
  scales independently of the single connection-owner replica. Done.
- Implement the owner lease, direct worker dialing, signed data token, and
  relay hop before running more than one environment-gateway replica in
  production. Deferred until a deployment needs it.

## Tests

### Domain and API

- Create returns one redacted secret; read/list never expose secret/hash;
  revoked, expired, malformed, and capacity-exhausted keys fail with typed
  outcomes.
- One shared key concurrently creates distinct server-assigned environments
  for distinct daemon public keys.
- The active limit is atomic under concurrency and reconnects do not consume
  it; refused registrations leave no rows.
- A grant naming only providers hides registered environments; a grant naming
  registration keys shows exactly those; the list filter by key id matches.
- Registration-key methods cannot appear in Configurator MCP or session tools.
- Non-cascading and cascading revocation have the documented descendant
  behavior.

### Identity and lifecycle

- Persistent identity reconnects after daemon and gateway restart with the
  same environment/incarnation and a new connection id.
- Ephemeral replacement with a new daemon key creates a new environment; the
  reconciler closes the old one after grace; a reconnect inside the grace
  keeps it.
- A closed environment's daemon identity receives a terminal rejection even
  when the original registration key remains active, and `envd` exits
  instead of retrying.
- A copied daemon identity yields one current control connection, fences the
  other's data sockets, and never two environments.
- A registration key cannot move a known daemon identity between universes.
- A stale `Ready` row with an old stamp is treated as unavailable and repaired.

### Protocol and security

- The complete existing envd filesystem/process/PTY/job conformance suite
  passes over reverse-dialed data sockets, with several concurrent routes to
  one daemon.
- Invalid signatures, oversized metadata/frames, handshake timeout, replayed
  nonces, and authentication floods fail before environment creation.
- Data tokens are single-use, expire, and are bound to the control connection
  that received them.
- Environment, incarnation, daemon, and connection fencing rejects stale
  sockets and cross-environment requests.
- Registration secrets do not appear in `Debug`, logs, close frames, receipts,
  persisted daemon state, or child process/job environments.
- Metadata is bounded, reserved names are rejected, and correlation values
  never influence identity, routing, or grouping.

### Live

- Multiple local containers using one key register distinct environments and
  execute commands through hosted Lightspeed.
- A disposable Kubernetes-style instance is closed after disconnect grace; a
  StatefulSet-style instance reusing the same daemon state reconnects the same
  environment.
- Gateway restart and transient network loss recover without duplicate
  environments or duplicate side effects.
- Many concurrent registrations respect per-key limits and leave no partial
  environment rows after refusals.

## Non-Goals

- One-time enrollment tickets, short-lived per-environment bootstrap tokens,
  or pre-creating a pending environment before `envd` connects.
- Letting `envd` choose its universe, `environmentId`, `incarnationId`,
  identity mode, or an existing environment to claim.
- Multiplexing environment-protocol frames over the control connection.
- Using client-supplied hostname, Pod UID, VM id, or Harbor identifiers as
  authentication, durable identity, or grouping.
- A pool-claim profile intent or selection by name within a key; both are
  later items.
- Outbound HTTP proxy support in `envd`.
- Replacing passive external environments or provider-mediated Incus routes.
- Defining the Harbor adapter, benchmark suite, task correlation workflow, or
  result publication; those belong to
  [P149](p149-harbor-end-to-end-agent-evaluation.md).
- Requiring operator-managed X.509 certificates or a deployment PKI. A later
  transport may encode the same daemon-key proof in mTLS without changing the
  registration-key or identity model.
- Kubernetes ServiceAccount, cloud workload-identity, SPIFFE, or provider-
  native attestation. They may become alternate registration authorities
  later; reusable keys are the only authority in this item.
- Provisioning or deleting the underlying VM, container, Pod, or host.
- Provider power control, public application ingress, scheduling, billing, or
  capacity placement for registered environments.
- Making the registration key a general universe API credential or a
  permanent data-plane bearer credential.
