# P179 — Core: universes, keys and actors

**Status:** Proposed, 2026-09-25. Fourth slice of
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
data and guards the runtime against its own agents.

Lightspeed is greenfield. Migrations are edited in place and contracts are
reshaped; nothing here keeps a compatibility path for principals, roles,
grants or execution identities in core.

## Outcome

> Core knows universes and actors. A key names the universe a call belongs
> to and the method groups it may call. A key that may assert an actor
> tells core who is asking; core stamps, filters and audits by that actor
> and never interprets it. Bots and sub-agents control only what they made.
> A blob is readable only through a resource it belongs to.

What core stops doing: resolving people, roles or groups; deciding who may
see or control a session; running work as a principal; checking authority
mid-run. What core keeps doing on every path: scoping to the universe,
authenticating keys, guarding its own controllers, admitting content to
resources, and writing one audit row per mutation.

## Why

The three initial use cases are restricted teams. They need who is in the
team, which team a call belongs to, a quiet place to investigate before
sharing, and evidence. The branch built that and more inside core: a
directory, six roles, groups, six capabilities, per-resource policies and
grants, two execution kinds, four decision outcomes and a privileged read.
Each is implemented and verified, and each taxes every future feature.

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
mid-run affects a run any more; one flag later if asked).

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
- Controller context for bots and sub-agents; admitted content roots for
  blob reads; hardening (auth mode required, poll audience, pinned HTTP,
  in-universe workflow references); `operator/*` renamed `deployment/*`.

## Decisions

### 1. The request context is a universe, a key and maybe an actor

Every request resolves to `{ universe, key: { scope, groups, assert_actor,
prefix }, actor: Option<String> }`. There are no roles, principals,
capabilities or effective access. `single` mode resolves to the configured
universe, all groups and no actor. Internal work resolves to a controller
context as today.

The actor is the value of `x-lightspeed-actor`, accepted only from a key
with `assert_actor`; on any other key the header is `invalid_request`. Core
treats it as an opaque string: it stamps it on roots it creates, copies it
to children through the audience root, writes it into audit rows and
approval decisions, and compares it when a list asks for `createdBy`. It
never resolves it, never decides from it.

### 2. Keys carry scope, groups and the actor flag

```sql
CREATE TABLE api_keys (
    key_hash        text PRIMARY KEY CHECK (key_hash ~ '^[0-9a-f]{64}$'),
    key_prefix      text NOT NULL UNIQUE CHECK (key_prefix <> ''),
    universe_id     uuid REFERENCES universes (universe_id) ON DELETE CASCADE,
    groups          text[] NOT NULL CHECK (cardinality(groups) > 0),
    assert_actor    boolean NOT NULL DEFAULT false,
    display_name    text,
    created_by      text NOT NULL,
    created_at_ms   bigint NOT NULL CHECK (created_at_ms >= 0),
    revoked_at_ms   bigint CHECK (revoked_at_ms IS NULL OR revoked_at_ms >= created_at_ms),
    last_used_at_ms bigint
);
```

A null `universe_id` is deployment scope: the key may hold `deployment/*`
groups and addresses a universe with `x-lightspeed-universe`. A universe key
addresses only its universe and rejects the header. A universe key minted
without groups receives every universe-scoped group, so the CLI and
benchmark cases need no configuration. `created_by` is the minting actor or
the minting key's prefix, attribution only.

Keys are immutable except for `display_name` and revocation. Changing a
key's power is revoke and mint, so a running process never gains or loses
rights silently and the audit trail is unambiguous. The group list is
validated in Rust at mint against the fixed enum below; it is not a
subtable, because the key lookup is on the hot path of every request.

The first deployment key is minted by the core CLI at bootstrap
(`server api-keys bootstrap`), replacing `identity development` and
`identity bootstrap`. Every later key is minted through
`deployment/api-keys/create`, which takes `scope`, `groups`, `assertActor`
and `displayName`.

### 3. Method groups are derived from the manifest

A group is a fixed enum in the API crate, one entry per prefix group, and
every method's group is derived from its name. Adding a method never touches
a key.

| Group | Covers |
| --- | --- |
| `session` | `session/*`, `blobs/*` |
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

`initialize` is always allowed. The gateway check is one membership test:
the method's group is in the key's array. Today's capabilities map onto
this: `assert_user` is `assert_actor`; `admit_channel_inbound` is
`channels/inbound`; `lease_credentials` is `auth/lease`;
`discover_channel_accounts` is `deployment/channels`; `manage_identity` and
`read_private_content` disappear.

The manifest keeps `audit: true|false` and gains two fields core does not
evaluate: `target`, the path of the session id in the params or none, and
`role`, the recommended minimum role. Both exist so the Platform, or any
integrator, can generate its gate from the contract; see P180.

### 4. Anchors attribute; they do not authorize

The anchor keeps `created_by` (now an actor string or a bot id),
`controller`, `audience_root` and `bot`, and gains `visibility` on roots. It
loses `run_as_principal_id` and `execution_kind`. It is still reserved before
the record is inserted, so a creation race cannot change who created a root,
and released with the record.

| Root | `created_by` | `visibility` | Changeable |
| --- | --- | --- | --- |
| Session started by a request | the request's actor, or the key prefix | `restricted` unless the request says `universe` | by `session/share`, one way |
| Bot | the request's actor or key prefix | `universe` | no |
| Bot session, delegated child | copied from the root | reads the root | no |
| Profile, workspace, environment, MCP server | the request's actor or key prefix | `universe` | no |

Wire vocabulary stays `restricted | universe`. `session/share` is a
`session`-group method that moves a root session to `universe`; it is
refused on a bot's session, a delegated child and an already shared session,
and it is audited. Widening never revokes anything.

Views carry `access: { visibility, createdBy }` on sessions and bots, and
`createdBy` on profiles, workspaces, environments and MCP servers.
`session/list` and `bots/list` accept `createdBy` and `visibility` filters,
applied in SQL so paging stays correct. The Platform asks for "shared or
mine" by composing them; core applies whatever it was asked.

### 5. What core refuses on its own

1. A missing, revoked or unknown key; a universe key with a universe
   header; a deployment key without one; a method outside the key's groups;
   an actor header on a key without `assert_actor`. These are
   `unauthenticated` or `forbidden` as today.
2. Anything outside the resolved universe: queries, content keys, workflow
   ids, endpoint ids. Unchanged.
3. A bot or sub-agent touching a session it does not control. A bot
   controls its own sessions and admitted children, never siblings; a
   sub-agent's audience root is its parent's. The controller context stays
   exactly as built.
4. A blob read through a resource the blob was not admitted to. The
   admitted content roots written at admission remain; edges are never
   traversed; the uploader may read its own upload.

That is the whole list. `Hidden` is gone: within a universe, a resource that
exists is returned to any key that may call the method. Whether a person may
see it was decided before the request reached core.

### 6. Audit rows carry the actor

One best-effort row per mutating call, as today: method, outcome, universe,
`actor` (text, nullable), `key_prefix`, target, and no policy revision. Rows
are written for denials after authentication and never for unauthenticated
failures. The identity change log and `access_audit_changes` are dropped;
membership and role changes are audited where they happen, in the Platform.
The audited set is today's 41 methods minus `access/policy/put` and
`access/execution/update`, plus `session/share`.

### 7. Nothing runs as anyone

Sessions and bots run under the universe, full stop. The Executor principal,
`universes.execution_principal_id`, `admit_run`'s authority check, the
per-turn check, `authority_revoked` and the attached resources on the
model-call and compaction activity inputs are removed. Attachments are
checked for existence in the universe at admission. Stopping work is
cancelling runs. Parked long polls no longer revalidate on a policy
revision; they end at their own timeout, and the next request is checked by
the Platform.

### 8. Removed from core

Tables `access_principals`, `access_groups`, `access_memberships`,
`access_role_assignments`, `access_capabilities`, `access_policy`,
`access_resource_policies`, `access_resource_grants`,
`access_audit_changes`; columns `universes.execution_principal_id`,
`universes.personal_execution_enabled`, `access_audit_events.privileged`,
`environment_registration_keys.created_by_principal_id` (becomes
`created_by text`). Types `Role`, `Capability`, `EffectiveAccess`,
`RoleDecision`, `ResourcePermission`, `ExecutionKind`, `Execution`,
`ResourcePolicy`, `ResourceGrant`, `PolicyReplacement`, `ResourceAccess`,
`Decision::{Hidden, Privileged}`, `AccessChange`, `RequestContext` rights,
`Revalidation`. Methods `access/*` and `deployment/identity/*`. The identity
CLI. The Members and Access UI in the web app, which P180 rebuilds on the
Platform's own tables.

## Contract sketch

```text
Request context       { universe, key { scope, groups, assert_actor, prefix }, actor? }
Headers               Authorization: Bearer <key>
                      x-lightspeed-universe  (deployment keys only)
                      x-lightspeed-actor     (assert_actor keys only)

deployment/api-keys/create { scope, groups?, assertActor?, displayName? }
                      -> { keyPrefix, secret }               (shown once)
deployment/api-keys/list, revoke                           unchanged shape
session/share { sessionId }                                  audit: true
session/list, bots/list  += createdBy?, visibility? filters
Views                 access: { visibility, createdBy } on sessions and bots;
                      createdBy on profiles, workspaces, environments, MCP servers
Manifest per method   { group (derived), audit, target?, role }  role and target
                      are metadata; core evaluates group and audit only

Removed               access/read, access/subjects, access/policy/read,
                      access/policy/put, access/execution/read,
                      access/execution/update, deployment/identity/self,
                      deployment/identity/directory, deployment/identity/apply
Removed inputs        grants, root, execution on session and bot creation;
                      access on workspace, environment and MCP server creation
                                                                (141 → 133)
```

## Persistence

Edited in place; schema revision stays at the highest migration number, so a
development database needs `./dev.sh reset`.

- `007_identity_access.sql` becomes `007_api_keys.sql`: the table in
  decision 2 and nothing else.
- `010_access_resources.sql`: `access_resources` keeps `created_by text`,
  `controller`, `audience_root_*`, `bot_id`, gains `visibility` (NOT NULL on
  roots, NULL on members, CHECK tied to `audience_root = self`); the policy
  and grant tables are dropped.
- `011_access_audit.sql`: `access_audit_events` replaces
  `authenticated_principal_id`, `acting_principal_id` and `credential` with
  `actor text` and `key_prefix text`, drops `privileged` and
  `policy_revision`; `access_audit_changes` is dropped.
- `005_environments.sql`: registration keys attribute to `created_by text`.

## Implementation order

Deletions first, smallest first, each landing green with regenerated
consumers; then the two additions.

1. [ ] Privileged reads: capability, `Privileged`, marker, audit column,
       `Reader::Privileged`, list tuples, `privileged-read.tsx`, the live
       scenario.
2. [ ] Execution: `ExecutionKind`, anchor and universe columns,
       `resolve_execution`, `access/execution/*`, `ExecutionInput`, the
       Executor principal, `admit_run`'s authority check, the per-turn
       check, `authority_revoked`, attached resources on activity inputs,
       `execution-settings.tsx`, the "Me" choice, "Running as".
3. [ ] Grants and restriction: policy and grant tables, `access/policy/*`,
       `access/read`, `access/subjects`, `AccessGrantInput`, `root`,
       `readable_predicate`, `Hidden`, P178 resource policies and dialogs,
       `access-dialog.tsx`, `shared.tsx`, demo access routes and state,
       `Revalidation`; `visibility` moves onto the anchor; `session/share`
       and the `createdBy`/`visibility` list filters are added.
4. [ ] Directory to keys: `api_keys` reshaped, groups enum and gateway
       check, `assert_actor` and the actor header, request context reduced,
       `created_by` as text everywhere, audit rows with actor and prefix,
       directory tables and types dropped, `deployment/identity/*` and the
       identity CLI removed, `api-keys bootstrap` added, capabilities mapped
       to groups for connectors and plugins, `dev.sh` and the development
       seed adjusted.
5. [ ] Manifest `target` and `role` metadata; contract and consumers
       regenerated; `docs/documentation/deployment/identity-and-access.md`
       and `authentication-and-tenancy.md` rewritten to "core scopes and
       records, the Platform decides".

Step 4 is the one the Platform half waits for.

## Validation

- Unit: group derivation covers every manifest method (a test fails on an
  unmapped method); key checks for scope, header, groups and actor flag;
  `session/share` refusals; anchor reservation race; controller rules for
  bot sessions and delegated children; blob-in-resource; list filters.
- Live (disposable services, serialized): a universe key cannot reach
  another universe or send the universe header; a deployment key without
  `assert_actor` is refused an actor header; a key limited to
  `channels/inbound` cannot read a session; a session created with an actor
  shows that `createdBy`, its sub-agent child inherits it, and
  `session/list { createdBy }` returns exactly those; `session/share` on a
  bot session is refused; a blob admitted in universe A is refused in B; a
  bot cannot cancel a session it does not control; audit rows carry actor
  and key prefix; `single` mode unchanged.
- Measured: key resolution and group check add no statement beyond today's
  key lookup; an ordinary resource read is one anchor lookup or none.

## Later

- A universe-wide kill switch: one flag, admission refusal, and the per-turn
  check removed here.
- Actor propagation to MCP servers, so a tool like the Configurator can act
  as the person who requested the run through the Platform. Needs the run
  requester stamped on the run, which this slice does not keep.
- Everything P177 and P178 built and this slice removes stays designed
  there: per-person grants, restricted resources, personal execution,
  exceptional reads, run-as grants, collections, execution bindings. None
  returns without a user asking.

## Current seams

- [Evaluator](../../crates/access/src/lib.rs),
  [anchor and policy store](../../crates/store-pg/src/resources.rs),
  [identity store](../../crates/store-pg/src/access.rs),
  [API keys](../../crates/store-pg/src/api_keys.rs).
- [Gateway authentication](../../crates/temporal-server/src/gateway/authentication.rs),
  [audit](../../crates/temporal-server/src/gateway/audit.rs),
  [request context](../../crates/temporal-server/src/gateway/principal.rs),
  [shared-service authorization](../../crates/temporal-server/src/gateway/service/authorization.rs),
  [content access](../../crates/temporal-server/src/gateway/service/content_access.rs).
- [Method manifest](../../crates/api/src/rpc.rs),
  [API access types](../../crates/api/src/access.rs),
  [identity CLI](../../crates/temporal-server/src/identity_cli.rs).
