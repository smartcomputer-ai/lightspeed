# P181 — Single sign-on and application access

**Status:** Implemented and locally validated, 2026-09-27. Deployment-provider
acceptance remains pending; core is unchanged. Builds on
[Platform: organizations, roles and unshared work](p180-platform-organizations-roles-and-unshared-work.md).
[Session expiry and access revocation](p182-deprovisioning-and-directory-updates.md)
completes the first SSO delivery described in
[enterprise authorization](later/pNNN-enterprise-authorization.md).

## Outcome

> The company grants access to Lightspeed as a user or a platform admin.
> People sign in with their company account. Platform admins and universe
> admins assign their universe memberships and roles inside Lightspeed.

There are two company entitlements: ordinary Lightspeed access and
platform-admin access. They may have separate IT requests and approval
processes. Lightspeed reads the resulting claims; it does not implement
those request processes.

Universe access stays in the existing Platform membership model. Creating
a universe requires no new directory group, provider registration or
company access request. There are no subgroup mappings or group-driven
universe assignments in this slice.

## The setup this is designed for

One company identity provider per deployment, using OIDC. The initial
integration must accommodate an on-premises federation service such as
AD FS; local Keycloak supplies the development test provider.

The application registration supplies a client id and secret, redirect
URLs, any target resource, and the claims for identity and the two access
entitlements. Corporate IT controls who receives those entitlements.

On managed internal devices, the provider may sign the person in after a
brief redirect. On external or unmanaged devices, it may require
credentials and MFA. The Platform uses the same OIDC flow in both cases;
those authentication requirements belong to the provider.

## Decisions

### 1. Company entitlements control application admission

Deployment configuration names the claim containing the application's
access groups or roles, plus the values meaning user and platform admin.
For example:

| Validated entitlement | Platform access |
| --- | --- |
| `lightspeed-users` | Sign in as an ordinary user; universe access follows local memberships |
| `lightspeed-platform-admins` | Sign in as a platform admin, with the existing authority over all universes |
| Both | Platform admin |
| Neither | Refuse Platform access |

The admin entitlement is sufficient by itself. It does not depend on a
second request for ordinary access. Group/role values are exact, opaque
strings, not conventions parsed from their names. The two configured
values must be distinct.

A valid company identity alone is insufficient. The provider should
restrict access to the application, and the Platform validates the
configured admission entitlement before creating a session. Missing,
malformed or incomplete entitlement claims cannot reuse old admission or
admin status. Unrelated groups have no effect.

On every successful provider login, refresh application admission and
platform-admin status before issuing the session. A company-managed
account's platform-admin role cannot be assigned manually in Lightspeed.
P182 defines how lost admission and admin status affect existing sessions.

### 2. The Platform owns OIDC login

The Platform is a confidential OIDC client and uses the authorization
code flow with PKCE:

1. Redirect the browser to the company provider.
2. Exchange the returned code on the server and validate the identity and
   application entitlements.
3. Find or create the Platform user by issuer and subject, refresh profile
   and application access, then create a secure, HttpOnly session cookie.
4. On subsequent requests, check the Platform session, suspension status
   and current permissions before calling core. Do not call the provider
   on each request.
5. At the absolute session limit, require another provider login as P182
   specifies. Existing company SSO may avoid another prompt.

Use Better Auth's [Generic OAuth plugin](https://better-auth.com/docs/plugins/generic-oauth),
configured strictly for OIDC with one deployment-controlled provider.
Upgrade Better Auth from the installed `1.6.29` to
[`1.7.6`](https://github.com/better-auth/better-auth/releases/tag/v1.7.6)
as part of this implementation, including the server and client consumers.
Better Auth handles the protocol; Lightspeed implements admission,
platform-admin status and session revocation. Provider login does not
provision universe memberships.

Configure discovery, `pkce: true`, `requireIdTokenVerification: true` and
keep OIDC nonce binding enabled. A small `getUserInfo` hook must require a
returned ID token and read identity claims after Better Auth verifies it.
`requireIdTokenVerification` requires usable verification metadata, but
does not itself reject a token response without an ID token. Do not accept
UserInfo-only authentication. Use `user.validateUserInfo` to check fresh
admission claims for both new and returning users before session creation;
apply provider-derived access and admin status before issuing the session.

The package evaluation on 2026-09-27 compared both plugins at `1.7.6`.
[`@better-auth/sso`](https://better-auth.com/docs/plugins/sso) supports a
single provider and has a useful transactional `resolveUser` hook, but its
[OIDC flow](https://raw.githubusercontent.com/better-auth/better-auth/v1.7.6/packages/sso/src/routes/sso.ts)
does not implement nonce binding. Generic OAuth implements it and fits
deployment configuration without SSO provider-management tables or routes.
Nonce is optional in the [OIDC code-flow specification](https://openid.net/specs/openid-connect-core-1_0.html#AuthRequest);
it is an explicit requirement here. Reconsider the SSO plugin if SAML or
customer-managed provider connections become requirements.

Local tests with a mocked provider confirmed both plugins reject invalid
signatures and missing entitlements before creating users or sessions.
They also confirmed Generic OAuth's nonce checks and, with the explicit
ID-token hook, rejection of missing ID tokens. These checks establish the
package choice; real-provider integration remains part of delivery.

Validate token signatures, issuer, audience and lifetime, and bind the
callback to the login attempt with state, PKCE and an OIDC nonce. Token
decoding alone is insufficient.

Read identity from the ID token and prefer entitlement claims there too.
AD FS can expose access-token claims there through `allatclaims`. If the
deployment supplies entitlements only in a JWT access token, validate it
against the provider's keys and configured resource audience and bind it
to the same login.
Resource selection in the request is distinct from audience validation;
configure the required resource parameter as well as the expected audience.

Do not depend on userinfo supplying custom claims. Retain the resulting
identity and access state, not provider tokens; do not request offline
access or implement background token refresh. Tokens must not appear in
UI or logs.

Authentication through proxy-supplied identity headers is deferred.
Ordinary reverse proxies for TLS and routing remain compatible.

### 3. A person is the provider's subject

Bind the exact issuer and subject to a stable Platform user id. Email,
name and username are profile fields refreshed at login. Account linking
by email stays off, including linking to an existing local account.
Changing the configured issuer must not take over an existing identity.

The current Better Auth user model requires a unique email. Require the
provider to supply one for this first slice. A collision with another
account fails explicitly; it never causes an implicit link. A missing
display name can fall back to username or email.

### 4. Password login is for explicit emergency admins

With OIDC configured, normal users sign in through the company provider
and GitHub login is off. Password login remains available to explicitly
designated local platform-admin accounts, starting with the bootstrap
admin, so provider outages do not prevent administration. A deployment
may disable password login entirely.

Company-admin entitlement does not enable password login. Enforce this
at password creation/reset and account-linking endpoints as well as at
sign-in. Local emergency accounts remain outside provider-derived access
updates, but have bounded, revocable sessions and can be suspended.
Without OIDC, existing local account behavior remains available.

### 5. Universe membership stays in Lightspeed

Platform admins manage membership in any universe. Universe admins manage
membership only in their own universe. Both use the existing Members page
to assign one of `viewer`, `contributor`, `operator` or `admin`.

These are ordinary local memberships, including for SSO users. Provider
login never adds, removes or changes them. Existing last-admin protection
and universe-creation behavior continue to apply. Every request uses the
current local role, so membership edits affect existing sessions on their
next request.

Admins select from users who have completed an admitted sign-in. A first
login creates the user record without requiring a local password.
Provisioning people before their first sign-in is deferred.

An admitted ordinary user with no memberships sees that they have not
been added to a universe and should contact a universe or platform admin.
Application admission alone grants no universe membership. The existing
platform-admin role still grants access across universes.

### 6. Use the existing administration surfaces

- **Admin → Users:** known users, company-managed versus local identity,
  effective application access, last provider check and suspension controls.
- **Universe → Members:** local memberships and roles.
- **Deployment configuration:** provider registration settings, the two
  admission values, session lifetime and emergency password-login policy.

There is no Directory page, group-mapping editor or per-universe directory
configuration in this delivery.

### 7. CLI and MCP continue to use core keys

Authorized admins can mint core API keys for the core CLI, API clients and
Configurator MCP. Their authority is independent of the creator's
Platform session, company admission and local memberships. P182 makes
their separate revocation explicit.

SSO login for the Platform administration CLI, personal API tokens and
user-authorized MCP access are deferred.

## Configuration

Implemented settings; the [environment reference](../documentation/reference/environment-variables.md#platform-server) gives defaults and validation:

```text
LIGHTSPEED_PLATFORM_OIDC_ISSUER
LIGHTSPEED_PLATFORM_OIDC_DISCOVERY_URL    optional metadata URL override
LIGHTSPEED_PLATFORM_OIDC_CLIENT_ID
LIGHTSPEED_PLATFORM_OIDC_CLIENT_SECRET
LIGHTSPEED_PLATFORM_OIDC_SCOPES          openid profile email (default)
LIGHTSPEED_PLATFORM_OIDC_GROUPS_CLAIM    groups (or the provider's role claim)
LIGHTSPEED_PLATFORM_OIDC_USER_GROUP      ordinary application access
LIGHTSPEED_PLATFORM_OIDC_ADMIN_GROUP     platform-admin access
LIGHTSPEED_PLATFORM_OIDC_CLAIMS_TOKEN    id (default) | access
LIGHTSPEED_PLATFORM_OIDC_RESOURCE        optional requested application resource
LIGHTSPEED_PLATFORM_OIDC_AUDIENCE        required when reading access-token claims
LIGHTSPEED_PLATFORM_PASSWORD_SIGN_IN     break-glass (default with OIDC) | off
LIGHTSPEED_PLATFORM_OIDC_SESSION_MAX_AGE_SECONDS  28800 (default)
LIGHTSPEED_PLATFORM_OIDC_AUTO_SIGN_IN     true (default) | false
```

P182 adds the absolute session limit. Provider settings are deployment
configuration; they are not per-universe data.

## Persistence

Keep the existing Platform users, accounts, sessions and local membership
tables. Add only what is needed for issuer/subject binding, identity source,
effective application admission, provider-derived admin status and the last
successful provider check. Local emergency-admin status must be distinct
from a company-derived admin grant.

No SSO provider table, directory-rule table, membership source model or
group-membership mirror is needed. Generate any Platform migrations from
the schema following
[the Platform migration guide](../documentation/development/changing-contracts.md#platform-migrations).

## Implementation order

1. Upgrade Better Auth to `1.7.6`, align its consumers and verify existing
   authentication and administration behavior.
2. Integrate Generic OAuth and validate the provider contract: discovery,
   client authentication, PKCE, nonce, required ID token, resource/scopes and
   sanitized identity/entitlement claims. Develop against local Keycloak;
   exercise a real test registration as soon as one is available.
3. Implement OIDC admission, stable account recognition, profile updates
   and emergency password login.
4. Connect SSO-created users to existing Users and Members pages; preserve
   local membership editing and add the no-universe state.
5. Complete P182 before delivering SSO. Update deployment documentation and
   `platform/README.md` with user review.

## Validation

- Upgrade: existing local sign-in, bootstrap, session handling and admin and
  universe-member operations still work; GitHub remains available without
  OIDC when configured.
- Admission: user only, admin only, both and neither, for new and returning
  users; invalid/missing claims refuse access before session creation.
  Admin-only access does not require the user entitlement.
- Identity: email changes preserve identity; reused email and issuer changes
  cannot attach another subject to an existing account.
- Protocol: reject missing ID tokens, invalid signatures, issuer, audience,
  expiry or state, and missing/mismatched nonces; PKCE is used.
  UserInfo cannot bypass the required ID token. Verify resource selection
  and both supported entitlement claim sources. Provider tokens are neither
  persisted nor exposed.
- Membership: SSO users can be assigned local roles; universe admins cannot
  edit another universe or grant platform-admin status. Provider login
  preserves local memberships. A normal user starts with no universes.
- Local Keycloak: exercise both entitlements, local membership changes and
  renewal; refuse password login for company users, including company
  admins, while allowing the designated emergency admin.
- Deployment acceptance: validate internal SSO and, when enabled, external
  provider-controlled credentials/MFA; complete P182's revocation checks.

## Later

Group-driven universe access, SCIM, provisioning before first sign-in,
SAML, multiple providers, trusted identity proxies, and personal CLI/MCP
authentication. None is required for this first delivery.

## Delivery record

- Better Auth server and web consumers upgraded to `1.7.6`; Generic OAuth
  integrates one deployment-configured provider with PKCE and nonce.
- Generated Platform migration `0002_company_identity` adds issuer/subject
  identity, explicit emergency admins, admission/check metadata, revocation
  versions and durable access records. Platform schema revision is 3.
- Normal unauthenticated app visits start company sign-in automatically by
  default. Deployment setting `LIGHTSPEED_PLATFORM_OIDC_AUTO_SIGN_IN=false`
  makes sign-in manual, including renewal; `/api/login-config` supplies this
  runtime setting without a frontend rebuild.
  Explicit login, logout and failed sign-in keep a manual page with admin
  access. A per-tab attempt guard prevents repeated automatic redirects.
- Company sign-in, emergency login, profile restrictions, local memberships,
  Users controls and the no-universe state are connected end to end.
- P182's absolute expiry, admission loss, admin demotion and persistent local
  suspension are included. Session versions close suspension/login races.
- The agreed small durable audit trail records access and membership changes,
  emergency sign-ins and Platform key operations, with recent events on Users.
  Comprehensive gateway-operation auditing and retention/export remain later.
- Signed-provider integration tests cover both entitlement sources, protocol
  rejection, identity collision, token non-persistence, local roles, emergency
  password changes, protected auth routes and cookie/bearer expiry. The local
  Keycloak check exercises real code exchange, silent renewal and revocation.
- Fresh and retained-user PostgreSQL migration checks cover installation and
  upgrade. Existing local authentication is covered separately without OIDC.

Actual deployment acceptance still needs the target provider registration,
claims and internal/external login policy. Verify its disabled-account and
entitlement propagation with existing company SSO cookies before committing
to an end-to-end offboarding bound. Local Keycloak results do not establish
that provider's behavior.

## Current seams

- [Better Auth setup](../../platform/server/src/auth.ts),
  [Platform settings](../../platform/server/src/env.ts),
  [universes routes](../../platform/server/src/routes/universes.ts),
  [bootstrap](../../platform/server/src/bootstrap.ts).
- [Platform schema](../../platform/db/src/schema/auth.ts),
  [Members page](../../platform/web/src/pages/MembersPage.tsx).
