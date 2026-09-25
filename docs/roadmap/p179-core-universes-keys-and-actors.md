# P179 — Core: universes, keys and actors

**Status:** In progress, 2026-09-25. Fourth slice of
[enterprise authorization](later/pNNN-enterprise-authorization.md) and the
first of two that replace the model built by
[identity and universe authorization](p176-identity-and-universe-authorization.md),
[session access and execution authority](p177-session-access-and-execution-authority.md)
and [resource access](p178-resource-access-and-session-authority.md). This
document is the core half; [the Platform half](p180-platform-organizations-roles-and-unshared-work.md)
follows it. Together they are the version 0.1 access design that the
`permissions` branch merges with.

It reverses one decision of the parent document: core no longer owns the
effective directory. People, roles and groups live in the Platform. Core
knows universes, keys and opaque actors, and enforces only what protects
tenancy and guards the runtime against its own agents.

Lightspeed is greenfield. Migrations are edited in place and contracts are
reshaped; nothing here keeps a compatibility path for principals, roles,
grants, execution identities, resource anchors or content admission.

## Outcome

> Core knows universes and actors. A key names the universe a call belongs
> to and the method groups it may call. A key that may assert an actor
> tells core who is asking; core stamps and filters by that actor and
> never interprets it. Bots and sub-agents control only what they made.

What core stops doing: resolving people, roles or groups; deciding who may
see or control a session; running work as a principal; checking authority
mid-run; recording a separate anchor per resource; tracking which content a
blob digest may be read through. What core keeps doing on every path:
scoping to the universe, authenticating keys, guarding its own controllers,
and recording who asked for each run, steer, cancellation and approval in
the session log.

## Why

The three initial use cases are restricted teams. They need who is in the
team, which team a call belongs to, a quiet place to investigate before
sharing, and evidence. The branch built that and more inside core: a
directory, six roles, groups, six capabilities, per-resource policies and
grants, two execution kinds, four decision outcomes, a privileged read, an
anchor table for six resource kinds, and content admission for every blob
reference. Each is implemented and verified, and each taxes every future
feature.

Identity belongs where sign-in, SSO and provisioning already are. The
Platform ran organizations as universes before this branch; with the
directory in core, every provisioning event has to be translated into a core
identity change through hooks, which is the layer that already broke sign-up
once. Moving the directory back out leaves core with the parts only core can
do, and leaves the Platform as the first integrator of a public contract
rather than a privileged client.

Rejected alternatives: keeping roles in core as a second gate (a table that
must match the Platform's, for a check the Platform already made); a session
to user mapping in the Platform with a user-agnostic core (the Platform would
mirror every session core creates, including bot and sub-agent sessions, and
lists could not page); a universe-wide kill switch (nothing that changes
mid-run affects a run any more; one flag later if asked); a separate anchor
table (its remaining jobs are two columns on sessions and one on bots; the
creation race it guarded is harmless now that a retry returns the existing
session); content admission of blob digests (see decision 6).

## Starting point

Implemented on `permissions`:

- Principals, groups, memberships, universe roles including Executor,
  capabilities, scoped keys bound to principals, identity change log,
  per-call audit rows, identity CLI, `deployment/identity/*`.
- Anchors for sessions, bots, profiles, workspaces, environments and MCP
  servers; root policies and grants; `access/*`; SQL list filtering;
  `Hidden` and `Privileged` decisions; governance without content.
- Execution identity per root, `execution_kind` and `run_as_principal_id`
  on anchors, `personal_execution_enabled` on universes, `admit_run`, the
  per-turn check, `authority_revoked`.
- `read_private_content` with privileged audit rows and list markers.
- Content admission: a blob read through a named resource it was admitted
  to, upload tracking, reference checks on every supplied document.
- Controller context for bots and sub-agents; hardening (auth mode required,
  poll audience, pinned HTTP, in-universe workflow references);
  `operator/*` renamed `deployment/*`.

## Decisions

### 1. The request context is a universe, a key and maybe an actor

Every request resolves to `{ scope, key: { prefix, scope, groups }, actor }`.
There are no roles, principals, capabilities or effective access. `single`
mode resolves to the configured universe with no key and no actor, which
holds every group. Internal work resolves to a controller context.

The actor is the value of `x-lightspeed-actor`, accepted only from a key
with `assert_actor`; on any other key the request is `forbidden`. Core
treats it as an opaque string: it stamps it on sessions and bots it creates,
records it on runs, steering, cancellations and approval decisions, and
compares it when a
list asks for `createdBy` or `visibleTo`. It never resolves it and never
decides from it.

### 2. Keys carry scope, groups and the actor flag

```sql
CREATE TABLE api_keys (
    key_hash        text PRIMARY KEY CHECK (key_hash ~ '^[0-9a-f]{64}$'),
    key_prefix      text NOT NULL UNIQUE CHECK (key_prefix <> ''),
    universe_id     uuid REFERENCES universes (universe_id) ON DELETE CASCADE,
    groups          text[] NOT NULL CHECK (cardinality(groups) > 0),
    assert_actor    boolean NOT NULL DEFAULT false,
    display_name    text,
    created_by      jsonb NOT NULL,
    created_at_ms   bigint NOT NULL CHECK (created_at_ms >= 0),
    revoked_at_ms   bigint CHECK (revoked_at_ms IS NULL OR revoked_at_ms >= created_at_ms),
    last_used_at_ms bigint
);
```

A null `universe_id` is deployment scope: the key may hold `deployment/*`
groups and addresses a universe with `x-lightspeed-universe`. A universe key
addresses only its universe and rejects the header. A key minted without
groups receives every group its scope allows, so the CLI and benchmark cases
need no configuration. `created_by` is an attribution: the minting actor or
key, or the CLI.

Keys are immutable except for revocation. Changing a key's power is revoke
and mint, so a running process never gains or loses rights silently and
every attribution names a key whose power never changed. The group list is validated in Rust at mint; it
is not a subtable, because the key lookup is on the hot path of every
request.

`server api-key bootstrap --universe-id <id>` creates the universe if needed,
revokes earlier keys of the same name, and mints a deployment key with every
group and `assert_actor`, printing it once as JSON. It replaces
`identity development` and `identity bootstrap`. Every later key is minted
through `deployment/api-keys/create` or `server api-key create`.

### 3. Method groups are derived from the method name

A group is a fixed enum in the `api` crate, and every method's group is
derived from its name. Adding a method never touches a key.

| Group | Covers |
| --- | --- |
| `session` | `session/*`, `blobs/read`, `blobs/has` |
| `blobs/put` | `blobs/put` alone, so connectors upload media without reading anything |
| `vfs` | `vfs/*` |
| `profiles` | `profiles/*` |
| `models` | `models/list` |
| `mcp` | `mcp/servers/*` |
| `environments` | `environments/*` including jobs and registration keys |
| `bots` | `bots/*` |
| `channels` | `channels/accounts/*`, `channels/pairings/*`, `channels/conversations/*` |
| `channels/inbound` | `channels/inbound/admit` |
| `auth` | `auth/*` except lease |
| `auth/lease` | `auth/grants/lease` |
| `deployment/universes` | `deployment/universes/*` |
| `deployment/api-keys` | `deployment/api-keys/*` |
| `deployment/environment-providers` | providers, bindings, adopt |
| `deployment/channels` | `deployment/channels/accounts/list` |

`initialize` is always allowed. The gateway check is one membership test,
repeated by every handler so in-process callers meet it too. Today's
capabilities map onto this: `assert_user` is `assert_actor`;
`admit_channel_inbound` is `channels/inbound`; `lease_credentials` is
`auth/lease`; `discover_channel_accounts` is `deployment/channels`;
`manage_identity` and `read_private_content` disappear.

The manifest keeps `access` (the action class internal work is held to, or
`service`/`deployment`), and exports three fields core does not
evaluate: `group`, the recommended minimum `role`, and `target` (`sessionId`
for every `session/` method but the list). They exist so the Platform, or
any integrator, can generate its gate from the contract; see P180.

### 4. Sessions and bots record who created them

The anchor table goes. Its remaining facts become columns on rows that
already exist:

| Row | Column | Meaning |
| --- | --- | --- |
| `sessions` | `created_by jsonb` | The attribution of the request or bot worker that started it; null on delegated children, which read their root's |
| `sessions` | `visibility text` | `restricted` (unshared) or `universe`; set on roots only |
| `sessions` | `bot_id text` | The bot whose worker controls it, if any |
| `bots` | `created_by jsonb` | The attribution of the request that created it; bots are always shared |

A session's root is `origin_root_session_id`, or the session itself; a bot's
session follows its bot, which is shared. Lineage is written only by the
runtime (delegation through the sub-agent service; bot sessions through the
bot worker, whose ids requests cannot create), so controller rules may trust
it.

The workflow creates the session row from its start arguments, which carry
no access facts. The gateway stamps `created_by`, `visibility` and `bot_id`
right after the start, once: `WHERE created_by IS NULL`, so a retry keeps the
original. A row not yet stamped reads as unshared and created by no one,
which fails closed.

Profiles, workspaces, environments and MCP servers carry no creator; nothing
decides by it. Credential grants and OAuth flows record `created_by` the same
way sessions and bots do.

`session/share` moves an unshared root session to `universe`, one way. It is
refused on a bot's session, a delegated child and an already shared session. Views carry `access: { visibility, createdBy }` on
sessions and bots, from the root. `session/list` accepts `createdBy`,
`visibility` and `visibleTo` (shared, or created by that actor: what a
non-administrator sees) filters, applied in SQL on the root so paging stays
correct; `bots/list` accepts `createdBy`. Core applies whatever it is asked.

### 5. What core refuses on its own

1. A missing, revoked or unknown key; a universe key with a universe
   header; a deployment key without one on a universe method; a method
   outside the key's groups; an actor header on a key without
   `assert_actor`.
2. Anything outside the resolved universe: queries, content keys, workflow
   ids, endpoint ids. Unchanged.
3. A bot or sub-agent touching a session it does not control. A bot
   controls its own sessions and their children, never another bot's; a
   sub-agent controls itself and the children it admitted; neither sees
   another root's unshared session. Internal work reads and uses universe
   resources and configures none.

That is the whole list. Within a universe, a resource that exists is
returned to any key that may call the method. Whether a person may see it
was decided before the request reached core.

### 6. A blob digest is a capability within its universe

Blobs are stored per universe: the CAS catalog and object keys are scoped to
it, so no digest reaches across universes. Within a universe, any key that
may call `blobs/read` reads a blob by digest, as before this branch. A
SHA-256 digest cannot be guessed; it is known only to someone who saw the
content or a view that referenced it, and the only such views inside a
universe that are not universe-wide are unshared sessions, which Admins read
anyway. The branch's content admission (a blob readable only through a
resource it was admitted to, upload tracking, reference checks on every
supplied document, a list of reference-bearing fields) defended against a
digest leaking out of band from an unshared session. That is not worth its
thirty call sites and the rule every new reference-bearing field must follow,
so it goes. CAS retention roots for collection stay.

### 7. The session log is the evidence; core keeps no audit table

Run acceptance, steering, cancellation (of an active or a queued run) and
approval decisions record who asked, as an attribution: the asserted actor,
the key acting for itself, the local caller, or the runtime's own work
(`internal`, naming the controller and its cause). The engine records it
and never branches on it; event views expose it as `requestedBy` and
`decidedBy`. Together with `createdBy` on sessions, bots, grants and flows,
that reconstructs a task from its own history.

Core writes no separate audit table. Nothing read one, people act through
the Platform, and the Platform already has to audit sign-in, membership and
role changes; it audits what people do through it, including evidence that
must outlive a deleted session (P180). Refusals of an authenticated key are
a log line naming the method and the key prefix.

### 8. Nothing runs as anyone

Sessions and bots run under the universe. The Executor principal, the
per-turn check, `authority_revoked` and the attached resources on the
model-call and compaction activity inputs are removed. Attachments are
checked for existence in the universe when a configuration is admitted,
through the API or the workflow's own preparation. Stopping work is
cancelling runs. Parked long polls end at their own timeout.

## Contract sketch

```text
Request context       { scope, key { prefix, scope, groups }?, actor? }
Headers               Authorization: Bearer <key>
                      x-lightspeed-universe  (deployment keys, universe methods)
                      x-lightspeed-actor     (assert_actor keys only)

deployment/api-keys/create { scope, groups?, assertActor?, displayName }
                      -> { apiKey, secret }                 (secret shown once)
deployment/api-keys/list { scope? }, revoke { keyPrefix }
session/share { sessionId } -> { access }
session/list          += createdBy?, visibility?, visibleTo?
bots/list             += createdBy?
Views                 access: { visibility, createdBy } on sessions and bots;
                      createdBy replaces principal on auth grants;
                      requestedBy on run accepted, steering, cancellation
                      and cancelled events; decidedBy on approvals
blobs/read, blobs/has by digest within the universe; no resource parameter
Manifest per method   { access, group, role?, target? }  (audit flag removed)

Removed               access/read, access/subjects, access/policy/read,
                      access/policy/put, access/execution/read,
                      access/execution/update, deployment/identity/self,
                      deployment/identity/directory, deployment/identity/apply
Removed inputs        grants, root, execution on session and bot creation;
                      access on bot, workspace, environment and MCP server
                      creation; resource on blob reads
                                                                (141 → 130)
```

## Persistence

Edited in place; the schema revision stays 9 with the release metadata,
but the tables changed, so a development database needs `./dev.sh reset`.

- `001_core.sql`: `sessions` gains `created_by`, `visibility`, `bot_id`;
  `blob_uploads` and the `origin` column of `cas_session_roots` go.
- `004_auth.sql`: grants and flows record `created_by` (an attribution,
  as sessions and bots do) in place of `principal_kind`/`principal_id`.
- `007_api_keys.sql`: the table in decision 2.
- `008_bots.sql`: `bots` gains `created_by`; the `origin` column of
  `cas_bot_event_roots` goes.
- The identity tables, the identity change log, `access_resources` and
  `access_audit_events` go; there is no migration 10.

## Implementation order

1. [x] Access vocabulary (scope, groups, attributions, visibility) in `api`;
       request context and controller rules in the gateway. No separate
       access crate.
2. [x] Keys: `auth` minting, `api_keys` store, gateway authentication,
       deployment key methods, CLI `api-key create|bootstrap|list|revoke`.
3. [x] Removals: directory, grants, policies, execution identities,
       privileged reads, per-turn check, identity CLI.
4. [x] Anchors to columns; `session/share`; list filters.
5. [x] Content admission removed.
6. [x] Audit table and manifest audit flag removed; `requestedBy` on run
       acceptance, steering and cancellation; grants and flows record
       `created_by`.
7. [x] Unit coverage and live-test targets compile; API and workflow
       contracts and generated TypeScript consumers regenerated.
   [x] The only Platform-side changes: the dev launcher bootstraps with
       `api-key bootstrap`, the Configurator MCP forwards
       `x-lightspeed-actor`, and the connectors drop the principal header.
       Everything else in the Platform is P180.
   [x] Local PostgreSQL, Temporal, MinIO and MCP live suites green against
       disposable services. The slow activity-timeout and external provider
       suites were excluded.
8. [ ] Deferred for now: documentation of `identity-and-access.md` and
       `authentication-and-tenancy.md` rewritten to "core scopes and
       records, the Platform decides". Until then both describe the
       removed directory, roles and audit table.

Step 4 is the one the Platform half waits for.

## Validation

- Unit: group derivation covers every manifest method; key checks for
  scope, header, groups and actor flag; `session/share` refusals; controller
  rules for bot sessions and delegated children; list filters.
- Live (disposable services, serialized): a universe key cannot reach
  another universe or send the universe header; a key without `assert_actor`
  is refused an actor header; a key limited to `channels/inbound` cannot read
  a session; a session created with an actor shows that `createdBy`, its
  sub-agent child reads it from the root, and `session/list` filters return
  exactly the matching roots and children; `session/share` on a bot session
  is refused; a bot cannot cancel a session it does not control; run
  events carry `requestedBy`; `single` mode unchanged.

## Later

- A universe-wide kill switch: one flag, admission refusal, and the per-turn
  check removed here.
- Actor propagation to MCP servers, so a tool like the Configurator can act
  as the person who requested the run through the Platform. The requester
  is on the run's accepted event; it is not yet carried to tool calls.
- Everything P177 and P178 built and this slice removes stays designed
  there: per-person grants, restricted resources, personal execution,
  exceptional reads, run-as grants, collections, execution bindings, content
  admission. None returns without a user asking.

## Current seams

- [API keys](../../crates/store-pg/src/api_keys.rs),
  [session store](../../crates/store-pg/src/session.rs).
- [Gateway authentication](../../crates/temporal-server/src/gateway/authentication.rs),
  [request context](../../crates/temporal-server/src/gateway/request_context.rs),
  [shared-service authorization](../../crates/temporal-server/src/gateway/service/authorization.rs),
  [controller rules](../../crates/temporal-server/src/gateway/service/controller.rs).
- [Method manifest](../../crates/api/src/rpc.rs),
  [access vocabulary and method classification](../../crates/api/src/access.rs).
