# P198 — Declared registered environments

**Status:** Implemented, 2026-11-11: runtime, store (schema revision 18),
CLI, Platform and documentation. See [Implementation](#implementation) for
what was built, where it differs from the proposal, and the test results.

## Goal

Let an operator declare a fixed machine as an environment with an id of their
choosing before its daemon has registered, so that profiles and credential
bindings can reference it, and so that the universe and the machines can be
deployed independently of each other.

The case this serves is a universe with one or a few long-lived machines, run
as StatefulSets or VMs, each starting `lightspeed-envd` with a registration
key. Today such a machine cannot be referenced in advance:

- A profile attaches an environment by its concrete `environmentId`.
- Every environment id is allocated by the server, at random, and for a
  registered daemon only at the moment it is admitted.

So a profile cannot name a machine that has not registered yet, and the id it
eventually gets differs in every deployment. This blocks declaring a universe
as a document ([declarative universe
configuration](p199-declarative-universe-configuration.md)).

Pools of interchangeable or autoscaled machines are a different problem and
are out of scope; see [Out of scope](#out-of-scope).

## Design

**An environment may be declared under a registration key with a chosen id and
the name its daemon will register under. The first daemon admitted by that key
under that name becomes that environment.**

The daemon protocol does not change. A daemon already sends an optional
display name when it registers (`LIGHTSPEED_ENVD_REGISTRATION_NAME`, commonly
the pod name). That name now also selects a declared environment.

```
environments/registered/put
  { "environmentId": "build-box",
    "registrationKeyId": "runners",
    "name": "lightspeed-runner-0" }
```

Self-registration without a declaration is unchanged: a daemon whose key and
name match no declaration is admitted as today, under a server-allocated id.

### Why the daemon does not choose its id

Profiles and credential bindings point at environment ids. If the gateway
honoured an id a daemon asked for, anyone holding a registration secret could
claim an id that sessions already use, and receive the agent's commands, its
files and the credentials injected for that environment.

The declaration is what makes a chosen id safe: an administrator states which
key and which name fill which id, and only that combination is given it. A
daemon that matches no declaration can never obtain an id that something
references.

### Declaring

`environments/registered/put` declares one environment. Its parameters:

| Field | Meaning |
| --- | --- |
| `environmentId` | The id profiles and bindings use. Chosen by the caller. |
| `registrationKeyId` | The key whose daemons may fill it. Must be a persistent key. |
| `name` | The registration name the daemon presents. Unique among the open environments of the key. |
| `displayName`, `metadata` | Optional, as on other environments. |

The call follows the other declarative writes. A missing environment is
created; declaring it again changes nothing; `displayName` and `metadata`
converge on the declaration. The key and the name are fixed once declared:
changing either is a conflict, as is an id already used by an environment of
another kind or by a closed one.

A declared environment exists at once, `open` and with no daemon. It reads as
`away`, like a registered environment whose daemon is disconnected, and the
view carries its declared `name` and no `daemonId` until one is bound.

An ephemeral key cannot have declared environments. An ephemeral environment
is closed when its daemon stays away, which is the opposite of a machine that
is expected.

### Admission

A register request from a daemon identity the deployment does not know is
admitted through its key, as today. One step is added before an environment is
created:

1. The key has an open declared environment with the daemon's name and no
   daemon bound: the daemon is bound to it. Its public key is recorded on that
   row and the environment keeps its declared id.
2. The declared environment of that name is already bound to another
   identity: refused. See [A lost identity](#a-lost-identity).
3. No declaration matches and the key admits only declared machines: refused
   with a retryable answer, so a daemon that starts before its declaration
   keeps trying.
4. Otherwise: admitted under a server-allocated id, as today.

A daemon identity the deployment already knows reconnects to its environment
by its key pair, as today. The name is not consulted again, so renaming a pod
does not move a bound machine.

Binding counts against the key's active-environment limit when the
declaration is made, not when the daemon arrives: a declared environment is an
open environment of the key.

### Only declared machines

A registration key gains one editable setting, `declaredOnly`, off by default.
When it is on, the key admits only daemons whose name matches one of its
declared environments. No environment appears in the universe that an
administrator did not declare.

It also closes a gap in ordering. Without it, a daemon that registers after
its key exists but before its environment is declared is admitted under a
random id and no longer matches its declaration. With it, that daemon is told
to retry and binds once the declaration exists.

### A lost identity

A daemon's identity is the key pair in its state directory. A machine that
loses that directory returns with a new identity and asks for an environment
that is bound to the old one.

It is refused. Letting it take over would let anyone with the registration
secret and the name displace a machine that profiles and bindings point at.
An operator releases the environment first:

```
environments/registered/release { "environmentId": "build-box" }
```

Release forgets the bound identity and ends its connection. The environment
keeps its id, its name, its bindings and every reference to it, and is `away`
until a daemon binds again. The released identity is free to register again,
including to the same environment by name.

A daemon drops its registration key after its first admission. So a released
daemon that is still running reconnects without one, is refused as an unknown
identity, and exits with status 3. Restarted with its key, it is an ordinary
candidate for the environment again. An operator who wants another machine
to take the place stops the old one first.

### Deployment order

| Order | Behaviour |
| --- | --- |
| Universe first, daemon later | The environment exists at once and is `away`. Profiles reference it and sessions start; a tool call waits until the daemon connects, as it does for any disconnected registered machine. |
| Daemon first, universe later | The daemon presents a key the runtime does not know, treats that as permanent and exits. The orchestrator restarts it until the key is declared. Unchanged. |
| Key declared, environment not yet | With `declaredOnly`, the daemon retries until its environment is declared. Without it, the daemon is admitted under a random id. |
| Daemon restarts | It reconnects to the same environment by its identity. Unchanged. |

### Chosen ids for the other kinds

`environments/create` and `environments/external/create` gain an optional
`environmentId`. When given, the environment is created under it; an id in
use is a conflict unless the request is the retry of the one that created it
(same `requestId`). Omitted, an id is allocated as today.

With this, a document names every environment the same way, and a profile in
it references `build-box` directly, identically in every deployment.

### Cleanup rules

- The offline close-after of a persistent key applies to a daemon that has
  been away. A declared environment that no daemon has ever bound, or whose
  daemon was released, is not closed by it: nothing has gone away.
- Closing a declared environment frees its name for a new declaration. Its id
  stays taken while the closed record exists.
- Revoking the key with `closeEnvironments` closes its declared environments
  too. Without it they remain, and no daemon can bind to them.

## Security summary

- A chosen environment id is obtained only through a declaration made by a
  caller who may configure environments.
- A declared environment is filled by the first daemon that presents the
  declared key's secret and the declared name. From then on only that
  daemon's key pair reconnects to it.
- There is no automatic takeover. A different identity needs an explicit
  release.
- A key set to `declaredOnly` admits nothing undeclared.

The remaining exposure is the one a shared join secret always has: while a
declared environment is unbound, any holder of the key's secret can fill it.
A key per StatefulSet, or per machine, and a tight active limit narrow that.
Pinning a daemon's public key in the declaration would close it and is not
planned.

## Storage

The `environments` table gains `registration_name`. For a registered row the
daemon public key becomes optional: a row has a name, a public key, or both.
Names are unique among the open rows of a key. Registration keys gain
`declared_only`.

## Clients

- CLI: `environment registered put ID --key KEY --name NAME` and
  `environment registered release ID`; `--id` on `environment create` and
  `environment register`; `--declared-only` on the registration key
  commands.
- Platform: the environment list and details show a declared environment that
  is waiting for its daemon, with its name; the registration key editor
  carries the new setting.
- Declarative universe document: an environment entry takes `source:
  { type: registered, registrationKeyId, name }`, the shape of the `source`
  object the API returns.

## Out of scope

- **Pools.** Attaching "any machine of this key" to a profile needs a
  selection policy and is not part of this.
- **Adopting a registered machine.** A daemon that already registered under a
  random id keeps that environment. To move it under a declared id, give it a
  fresh state directory.
- **Pinned daemon keys** in the declaration.
- **Retrying on an unknown key.** A daemon still exits when its key is not
  recognised.

## Implementation

Built as proposed, with these additions and deviations:

- **Refusals a daemon retries.** "Bound to another identity" and "not
  declared under a `declaredOnly` key" are answered `unavailable` with a
  15-second retry hint. No protocol change was needed: the daemon already
  retries that kind and keeps its registration key until it is admitted.
- **Release reaches the live connection.** The API call signals the
  environment's routes; the control connection re-reads its row, sees it is
  no longer bound to its daemon, and closes. The gateway's route sweep covers
  a release issued from a process without the environment-gateway role.
- **`release` reports `released`**, false when no daemon was bound, and is
  refused for an undeclared environment (closing is the operation there).
- **Use while waiting.** A declared environment with no daemon uses the same
  gate as a disconnected persistent machine: a tool call waits up to two
  minutes from the declaration or release, then fails as `away`.
- **Configurator MCP.** `environments/registered/put` and `release` are
  excluded from the Configurator's tool surface, like the registration key
  methods. `environments/registration-keys/put` was found exposed there and
  is now excluded too.
- **Platform REST shape.** `POST /universes/:id/environments/registered`
  declares and `POST /universes/:id/environments/:environmentId/release`
  releases; both require the operator role through the generated method
  roles.
- **Migration test.** The schema change is covered by constraint tests
  against the migrated schema and by the dev database, which was upgraded in
  place with existing registered rows. There is no test that rebuilds the
  previous table shape.

### Tests

| Suite | Coverage | Result |
| --- | --- | --- |
| `environments` unit | declaration convergence and conflicts, bind, release, capacity, the sweep leaving waiting environments open, `declaredOnly` validation | pass |
| `temporal-runtime` unit (`gateway::registration`) | admission planning: bind, a second identity refused, a known identity ignoring names, undeclared with and without `declaredOnly`, a declaration scoped to its key, release then rebind, two racing binds | pass |
| `api`, `cli` unit | dispatch of both methods, the wire shape of a waiting environment, CLI commands and flags | pass |
| `store-pg` live | the same rules against Postgres, the schema constraints, the route sweep query, migrations | pass |
| `environment_registration_live` | real daemons: one that starts before its declaration retries and binds under the declared id, a second identity is refused and keeps retrying, release ends the first daemon and binds the second, chosen ids for external environments, close frees the name, key revocation closes waiting environments | pass |
| `profiles_live` | a session started from a profile whose default is a waiting declared environment; chosen ids for provisioned environments | pass |
| Platform web, backend, Configurator | the waiting state, declare and release, `declaredOnly` in the key editor, the demo routes, role checks | pass |

## Documentation

Updated: registered daemons (declaring fixed machines, `declaredOnly`,
replacing a machine, what a declaration protects), the environment overview,
using environments, power and cleanup, the CLI guide, the environment
variable reference, the Configurator page, the generated API reference, and
the declarative universe document.
