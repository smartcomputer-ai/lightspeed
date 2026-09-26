# P182 — Deprovisioning and directory updates

**Status:** Proposed, 2026-09-26. Platform only; core is unchanged. Follows
[single sign-on and directory membership](p181-single-sign-on-and-directory-membership.md),
whose rules and directory memberships it keeps current. Second item of
[enterprise authorization](later/pNNN-enterprise-authorization.md).

## Outcome

> When the company disables someone's account or takes them out of a group,
> their Lightspeed access follows within a stated time, without anyone
> touching Lightspeed. Role changes in the directory apply the same way.

P181 recomputes a person's memberships when they sign in. That grants access
correctly but does not take it away from someone who never signs in again:
a Platform session lasts days, and a disabled person's session keeps working
until it expires. Security reviews ask one question here: how long after
we disable someone can they still get in, and who has to remember to act.
This slice makes the answer a number, and makes the answer to the second
part "nobody".

## Why two paths

Providers differ in whether they tell applications about changes:

- **Push (SCIM 2.0).** Entra ID, Okta and other cloud providers push user and
  group changes to an application's SCIM endpoint as they happen.
- **No push.** On-premises federation services and many OIDC providers do
  not speak SCIM. An application learns about a change only when the person
  signs in again or a token is refreshed.

The first deployments may have either, so both are supported, and both feed
the same recomputation P181 defines. SCIM makes changes prompt; the bounded
session is what makes a promise possible without it.

## Decisions

### 1. Sessions are bounded by the provider

With a provider configured, a Platform session lasts at most a set time
(8 hours by default, configurable) and is then renewed only through the
provider. The browser is sent back to the provider, which signs a person who
is still signed in there straight back in without a prompt, and P181's
recomputation runs on the fresh claims. A disabled person cannot renew, and a
person removed from a group renews without the membership.

So without SCIM the promise is: **a person removed in the directory loses
access at the latest one session lifetime later.** Admin → Directory states
the configured value.

A failed renewal says nothing to the Platform: the person is stopped at the
provider and never comes back. Their memberships then linger in the
database, unusable but listed on Members, until decision 3 deactivates them
for inactivity.

API requests signed with a Platform bearer session follow the same bound.

### 2. SCIM, when the provider pushes

The Platform serves a SCIM 2.0 endpoint for Users and Groups, authenticated
by a bearer token a platform admin creates under Admin → Directory and gives
to the provider. The token is shown once and can be revoked.

- **Users.** Create records the person ahead of their first sign-in, keyed
  by the provider's external id, so admins see them before they arrive.
  Update refreshes name, email and username. `active: false` or delete
  deactivates (decision 3).
- **Groups.** Membership changes, renames and deletes recompute the directory
  memberships of every affected person at once, through the same rules.

Better Auth's SCIM package is evaluated first; if it does not fit the
membership model, a small SCIM subset is written against these two
resources only. Either way, SCIM is an input to the recomputation, never a
second membership model.

### 3. What deactivation does

Deactivation happens for one of three reasons, recorded with it:

- **Directory:** SCIM sends `active: false` or deletes the user.
- **Inactive:** the provider has not signed the person in for a set period
  (30 days by default, configurable), which is how departures show up
  without SCIM.
- **Admin:** a platform admin deactivates them under Admin → Users.

Deactivation:

- Ends every Platform session of the person at once.
- Removes all their memberships, manual ones included.
- Blocks sign-in. An inactive person is reactivated by signing in
  successfully, since that is the provider vouching for them; a directory
  deactivation lifts when SCIM marks them active; an admin deactivation
  only when an admin lifts it.
- Keeps the user row. Core's records name the person by user id (who
  created a session or bot, who asked for a run, who decided an approval),
  and those names must still resolve after they leave.

It leaves their work where it is:

- Their private sessions stay private. Admins can already read, share or
  delete them; nothing is shared or deleted automatically. Retention applies
  as for any other session.
- Runs already started run to their end. Every new request goes through the
  Platform and is refused.
- Bots they created keep running: bots are shared and run under the
  universe. Their creator shows as deactivated, so an operator knows whose
  bot to adopt.
- Universe API keys are not personal and stay. When personal keys exist,
  deactivation revokes them.

Reactivation is the same person again (same subject), with memberships
recomputed at their next sign-in.

### 4. Role changes are membership changes

Moving someone from a contributor group to an admin group, or the reverse,
is recomputation like any other: SCIM applies it at once, and without SCIM
it applies at the next sign-in or renewal. A demotion takes effect on the next request after
the recomputation, because the Platform reads the role per request.

### 5. Nothing waits for someone to act

No path requires an administrator in Lightspeed to notice a departure. The
directory is the authority; Admin → Users shows when each person was last
seen by the provider and when they were deactivated, for access reviews.

Every membership change and deactivation belongs in the Platform's audit
trail. That trail is deferred in P180; until it exists these changes are
logged, and this slice must not be the reason it slips: the first customer
with SCIM will ask for it.

## Persistence

Platform database, migrations edited in place:

- `user.directory_external_id` (SCIM), `user.deactivated_at`,
  `user.deactivation_reason` (`directory`, `inactive`, `admin`).
- `scim_tokens`: hash, prefix, created by, created at, revoked at.
- `user.directory_seen_at` from P181 doubles as "last seen".

## Implementation order

1. Bounded sessions with provider renewal; deactivation by an admin and
   for inactivity, with reactivation rules; deactivated creators shown on
   bots.
2. SCIM Users and Groups with bearer tokens; Admin → Directory token
   management.
3. `platform/README.md` and deployment documentation: the session bound,
   what deactivation does, and how to point a provider's SCIM client at the
   Platform.

## Validation

- Unit: deactivation ends sessions, removes all memberships and keeps the
  user row; a SCIM group change recomputes exactly the affected people; a
  revoked SCIM token is refused; reactivation restores directory memberships
  at the next sign-in and not before.
- Live against a local Keycloak: a user removed from a group loses the
  universe after renewal; with a short session bound, a signed-in user who
  is disabled at the provider is out within that bound; with a short
  inactivity period, that user is then deactivated and their memberships
  removed. SCIM through a test client: `active: false` ends a live
  browser session on its next request; a group removal removes the
  membership immediately.

## Later

Revoking personal API keys on deactivation, once they exist; access-review
export; SCIM for Platform-side teams when teams exist; ending runs a
deactivated person started, if a customer asks.

## Current seams

- [Better Auth setup](../../platform/server/src/auth.ts),
  [Platform schema](../../platform/db/src/schema/auth.ts),
  [universes routes](../../platform/server/src/routes/universes.ts),
  [Members page](../../platform/web/src/pages/MembersPage.tsx).
