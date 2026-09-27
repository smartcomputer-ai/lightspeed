# P182 — Session expiry and access revocation

**Status:** Implemented with P181 and locally validated, 2026-09-27.
Deployment-provider revocation acceptance remains pending; core is unchanged. Follows
[single sign-on and application access](p181-single-sign-on-and-directory-membership.md).
Both belong in the first SSO delivery described in
[enterprise authorization](later/pNNN-enterprise-authorization.md).

## Outcome

> Company application access must be checked again within a configured
> interval. Platform admins can suspend a person's Platform access
> immediately. Universe membership remains managed inside Lightspeed.

P181 checks company admission and platform-admin entitlement at login.
This slice bounds how long those checks remain usable and provides local
suspension. Core API keys retain their independent authority.

## Scope

Use the OIDC login again at a fixed session limit. There is no SCIM endpoint,
directory polling, background token refresh or inactivity deactivation.
Someone who has not visited for months can remain in the user list; an
expired session grants no access.

The provider decides whether renewal is a brief redirect or requires
credentials and MFA. Internal and external use have the same Platform
session behavior.

## Decisions

### 1. Sessions have an absolute limit

With OIDC configured, each company user's Platform session lasts at most
8 hours by default, configurable through
`LIGHTSPEED_PLATFORM_OIDC_SESSION_MAX_AGE_SECONDS` (28800 by default).
Measure from the successful provider check. Activity, cookie refresh and
bearer requests must not extend it.

Disable Better Auth's ordinary sliding renewal for these sessions. Check
expiry, application admission and suspension on every authenticated
Platform request, including auth/admin endpoints, without a cookie cache
that could keep revoked access alive.

At expiry, APIs return an authentication failure and the browser starts
company sign-in again by default. With
`LIGHTSPEED_PLATFORM_OIDC_AUTO_SIGN_IN=false`, the login page waits for a click. Successful login refreshes admission and
platform-admin status before issuing a session. Other sessions use that
updated access state on their next request, but keep their original expiry.
Local universe memberships are not recomputed from provider claims.

Failed renewal, provider unavailability or unusable claims cannot create
or extend a session. Unexpired sessions remain usable during a provider
outage. Local emergency accounts retain bounded, revocable password
sessions that do not depend on provider availability.

Signing out revokes the current Platform session. It does not promise
company-wide logout or logout on other devices; suspension revokes every
Platform session of the person.

### 2. Loss of company admission blocks all Platform access

If a validated provider response contains neither application entitlement,
mark company admission absent, remove provider-derived platform-admin
status, revoke all of that user's Platform sessions and refuse login.
Local universe memberships remain recorded, but cannot authorize requests
without application admission.

If the admin entitlement disappears while ordinary user admission remains,
remove platform-admin status. The person's local universe roles then
determine access on the next request, including in other sessions.

The provider may instead reject a disabled or unassigned person without
returning to Lightspeed. In that case existing sessions stop at their
absolute limit. The Platform cannot infer the reason from the person's
absence and does not auto-deactivate or delete them.

Restored company admission permits a fresh login, unless local suspension
still applies. It restores access through any retained local memberships;
it does not resurrect expired or revoked sessions. Admins can independently
remove those memberships in Lightspeed.

### 3. Verify the provider's revocation behavior

While granting Platform access, Lightspeed limits the age of the provider
check. An end-to-end offboarding bound also depends on directory propagation
and the provider enforcing current account status and application
entitlements when an existing company SSO session returns.

Before deployment acceptance, verify disabled accounts, loss of both
entitlements, and loss of admin entitlement while ordinary access remains.
Include browsers with an existing company SSO cookie. State the resulting
bound with any provider delay; do not infer universal removal within eight
hours from the Platform setting alone.

If the provider retains stale access, resolve its reauthentication policy
or agree another revocation mechanism before claiming that bound. Admin →
Users shows the last successful provider check, without treating absence
as deactivation.

### 4. A Platform admin can suspend access immediately

Suspension under Admin → Users is a persistent local block:

- Revoke all the person's Platform sessions and refuse new authenticated
  requests and sign-ins, including password login for a local account.
- Provider entitlements and local membership changes cannot lift the block.
  Only a Platform administrator can do so.
- Keep the user and membership records. Historical attribution still
  resolves; suspension overrides every role.
- Lifting suspension restores no session. A company user must sign in again
  and pass the current admission check; a local user must authenticate with
  their password. Retained local memberships then apply.

Use Better Auth's existing ban/session-revocation support where it meets
these rules, with no automatic expiry or inactivity timer. Ensure login
and suspension cannot race to leave usable access after the block takes
effect.

### 5. Universe membership changes take effect locally

A platform admin can edit membership in any universe; a universe admin can
edit their own. Removal or role changes apply on the next request across
all the affected user's sessions, without waiting for a provider check.
Company login does not restore a locally removed membership.

Existing last-admin protection continues to apply to local membership
changes. Company admission and provider-derived platform-admin status
remain authoritative even when removing them reduces available access.

### 6. Existing work and core keys remain independent

- Private sessions are not automatically shared or deleted; administrators
  retain their existing access.
- Admitted requests, including bounded long-poll reads, may finish. The
  next request is checked again.
- Runs already started and shared bots continue. Revocation does not cancel
  jobs, kill processes or recall credentials delivered to tools.
- Company offboarding, local suspension and membership changes do not
  revoke core API keys. They carry their own scope and method groups.

If a departing person retained a key, an administrator must separately
revoke or rotate it. Explicit key revocation rejects subsequent core
requests; already admitted work follows the same boundary above. Platform
offboarding must not be described as removing all access for key holders.

### 7. Record access changes

Persist changes to company admission and platform-admin status, local
membership edits, suspension/reinstatement, session revocation, emergency
sign-ins and key creation/revocation through Platform routes. Include acting
admin, target, time, outcome and safe details. Records have no foreign keys
and survive user and session deletion; Users displays the latest 100. Never
record provider tokens, passwords or whole request bodies. Comprehensive
gateway-operation auditing, export and retention controls remain deferred.

## Persistence

Use the existing session expiry and administrative ban fields where
possible, plus P181's identity source, effective application access and last
provider-check metadata. Add only the provenance needed to enforce absolute
expiry and distinguish local emergency accounts.

No SCIM tables, group mappings, membership-source model or inactivity
deactivation state are needed. Generate Platform migrations following
[the Platform migration guide](../documentation/development/changing-contracts.md#platform-migrations).

## Implementation order

1. Absolute session expiry and browser reauthentication, covering bearer
   sessions and every protected Platform route.
2. Apply admission loss and admin demotion across existing sessions.
   Preserve local membership records and enforce their current roles.
3. Admin suspension/reinstatement and display of the last provider check.
4. Validate the deployment provider's revocation behavior and agreed
   interval. Update deployment documentation and `platform/README.md`
   with user review, including separate API-key revocation.

## Validation

- With a controlled clock, repeated browser and bearer requests cannot
  extend the provider-check limit. Outages and failed renewal grant no
  extra time; unexpired sessions remain usable.
- Neither entitlement blocks access and revokes all sessions while keeping
  local memberships. Admin demotion leaves only ordinary admission and
  local roles. Restored admission requires fresh login.
- Suspension blocks all devices and protected auth/admin routes on their
  next request, blocks both login methods, and survives concurrent login
  or membership changes. Reinstatement restores no session; idle users
  are never auto-deactivated.
- Local membership edits affect all existing sessions immediately on their
  next request and are preserved through SSO renewal.
- Against local Keycloak and then the deployment provider, test account
  disablement and both entitlement-removal cases with an existing company
  SSO cookie and a short Platform session limit.
- Core keys and admitted work remain independent; explicit key revocation
  still rejects the next core request.

## Later

SCIM, provisioning before first sign-in, personal tokens and Platform-user
CLI/MCP login, access-review export, and stopping already admitted work on
revocation. Group-driven universe access requires a separate design if it
is needed later.

## Delivery record

Absolute expiry is enforced in session creation and every protected request,
with sliding renewal and cookie caching disabled under OIDC. Local suspension
increments a persistent session version as well as deleting sessions, so an
in-flight login cannot leave usable old access after reinstatement. Current
admission and roles govern existing sessions on their next request.

Signed-provider and local Keycloak tests cover admission loss, demotion,
restoration, disabled-provider renewal, outage behavior, emergency access,
local role edits, retained audit events, and expired cookie/bearer sessions.
The deployment provider's propagation and reauthentication policy remain an
acceptance step; no automatic deactivation or core-key cascade was added.

## Current seams

- [Better Auth setup](../../platform/server/src/auth.ts),
  [Platform schema](../../platform/db/src/schema/auth.ts),
  [universes routes](../../platform/server/src/routes/universes.ts),
  [Members page](../../platform/web/src/pages/MembersPage.tsx).
