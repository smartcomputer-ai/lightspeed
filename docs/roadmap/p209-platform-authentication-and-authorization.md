# P209 — Platform authentication and authorization

**Status:** Proposed, 2026-11-16. Nothing is implemented. What is settled and
what is still open is listed under [Decisions](#decisions).

This is the second of two documents.
[Runtime access simplification](p208-runtime-access-simplification.md) takes
the permission model for callers out of the runtime. This one builds the
single place that replaces it: the Platform backend as the door for every
person and every program.

## Goal

One access system, built once:

- **Everyone comes through one door.** The web app, the CLI, SDKs,
  applications and pipelines all reach the runtime through the Platform
  backend.
- **One model of who may do what.** Roles apply to people and to programs
  alike. A developer's token can do what the developer can do, no more and no
  less.
- **Several ways to authenticate,** and room for more, without the runtime
  knowing about any of them.
- **A level between one universe and the whole deployment.** People and
  applications can create universes and control the ones they own, without
  administering the deployment. This is what the creator design set out to
  do, rebuilt on roles.
- **Programs that work with a chosen set of universes,** whoever created
  them.

## Terms

| Term | Meaning |
| --- | --- |
| Principal | Someone or something that can be given roles: a person, or a service account. |
| Authentication method | How a principal proves who it is: signing in, or presenting a token. |
| Token | A secret that stands for one principal. |
| Role | What a principal may do, in one universe or for the deployment. |
| The door | The Platform backend's API. It authenticates, applies roles, and forwards to the runtime. |

## Principals

- **A person** has an account, from company sign-in or created locally, as
  today.
- **A service account** is a principal that is not a person. It cannot sign
  in. It is given roles the same way a person is. An application, a
  pipeline, or a credential for an agent is a service account.

## Authentication methods

| Method | For | State |
| --- | --- | --- |
| Sign-in in the browser, with the company account or a local password | People in the web app | Exists |
| Sign-in from the CLI, approved in the browser | People at a terminal | New, in a later slice |
| Personal access token | A person's own scripts and tools | New |
| Service account token | Applications, pipelines, agents | New |
| A token from the company identity provider; a pipeline's short-lived identity | Programs that should hold no long-lived secret | Later; the model leaves room |

All of them end the same way: the door knows the principal, and from there
only roles matter.

### Signing in from the CLI

This comes after the rest. Until then a person uses a personal access token
with the CLI.

`lightspeed connect login <address>` signs a person in without an API key:

1. The CLI asks the door to start a sign-in and prints a short code and an
   address.
2. The person opens the address in a browser, signs in the way they normally
   do, including company sign-in, and approves the code.
3. The CLI receives a token for that person and stores it in its private
   credential file.

This is the device authorization flow. It needs no browser on the machine
running the CLI, so it also works over SSH and in a container. The version
of the authentication library the Platform uses ships it as a plugin.

### Tokens

- **A personal access token acts as the person.** It sees exactly what they
  see, in every universe they are a member of, and stops working on the next
  request after they are suspended or lose a membership. A person creates
  and revokes their own under **Account**.
- **A service account token acts as the service account.** Whoever
  administers the service account creates and revokes its tokens.
- **Every token has an expiry.** How long each kind lasts is still to be
  decided (open decision 2).
- **A token can be narrower than its principal.** It may carry a list of API
  families, taken from the runtime contract's classification of methods, and
  then reaches only those methods, within the principal's roles. A
  credential handed to an agent needs this: it may configure a universe and
  not read its sessions.
- **A token can be declared** with a supplied secret, so a pipeline that
  keeps secrets in a secret store never has one printed to it. Repeating a
  declaration changes nothing; a changed secret replaces the old one at once.
- **Only a hash is stored,** with a short prefix for recognizing a token in
  listings. Tokens have their own prefix, so a secret shows whether it is a
  door token or a runtime token.

## Roles

### In a universe

The four roles stay as they are: Viewer, Contributor, Operator, Admin. They
now apply to service accounts as well. A service account is added to a
universe with a role, like a member.

An application that works with a set of universes is a service account with
a role in each of them. It does not matter who created those universes: an
administrator adds it to a universe made in the web app or deployed from a
configuration document the same way.

### For the deployment

| Role | May |
| --- | --- |
| Creator | Create universes, and control the ones it owns. |
| Platform admin | Everything, as today. |

Both can be held by a person or by a service account.

- **With company sign-in,** Creator comes from a third entitlement group,
  `LIGHTSPEED_PLATFORM_OIDC_CREATOR_GROUP`, next to the user and admin
  groups. Setting it to the user group makes every user a Creator, which
  suits a development deployment. Leaving it unset means only Platform
  admins create universes.
- **Without company sign-in,** a Platform admin sets it under **Platform
  admin → Users**.
- **For a service account,** a Platform admin grants it.

### Owning a universe

A universe has at most one owner, a principal.

- **Creating.** A Creator who creates a universe owns it. A universe a
  Platform admin creates has no owner unless they name one.
- **What the owner may do.** The owner acts as Admin in the universe without
  a membership, and decides about the universe itself: protecting it,
  setting its expiry, transferring it and deleting it.
- **Deleting.** Only the owner or a Platform admin deletes a universe. A
  universe Admin cannot: that role is about what happens inside the
  universe, and it can come from a directory group mapping.
- **Transfer.** The owner hands a universe to another Creator. A Platform
  admin assigns or clears any owner.
- **The limit.** A principal may own a bounded number of universes, 25 by
  default, set for the deployment with
  `LIGHTSPEED_PLATFORM_UNIVERSES_PER_OWNER_MAX`. It stops a runaway loop and
  bounds what a leaked token can create. A limit for one principal can be
  added later; the door has a record of each principal to hold it.
- **Losing the Creator role.** The person stays Admin in the universes they
  own and loses the rest: creating, the admin area, deleting, protecting,
  expiry and transfer. The universe keeps its owner, so the authority
  returns with the role. Meanwhile a Platform admin can delete or reassign
  it.

### What a Creator sees

A Creator gets the Platform admin area, restricted to what they own:

| Page | For a Creator |
| --- | --- |
| Universes | The universes they own, with their metadata, protection, expiry and deletion state. Create, protect, set expiry, transfer, delete. |
| Service accounts and tokens | Those they administer. |
| Users, Audit log, Environment providers | Not shown, and refused by the door. |

A Creator adds members to their universes like any universe Admin, and so
sees the list of accounts. That is what every universe Admin sees today, and
it stays open. A closed mode, where members are added by exact email address
without a list and the grant takes effect when that identity signs in, is a
described option and is not built.

### Naming an application's users

A service account may be allowed to name its own users, so an application
with its own sign-in can have the runtime record which of them acted. The
door forwards the name prefixed with the service account, so it can never
pass for a Platform user. Without this permission, a caller cannot set the
actor at all.

## The door

### One endpoint for the runtime's contract

The Platform backend gets an RPC endpoint that speaks the runtime's
contract: the same methods, parameters and results, the same universe
header, the watch stream and blob transfer. A client that works against the
runtime works against the door by changing the address and the credential.

Today the Platform has 95 hand-written routes for the web app and no general
endpoint. The web app keeps its routes. They and the endpoint pass through
the same gate.

### The gate

The gate exists for members. It is generalized from "a member" to "a
principal and, if any, its token's restriction":

1. An unknown method is refused. The gate fails closed.
2. A method outside the token's API families is refused.
3. The feature the method belongs to is on in the universe.
4. The principal's role in the universe meets the method's role.
5. The finer rules that exist today: what a Contributor may set when
   starting a session, visibility of private sessions, creator-only methods,
   who may close and delete, forced soft deletion, the narrowed session
   list, and reading only one's own transcriptions.
6. Deployment methods get deployment roles:

| Methods | Who |
| --- | --- |
| Create a universe | Creator, Platform admin |
| List universes | A Creator sees the ones they own; a Platform admin sees all |
| Read, rename, configure, protect, set expiry, delete; purge deleted sessions | The owner, Platform admin |
| Environment providers and their bindings | Platform admin |
| Methods meant for machines only | Nobody through the door |

The door alone sets the universe and the actor on what it forwards.

### The door's own methods

Tokens, service accounts, members, roles and ownership are operations of the
door, not of the runtime. They are exposed on the same endpoint under their
own prefix, so the CLI and configuration documents manage them like
everything else. Through the door, `initialize` reports the principal and
its roles.

## Clients

### CLI

One CLI, `lightspeed`, with two kinds of connection:

| Connection | Credential | Authority |
| --- | --- | --- |
| Through the door | Sign-in, or a personal or service account token | The principal's roles |
| Direct to the runtime | A runtime token | Everything |

- `lightspeed connect add <address>` asks the address what it is, so the
  person does not have to say.
- `lightspeed connect login <address>` signs in through the browser, once
  that slice is built.
- A direct connection works wherever the runtime can be reached and a token
  is at hand. That is the normal case on a local stack and an operator's
  choice elsewhere; a shared deployment keeps the runtime's listener private,
  and then only the door answers.
- `connect status` shows the principal and its roles, or that the
  connection is direct.
- New command groups manage what the door owns: `token`, `service-account`
  and `member`. On a direct connection they say that they need the door.
- With a Creator's sign-in, `universe create`, `universe list` and
  `universe delete` work on the universes that person owns.

The separate `lightspeed-platform` CLI, which signs in with a password and a
session token, is retired. What it does that is still needed moves into
`lightspeed`.

### SDKs

The TypeScript and Python clients take the door's address and a token. Their
actor option is honoured for a service account allowed to name its users,
and on a direct connection.

### Configuration documents

`config apply` runs through the door with a service account's token.

- **Declared access goes to the door.** Members and directory group mappings
  in a universe document are declared to the door directly. They are stored
  in the runtime today only because the CLI could not talk to the Platform,
  which then reads them back and enforces them. The runtime's fields for
  them are removed.
- **Service accounts and tokens can be declared,** with supplied secrets.
- **A universe created through the door is known to it at once.** Adoption
  and the periodic inventory comparison remain only as a repair for
  universes created directly in the runtime.

### The Configurator setup

The setup that lets an agent configure its universe comes back. It creates a
service account with the Operator role in that universe and a token narrowed
to the configuration API families, and stores that token for the agent. The
agent cannot read sessions with it.

## Security

- **One place decides.** There is no second check to keep in step.
- **A token is bounded by its principal.** A leaked personal token can do
  what that person can do and is revoked under their account. A leaked
  service account token is bounded by its roles and its restriction.
- **People leaving is handled once.** Suspending a person stops their
  sign-ins and every token they have, on the next request. Removal at the
  company identity provider is only seen when the person signs in again, so
  how long a token may live without a fresh sign-in is part of the open
  decision on lifetimes.
- **The actor cannot be forged through the door.** The door sets it, and a
  service account that may name users names them under its own prefix.
- **Isolation between universes rests on the door.** One function sets the
  universe header from the principal's memberships, and the gate refuses
  methods it does not know.
- **The door is on the path of every request.** It is built as a gateway:
  streams and transfers pass through without buffering, and it runs in
  several replicas.
- **Everything passes one place,** which makes complete request auditing
  possible later.

## Storage

Platform database:

- **Tokens:** hash, prefix, principal, label, API families, expiry, last
  use, who created it.
- **Service accounts:** principals that cannot sign in, with memberships
  like people.
- **Universes:** the owner.
- **Roles:** the Creator value next to user and admin.

The installed version of the authentication library has a device
authorization plugin and no API key plugin, so tokens are a table of the
Platform's own.

Runtime: the fields and the column for declared access are removed, with a
migration and `release/metadata.env` aligned.

## Slices

1. **The endpoint and personal tokens.** The RPC endpoint with streams, the
   gate generalized to principals, deployment roles for Platform admin,
   personal access tokens, the CLI and SDKs through the door.
2. **Service accounts.** Memberships, narrowed tokens, declared tokens, the
   door's own methods in the CLI, the Configurator setup.
3. **Creators.** The role and its entitlement, ownership, the limit, the
   restricted admin area, and the universe fields from
   [universe metadata, protection and expiry](p207-universe-metadata-protection-and-expiry.md)
   in the web app.
4. **Declared access moves to the door,** and the runtime's fields go.
5. **Documentation,** for review before it lands: the access overview,
   people and roles, API keys and service access, company sign-in, the CLI
   guide, configuration as code.
6. **Signing in from the CLI,** later.

Later, and not designed here: tokens from the identity provider, pipeline
identities, a limit per principal, the closed mode for the account list,
complete request auditing, rate limits.

## Tests

- **The gate fails closed.** Every method in the contract is either
  classified in the gate or refused, checked against the generated manifest.
- **Role by method.** For each role, a method above it is refused through
  the endpoint, for a personal token and for a service account.
- **No crossing.** A principal with a role in one universe is refused in
  another, by UUID and by handle. A caller-supplied actor header is ignored.
- **A personal token equals its person.** It lists and reads exactly the
  sessions the person sees in the web app, including private ones.
- **Narrowed tokens.** A token limited to some API families is refused
  outside them even when the role would allow the call.
- **Revocation.** Suspension, a removed membership and an expired token each
  stop access on the next request.
- **CLI sign-in,** when it is built: end to end against a local identity
  provider, including approval by a person without a universe membership.
- **Creators.** A Creator creates, lists only their own, cannot read
  another's, hits the limit, transfers and deletes. A universe Admin who is
  not the owner cannot delete. Losing the role leaves Admin and removes the
  rest.
- **Service accounts.** One with roles in two universes reaches those two
  and no third. One allowed to name users has them recorded under its
  prefix.
- **Streams and transfers** pass through the door: the watch stream, long
  polls, a large blob.
- **Declared access** applied through the door takes effect, and the runtime
  no longer stores it.

## Decisions

Decided, 2026-11-16:

- **Authentication and authorization live in the Platform,** in one system.
- **The CLI can go through the Platform or directly to the runtime,** where
  the deployment allows the direct path, as on a local stack.
- **The CLI can sign in through the Platform** to obtain a token, without an
  API key. This can come after the rest.
- **Channel connectors are switched off** in this work; channels are being
  removed for now.
- **Declared access leaves the runtime.**
- **Creators are rebuilt here, on roles,** and not as a kind of key.
- **The `lightspeed-platform` CLI is retired.** What is still needed from it
  moves into `lightspeed`.
- **The runtime's key model is removed first** and this is built after, on
  one branch.

Carried over from the creator design:

- The name Creator.
- Only the owner or a Platform admin deletes a universe.
- The limit is one number for the deployment, 25 by default.
- A person who loses the Creator role stays Admin in what they own.
- A universe a Platform admin creates has no owner unless one is named.
- The account list stays open to Creators; the closed mode is described and
  not built.

Still to confirm:

1. **The device authorization flow for CLI sign-in,** to settle when that
   slice is built. The alternative opens a browser on the same machine and
   receives the answer on a local port, which is one step shorter and does
   not work over SSH.
2. **Token lifetimes.** To be discussed. The question is how long a token
   from CLI sign-in and a token created by hand may live, given that removal
   at the company identity provider is only noticed at the next sign-in. A
   starting point: as long as a browser session for the first, and a
   deployment maximum for the second.
3. **Tokens narrowed by API family,** from the first version.
4. **A service account may name its own users,** under its own prefix.
5. **Transfer goes to another Creator.** Only a Platform admin assigns a
   universe to a principal without the role.

## Not doing

- A permission model in the runtime, of any size.
- A second door. The web app's routes and the RPC endpoint share one gate.
- Several owners on one universe.
- Rules driven by universe metadata.
- Splitting what a Platform admin can see or do.

## Validation and progress

- 2026-11-16: design written from the conversation that led to it and from a
  survey of the Platform's authentication, its member gate and routes, the
  CLI's connection model and the holders of runtime keys. No code changed.
