# P208 — Runtime access simplification

**Status:** In progress, 2026-11-16. All implementation slices are in place.
Scoped checks, live integration checks and the runtime smoke test passed;
the final broad Rust rerun was interrupted when `target/` was cleared.
Decisions are listed under
[Decisions](#decisions).

This is the first of two documents. It takes the permission model for
callers out of the runtime.
[Platform authentication and authorization](p209-platform-authentication-and-authorization.md)
builds the replacement, in one place.
[Universe deletion](p206-universe-deletion.md) is unaffected.

The companion [resource HTTP API design](p208b-resource-http-api.md) specifies
the agreed replacement of the client JSON-RPC transport before the general
Platform API is built. Its HTTP implementation has not started.
It also specifies the subsequent `/ingress` grouping and `/environments`
machine-route prefix. The paths described below record the implementation
before that cutover.

## Problem

Who may do what is decided in two places today, divided by the kind of
credential:

| Caller | Decided by | With |
| --- | --- | --- |
| A person in the web app | The Platform | Roles: Viewer, Contributor, Operator, Admin, Platform admin |
| A program with an API key | The runtime | A key's scope (one universe or the deployment) and its method groups |

Neither side knows what the other knows. A key is not a personal token: it
has no role, and a key with the `session` group reads private sessions its
holder could not open in the web app. A person cannot have a key that acts as
them. Every new need lands on this seam and has to be solved twice or
awkwardly.

The design for self-service universes showed it. Putting a level between one
universe and the whole deployment into the runtime needed an owner on
universes, then several keys per owner, limits per owner, a record for
owners and a flag for naming users. That was principals and memberships
being built a second time, in the layer that has no directory. The runtime
has been there before: an authorization model was built into it and taken out
again.

We want a more capable permission system, not a smaller one. Building it
across two systems is what makes it painful, so it is built in one.

## Decision

The split is by layer, not by credential.

- **The runtime provides mechanism.** It partitions data by universe, records
  who acted, and enforces facts about resources. It does not decide who may
  call what.
- **One door decides.** People and programs reach the runtime through it. The
  door owns authentication, tokens, roles and every rule about who may do
  what. The door is the Platform backend; its design is the second document.

```
people, CLI, SDKs, applications, pipelines
                  │   sign-in, tokens, roles
                  ▼
        the door (Platform backend)
                  │   a static token; names the universe and the actor
                  ▼
        the runtime ── Temporal, PostgreSQL, object store
                  ▲
                  │   their own credentials, unchanged
        environment daemons, environment providers
```

The runtime's RPC endpoint joins Temporal, PostgreSQL and the object store as
infrastructure that only operators and trusted components hold. None of
those has permissions per universe, and the runtime's key check was never
the boundary of the system. What differs is who holds a credential: API
credentials go to developers, pipelines and applications, so that surface has
to be the careful one. After both documents it is exactly one surface.

Someone who builds on the runtime without the Platform gets an engine with no
permission system. They put their own door in front, or run it on a trusted
network. A universe is then a separation of concerns, not an access boundary.

## The runtime's credential

The runtime authenticates components, not tenants. Its RPC endpoint accepts
tokens taken from configuration, the way workers already reach the
environment gateway with `LIGHTSPEED_ENVIRONMENT_GATEWAY_TOKEN`. There is no
key table, no minting, no rotation method and no declared keys.

**The simple form** is one token:

```text
LIGHTSPEED_RPC_TOKEN=<secret>
# or, from a mounted secret:
LIGHTSPEED_RPC_TOKEN_FILE=/run/secrets/lightspeed-rpc-token
```

**Naming is optional.** A suffix names a token, and any number may be set:

```text
LIGHTSPEED_RPC_TOKEN_PLATFORM_FILE=/run/secrets/platform-token
LIGHTSPEED_RPC_TOKEN_OPERATOR_FILE=/run/secrets/operator-token
```

- The lower-cased suffix is the name: `platform`, `operator`. The token
  without a suffix is called `default`.
- Every token has the same power: every method, every universe, any actor.
  A name gives no permission.
- The name is what records show when a request names no actor, so one can
  tell the Platform's calls from an operator's.
- Rotation without a gap: add a second token, move the consumer to it,
  remove the first.
- A secret has at least 32 non-whitespace printable ASCII characters, so it
  can travel unchanged in an HTTP bearer header. `lightspeed token
  generate-secret` generates one offline. Mounted files may end in one LF or
  CRLF, which is removed. Duplicate names, duplicate secrets and inline/file
  conflicts refuse startup.

`LIGHTSPEED_AUTH_MODE` keeps its two values and stays required.
`authenticated` now means that a request presents one of the configured
tokens, and the runtime refuses to start in that mode with none configured.
`single` stays the unauthenticated mode for local development.

## What a request carries

- `Authorization: Bearer <token>`.
- `x-lightspeed-universe` for a universe method, as a UUID or a handle.
- `x-lightspeed-actor`, optional: an opaque name the runtime records as the
  one who acted. Any caller may send it. The runtime decides nothing by it.

Records attribute a request to the actor, or else to the token's name.

## What is removed

- **API keys:** the table, the store, hashing and prefixes, the
  `deployment/api-keys/*` methods and their group.
- **Scopes and method groups as checks:** the caller's scope, the group
  check on every method, and the key's part of the request context.
- **The actor flag.** Every holder of a token may name an actor.
- **The `api-key` commands** of `lightspeed-runtime` and of `lightspeed`,
  and keys in configuration documents. The deployment document held nothing
  else and goes with them.
- **Most of what `initialize` reports about the caller.** It keeps whether
  the connection is unauthenticated and the token's name.
- **The one refusal the runtime makes by actor:** a transcription may only
  be read by the actor that created it. The Platform applies that rule.
- **The watch-stream limit per key.** With one caller it would be one limit
  for the deployment. It becomes a limit per actor.

## What stays

- **Universes as a partition,** named on every request.
- **Attribution:** `createdBy`, `requestedBy`, `decidedBy`.
- **What a door needs to drive visibility:** a session's creator and whether
  it is shared, the `visibleTo` filter on `session/list` and
  `universe/watch`, and the `softDelete` and `sharedOnly` parameters. These
  are already filters and parameters the Platform sets; the runtime does not
  decide by them.
- **Rules for the runtime's own work.** Bots and delegated sessions act
  under controller rules that use the target's visibility and lineage, never
  a caller. They are unchanged.
- **Facts and guards on resources:** metadata, protection, expiry and
  deletion of universes, session retention, and the like.
- **The method classification in the API contract.** Each method in
  `crates/api/contract/methods.json` says whether it addresses a universe or
  the deployment, what kind of action it is, the least role that should call
  it, the API family it belongs to and whether it names a session. The
  runtime publishes this and enforces none of it. A door reads it, so nobody
  classifies 151 methods by hand and a new method arrives classified. The
  test that every method is classified stays.

## Credentials the runtime holds stay, under a clearer name

The runtime has a second thing called "auth": the `auth/*` methods and the
`auth` crate. It is not about who may call Lightspeed. It is the credentials
Lightspeed holds to call others: model providers, MCP servers and GitHub.

None of it moves to the Platform. Workers use these credentials in the
middle of a run, with no person and no Platform in the path, and a run can
last hours while tokens are refreshed underneath it.

| Piece | What it does |
| --- | --- |
| Secret store | Encrypted values, sealed with the deployment's key. |
| Grants and the broker | Resolves a credential for a model call, an MCP call or an environment; refreshes tokens; mints GitHub installation tokens; keeps tokens away from the agent. |
| Providers | Model endpoints and GitHub App configuration. |
| OAuth clients | The registration with each authorization server, needed for refresh. |
| OAuth flows | Starting a login, the browser callback, exchanging the code. |

The interactive login is the one part with a person in a browser, and the
Platform could run it and hand the result over as a grant. It stays in the
runtime: the protocol work is done by the Rust MCP library today, refresh
would stay in Rust in any case, and a runtime without the Platform keeps the
ability to log in to an MCP server.

**The rename.** With the key model gone, the two meanings of "auth" are what
is left to confuse, so this side is renamed to credentials, which is what
the web app already calls it:

- The `auth/*` methods become `credentials/*`, and the API family in the
  method classification is renamed with them.
- The crate is `credentials`; domain and API types use `Credential*`.
  Tables use `credentials_*`. Public paths are `/credentials/callback` and
  `/credentials/client-metadata.json`.
- OAuth protocol terminology, contextual `authGrantId`/`authProvider*`
  references and opaque stored IDs remain unchanged. Platform sign-in
  authentication is separate from this outbound credentials namespace.

After that, "authentication" and "authorization" mean one thing in
Lightspeed, the Platform's, and "credentials" means what a universe uses to
reach other systems.

## The machine plane is untouched

Machines the runtime uses for compute do not use RPC tokens and will not go
through the door:

- **Environment daemons** dial in to the environment gateway and prove
  themselves with a registration key and their own key pair.
- **Environment providers** are dialed by the runtime, which presents the
  provider's credential.
- **Workers** reach the environment gateway with its deployment token.

Managing them is an API call: registering a provider, minting a registration
key, declaring an environment. The secrets involved stay stored in the
runtime, because they are resources and not caller credentials.

## A private listener for RPC

`/rpc` has a private listener, separate from three endpoints that the public
must reach and that authenticate themselves:

- `/credentials/callback`, where an OAuth provider sends a person's browser back;
- `/credentials/client-metadata.json`, which authorization servers fetch;
- `/hooks/bots/…`, the webhook ingress for bot triggers.

The `gateway` role starts two listeners: `LIGHTSPEED_GATEWAY_BIND` (default
`127.0.0.1:18080`) for RPC and `LIGHTSPEED_PUBLIC_BIND` (default
`127.0.0.1:18082`) for public callbacks and webhooks. The
`environment-gateway` role starts `LIGHTSPEED_ENVIRONMENT_GATEWAY_BIND`
(default `127.0.0.1:18083`) for machine routes. All three serve `/health`
and `/ready`, with no cross-family routes.

`LIGHTSPEED_PUBLIC_BASE_URL` names the callback/webhook origin.
`LIGHTSPEED_ENVIRONMENT_PUBLIC_BASE_URL` names the daemon origin; additional
registration origins still use `LIGHTSPEED_ENVIRONMENT_REGISTRATION_URLS`.
Worker routing defaults to the environment listener, and its dedicated URL
setting still overrides it. Machine credentials and protocols are unchanged.
Network rules can close RPC to everything but the door and trusted components.

## Who holds a runtime token

| Holder | Why |
| --- | --- |
| The Platform backend | It is the door. |
| An operator, or a developer on a local stack | Direct access with the CLI or an SDK, with full authority. |

A runtime token is never given to an agent or stored where one can read it.

### Channel connectors are switched off

The connector host is the separate process that bridges chat providers,
today Telegram and WhatsApp, to the runtime. It calls the runtime with an
API key of its own. Channels are being removed for now, so the connector
host is not moved to the new tokens: it is switched off. The development
launcher no longer starts it, and it is given no runtime token. Its removal,
with the rest of channels, is separate work.

## Clients

- **CLI.** `lightspeed connect add` with the runtime's address and a token
  is a direct connection with full authority. That is the right connection
  for local development and for operators. Commands, the universe selection
  and `--universe` work as today. Connecting through the door is part of the
  second document.
- **SDKs.** The TypeScript and Python clients keep the token, universe and
  actor options. Against the runtime, the actor option always works.
- **Configuration documents.** `config apply` for universe documents works
  with a direct connection. Keys leave the document format.
- **The development launcher** sets tokens in the environment instead of
  provisioning keys.

## Between the two phases

The key model is removed first and the door is built after. Both happen on
one branch before anything is released, so this is a state of the branch and
not of any deployment. While the branch is in it:

- **People in the web app** notice nothing. The Platform's gate is
  unchanged; it calls the runtime with a token instead of a key.
- **Programs** reach the runtime only with a runtime token, which is full
  authority.
- **The API key pages** in the web app go away, for a universe and for the
  deployment.
- **The Configurator setup is switched off.** It hands an agent a key
  limited to configuration methods. Its replacement is a narrowed door
  token, and until that exists there is nothing safe to hand over.
- **Declared members and directory group mappings** keep working as they do
  today. They leave the runtime when the door can receive them, which is in
  the second document.

## Security

- **One trust line.** Behind the door is one zone: the runtime's RPC,
  Temporal, PostgreSQL, the object store and the workers. Their credentials go to operators and trusted components only.
- **A runtime token is everything.** It is kept like the database
  credential.
- **The RPC listener is private.** Network rules admit the door and trusted
  components. The public endpoints have their own listener.
- **Isolation between universes for API callers rests on the door.** For
  people it already does. The check the runtime made for universe keys goes
  away. What replaces it is that the door alone sets the universe header, in
  one place, and refuses any method it does not know.
- **Authentication stays ahead of the body.** A request without a valid
  token is refused before its body is read, as today.

## Storage

Runtime migration 26 drops `api_keys` and rewrites stored attributions that
name a key by scope to name it alone. Migration 27 renames the outbound
credential tables and their constraints/indexes without rewriting encrypted
values or IDs. `release/metadata.env` is aligned at revision 27. The API
contract and every generated consumer are regenerated.

## Slices

1. **Tokens from configuration.** The simple and the named form, the mode
   check at start-up, authentication before the body.
2. **The key model removed.** Keys, scopes and group checks; the key
   methods; the commands; keys in documents; `initialize`; the transcription
   rule moved to the Platform; the watch limit per actor.
3. **Consumers.** The Platform backend, the development launcher, the CLI's
   direct connection, the SDKs, the web app's key pages removed, the
   Configurator setup and the connector host switched off.
4. **The private listener.**
5. **The rename to credentials.** Mechanical, with the contract and every
   generated consumer regenerated.
6. **Documentation,** for review before it lands: the access overview, API
   keys and service access, self-hosting, the environment-variable
   reference, the CLI guide, configuration as code.

## Tests

- A request without a configured token is refused before its body is read.
- The simple token works. Two named tokens both work, and records show their
  names. A token shorter than the minimum refuses start-up.
- `authenticated` with no token configured refuses start-up.
- A token reaches every universe, by UUID and by handle, and deployment
  methods.
- The actor a request names is recorded and changes no outcome, including
  for transcriptions.
- `/rpc` is not served on the public listener; the callback, client metadata
  and webhook endpoints are not served on the private one.
- Every method is still classified in the contract.
- After the rename, no method or API family named `auth` remains in the
  contract, and a login to an MCP server still completes end to end.
- The Platform's member gate passes its existing suite against a runtime in
  the new mode, including the transcription rule it now applies.

## Decisions

Decided, 2026-11-16:

- **Access is decided in one place, outside the runtime.** The runtime keeps
  no permission model for callers.
- **The runtime keeps no API keys in its database.** Its credential comes
  from configuration.
- **A plain token works; naming tokens is optional.**
- **RPC and the public callback and webhook endpoints are split.**
- **Environment daemons and providers do not go through the door.**
- **This is the time.** There are no production deployments, so nothing has
  to stay compatible.
- **The key model is removed first and the door built after,** on one
  branch.
- **Channel connectors are switched off** in this work. Channels are being
  removed for now.
- **The credentials the runtime holds for calling others stay in the
  runtime,** and `auth/*` is renamed to `credentials/*`.

## What this reverses

- API keys as a runtime concept, including declared keys and the recent
  hardening of key groups.
- Method groups and scopes as something the runtime enforces. The
  classification stays as published information.

## Validation and progress

- 2026-11-16: design written after the design for self-service universes had
  started to rebuild principals in the runtime, from a survey of the
  runtime's key model, the Platform's gate, the environment plane and the
  other holders of runtime keys. No code changed.

- 2026-11-16: implemented configured tokens with authentication before body
  reads; removed caller scopes/groups, API-key methods/store/commands/pages
  and declared keys. Request attribution and `initialize` now name tokens.
  Migration 26 rewrites stored attribution and drops the key table; event
  replay rebuilds discarded checkpoint pointers. Watch limits follow actors
  with token-name fallback. Platform checks transcription read and cancel
  ownership. CLI, SDKs and launcher use tokens; Configurator setup and channel
  connector startup are disabled. Contracts and generated consumers are
  regenerated. The public listener split and `credentials/*` rename are
  separate remaining slices; the Platform token gateway remains subsequent work.

- 2026-11-16: validation passed: exact workspace/all-target Clippy with warnings
  denied; 1,926 tests across the affected Rust crates; three binary startup
  regressions; isolated PostgreSQL authentication and attribution-upgrade tests;
  CLI, TypeScript/Python SDK, Platform backend/web and Configurator suites;
  52 launcher tests; typechecks, builds, documentation and release-metadata
  checks. The launcher and daemon scrub runtime tokens from child command
  environments. Broader Temporal and provider live suites were not run.

- 2026-11-16: implemented separate RPC, public callback/webhook and environment
  listeners, including coordinated drain/shutdown and distinct origin defaults.
  Launcher preflight/readiness and deployment guides follow the three ports.
  Renamed outbound credential methods, API families, crate, DTOs, tables and
  callbacks; preserved contextual reference fields and machine protocols.
  Added migration preservation, listener isolation and callback coverage.
  Final validation results follow when the end-of-change checks complete.
