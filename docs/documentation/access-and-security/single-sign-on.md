# Company sign-in (SSO)

Connect Lightspeed to one company OIDC provider so people sign in with their
company account. The provider grants ordinary application access or Platform
admin access. Platform admins and universe admins then assign local universe
memberships using the four roles described in [People and roles](people-and-roles.md).
No directory group is needed for each universe.

Opening Lightspeed without an active session starts company sign-in
automatically by default. The explicit `/app/login` page remains available after logout
and for manual sign-in or emergency admin access. A failed attempt stays on
the login page for an explicit retry; it does not repeatedly redirect.

On managed internal devices, the provider may sign someone in after a brief
redirect. For external access, it may require credentials and MFA. Lightspeed
uses the same flow in both cases; those requirements belong to provider policy.

This guide covers deployment setup, emergency access and acceptance checks.
For a runnable development provider, use
[local Keycloak](../development/local-development.md#company-sign-in-with-local-keycloak).
The [environment-variable reference](../reference/environment-variables.md#platform-server)
is the complete settings reference.

## Configure the provider and Platform

Configure one confidential OIDC client for the deployment. Lightspeed uses an
authorization code, PKCE and nonce, verifies the returned ID token, and keeps
its own browser session. It does not trust proxy identity headers or store
provider tokens.

Register a confidential OIDC application with your provider and obtain its
issuer, client ID and client secret. Arrange for it to return a unique email
and the two application entitlements. The default claim name is `groups`;
set `LIGHTSPEED_PLATFORM_OIDC_GROUPS_CLAIM` if the provider uses another name.

On an existing installation, first check the
[upgrade guidance](../deployment/upgrades-and-recovery.md#enable-company-sign-in-on-an-existing-platform)
and designate an [emergency administrator](#emergency-administrator-access).
Existing local accounts are not automatically converted to company accounts.

Set these deployment variables and restart the Platform:

```text
LIGHTSPEED_PLATFORM_BASE_URL=https://lightspeed.example.com
LIGHTSPEED_PLATFORM_OIDC_ISSUER=https://identity.example.com
LIGHTSPEED_PLATFORM_OIDC_CLIENT_ID=lightspeed
LIGHTSPEED_PLATFORM_OIDC_CLIENT_SECRET=<deployment secret>
LIGHTSPEED_PLATFORM_OIDC_USER_GROUP=lightspeed-users
LIGHTSPEED_PLATFORM_OIDC_ADMIN_GROUP=lightspeed-platform-admins
```

The two group values are exact strings in the `groups` claim by default. The
admin value is sufficient by itself; neither value means no Platform access.
They control application access only. The company can use separate access
requests for ordinary users and administrators.

Retrieve `GET /api/login-config` from the Platform origin and register the
redirect URL `<LIGHTSPEED_PLATFORM_BASE_URL>/api/auth/callback/<providerId>`
using its returned `providerId`. Use the public HTTPS origin in production.
An issuer change produces a different provider ID and requires a new redirect
registration; it cannot take over existing accounts.

## Choose automatic or manual sign-in

`LIGHTSPEED_PLATFORM_OIDC_AUTO_SIGN_IN` defaults to `true`. Normal visits without
an active session and session expiry start company sign-in automatically. Set
it to `false` to show the login page and wait for **Sign in with your company
account**, including after session expiry.

```text
LIGHTSPEED_PLATFORM_OIDC_AUTO_SIGN_IN=false
```

Restart the Platform after changing the setting, then refresh the browser.
The statically built frontend reads `autoSignIn` from `/api/login-config` at
runtime, so this setting requires no frontend rebuild. Client secrets remain
on the server. Both modes retain `/app/login` for manual and emergency access,
and use the same authentication, permissions and session limits.

## Configure identity and entitlement claims

Discovery defaults to `<issuer>/.well-known/openid-configuration`. For a
provider such as AD FS whose metadata lives elsewhere, set
`LIGHTSPEED_PLATFORM_OIDC_DISCOVERY_URL`. Metadata must describe the exact
configured issuer. The default scopes are `openid profile email`. Prefer
application entitlements in the ID token; a provider may require additional
scopes such as `allatclaims` or a target `RESOURCE` parameter. If entitlements
are supplied only in a JWT access token, set `CLAIMS_TOKEN=access` and the
expected resource `AUDIENCE`. Resource selection and audience verification are
separate settings. All these names use the `LIGHTSPEED_PLATFORM_OIDC_` prefix;
see the [complete variable reference](../reference/environment-variables.md#platform-server).
UserInfo alone is insufficient, and offline access is unsupported.

A company account is bound to the exact issuer and subject. Email and display
name are profile fields refreshed at login. An email collision with another
account refuses sign-in; it never links the accounts. The provider must supply
a unique email for this release.

Any partial OIDC configuration fails startup. With complete configuration,
company sign-in replaces ordinary password login and disables GitHub login.
Without OIDC, existing local authentication remains available; configured
GitHub OAuth remains available through the API.

## Emergency administrator access

Open `/app/login` directly and choose **Admins** to open the separate
password form without an automatic company redirect. Only explicitly designated local Platform admins can use it;
a company-admin entitlement never enables a local password.

The bootstrap admin is designated automatically. On an existing installation,
`LIGHTSPEED_PLATFORM_ADMIN_EMAIL` can designate an existing local admin that
already has a password; it cannot promote an ordinary or company account and
does not reset the password. Use a separate email from your company identity.
Platform admins can create additional emergency admins under **Users**. Store
these credentials through your organization's protected recovery process.
Emergency sign-ins are recorded in the access history.

Set `LIGHTSPEED_PLATFORM_PASSWORD_SIGN_IN=off` to disable password sign-in and
password changes, including emergency access. With OIDC, the default is
`break-glass`; without OIDC, it is `local`.

## Session expiry and suspension

With company sign-in configured, Platform sessions have an absolute eight-hour
limit, adjustable with `LIGHTSPEED_PLATFORM_OIDC_SESSION_MAX_AGE_SECONDS`.
Activity does not extend that limit. Renewal sends company users back to the
provider, automatically or after a click according to the sign-in setting.
The provider may sign them in silently using an existing company session.
Emergency password sessions have the same limit. A provider outage does not
extend sessions or stop unexpired sessions from working.

Under **Platform admin → Users**, admins can suspend a person, reinstate them,
or sign out all their sessions. Suspension blocks new sign-ins and subsequent
requests immediately and persists until an admin lifts it. Reinstatement
requires a fresh login; it does not revive old sessions. Users shows identity
source, application access and the last successful provider check.

A valid provider response without either entitlement removes application
access and revokes every Platform session. Loss of only the admin entitlement
removes Platform admin authority on the next request in every session. If the
provider refuses login without returning claims, existing Platform sessions
expire at their original limit. Validate your provider's account-disable and
entitlement-removal behavior, including existing company SSO cookies, before
claiming an offboarding time bound. Directory propagation may add delay.

There is no automatic deactivation for inactivity. Suspension and company
admission changes preserve local memberships, historical records and running
work. Core API keys must be revoked separately.

## Validate the deployment

Before opening access, test the actual provider registration:

1. Sign in with ordinary access, admin access alone, and neither entitlement.
   The first two succeed with the corresponding Platform role; the third is
   refused. An ordinary user starts without universe memberships.
2. Add the ordinary user to a universe on **Members**, change their role, and
   sign in again. The local role applies on the next request and survives
   company sign-in.
3. Verify internal sign-in and any supported external credentials/MFA flow.
   Confirm the public callback uses the same Platform hostname as the browser.
4. With an existing company SSO cookie, remove admin access while retaining
   ordinary access, remove both entitlements, and disable the account at the
   provider. Repeat sign-in and check the session behavior described above.
   Record directory propagation and provider delays before promising a
   revocation bound.
5. Suspend the user in Lightspeed and check that existing sessions and new
   sign-ins are blocked. Reinstate them and confirm a fresh login is required.
6. Check absolute expiry and emergency password access during a provider
   outage. Verify that core API keys still require separate revocation.

Platform admins can inspect **Users → Recent access changes** for company
admission and admin-status changes, emergency sign-ins, suspension, session
revocation, membership edits and Platform key operations. This small durable
trail survives user and session deletion; comprehensive gateway auditing,
retention controls and export remain deferred. See
[what is recorded](overview.md#what-is-recorded-today).

## Resolve sign-in failures

If startup refuses the configuration, check that all required OIDC variables
are supplied and that the two entitlement values differ. Production issuer
and discovery URLs use HTTPS; loopback HTTP is available for development.

For a refused callback, check the exact registered redirect, issuer, client
credentials, ID-token email and configured entitlement claim. For access-token
entitlements, also check the JWT audience and subject. A valid company login
without an application entitlement is still refused. An email already used by
a local account must be resolved explicitly; email-based linking is disabled.

If login succeeds but no universes appear, add the person through **Members**.
Application admission grants no universe membership. A local suspension must
be lifted by a Platform admin even after the provider restores access.
