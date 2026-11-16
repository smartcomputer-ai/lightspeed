# P207 — Creators and self-service universes

**Status:** Proposed, 2026-11-16. Nothing is implemented. What is settled and
what is still open is listed under [Decisions](#decisions).

Builds on [universe deletion](p206-universe-deletion.md), which has to land
first: this work makes creating and deleting universes routine.

## Problem

A deployment is now used as a platform. People want to stand up a universe
for a test and remove it again, run tests that create dozens of universes,
keep a universe for a team for a while, and deploy long-lived universes from
configuration documents. Today there are only two levels of reach, and they
are the same two for keys and for people:

| Reach | Key | Person |
| --- | --- | --- |
| One universe | Universe key | Member, with a role from Viewer to Admin |
| The whole deployment | Deployment key | Platform admin |

Creating a universe needs the second level, which also sees and can delete
every other universe, mints any key, and manages providers and accounts. So a
developer who only wants a universe of their own needs root, and a test
pipeline holds a key that can delete production universes.

The universe record has no place for the things this use needs either: who a
universe belongs to, what it is for, whether it should go away by itself, and
whether it must not be deleted casually.

## The model

One level is added between the two, with the same name on both sides:

| Reach | Key (runtime) | Person (Platform) |
| --- | --- | --- |
| One universe | Universe key | Member, with a role from Viewer to Admin |
| The universes you own | **Creator key** | **Creator** |
| The whole deployment | Deployment key | Platform admin |

- There are no new universe roles and no partial Platform admin.
- A creator can create universes and has full authority over the ones they
  own, and nothing else.
- The level is defined in the runtime, because developers and pipelines work
  with an API key and should not need the Platform. The Platform follows the
  same rule for people.
- Platform admin and deployment keys keep the authority they have today.

## Ownership

A universe has an optional owner, `ownedBy`.

- **It is an opaque string** that the runtime only compares for equality. It
  has the bounds of an actor id: not empty, at most 256 bytes, no control
  characters.
- **A creator key carries an owner.** It reaches exactly the universes whose
  `ownedBy` equals it, and every universe it creates is stamped with it.
- **For a person, the owner is their Platform user id,** the same id the
  Platform already sends as `x-lightspeed-actor`. A universe a Creator makes
  in the web app and one they make with their personal creator key have the
  same owner and form one set.
- **For a machine, the owner is whatever the Platform admin chooses** when
  minting the key, for example `ci-integration`. Several keys may carry the
  same owner.
- **A universe may have no owner.** That is the case for every universe that
  exists today and for universes a deployment key creates without naming one,
  such as those deployed from configuration documents. Only the deployment
  level reaches them.

The owner is not a key id, because a revoked key keeps its id forever and its
replacement needs a new one: losing a key would orphan every universe it
made, and a person with two keys would have two separate sets. It is not a
user either, because the runtime has no users.

Ownership can be passed on with `deployment/universes/owner/put`.

- **A creator key** may hand a universe it owns to another owner, provided
  at least one active creator key carries that owner. This keeps a universe
  from being given to a mistyped or unknown owner, where nobody but an
  administrator could reach it. The key loses its reach over the universe at
  once. It cannot clear the owner.
- **A deployment key** may set any owner or clear it. The Platform uses this
  for people, who own universes by user id whether or not they have a key,
  and an administrator uses it to assign or take back any universe.
- **The recipient's limit applies** in both cases.

A universe handed over this way arrives without the recipient agreeing, and
it counts against their limit. Inside one deployment that is acceptable: the
transfer is attributed, and the recipient can delete the universe or hand it
back.

## Creator keys

`AccessScope` gains a third value, `{"kind": "creator", "owner": "…"}`. The
owner is part of the scope and, like the scope, never changes on a key.

### What a creator key reaches

| Request | Result |
| --- | --- |
| A universe method with `x-lightspeed-universe` naming a universe it owns, by UUID or handle | Allowed, within the key's method groups. |
| The same for a universe it does not own | `notFound`, as if the universe did not exist. Handles can be guessed; the answer must not confirm them. |
| A universe method without the header | Refused, as for a deployment key. |
| `initialize` | Allowed. It reports the creator scope and the owner. |

Deployment methods are either open to creator keys, with the stated
restriction, or refused:

| Method | Creator key |
| --- | --- |
| `deployment/universes/create` | Allowed. The universe is stamped with the key's owner; naming another owner is refused. Refused when the owner is at its limit. |
| `deployment/universes/list` | Only universes it owns. |
| `deployment/universes/read`, `handle/put`, `configuration/put`, `expiry/put`, `delete` | A universe it owns. Otherwise `notFound`. |
| `deployment/universes/deleted/list` | Only records of universes it owned. |
| `deployment/universes/owner/put` | A universe it owns, to an owner that an active creator key carries and that is below its limit. It cannot clear the owner. |
| `deployment/api-keys/create`, `put`, `list`, `rotate`, `revoke` | Only keys whose scope is a universe it owns. Never a key that asserts actors. `list` must name such a scope. |
| `deployment/sessions/deleted/list`, `deployment/sessions/purge` | A universe it owns. |
| `deployment/environment-providers/*`, provider bindings | Refused. |
| `deployment/channels/accounts/list` | Refused. |

A test asserts that every deployment method appears in this table, so a
method added later is refused for creator keys until someone decides
otherwise.

Further rules:

- **Groups.** A creator key holds method groups like any key. Its scope
  allows every universe group and `deployment/universes`,
  `deployment/api-keys` and `deployment/sessions`. A pipeline key that only
  creates, uses and deletes universes does not need `deployment/api-keys`.
- **Actor assertion only when minted so.** A key that asserts actors may tell
  the runtime which person a request is for, with the `x-lightspeed-actor`
  header, and the runtime records that person as the one who acted. Only a
  gate that authenticates people should hold such a key; the Platform's key
  is one, and an application with its own sign-in is another. A creator key
  has the flag only if the deployment key that minted it set it, and like
  the scope it never changes. Without it the key acts as itself and records
  name the key. Universe keys minted by a creator key never have the flag.
- **No escalation through keys.** A creator key mints universe keys for its
  own universes only. It cannot mint a deployment key or another creator key,
  and it cannot change the owner it carries. Only a deployment key with
  `deployment/api-keys` mints creator keys.
- **Key ids.** A creator key's id is unique among the deployment's keys,
  the namespace deployment keys use. Records attribute its requests as
  `{"kind": "key", "keyId": "…", "scopeKind": "creator"}`.

### Where it is checked

The gateway resolves a caller from the headers before reading the body, and
the handle lookup for deployment keys already happens there. The ownership
check joins it: when a creator key names a universe, the same lookup returns
the universe only if its owner matches. Deployment methods that take a
`universe` parameter go through one resolver in the deployment API, which
gets the same condition. The check reads the database on every request and
is not cached.

### The limit

An owner may have a bounded number of universes that are not deleting. This
stops a runaway loop and bounds what a leaked key can create.

- It is one number for the deployment, set with
  `LIGHTSPEED_UNIVERSES_PER_OWNER_MAX`, 25 when unset.
- It counts per owner, so a second key with the same owner does not double
  anything.
- It applies whenever a universe gets an owner: on creation and on transfer,
  whichever key makes the call.
- It is a safety net, not a quota. A deployment where an application or a
  pipeline needs more raises the number, which raises it for every owner.

A different limit for one owner is left for later. The limit belongs to the
owner, and there is no record of an owner to hold it; storing it on keys
would need a rule for keys of one owner that disagree. When the need comes
up, the owner becomes a small record of its own, with a name and its limits,
managed by deployment keys. Other limits per owner would go there too, and
the transfer rule would become "that owner exists". Keys and universes refer
to the owner by name either way, so nothing here has to change for it.

There is no maximum lifetime on a key. A loop that forgets to set an expiry
runs into the limit and fails where it can be seen.

## Applications built on the runtime

Some applications use the runtime directly, with their own interface and
sign-in and without the Platform. Such an application should hold a key that
reaches the universes it works with and no others, and it wants the runtime
to record which of its users acted. This needs no further kind of key: the
application is an owner.

- **Its key** is a creator key with an owner such as `support-portal`,
  minted by a deployment key with the actor flag set.
- **Universes it creates** through the SDK are stamped with that owner.
- **Universes others create for it** are assigned to it. An administrator
  names the owner when creating a universe in the web app or sets it later,
  and a configuration document states `ownedBy`. A universe deployed from a
  document and one made in the web app are then in the application's set
  like the ones it made itself.
- **What it may do** is decided by the key's groups, as for every key. An
  application that should work in its universes but not create or delete
  them gets the universe groups and not `deployment/universes`.
- **Its users** are named with the actor header. The runtime records them and
  decides nothing by them; the application checks its own users' rights, as
  the Platform does for members.

The set of universes a key reaches is therefore never a list on the key. It
is whatever the owner owns at the time of the request, which changes without
touching the key.

Two limits of this:

- **A universe has one owner.** It cannot be in two applications' sets, or
  belong to a person and an application at once. A second party gets a
  universe key for that universe, or people get memberships in the Platform,
  which work independently of the owner. See
  [One owner, several holders](#one-owner-several-holders).
- **The limit on universes is one number for all owners.** An application
  that keeps many universes needs the deployment's number raised. See
  [The limit](#the-limit).

### One owner, several holders

A universe has exactly one owner, so "whose universe is this" always has one
answer, and so does "who may delete it". What can be several is who acts as
that owner:

- **Several keys may carry the same owner,** each with its own groups. An
  application can run its service with a key that holds only the universe
  groups and keep a second key with `deployment/universes` for creating and
  deleting. Both reach the same set.
- **Several people sharing an owner** is the way to give a team joint
  control later, by letting the Platform treat the members of a group as one
  owner. It is not part of this work.

Several owners on one universe are deliberately left out. They would make
every party able to delete it, raise the question of who may remove whom,
and count one universe against several limits. The cases they would serve
are covered: different authority for different keys by several keys per
owner, a second party's access by a universe key or a membership. The owner
is a single value on the universe; turning it into a set later would not
change how keys work.

## The universe record

Four fields are added. All but the expiry can be stated in a configuration
document.

| Field | Meaning | Set with |
| --- | --- | --- |
| `metadata` | A bounded string map for marks such as team, purpose or pipeline run. | Creation, `configuration/put`, documents |
| `protected` | The universe cannot be deleted while this is set. | Creation, `configuration/put`, documents |
| `expiresAtMs` | When the universe is deleted by itself. Absent means never. | Creation, `expiry/put` |
| `ownedBy` | The owner. | Creation, `owner/put`, documents |

A document that states `ownedBy` has `apply` call `owner/put` when the owner
differs, under the rules for whichever key applies it.

`deployment/universes/read` and `list` return all four. `list` gains a
`metadata` containment filter and, for deployment keys, an `ownedBy` filter.
The record of a deleted universe keeps `ownedBy` and `metadata`.

### Metadata

The same map as on sessions and environments, with the same validator and
bounds: 32 entries, keys up to 64 bytes, values up to 256 bytes, and no
`lightspeed.` prefix for callers. A put replaces the whole map. The list
filter is containment, as on `session/list`.

Metadata is for people and their tools to read and filter by. Nothing in the
runtime acts on it.

### Protection

`protected` is a guard against accidents, not a permission.

- `deployment/universes/delete` is refused with `rejected` while it is set.
  The refusal is in the server, so it holds for the CLI, the web app, an SDK
  script that loops over universes, and expiry alike.
- Clearing it takes the same authority as deleting. Whoever may delete a
  universe may unprotect it first; it is two steps.
- A protected universe cannot have an expiry, and an expiring universe cannot
  be protected. One of the two has to be cleared first.

Universes deployed from documents are the ones this exists for:

- A document may state `protected: true` or `protected: false`, and `apply`
  then keeps the universe that way.
- When a document leaves it out, `apply` protects a universe it creates and
  otherwise leaves the field alone. A universe that was created first and
  configured from a document afterwards, such as an expiring test universe,
  is therefore not protected by accident.

### Expiry

A universe with `expiresAtMs` is ephemeral. The deletion pass from
[universe deletion](p206-universe-deletion.md#the-deletion-pass) also looks
for universes whose time has passed and starts deleting them, with the
request attributed to the runtime.

- Activity does not extend it. The owner extends or clears it with
  `expiry/put`.
- A new expiry must lie in the future.
- The record of the deleted universe shows that it expired.

Expiry is opt-in. A universe without it stays until someone deletes it.

## Command line

All of this is reachable with `lightspeed` and a key. With a creator key the
commands show and reach only the owner's universes.

```bash
# A test run: create, configure, use, remove.
lightspeed universe create --handle "t-$RUN" --expires-in 2h --metadata purpose=ci
lightspeed --universe "t-$RUN" config apply -f test-universe.yaml
# … run the test …
lightspeed universe delete "t-$RUN" --yes
```

If the run crashes before the last line, the universe expires. If it forgets
`--expires-in` as well, the owner's limit stops the pile from growing.

| Command | Does |
| --- | --- |
| `universe create [--handle H] [--expires-in D] [--metadata k=v]… [--protected]` | Creates a universe. With a deployment key, `--owner O` names an owner. |
| `universe list [--metadata k=v]… [--owner O] [--deleted]` | Lists universes with owner, protection and expiry. |
| `universe protect U`, `universe unprotect U` | Sets or clears protection. |
| `universe set-expiry U <duration\|never>` | Sets or clears the expiry. |
| `universe metadata put U k=v…` | Replaces the metadata. |
| `universe transfer U <owner\|none>` | Hands the universe to another owner. Only a deployment key may pass `none`. |
| `universe delete U` | From universe deletion. Refuses a protected universe and names `unprotect`. |
| `api-key create --creator OWNER --group …` | Mints a creator key. Deployment keys only. |
| `connect status` | Shows the creator scope and its owner. |

`config apply` works with a creator key: it creates the universe if it is
missing, which stamps the key's owner, and writes every resource through
methods a creator key reaches. Deployment documents can declare creator keys
in `apiKeys`, in the wire format of the key put.

## Platform

The Platform gives people the same middle level and adds no rule of its own.

### The Creator entitlement

A person is a user, a Creator or a Platform admin. The entitlement decides
whether someone may create universes, mint creator keys, and exercise an
owner's authority over the universes they own.

- **With company sign-in:** a third entitlement group, set with
  `LIGHTSPEED_PLATFORM_OIDC_CREATOR_GROUP`, next to the user and admin
  groups. It is sufficient for admission by itself, and it is refreshed at
  every sign-in like the other two. Setting it to the same value as the user
  group makes every user a Creator, which suits a development deployment.
  Leaving it unset means only Platform admins create universes, as today.
- **Without company sign-in:** a Platform admin picks `user`, `creator` or
  `admin` under **Platform admin → Users**.

A Platform admin is a Creator too.

### Owning a universe

The Platform uses the person's user id as the owner.

- **Creating.** A Creator creates a universe in the web app; the Platform
  creates it in the runtime with `ownedBy` set to their id and adds them as
  an Admin member, as it does for the creator of a universe today. A Platform
  admin may leave the owner empty or name one.
- **Authority.** The owner acts as Admin in the universe without needing a
  membership, the way a Platform admin does in every universe. That includes
  reading private sessions, which a universe Admin can already do. On top of
  that the owner decides about the universe itself: protecting it, setting
  its expiry, transferring it and deleting it.
- **Without the entitlement.** A person who becomes an ordinary user again
  stays Admin in the universes they own and loses the rest: they no longer
  see the admin area, and cannot delete, protect, expire or transfer. The
  universe keeps its owner, so nothing has to be converted and the authority
  returns with the entitlement. Meanwhile a Platform admin can delete or
  reassign those universes.
- **Deleting.** The owner or a Platform admin deletes a universe. An Admin
  member who is not the owner cannot: Admin is a role inside a universe, and
  it can come from a directory group mapping, which would let a whole group
  delete the universe. A universe without an owner is deleted by a Platform
  admin, as today. Archiving is no longer a precondition, as decided for
  universe deletion.
- **Transfer.** The owner picks another person; the Platform checks ownership
  and sets the new owner with its deployment key. The recipient needs no key.
- **Projection.** The Platform learns `ownedBy`, `protected`, `expiresAtMs`
  and `metadata` from the runtime inventory it already reads every 30
  seconds, and writes them at once when it makes the change itself.

### What a Creator sees

A Creator gets the Platform admin area, restricted to what they own:

| Page | For a Creator |
| --- | --- |
| Universes | The universes they own, with their marks, protection, expiry and deletion state. Create, protect, set expiry, transfer, delete, and purge deleted sessions. No reconciliation, adoption or engine repair. |
| API keys | The universe keys of universes they own, and their personal creator keys. No deployment keys, no keys that assert actors. |
| Users, Channels, Audit log, Environment providers | Not shown, and refused by the server. |

### Personal creator keys

A Creator mints a creator key for themselves on the API keys page. The
Platform mints it with its deployment key: scope creator, owner their user
id, the groups they chose, no actor assertion. The secret is shown once.

- **Adoption.** A runtime universe whose owner is a Platform user's id is
  adopted automatically, named by its display name or else its handle. A
  universe made with a personal key therefore appears in that person's web
  app within the sync interval. Universes whose owner is not a user, such as
  a pipeline's, are not adopted; a Platform admin sees them in the inventory.
- **Revocation.** When a person loses the Creator entitlement or is
  suspended, the Platform revokes their personal creator keys. Otherwise the
  keys would still delete and transfer through the runtime what the person
  no longer may in the Platform.

### Members and the user list

A Creator adds members to their universes like any universe Admin, and so
sees the user list: names and email addresses of every account. That is not
a new exposure in kind, because any Admin of any universe sees it today, and
it fits a deployment being one trust zone. The list stays open.

**A closed mode is described here and not built.** For a deployment where
the list must not be visible, a deployment switch would replace the picker
with an exact email field. Adding an email stores a grant that takes effect
when that identity signs in, which is how declared members already work. The
add always succeeds, so it does not reveal whether an account exists; the
person appears by name once the grant matches. One leak would remain: a
grant for an existing account resolves at once.

### Audit

The Platform's audit trail records universe creation, deletion, protection
changes and transfers, and the creation, rotation and revocation of creator
keys.

## Environments in a new universe

- **Registered daemons** need nothing new. Registration keys are universe
  resources, so the owner declares them, by hand or from a document, and
  daemons dial in. This is how universes that come and go are expected to get
  compute.
- **Deployment providers** are bound to a universe by a Platform admin, and
  there is no default. A creator's universe therefore cannot provision from a
  deployment provider until an admin binds it. An optional later step is a
  provider flag that binds the provider to every new universe.

## Security

- **The check is in the runtime, on every request, from the database.** A
  creator key's reach does not depend on the Platform or on a cache.
- **Other universes do not exist for a creator key.** It gets `notFound`,
  never `forbidden`, for a universe it does not own.
- **A creator key cannot widen itself.** It cannot mint keys beyond its own
  universes, give itself or another key the actor flag, take a universe it
  does not own, or reach providers, channel accounts or other deployment
  resources.
- **An asserting creator key can misattribute only inside its own
  universes.** The runtime does not verify the actor a key names, which is
  why the flag is given by a deployment key alone. Where the Platform is also
  used on such a universe, it treats a session recorded for one of its user
  ids as that person's. The owner has full authority there in any case.
- **A creator key can only give away.** It hands over universes it owns, and
  only to an owner some active creator key carries. Which owners exist is
  decided by deployment keys alone: when minting a creator key and when
  assigning a universe to someone.
- **The limit bounds a leaked key and a runaway loop.**
- **Key management through a creator key is bounded.** The existing warning
  that `deployment/api-keys` is full authority applies to deployment keys.
  On a creator key the group reaches only its own universes.
- **Leaving is handled.** The Platform revokes personal creator keys with
  the entitlement. Keys minted for machines are revoked by whoever minted
  them, as for every other key.
- **Protection is a guard, not a boundary.** Anyone who may delete may
  unprotect.
- **Nothing changes between universes.** Universes in a deployment already
  share a trust zone. Platform admins stay root.

## Storage

Runtime, one migration after the one for universe deletion, with
`release/metadata.env` aligned:

- `universes`: `owned_by`, `metadata_json` with the containment index used on
  sessions, `protected` (false by default), `expires_at_ms`.
- `universes_deleted`: `owned_by` and `metadata_json`.
- `api_keys`: `owner`, only for creator keys.

Platform: the universe row caches `ownedBy`, `protected`, `expiresAt` and
`metadata` from the runtime. The role column takes the new value `creator`.

The API contract, the configuration document schema and every generated
consumer are regenerated.

## Slices

1. **The universe record.** The four fields, their methods and filters, the
   CLI commands, the document fields, expiry in the deletion pass, delete
   refusing a protected universe. This is useful with today's deployment
   keys alone.
2. **Creator keys.** The scope, the ownership check, the method table and
   its test, the limit, minting, `initialize`, the CLI.
3. **Platform.** The entitlement, ownership and its projection, the owner's
   authority, the restricted admin area, protect, expiry, transfer and
   delete in the web app, adoption, personal creator keys and their
   revocation, audit entries, the demo backend.
4. **Documentation,** for review before it lands: people and roles, API keys
   and service access, managing universes, the CLI guide, configuration as
   code, company sign-in.
5. **Optional, later:** the provider flag for new universes; the closed mode
   for the user list.

## Tests

Unit:

- The scope and target matrix for creator keys, next to the existing one.
- Every deployment method classified for creator keys.
- The ownership check, including `notFound` for a universe that exists but
  is not owned.
- The limit on creation and on transfer, counted per owner across keys.
- Protection and expiry: neither can be set while the other is, and delete
  is refused while protected.

Live, runtime:

- A creator key creates several universes, lists only its own, cannot read
  or name another owner's universe, mints a universe key for its own, cannot
  mint a deployment or creator key, hits its limit, and deletes.
- Two keys with one owner share one set of universes.
- An expired universe is deleted by the pass and its record says so. A
  protected universe is neither deleted nor expirable.
- `config apply` with a creator key creates and configures a universe, and
  protects it when the document does not say otherwise.
- A creator key hands a universe to another owner and no longer reaches it;
  the recipient's key does. A transfer to an owner no active creator key
  carries, or to an owner at its limit, is refused. A creator key cannot
  clear the owner.
- A deployment key assigns and clears owners freely.
- A creator key minted with the actor flag has its requests recorded for the
  actor it names, in universes it owns only. One minted without the flag is
  refused when it sends the header, and it cannot mint a key that has it.
- A universe created by a deployment key and assigned to an owner, by call
  or by document, is reached by that owner's key.

Live, Platform:

- A Creator sees only their universes in the admin area and is refused the
  other pages by the server.
- An owner without a membership acts as Admin; a person who is neither
  member nor owner sees nothing.
- A universe made with a personal creator key appears in the web app; one
  made with a pipeline key does not.
- Losing the entitlement revokes personal creator keys and leaves the person
  an Admin of the universes they own, without the admin area, delete,
  protect, expiry or transfer. Regaining it restores them.

## Decisions

Decided, 2026-11-16:

- **The level is a runtime concern.** Developers work with an API key and do
  not go through the Platform.
- **The name is creator,** for the key and for the person.
- **Ownership is `ownedBy` and can be passed on.**
- **The universe record gains `metadata`, `protected`, `expiresAtMs` and
  `ownedBy`.** Metadata reuses the map sessions and environments have.
- **Protection is enforced by the server,** not only by the CLI.
- **No maximum lifetime on creator keys.**
- **A Creator sees the Platform admin area, restricted to what they own.**
- **The user list stays open to Creators.** The closed mode is described and
  not built.
- **Universe deletion comes first,** as its own document.
- **Only the owner or a Platform admin deletes a universe.** Universe Admin
  is a role inside a universe and does not include deleting it.
- **A creator key may hand a universe to another owner,** as long as an
  active creator key carries that owner.
- **The limit is one number for the deployment,** set by environment
  variable. A limit per owner waits until it is needed, and then comes with
  a record for the owner that can hold other limits too.
- **`config apply` protects a universe it creates,** unless the document
  says `protected: false`.
- **The limit defaults to 25.**
- **A person who loses the Creator entitlement stays Admin in the universes
  they own** and loses an owner's further authority and their personal keys.
- **A universe a Platform admin creates has no owner** unless they name one.

Still to confirm:

1. **Applications as owners.** Proposed in
   [Applications built on the runtime](#applications-built-on-the-runtime):
   a creator key may assert actors when a deployment key mints it so, and a
   document may state `ownedBy`. The alternative for actor assertion is to
   keep it to deployment keys, which leaves an application that wants its
   users recorded with a key that reaches every universe.
2. **One owner per universe.** Proposed, with several holders per owner, as
   described in [One owner, several holders](#one-owner-several-holders).
3. **The provider flag for new universes.** Nothing to decide until someone
   wants deployment providers in a universe they created themselves.

## Not doing

- New universe roles, or a Platform admin with part of the permissions.
- A Platform endpoint or personal Platform tokens for the CLI. The CLI talks
  only to the runtime.
- A maximum lifetime on keys.
- Several owners on one universe, or a list of universes on a key.
- A limit per key or per owner, and a record for owners, until one is needed.
- Rules driven by metadata. Metadata is read and filtered, never acted on.
- Splitting what a Platform admin can see or do.
- Quotas other than the number of universes.

## Validation and progress

- 2026-11-16: design written from the conversation that led to it and from a
  reading of key resolution, the deployment API, the universe record and the
  Platform's admin routes. No code changed.
