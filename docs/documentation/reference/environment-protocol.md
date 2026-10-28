# Environment protocol

This page states the environment protocol's rules: what every peer sends,
accepts and answers. It is written for anyone who implements a provider
controller, a relay in front of the daemon, or a client. The
[provider guide](../integrating-and-extending/environment-providers.md)
explains how to build a provider; this page is the contract that guide
refers to.

The wire types live in the
[environment protocol crate](../../../crates/environment-protocol/src/lib.rs).
Its doc comments use wire names and state the per-field rules. Where this page
and the crate differ, the crate is authoritative.

## Planes and versions

The protocol has three planes. Each runs over its own WebSocket connection.

| Plane | Client (dials) | Server | Version number | Purpose |
| --- | --- | --- | --- | --- |
| Data | The Lightspeed runtime | `lightspeed-envd` in the environment | `ENVD_PROTOCOL_VERSION` | Filesystem, transfer, process, and job operations in one environment. |
| Controller | The Lightspeed runtime | A provider controller | `CONTROLLER_PROTOCOL_VERSION` | Create, observe, power, and close the provider's targets. |
| Registration | `lightspeed-envd` | The Lightspeed environment gateway | `ENVD_PROTOCOL_VERSION` | Admit an outbound daemon and ask it for reverse-dialed data streams. |

Both numbers are currently `4`. They are matched exactly. A peer that states
any other number is refused with `versionMismatch`. There are no version
ranges and no negotiation. A change to a plane's wire shape, or to the meaning
of an existing field or method, needs a new number for that plane. The data
plane and the registration channel share one number, so a data-plane change
also changes the number in the gateway's challenge, which is what prompts a
registered daemon to upgrade.

`release/metadata.env` records the same numbers as
`LIGHTSPEED_ENVD_PROTOCOL_VERSION` and `LIGHTSPEED_CONTROLLER_PROTOCOL_VERSION`.
`scripts/release/verify-metadata.sh` checks them against the crate.

The controller plane does not depend on data-plane types. A provider that
changes only its controller does not force a new daemon, and a daemon change
does not force a provider redeploy unless the provider bakes the daemon into
its images.

## Contract documents

The [method manifest](../../../crates/environment-protocol/src/manifest.rs)
lists every method with its plane, direction, family, params and response
types, and the facts servers act on: the capability a method needs, whether
it counts as use of the environment, and whether `$/cancelRequest` aborts it.
A method exists exactly when it has a manifest entry.

Generated from the manifest and committed under
[`crates/environment-protocol/contract/`](../../../crates/environment-protocol/contract/):

| File | Contents |
| --- | --- |
| `data.openrpc.json`, `controller.openrpc.json`, `registration.openrpc.json` | One OpenRPC document per plane: every method, its params, result, and summary. |
| `data.schema.json`, `controller.schema.json`, `registration.schema.json` | JSON Schema bundles of every type the plane uses. |

The controller documents also name two types that no method carries as a
whole: `x-lightspeed-data-route`, the data route refusal body with its hold
and `Retry-After` bounds, and `x-lightspeed-idle-policy`, the idle policy
providers run.

Each method also has a fixture under
[`fixtures/<plane>/`](../../../crates/environment-protocol/fixtures/), named
after the method with `.` for `/`, such as `fixtures/data/process.read.json`.
It holds a valid params object, a valid response, and a typed error. The crate's
tests decode each fixture strictly and check that the committed contract
matches the current types.

## Envelope

Every plane uses one JSON-RPC 2.0 envelope:

- Each WebSocket **text** frame holds exactly one JSON-RPC message. Binary
  frames are not part of any plane: a server answers one with
  `invalidRequest` and a null id, and a client closes the connection.
- `jsonrpc` must be `"2.0"`. An unknown member of the envelope is
  `invalidRequest`.
- Params are passed by name, as an object. Absent params mean `{}`. Params
  that are not an object are `invalidParams`.
- A request has an integer or string `id`. A notification has no `id` and is
  never answered, not even with an error. A malformed frame that names a
  string `method` and has no `id` is treated as a malformed notification:
  it is dropped without an answer.
- A batch (a JSON array) is answered with one `invalidRequest` error and a
  null id.
- A frame that is not JSON is answered with `parseError` and a null id.
- The connection stays open after every framing error.

On the data and controller planes, the server never sends a request or a
notification. A client that receives one treats it as a protocol violation
and closes the connection. The registration channel carries requests and
notifications in both directions; see [Registration channel](#registration-channel).

## Handshake

The data and controller planes each begin with `initialize`. The server
enforces these rules per connection:

1. Before a successful `initialize`, any other request is answered with
   `notInitialized`, and notifications are dropped.
2. An `initialize` whose `protocolVersion` differs from the server's number is
   answered with `versionMismatch`. Its `expected` field is the server's
   number and `received` is the number the client stated. The connection
   stays open and uninitialized; the client closes it.
3. A second `initialize` after a successful one is `invalidRequest`.
4. The response names its `plane`. A client checks `plane` and
   `protocolVersion` before it uses the connection, so a client that reached
   the wrong socket fails at the handshake.

`clientInfo` and `serverInfo` describe the peer's build: a `name` and
optional `version`, `gitSha`, and `target`, each 1 to 256 bytes without
control characters. They are descriptive only. Nothing selects, gates, or
authenticates on them.

## Decoding

Every params, response, and message type on the three planes decodes
strictly: an unknown member is refused, and defaults exist only for
documented optional knobs. An omitted `protocolVersion` is never assumed.

The frozen bootstrap surface is the exception. It is the gateway's first
frame (the `registration/challenge` notification), the deployment's daemon
discovery document, and the JSON that `lightspeed-envd --print-build`
prints. These only gain fields, and they are decoded tolerantly, so a daemon
of any version can read them.

## Encoding conventions

| Topic | Rule |
| --- | --- |
| Methods | `domain/verb[Object]` in lowerCamel, singular domains, no plane prefix. |
| Fields | camelCase. Lists are plural nouns; counts end in `Count`, sizes in `Bytes`, durations in `Ms`, instants in `AtMs`, ids in `Id`. Payload bytes are always `data`. |
| Optionality | No `null` except JSON-RPC's own `id`. Absent options and empty collections are omitted. Flags are booleans that default to false and are omitted when false. |
| Numbers | Byte counts, offsets, and durations are unsigned 64-bit; counts, indices, and depths unsigned 32-bit; instants are signed 64-bit milliseconds since the Unix epoch. Every value stays below 2^53. |
| Unions | Data unions carry a `kind` tag. |
| Ids | Lightspeed-assigned ids follow the general grammar `^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$`. Provider-assigned ids (`targetId`, `templateId`) are opaque: 1 to 255 bytes without control characters. |
| Bytes | Payloads are padded standard base64. Fixed-size cryptographic values are lowercase hex. Content digests are `sha256:<64 hex>`. Opaque fingerprints carry their own prefix, such as `scan:v1:`. |
| Paths | `EnvironmentPath`: absolute, normalized, at most 4096 bytes; `.` and `..` segments are refused, not rewritten. Tree entries use relative `TreePath` values; the selected root itself is `.`. |
| Text | Descriptive text such as a target `message` is 1 to 1024 bytes, without control characters other than line feed and tab. URLs are at most 2048 bytes. |

## Errors

An error is a JSON-RPC error object. Its `code` is fixed by its kind, and its
`data` carries the kind and its typed fields:

```json
{"jsonrpc": "2.0", "id": 7, "error": {"code": -32004, "message": "unknown process id: p1", "data": {"kind": "notFound", "reason": "expired"}}}
```

`data.kind` is authoritative. Receivers branch on it, never on the code or
the message. A message is at most 4096 bytes and is never parsed.
Retryability is a property of the kind and is never sent. The same error
object appears inside results that report per-entry failures, such as
`job/read` entries, `fs/scan` roots, and a failed transfer's status.

| Kind | Code | Retryable | Typed fields | Use |
| --- | --- | --- | --- | --- |
| `parseError` | -32700 | No | | The frame is not JSON. |
| `invalidRequest` | -32600 | No | | A malformed envelope, a batch, a binary frame, a repeated `initialize`. |
| `methodNotFound` | -32601 | No | | No such method on this plane. |
| `invalidParams` | -32602 | No | `field`, `ceiling` | Params do not decode, a value is out of range, or a value exceeds an advertised ceiling. `field` is the wire path of the offending member, such as `jobs[0].dependsOn`. |
| `internal` | -32603 | No | | A defect inside the server. |
| `notInitialized` | -32002 | No | | A request before `initialize`. |
| `permissionDenied` | -32003 | No | `reason` | Confinement, operating-system permissions, or registration credentials. |
| `notFound` | -32004 | No | `reason` | An unknown handle, path, target, or job. |
| `versionMismatch` | -32005 | No | `expected`, `received` | The stated protocol version differs. |
| `capabilityUnavailable` | -32006 | No | `capability` | The endpoint lacks a capability family or fact the request needs. |
| `failedPrecondition` | -32007 | No | `reason` (required) | The current state forbids the request. |
| `busy` | -32008 | Yes | `reason`, `retryAfterMs` | A concurrency, capacity, or rate limit. |
| `alreadyExists` | -32009 | No | | The thing to create already exists. |
| `payloadTooLarge` | -32013 | No | `limitBytes`, `limitEntries` | A message, payload, or result exceeds its limit. |
| `deadlineExceeded` | -32015 | No | | A server-side duration bound ran out. |
| `expired` | -32016 | No | | A credential or token is past its validity. |
| `aborted` | -32017 | Yes | `reason` | Cancelled, or the input changed underneath the operation. |
| `processFailed` | -32018 | No | | A process could not be started or supervised. |
| `unavailable` | -32019 | Yes | `retryAfterMs` | A dependency of the server is temporarily unreachable. |

Reasons by kind:

| Kind | Reasons |
| --- | --- |
| `permissionDenied` | `outsideRoot`, `daemonState`, `filePermission`, `invalidSignature`, `invalidRegistrationKey`, `registrationKeyRevoked` |
| `notFound` | `expired` (the handle was evicted), `daemonRestarted` (the handle belongs to an earlier daemon instance), `unknownDaemon` |
| `capabilityUnavailable` | `filesystem`, `filesystemWrite`, `process`, `processPty`, `job`, `power`, `ingress` |
| `failedPrecondition` | `inputClosed`, `ifMatchFailed`, `createCondition`, `notEmpty`, `isDirectory`, `notDirectory`, `notRegularFile`, `symlinkLoop`, `symlinkRejected`, `filesystemRoot`, `crossDevice`, `noSpace`, `jobIdInUse`, `operationIdInUse`, `transferPhase`, `transferOffset`, `digestMismatch`, `environmentClosed`, `identityInUse`, `ownerMismatch`, `templateDeprecated`, `ingressUnsupported`, `targetNotActive` |
| `busy` | `requestLimit`, `transferCapacity`, `rateLimited`, `capacityExhausted` |
| `aborted` | `cancelled`, `sourceChanged` |

An endpoint that cannot honour a parameter refuses the request with
`capabilityUnavailable`. It never ignores the parameter.

## Liveness

A peer that is waiting for a response sends a WebSocket ping at least every
20 seconds while any request it sent is unanswered. It may also ping while
idle. The daemon, providers, and the gateway answer pings and never originate
them on data or controller connections. A server answers pings even while it
is busy writing responses.

Intermediaries, such as a gateway route, a provider relay, or an edge proxy,
forward ping and pong frames, count every frame as activity, and never close
a connection for idleness sooner than 60 seconds. Request deadlines, not
pings, detect a server that has stopped answering.

The registration channel has its own heartbeat; see
[Registration channel](#registration-channel).

## Limits

The protocol has three kinds of limit:

1. **Protocol constants.** Every message, and therefore every frame, is at
   most 16 MiB (16,777,216 bytes) on every hop. Each reader and writer
   configures that limit explicitly, with the frame limit equal to the
   message limit. A sender whose request would exceed it fails locally
   instead of sending. A server whose response would exceed it answers
   `payloadTooLarge` for the same id instead.
2. **Endpoint ceilings.** The daemon advertises its operational ceilings in
   the data-plane `initialize` response as `limits`. A request above a
   ceiling is `invalidParams` naming the field and the ceiling. Nothing is
   clamped.
3. **Caller quotas.** Each request states its own bounds as required fields,
   such as `maxBytes`, `waitMs`, or a `limits` object.

Response sizes are bounded by construction. A `process/read` page or one
`job/read` response holds at most 4096 chunks plus gaps. Search and glob
matches, and the entries, skips, and content of all roots of one scan, stop
at a 12 MiB result budget. A scan root or transfer lists at most 100 skipped
entries and counts the rest. A leftover-process report lists at most 64
processes, each command cut to 1024 bytes.

## Pipelining and cancellation

Clients may pipeline requests. Responses arrive in any order and are matched
by id. Servers accept at least 16 requests in flight per connection; the
daemon advertises its own number as `limits.maxConcurrentRequests`. Beyond
it, a server answers `busy` with reason `requestLimit` at once.

On the data plane, the client notification `$/cancelRequest {id}` aborts a
pending request. The manifest marks which methods it stops: `process/read`,
`job/read`, `fs/searchText`, `fs/globFiles`, and `fs/scan`. They stop
waiting or walking and answer `aborted` with reason `cancelled`; an aborted
`process/read` never advances the output cursor. Every other method ignores
the notification and runs to a consistent state. An unknown or finished id is
ignored. The controller plane has no cancellation.

When a data connection closes, the daemon cancels that connection's pending
requests as if each had been cancelled, and drops their replies.

## Controller plane

A provider controller manages targets: machines, containers, or other units
of compute. The runtime is the client. Calls go one way only: the runtime
calls the provider, and the provider never calls Lightspeed.

The provider owns everything that happens to a running target: its power,
its idle policy, its ingress, its placement, and any pools or snapshots
behind it. Lightspeed decides only two things, that an environment is
created and that it is closed, and it retries both until the provider
accepts them. Lightspeed records what the provider reports for display;
it stores no power or ingress intent and never reverts a change the
provider made.

### Transport and credential

The controller is a WebSocket endpoint at the URL the operator registers in
Lightspeed: `ws://`, `wss://`, or `unix:///absolute/path`. Any path is
allowed.

The credential is optional. When the provider is registered with one, every
connection to the controller and to its data routes carries it as
`Authorization: Bearer <credential>`; the provider compares it in constant
time and answers HTTP 401 before the upgrade when it is missing or wrong.
When the provider is registered without one, the runtime sends no
`Authorization` header. Run a provider without a credential only where
nothing but Lightspeed can reach its listener.

### `initialize`

The params carry `protocolVersion` and `clientInfo`. The response carries:

| Field | Meaning |
| --- | --- |
| `protocolVersion` | The controller's number. |
| `plane` | Always `controller`. |
| `serverInfo` | The controller's build. |
| `capabilities` | Optional methods: `power` serves `target/setPower`, and `ingress` serves `ingress/ensure` and `ingress/remove`. `template/list`, `target/create`, `target/get`, and `target/close` are mandatory. |

```json
{
  "protocolVersion": 4,
  "plane": "controller",
  "serverInfo": {"name": "lightspeed-provider-incus", "version": "0.5.0"},
  "capabilities": {"power": true, "ingress": true}
}
```

The runtime reads `capabilities` from the live handshake of each
connection and never stores it. It refuses an API request for a power or
ingress change up front when the provider does not advertise the method.
A controller that receives an optional method it does not serve answers
`capabilityUnavailable`.

### Methods

| Method | Params | Semantics | Errors |
| --- | --- | --- | --- |
| `template/list` | `universeId` | The templates the universe may create targets from. A provider may offer each universe a different set. | `unavailable` |
| `target/create` | `owner`, `templateId`, `displayName?`, `idle?` | Idempotent per owner. If the provider already holds a target for the owner, answer its observation and apply no creation policy (deprecation or image checks). Otherwise apply policy, start creation, and answer promptly with `phase: provisioning`. Handing out a target from a warm pool counts as a create. `idle` replaces the template's default idle policy for this target. | `failedPrecondition` (`templateDeprecated`, `ownerMismatch`), `notFound` (template), `invalidParams` (including an `idle` that does not validate), `unavailable` |
| `target/get` | `target` | A pure read: it never creates, starts, or changes anything. Resolves by `targetId` when given, otherwise by owner. | `notFound`, `failedPrecondition` (`ownerMismatch`), `unavailable` |
| `target/setPower` | `target`, `power` | Needs `power`. Move the target to one of its template's `powerStates`. A target already there, or moving there, is observed without action. Answer promptly with the transitional observation. The provider stays the owner of power afterwards. | `capabilityUnavailable`, `notFound`, `invalidParams` (a state outside `powerStates`), `failedPrecondition` (`targetNotActive`), `unavailable` |
| `target/close` | `target` | Destroy everything the owner's target holds, public ingress included: gracefully within the provider's own bound, then forced. Answer `closing` while it runs, `closed` once nothing remains, and `closed` without `targetId` when the provider holds nothing for the owner. Idempotent. | `unavailable` |
| `ingress/ensure` | `target` | Needs `ingress`. Route the template's public ingress to the target. Idempotent. | `capabilityUnavailable`, `notFound`, `failedPrecondition` (`ingressUnsupported`) |
| `ingress/remove` | `target` | Needs `ingress`. Stop routing public ingress to a target that stays. Idempotent. | `capabilityUnavailable`, `notFound` |

Every target and ingress method answers `{"target": <observation>}`.

### Ownership

A target belongs to exactly one owner, the Lightspeed environment it serves:

```json
{"universeId": "u1", "environmentId": "env_1"}
```

A provider holds at most one target per owner. Every target method names the
owner in a target reference, `{owner, targetId?}`. Without `targetId`, the
provider resolves the owner's target itself; this is how the runtime finds
and closes a target whose id it never learned. A target that belongs to
another owner is refused with `failedPrecondition` (`ownerMismatch`).
Knowing a provider's target id is never enough to reach a target. An
environment id is never reused, so a closed environment's owner cannot
reach a later environment's target.

Record ownership durably enough to survive the provider's own restarts and
an ambiguous create, where the target was created but the response was lost.
The Incus provider uses deterministic names and ownership metadata on its
native resources; another provider may use its own store.

### Bounded calls

Every method answers within a bounded time, a few seconds. Slow work, such as
an image pull, a boot, or a graceful shutdown, continues in the background,
and the runtime follows it with `target/get`. An asynchronous provider may
answer `target/create` before it has assigned an id; the observation then
omits `targetId`, and the runtime records the id from the first observation
that carries one.

### Observation

```json
{
  "targetId": "ls-3f2a",
  "owner": {"universeId": "u1", "environmentId": "env_1"},
  "phase": "active",
  "power": "paused",
  "ingress": {"endpoint": "https://acorn.env.example.com"}
}
```

| Field | Meaning |
| --- | --- |
| `targetId` | The provider's id. Absent only while provisioning before an id exists, and in a `closed` observation of an owner with nothing. |
| `owner` | Always echoed from the request. |
| `phase` | `provisioning`, `active`, `closing`, `closed`, `failed` (broken beyond the provider's own recovery), or `unknown` (the provider cannot tell). |
| `power` | Present exactly when `phase` is `active`: `running`, `paused` (execution halted, memory retained), `suspended` (execution state persisted, compute released), or `stopped` (powered off, disks retained). |
| `message` | Why the target is `failed` or `unknown`, or what a transition waits for. |
| `ingress` | Realized public ingress, an `http` or `https` URL. Absent when disabled. |

Report what the infrastructure shows, not what a caller asked for. An
observation says nothing about whether the daemon answers: the runtime
proves that itself by dialing the data route.

### Templates

A template has a `templateId`, a non-empty `displayName`, and these
optional fields:

| Field | Meaning |
| --- | --- |
| `description` | A short description for people choosing a template. |
| `powerStates` | The power states `target/setPower` can move this template's targets to. Absent or empty when the provider offers no power control beyond create and close. |
| `publicIngress` | Targets of this template can serve public ingress. |
| `idle` | The idle policy a target gets when `target/create` names none. Absent when the provider applies none by default. |
| `deprecated` | The template creates no new targets; existing targets keep working. |
| `resources` | `vcpus`, `memoryBytes`, and `diskBytes` in provider-neutral units, each omitted when the provider does not fix it. |
| `metadata` | Provider display facts, such as an image fingerprint. |

### Idle policy

An idle policy names when a provider powers down or closes an idle target:

```json
{"pauseAfterMs": 600000, "stopAfterMs": 3600000, "closeAfterMs": 86400000}
```

`pauseAfterMs`, `suspendAfterMs`, `stopAfterMs`, and `closeAfterMs` are each
optional, but a policy sets at least one. Every set threshold is positive,
and the set thresholds are non-decreasing in the order pause, suspend, stop,
close. A policy that breaks these rules is `invalidParams`.

The policy belongs to the provider. A template names a default, and
`target/create` may replace it for one target; the runtime validates a
policy and passes it through without interpreting it. The provider runs the
clock. For a running target it reads the daemon's `env/getIdle` over its own
channel to the guest and applies the most escalated stage whose threshold
`idleForMs` has reached and whose power state the template supports; `close`
always qualifies. The protocol crate's
[`IdlePolicy::due_action`](../../../crates/environment-protocol/src/idle.rs)
implements that rule. Running processes, running jobs, and leftover process
groups hold `idleForMs` at zero, so they keep the target awake.

### Data routes

A provider serves the daemon's data plane for each target on a data route:
the provider's data route base followed by
`/{universeId}/{environmentId}/{targetId}`, each segment percent-encoded
(everything except RFC 3986 unreserved characters). The runtime dials it
with the bearer credential when one is configured. The provider serves the
data plane unchanged on that route, under the liveness rule and the message
limit above, and refuses a route whose owner does not own the target.

The data route base is a `ws://` or `wss://` URL with a lowercase scheme, a
host and optional port, an optional path that does not end with `/`, and no
user information, query, or fragment. The operator may set it when
registering the provider. Without one, the runtime derives it from the
controller URL: the controller's scheme, host, and port followed by
`/routes`, so `wss://provider.example.com/control` gives
`wss://provider.example.com/routes`. A `unix://` controller has no derivable
base, so its registration must name one. A configured base for a controller
reached over `wss` (or `https`) must itself use `wss`, so the credential
never travels in cleartext; registration refuses a downgrade.

A dial of a powered-down target's data route wakes the target. This is the
only wake signal: Lightspeed has no separate wake call. The provider may
hold the dial for at most 5 seconds (`MAX_DATA_ROUTE_HOLD_MS`) while it
resumes the target. If the daemon answers in time, the route upgrades and
relays as usual. Otherwise the provider refuses the upgrade with a JSON
body, `Content-Type: application/json`, tagged by `state`:

| Status | Body | Meaning |
| --- | --- | --- |
| 503 | `{"state": "waking"}` | The target is resuming from a powered-down state. |
| 503 | `{"state": "booting"}` | The target runs, but the daemon does not answer yet. |
| 410 | `{"state": "gone"}` | The target no longer exists. |
| 409 | `{"state": "failed", "message": "..."}` | The target is broken beyond the provider's recovery. |

Every 503 carries `Retry-After` in whole seconds, from 1 to 60
(`RETRY_AFTER_MAX_SECS`); receivers clamp a larger value. A 503 may succeed
on a later dial; 410 and 409 do not. The body is an HTTP refusal, not a
JSON-RPC message. The controller schema bundle names it as
`x-lightspeed-data-route`, together with both bounds.

The environment gateway's worker route uses the same body. It adds three
states of its own and forwards a provider's refusal unchanged, status and
`Retry-After` included:

| Status | Body | Meaning |
| --- | --- | --- |
| 503 | `{"state": "provisioning"}` | The environment is still provisioning. |
| 503 | `{"state": "disconnected"}` | The registered daemon is not connected. |
| 410 | `{"state": "away"}` | The registered daemon has been away longer than it is waited for. |

### Provider obligations

The provider owns its targets' power, idle policy, and ingress. Every
provider:

1. makes `target/create` and `target/close` idempotent per owner;
2. wakes a powered-down target when its data route is dialed, holding the
   upgrade at most 5 seconds before it answers a typed refusal;
3. decides idleness from the daemon's `env/getIdle`, so running processes,
   jobs, and leftover process groups keep the target awake;
4. never powers a target down while it relays a data connection to it;
5. reports `closed`, or `notFound` from `target/get`, for a target it
   removed on its own, such as by a lifetime limit or an idle
   `closeAfterMs`;
6. never snapshots or clones a daemon state directory that holds a daemon
   identity.

### How the runtime drives a controller

Providers can rely on this behaviour:

- Until it has recorded a target id, the runtime sends `target/create`, with
  the same `displayName` and `idle` on every attempt. Afterwards it observes
  with `target/get` and never sends create again.
- `phase: provisioning` is checked again after 2 seconds.
- `phase: active` leads the runtime to run the data-plane handshake on the
  target's route. This handshake is the only readiness proof: success opens
  the environment and records the daemon's build. A daemon that speaks
  another protocol version fails the environment, naming both versions. A
  503 refusal is retried after its `Retry-After`; `gone` and `failed`
  refusals fail the environment.
- `phase: failed` fails the environment with its `message`. `closing` or
  `closed` before the environment opened fails it too. `phase: unknown`
  is retried with backoff.
- Retryable kinds, `internal`, transport failures, timeouts, and failed
  handshakes are retried with backoff: 2 seconds, doubling per attempt, at
  most 5 minutes. Other refusals, `notFound` included, fail a provisioning
  environment at once, so reserve them for requests that can never
  succeed. An environment that has not opened within 30 minutes of entering
  provisioning fails with the last recorded problem.
- Every 5 minutes, the runtime reads each open environment's target with
  `target/get` and records the observation for display. `phase: failed`
  fails the environment. `closed`, or `notFound`, closes it with the reason
  `providerEnded`. This sweep never changes power.
- Close sends `target/close` by owner, with the target id when one is
  recorded, until the provider answers `closed` or `notFound`. `closing` is
  checked again after 2 seconds, and errors are retried with backoff. Close
  has no deadline, and the runtime never removes ingress before it.
- `target/setPower` and the ingress methods are sent only when a user asks
  for a change, synchronously, with a 30-second deadline, and only when the
  controller advertises them. No loop calls them, and nothing retries them.
- Connect and `initialize` each have a 10-second deadline, and the
  runtime's other controller calls a 120-second deadline. The runtime keeps
  at most eight calls in flight per provider, over one connection.

## Data plane

The daemon, `lightspeed-envd`, is the data server on every provider. The
data plane is specified for that daemon; a provider routes to it rather than
reimplementing it.

### `initialize`

The response carries `protocolVersion`, `plane: data`, `serverInfo`, and:

| Field | Meaning |
| --- | --- |
| `daemonInstanceId` | Random per daemon start. A different value than on an earlier connection means the daemon restarted and the earlier instance's process handles are gone. |
| `capabilities` | Capability families (below). |
| `limits` | The daemon's ceilings: `maxConcurrentRequests`, `maxWaitMs`, and per-family `process`, `job`, and `filesystem` limits. |
| `defaultCwd` | The daemon's working directory, the default `cwd` of commands. |
| `homeDirectory` | The home directory of the user commands run as, when it lies inside the filesystem root. |

A present capability family means all of its methods exist: `filesystem`
covers `fs/*` and `transfer/*`, including search, glob, scan, and transfers;
`process` covers `process/*`; `job` covers `job/*`. Inside a family are only
facts that vary per instance: `filesystem.write` is false on a daemon started
read-only, and `process.pty` says whether `process/start` accepts `pty`. A
request that needs an absent family or fact is `capabilityUnavailable` before
any side effect.

### Methods

| Method | Counts as use | Abortable | Needs | Purpose |
| --- | --- | --- | --- | --- |
| `initialize` | No | | | Handshake. |
| `env/getIdle` | No | | | `idleForMs` since the last use, 0 while any process root or job runs or any leftover process group has live members, and diagnostic counts. |
| `$/cancelRequest` | No | | | Notification: abort a pending abortable request. |
| `fs/readFile` | Yes | | `filesystem` | Read at most `maxBytes` of a regular file from `offset`. |
| `fs/writeFile` | Yes | | `filesystem.write` | Write a file atomically, with `create`, `ifMatch`, and `executable`; answers the new `version`. |
| `fs/createDirectory` | Yes | | `filesystem.write` | Create a directory, optionally with its parents. |
| `fs/getMetadata` | Yes | | `filesystem` | Describe one entry, optionally following a symlink. |
| `fs/readDirectory` | Yes | | `filesystem` | List one page of a directory, sorted by name, with an opaque cursor. |
| `fs/remove` | Yes | | `filesystem.write` | Remove a file, symlink, or directory without following links. |
| `fs/rename` | Yes | | `filesystem.write` | Rename atomically, without replacing an existing destination unless `overwrite` is set. |
| `fs/searchText` | Yes | Yes | `filesystem` | Search file contents below a root with a regular expression. |
| `fs/globFiles` | Yes | Yes | `filesystem` | List non-directory entries below a root that match patterns. |
| `fs/scan` | No | Yes | `filesystem` | Observe small trees below several roots, each reported on its own. |
| `transfer/begin`, `transfer/advance`, `transfer/getStatus`, `transfer/readInventory`, `transfer/append`, `transfer/listMissing`, `transfer/read`, `transfer/write`, `transfer/commit`, `transfer/abort` | Yes | | `filesystem` (writes also `filesystem.write`) | One capture, materialize, or file write as many bounded calls. |
| `process/start` | Yes | | `process` | Start a process under a caller-chosen `processId`. |
| `process/read` | Yes | Yes | `process` | Wait for and read a page of output and the process state. |
| `process/write` | Yes | | `process` | Queue bytes for a pipe or PTY input. |
| `process/terminate` | Yes | | `process` | Interrupt, terminate, or kill the process group. |
| `job/start` | Yes | | `job` | Start jobs, or replay an identical earlier start. |
| `job/read` | Yes | Yes | `job` | Read jobs' summaries and output pages. |
| `job/cancel` | Yes | | `job` | Cancel jobs and optionally their dependents. |

Methods that count as use reset the environment's idle clock. Handshakes,
idle reports, cancellations, and scans do not, so discovery and a
provider's idle policy can look at an environment without keeping it awake.
The idle clock also restarts when the last running process, job, or
leftover process group ends: a service that a command started and left
running keeps `idleForMs` at zero like running work.

### Paths and confinement

A wire path is absolute, in the environment's own filesystem namespace: the
one its processes see. The daemon's filesystem root is not a chroot. With the
root narrowed to `/workspace`, `/workspace/src` names that directory and
`/etc` lies outside the root.

One rule applies to every method and to every command's `cwd`:

- A path that is neither the root nor below it is `permissionDenied`
  (`outsideRoot`).
- The daemon resolves the remaining segments one at a time beneath the root.
  A symlink is followed only while its target stays inside the root; a link
  that leaves it is refused the same way.
- No method addresses the daemon's state directory. A path that names it or
  lies inside it, and a remove, rename, write, or capture whose subject
  contains it, is `permissionDenied` (`daemonState`). Walks prune it.

Names are UTF-8 and POSIX: `/` is the only separator. An entry whose name is
not UTF-8 is skipped and counted wherever a method enumerates entries.

### Pattern grammar and walks

`fs/searchText`, `fs/globFiles`, `fs/scan`, and transfer captures share one
filter: `include` and `exclude` patterns, `ignoreCase`, `includeHidden`,
`includeIgnored`, and `maxDepth`. Patterns match paths relative to the walk's
root, gitignore-style: `*` and `?` match within one segment, `**` spans
segments, `[...]` is a character class, and `{a,b}` an alternation. A pattern
without `/` matches the entry's name at any depth; a leading `/` anchors it
to the root. An empty `include` selects everything, and an `exclude` that
matches a directory prunes it. The root's direct children are depth 1.

By default, walks respect `.gitignore`, `.ignore`, and git excludes, and
skip names that start with `.`. `includeIgnored` and `includeHidden` opt out.
`.git`, `.hg`, `.svn`, `.jj`, and the daemon state directory are always
pruned. Entries a walk cannot read or represent are skipped and counted; they
never end the walk. A file larger than the remaining byte budget is skipped
and counted too.

### Process output

A process has one combined output stream: stdout, stderr, or PTY output
interleaved in arrival order. Positions are byte offsets into it.
`process/read` returns a page of `chunks`, the `next` offset after the page,
the `end` offset of everything produced so far, and `gaps` for ranges it
cannot return (`overflow` when the retention cap dropped them, `expired` when
they were released after delivery). A page cut at `maxBytes` loses nothing.

A read starts at `after`, at `end - tailBytes`, or at the daemon-owned
cursor when it names neither. A cursor read advances the cursor only after
the reply was written to the socket, so a lost connection never consumes
output. Reads at an explicit `after` or `tailBytes` never move the cursor,
so a second reader can follow without consuming. An optional `ack` advances
the cursor to an offset the caller has already processed, which gives a
caller that tracks positions exactly-once delivery. `returnOn` chooses when a
wait ends:
`anyOutput`, `window` (enough bytes for `maxBytes`), or `exit`. Every mode
returns when `waitMs` elapses.

`state` is `running` or `exited`. `exited` means the root process exited and
its output has settled. An exit is `{kind: code, code}` or
`{kind: signal, signal: "SIGSEGV", coreDumped}`, with signals named because
their numbers differ by platform. `stoppedBy` (`timeout`, `terminate`,
`kill`) is set when the daemon stopped a running root. A process that leaves
descendants running reports them as `leftovers` with disposition `kept`;
processes never sweep their group on a natural exit.

An unknown handle is `notFound`. An evicted handle is `notFound` with reason
`expired`, and a handle from before a daemon restart is `notFound` with
reason `daemonRestarted`.

### Jobs

A job's identity is its `jobId` together with the `requestId` of the
`job/start` that created it. A start that repeats an existing job's
`requestId` and spec replays it; any other reuse of the id is
`failedPrecondition` (`jobIdInUse`). Statuses are `queued`, `running`,
`succeeded`, `failed`, `timedOut`, `cancelled`, `dependencyFailed`, and
`interrupted`. Dependencies name a job id or a name within the same request.
Jobs that share a `queueKey` run one at a time, in start order. At job
completion, the daemon sweeps the job's process group and reports what it
stopped as `leftovers` with disposition `swept`.

`job/read` takes a list of entries, each reading one job from `after` or the
last `tailBytes`; a job may appear twice to read its head and tail in one
call. The request's `maxBytes` is a total budget across entries. An unknown
job is answered with a per-entry `notFound` error.

### Files and transfers

`fs/writeFile` stages the new content next to the final target and renames
it into place, so readers see the old or the new file and never a partial
one. An in-root symlink at the path is followed to its final target. An
existing target keeps its mode. `ifMatch` makes the write conditional on the
version a read returned.

A transfer moves a whole file or tree in bounded calls under a
caller-chosen `operationId`. Retrying a call with the same input is safe. A
capture reads a tree out of the environment; a materialize replaces a
destination tree atomically at commit; a file write replaces one file with
`fs/writeFile` semantics. Content moves by `sha256` digest, so content
already present is not sent again. The first failure moves the operation to
`failed`; later calls answer that error and nothing is committed. Finished
operations keep a receipt for the advertised retention period. The
[transfer module](../../../crates/environment-protocol/src/data/transfer.rs)
documents the phases.

## Registration channel

An outbound daemon dials
`wss://<gateway>[/prefix]/environment-gateway/connect` and keeps that control
connection open. The daemon is the JSON-RPC client and the gateway the
server; both directions carry requests and notifications.

```text
gateway -> daemon  notification registration/challenge {protocolVersion, nonce}   always first
daemon  -> gateway request      registration/register {protocolVersion, daemonPublicKey,
                                 signature, registrationKey?, displayName?, metadata?,
                                 implementation}
gateway -> daemon  response     {receipt, heartbeatIntervalMs} or an error
gateway -> daemon  request      registration/openStream {token, stream}            any number of times
daemon  -> gateway response     {} once the stream socket is upgraded, or unavailable
daemon  -> gateway notification registration/goodbye {reason: shutdown}            before a deliberate stop
gateway -> daemon  notification registration/closed {reason, message?}             superseded or environmentClosed
```

Rules:

- The challenge is frozen. A daemon reads its `protocolVersion` before
  decoding anything else, and a mismatch leads to an upgrade or a refusal,
  never to a decode loop.
- The daemon signs `"lightspeed-envd-registration/v2" ‖ nonce ‖ sha256(origin)`
  with its Ed25519 key, where `origin` is the canonical WebSocket origin of
  the gateway URL it dialed. The gateway accepts a signature over any of
  its configured registration origins: the canonical origin of its public
  base URL, plus any further registration URLs the operator lists, such as
  a cluster-internal service address. Every other origin is refused, so a
  gateway cannot relay another gateway's challenge.
- The registration key is sent only until the daemon's first admission.
  `displayName` and `metadata` are read only then.
- Before admission the gateway accepts only `registration/register`. Out of
  bounds build facts, display names, or metadata refuse admission with
  `invalidRequest`; other undecodable params are `invalidParams`.
- The receipt names `environmentId`, `daemonId`, `controlConnectionId`,
  `identityMode` (`persistent` or `ephemeral`), and `admission` (`created`
  or `reconnected`).
- `registration/goodbye` from a persistent identity marks its environment
  disconnected; the environment stays open and waits for the daemon to
  return. From an ephemeral identity, it closes the environment.
- For each `registration/openStream`, the daemon dials the stream URL derived
  from its configured connect URL, replacing the
  `/environment-gateway/connect` suffix with `/environment-gateway/data`, with
  the one-time token as a bearer header, and serves the data plane on that
  socket. The gateway never names a URL.
- The gateway pings every `heartbeatIntervalMs`. Either side treats three
  intervals without any frame as a dead connection.
- The newest connection for a daemon identity wins. The gateway sends
  `registration/closed` with reason `superseded` to the older one before
  closing it.
- When the environment closes while its daemon is connected, the gateway
  sends `registration/closed` with reason `environmentClosed`. A closed
  identity that registers again is refused with `failedPrecondition`
  (`environmentClosed`). Either way the identity is spent: the daemon exits
  or parks, as configured, and never reconnects.

The daemon's exit status says why it stopped: 0 after a deliberate shutdown,
1 for an unexpected failure, 2 for a local configuration or receipt-file
error, 3 for a terminal
registration refusal or an unsupported gateway protocol, 4 when its
environment was closed, and 5 when another process with the same identity
superseded it. A daemon configured to park on close instead stops
registering and listening, waits for `SIGTERM` or `SIGINT`, and then exits
with 0. Retryable refusals, disconnects, and heartbeat timeouts never
end the process.

## Conformance

Two suites check an implementation against this contract without
provisioning anything else:

- The controller suite,
  [`environment_client::conformance::controller`](../../../crates/environment-client/src/conformance/controller.rs)
  behind the client crate's `conformance` feature, drives any controller URL
  with one template, and with the credential when the provider uses one. It
  checks the handshake rules, the credential refusal, per-owner create
  idempotency, pure `target/get`, owner fencing, close by owner and its
  repetition, strict decoding of every observation, and
  `capabilityUnavailable` for methods the controller does not advertise.
  It creates and closes one target.
- The data-route helpers,
  [`environment_client::conformance::data_route`](../../../crates/environment-client/src/conformance/data_route.rs),
  check that a refused data route answers a typed body with the status it
  names, a 503 with `Retry-After`, within the hold bound.
- The data suite,
  [`tools::environment_protocol::conformance`](../../../crates/tools/src/environment_protocol/conformance.rs),
  drives a data endpoint and checks file writes and reads, confinement,
  walks, scans, process output and exits, and job identity. It works in a
  fresh directory and removes it afterwards.
