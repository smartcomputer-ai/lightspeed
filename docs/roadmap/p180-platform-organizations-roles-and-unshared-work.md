# P180 — Platform: organizations, roles and unshared work

**Status:** In progress, 2026-09-26: steps 1 to 5 done; audit and
invitations deferred, customer documents held, live Platform validation and
the merge open (see [what is left](#what-is-left)). Second half of the
version 0.1 access design; builds on the contract that
[core: universes, keys and actors](p179-core-universes-keys-and-actors.md)
defines. [The retrospective](archive/p176-p178-access-retrospective.md)
tells how the first, much larger attempt got here; later work is in
[enterprise authorization](later/pNNN-enterprise-authorization.md).

The Platform is the first integrator of core's public contract, not a
privileged client. Everything it uses to model people is available to any
holder of a key with `assert_actor`.

## Outcome

> Start an investigation, explore freely, share it with the universe when it
> is useful. A universe is a team. Its members hold one of four roles. The
> Platform decides what a person may do before the request reaches core, and
> tells core who asked.

Sessions start unshared, readable and controllable by their creator, and
readable by universe Admins. Sharing with the universe is one action, one
way. Bots and their conversations are shared. There are no per-person
grants, no reader lists, no execution choice.

## Why

The Platform ran organizations as universes before the `permissions` branch
moved the directory into core. Sign-in, SSO, provisioning and invitations all
live in the Platform's ecosystem, and a directory in core forces every one of
them through a translation hook. P179 removes that directory; this slice
restores the Platform's own, with the four roles the customer documents are
built on, and puts the two decisions that need a person, which method and
which session, in the one place that knows people.

## Starting point

- Better Auth with email and password, GitHub, `bearer`, `disableSignUp`,
  `accountLinking` off; the `corePrincipalId` create hook (goes).
- `universes.ts`, `identity.ts`, `access.ts` and the Members page written
  against `deployment/identity/*` and `access/*` (go).
- The gateway proxy (`routes/gateway.ts`) forwarding to core with the
  Platform's service key and `assert_user`.
- On `main`: `organization()` and `admin()` plugins, an organization-backed
  `universes.ts` with owner/admin/member gating, `bootstrap.ts`, the
  universe switcher and admin area. These come back, adapted.

## Decisions

### 1. Organizations are universes

Restore the `organization()` and `admin()` plugins. One organization per
universe; the Platform `universes` table maps organization id to core
universe id and gateway URL, as it did on `main`. Creating an organization
calls `deployment/universes/create` with the Platform's deployment key;
deleting one calls `deployment/universes/delete` after the last member
confirms. A platform admin, the `admin` plugin role, is the deployment
administrator: creates universes, mints keys, sees every organization.

### 2. Four member roles

Member roles are `viewer`, `contributor`, `operator`, `admin`, replacing the
plugin's default owner/admin/member. The organization creator is `admin`.
Last-admin protection stays a Platform rule. Teams, the plugin's groups, are
not enabled in 0.1; they arrive with directory sync.

### 3. The gateway proxy decides

For every forwarded request, in order:

1. **Membership.** The signed-in user is a member of the organization
   behind the universe in the path, or a platform admin. Otherwise
   `not_found` for the universe.
2. **Method.** The member's role meets the method's minimum role. The table
   is generated from the manifest's `role` metadata into
   `platform/server/src/routes/method-roles.ts`; a test asserts every
   manifest method has an entry, so a new core method fails the Platform
   build until it is classified. Otherwise `forbidden`.
3. **Target.** When the manifest names a `target` for the method and the
   caller is not `admin`, the proxy reads the session view from core and
   requires `visibility = universe` or `createdBy = user id`. Otherwise
   `not_found`. A creation method may name a session that does not exist
   yet, which passes. This is one extra core read per session-scoped request
   from non-admins; lists use filters instead.
4. **Forward** with the deployment key, `x-lightspeed-universe` and
   `x-lightspeed-actor: <user id>`.

Lists: `session/list` is called with `visibleTo: user id` for members, which
returns shared work and their own in one pageable query; admins call it
unfiltered and the Platform marks unshared rows. `bots/list` is unfiltered;
bots are shared.

Blobs are read by digest within the universe (P179 decision 6), so the gate
has nothing to check on `blobs/read` beyond the method's role.

`session/share` is allowed to the creator and to admins.

### 4. Roles by method, the intent

| Role | May |
| --- | --- |
| viewer | read shared work, lists, files, models |
| contributor | viewer, plus start and control sessions and runs, create workspaces, invoke bots, approve tool calls in sessions they control, share their own sessions |
| operator | contributor, plus create and configure profiles, bots, environments, MCP servers, credentials, channels |
| admin | operator, plus members and roles, delete any session, share any session, read unshared work |

The exact per-method table is the generated file; this table is what the
manifest's `role` metadata must reproduce.

### 5. Unshared work in the product

- A new session starts **private**, shown by a lock in its header.
  Creation asks no visibility question; prepared team work created by an
  operator may pass `access: { visibility: "universe" }`.
- The session's ⋯ menu has one action, **Share with universe**, confirmed
  once because it is one way. A shared session shows a people icon in its
  header and in the list. Bots and their sessions are always shared.
- That admins see private work is said where it matters and nowhere else:
  in the new-session dialog, in the note on a private session and its lock,
  and in the admin role's description on Members. Never as a banner, and no
  settings card.
- Members' lists show shared work and their own private work; admins' lists
  show everything, with shared rows marked.
- No "Running as", no reader lists, no execution settings anywhere.
- Files written into a shared workspace or environment follow that
  resource, and content digests are readable across the universe; the
  sharing help says so once.

### 6. Members and audit

The Members page is rebuilt on the organization plugin: list, invite by
email, change role, remove. An `identity_audit` table records member added,
role changed, member removed, organization created and deleted, key minted
and revoked, with the acting user, the subject, timestamps and the
organization. Rows have no foreign keys and outlive the user.

Core keeps no audit table, so this is the only audit trail. It also records
the significant operations people perform through the gateway: session
deletion and sharing, run cancellation, approval decisions, credential,
MCP server and environment changes, and refusals. Each row has the user,
the method, the target identifiers and the outcome, and outlives both the
user and the session. Core's session log carries `requestedBy` and
`decidedBy` for the in-session detail.

### 7. Keys

An admin area mints core keys through `deployment/api-keys/create`,
choosing scope, groups and `assert_actor`, and revokes them. The secret is
shown once. The Configurator installer mints its own universe key with the
groups the install needs (`profiles, mcp, environments, bots, channels, auth,
models` by default) and registers the MCP server unrestricted: whoever can
attach it acts with that key's groups, which is the accepted 0.1 rule.

### 8. Features can be switched off per universe

An admin switches Bots and Channels on or off for the universe on General
settings. Channels need Bots, so switching Bots off takes Channels with it.
The universe stores only the switches that differ from the default; views
return the effective state to every member, because the web hides pages,
navigation and trigger kinds from it. Only admins change them: the universe
update refuses anyone else. Switching off hides and does not enforce; core
knows nothing of it, and existing bots keep running.

### 9. Bootstrap and errors

The first platform admin is created by `bootstrap.ts` as on `main`; the
deployment key comes from `server api-keys bootstrap` and is configured as
`LIGHTSPEED_PLATFORM_API_KEY`. Core's `unauthenticated` and `forbidden`
kinds map to 502 and 500 respectively when they reach the Platform, since
both mean the Platform's own key or gate is wrong; the Platform's own
refusals are 403 and 404 as decided above.

## Contract with core

Uses only: `deployment/universes/*`, `deployment/api-keys/*`, the actor
header, `access: { visibility }` on session starts, `session/share`, the
`visibleTo` session list filter and the `createdBy` bot list filter,
`access` on session and bot views, and the manifest's `group`, `target` and `role` fields. Nothing
Platform-only exists in core; the CLI can do everything the Platform can do
with a key.

## Persistence

Platform database, Drizzle migrations edited in place per the project's
greenfield rule:

- Better Auth `organization`, `member`, `invitation` tables restored;
  `member.role` constrained to the four roles.
- `universes` (organization id, core universe id, gateway URL) restored.
- `user.corePrincipalId` dropped.
- `identity_audit` added (deferred, see step 4).
- `universes.features`: the feature switches an admin changed.

## Implementation order

1. [x] Plugins and tables: restore `organization()` and `admin()`, the
       four roles, `universes.ts`, bootstrap; drop the core-principal hook
       and the identity and access routes.
2. [x] Gateway gate: generated `method-roles.ts` with its coverage test,
       membership and target checks in the proxy, actor header, list
       composition, error mapping.
3. [x] Web on the new server: access dialogs, execution settings,
       privileged-read markers and the groups page removed; permission hints
       from the member's role in the universe on screen; Members, API keys
       (admins only) and platform Users pages on the restored routes and the
       admin plugin; demo routes and fixtures on the same shapes. The
       private work UI: a lock on private sessions and a people icon on
       shared ones, Share with universe in the session menu for the creator
       or an admin with a one-way confirmation, delete offered to the
       creator or an admin, and the explanation where decision 5 puts it.
4. [x] Keys admin area: platform admins list, mint (scope, groups,
       `assert_actor`) and revoke every core key under Admin → API keys;
       universe API keys are for universe admins.
   [ ] Deferred for now: `identity_audit` (decision 6) and member
       invitations. Until then accounts are created under Admin → Users and
       added on Members; the session log and `created_by` columns are the
       only records, and nothing outlives a deleted session. Both return
       before customer data is loaded.
5. [x] `platform/README.md`.
   [x] Per-universe feature switches (decision 8).
   [ ] Held for now: customer documents (below).
   [ ] Merge of `permissions`.

Notes on steps 1 and 2 as built:

- The gate is the member client (`platform/server/src/runtime-client.ts`):
  every core call a route makes for a member passes the role and target
  checks, so a new route cannot forget them. Deployment methods use a
  separate client and are never called on a member's behalf.
- `session/share` and `session/delete` need the creator or an admin;
  other session methods need a shared session or the creator. A member's
  `session/list` is always narrowed with `visibleTo`.
- The organization plugin's endpoints are not served; membership changes
  only through the universe routes, which keep the last admin. The four
  roles are enforced on every Platform write rather than by a database
  check constraint.
- Universe admins mint universe keys (every universe group, no actor) from
  the universe's API keys page. The Configurator installer already mints its
  key with the configuration groups.

## Validation

- Unit (Platform): every manifest method has a role entry; a viewer is
  refused `session/runs/start`; a contributor is refused `profiles/put`; a
  non-admin is `not_found` on another member's unshared session and allowed
  after `session/share`; an admin reads it before; lists compose correctly
  across pages.
- Live (Platform against a disposable core): sign in as two members and an
  admin; the unshared, share, continue and admin-share sequence; a removed
  member gets `not_found` on the next request; a minted key with
  `channels/inbound` only is refused a session read by core; `identity_audit`
  rows for each membership change.
- Demo build unchanged in behaviour.

The unit cases and the demo build pass. The live Platform suite has not been
written; the live core suites cover the core half.

## What is left

1. `identity_audit` and member invitations (step 4), before customer data
   is loaded.
2. The live Platform suite above, without its audit rows until the audit
   trail exists.
3. The customer documents below, and core's user documentation
   (`identity-and-access.md`, `authentication-and-tenancy.md`, P179 step 8).
4. A development database reset, then the Temporal live suites, since
   `001_core.sql` was edited in place.
5. The merge of `permissions`.

## Documents that move with this slice

- Customer solution design: §2.1 and §2.2 (Platform decides, core scopes
  and records); R-04's acceptance check becomes "unshared and shared work;
  Operators stop without reading; Admins read everything"; §2.4 sharing
  paragraphs and Figure 8; §2.4.3 the "administrators can stop work without
  receiving permission to read" and exceptional-access sentences, and "a
  content hash alone does not authorize a download" becomes "within a
  universe, a content digest is a capability"; §3
  Figures 10 and 11 and the boundary table; §6.1 and §6.2 audit streams
  (one Platform audit trail; core's session log attributes runs, steering,
  cancellations and approvals); OP-03
  "private/shared audiences and resource permissions".
- Customer blueprint: PP-03 stands; the Application Architecture paragraph
  "the operator selects the service principal" becomes "the operator
  prepares the universe".
- [Enterprise authorization](later/pNNN-enterprise-authorization.md): done;
  rewritten to hold only later work.

## Later

- Sign-in through the company's identity provider with directory groups
  mapped to universes and roles
  ([P181](p181-single-sign-on-and-directory-membership.md)), and
  deprovisioning and SCIM
  ([P182](p182-deprovisioning-and-directory-updates.md)).
- Human API keys issued by the Platform and proxied to core.
- Unshare; Operator visibility of unshared running work; private bots.
- The Configurator acting as the requester, once core propagates the actor
  to MCP servers.

## Current seams

- [Better Auth setup](../../platform/server/src/auth.ts),
  [gateway proxy](../../platform/server/src/routes/gateway.ts),
  [universes routes](../../platform/server/src/routes/universes.ts),
  [runtime client](../../platform/server/src/runtime-client.ts),
  [bootstrap](../../platform/server/src/bootstrap.ts).
- [Platform schema](../../platform/db/src/schema/auth.ts),
  [Members page](../../platform/web/src/pages/MembersPage.tsx),
  [Configurator installer](../../platform/server/src/routes/setups.ts).
