# P176 to P178 — Access, the first attempt: a retrospective

**Status:** Archived, 2026-09-26. Replaces the three slice documents
(identity and universe authorization, session access and execution
authority, resource access and session authority) and the review of the
first one. What shipped instead is
[core: universes, keys and actors](../p179-core-universes-keys-and-actors.md)
and [Platform: organizations, roles and unshared work](../p180-platform-organizations-roles-and-unshared-work.md).

## The short version

We set out to give three restricted teams a way to say who is in the team,
keep an investigation quiet until it is worth sharing, and show evidence of
who did what. Over three slices we built a complete enterprise authorization
system inside core instead. Each slice was implemented, reviewed, hardened
and live-verified. Then we looked at what the product actually needed and cut
nearly all of it: core went back to universes, keys and opaque actors, and
people and roles went back to the Platform, where sign-in already lived.

## What was built

**Slice one: identity and universe authorization.** Core became the
directory: principals (users and services), groups, memberships, six universe
roles, service capabilities, scoped API keys bound to principals, an identity
change log and a per-call access audit table. The Platform's membership pages
wrote to core through `deployment/identity/*`, and every Better Auth user got
a `corePrincipalId` through a create hook. Every one of the 113 service
handlers authorized against the resolved principal. Parked long polls
revalidated against a policy revision, so a removed member's stream ended
within seconds.

A review found the model sound and the layering expensive: identity was
resolved three to five times per request, about 67 SQL statements for an
ordinary read through the Platform, 111 for an event read, and 22 every
250 ms per parked long poll. Unauthenticated calls each wrote an audit row.
A simplification pass got that to one key read and one rights read per
request, one best-effort audit row per audited call, and flat ownership
reservations.

**Slice two: session access and execution authority.** Sessions got
audiences. Every governed resource got an anchor row (creator, controller,
audience root, execution); roots got a policy row (owner, visibility) and
grant rows (`read`, `write` per principal or group); one evaluator read all
three in one statement. Sessions ran as an execution identity, personal or a
universe service principal, fixed at creation. Runs were admitted with an
authority that was rechecked at every model call and failed the run with
`authority_revoked` when it lapsed. Admins could stop work but not read it;
reading unshared content took a separate `read_private_content` capability,
audited on every use. Blob digests stopped being enough to read a blob: every
reference placed in content was admitted to the resource that held it, and a
read had to name that resource. Collections gave bots and sessions a shared
root; their UI was removed a day later and the collections themselves in the
next slice.

**Slice three: resource access.** Workspaces, environments and MCP servers
got anchors, policies and a `use` grant. Agent identities moved to a system
Executor role. Session attachments were admitted against the execution
identity, and the per-turn check covered attached resources too.

By the end, core had a directory, six roles, groups, six capabilities,
per-resource policies and grants, two execution kinds, four decision
outcomes, a privileged read, an anchor table for six resource kinds, content
admission at about thirty call sites, two audit tables, and 141 API methods.

## Why it was cut

- **Nobody needed most of it yet.** The three initial customers need a team,
  a place to investigate before sharing, and evidence. Per-person grants,
  restricted resources, personal execution, run-as grants and exceptional
  reads answered questions no user had asked.
- **Every feature paid for it.** A new resource kind needed an anchor, a
  policy row, a permission vocabulary and admission rules; a new
  reference-bearing field needed content admission; a new method needed an
  action class, an audit flag and handler checks.
- **Identity was in the wrong place.** Sign-in, SSO, provisioning and
  invitations live in the Platform's ecosystem. With the directory in core,
  every provisioning event had to be translated through hooks, and that layer
  already broke first-time sign-up once (a required `corePrincipalId` made
  Better Auth throw before the hook ran).
- **Administrators reading everything is the simpler, expected rule** for
  restricted teams. Separating administration from content was principled
  and cost a capability, a privileged read path, audit rows and UI markers
  for a case none of the teams had.

## What survived

Kept in core:

- Universes as the tenancy boundary, enforced on every query, content key,
  workflow id and endpoint id.
- Scoped API keys, now carrying a scope, fixed method groups derived from
  the method name, and an `assert_actor` flag; immutable except for
  revocation.
- The actor: an opaque string core stamps on sessions and bots and records
  on runs, steering, cancellations and approvals (`requestedBy`,
  `decidedBy`), and never interprets.
- Controller rules for the runtime's own work: a bot controls its own
  sessions and their children, a sub-agent itself and the children it
  admitted.
- The anchor table's remaining facts as columns: `created_by`, `visibility`
  and `bot_id` on sessions, `created_by` on bots.
- The manifest's `access` class, plus exported `group`, `role` and `target`
  hints the Platform generates its gate from.

Kept from the hardening work: `LIGHTSPEED_AUTH_MODE` required, distinct
`unauthenticated` and `forbidden` error kinds, Better Auth account linking
off, poll-trigger credentials bound to their audience with a private-network
guard, managed-session workflow ids checked against the universe, HMAC
webhooks leasing from the trigger configuration, generic Platform error
bodies, `operator/*` renamed `deployment/*`, and a test that every manifest
method is classified.

Moved to the Platform: organizations as universes, four member roles
(viewer, contributor, operator, admin), the gate that decides method and
target before a request reaches core, and the audit trail that must outlive a
deleted session.

## Lessons

- Start from what the first users must be able to do, and stop there. A
  model that is complete on paper is not the goal.
- Put identity where sign-in is. Core should enforce what only core can:
  tenancy, and the runtime's own agents.
- Measure the cost per request early. The layering problem was visible in
  statement counts long before it was visible in the design.
- A digest that cannot be guessed is a reasonable capability inside a tenant.
  Defending against its out-of-band leak cost more than the leak.

## Not coming back unasked

Per-person grants and reader lists, restricted resources, personal
execution, execution identities and `run_as` grants, the Executor role,
per-turn authority checks, exceptional private-content reads, collections,
execution bindings, resource anchors, and content admission of blob digests.
Their designs are in this repository's history, in the three slice documents
this retrospective replaces. None returns without a user asking for it.
