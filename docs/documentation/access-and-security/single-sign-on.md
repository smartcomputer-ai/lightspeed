# Company sign-in (SSO)

Connect the Platform to your company's OpenID Connect (OIDC) provider so
people sign in with their company accounts. Provider entitlements control who
can use Lightspeed and who is a Platform Admin. Organization membership and
universe roles decide where ordinary users can work. You can manage those
roles locally or map company groups through [declared
access](../deployment/configuration-as-code.md#configure-organizations-and-access).

## Configure the provider and Platform

Register one confidential OIDC application. Obtain the issuer, client ID,
and client secret, and configure the provider to return an email unique to
each user and your two application entitlements. Lightspeed reads the
`groups` claim by default.

On an existing deployment, first review [upgrading to company
sign-in](../deployment/upgrades-and-recovery.md#enable-company-sign-in-on-an-existing-platform)
and designate an [emergency administrator](#emergency-administrator-access).
Company identities are matched by issuer and subject. An email already in
use by a different account needs to be resolved before that person can sign in.

Set these deployment variables and restart the Platform:

```text
LIGHTSPEED_PLATFORM_BASE_URL=https://lightspeed.example.com
LIGHTSPEED_PLATFORM_OIDC_ISSUER=https://identity.example.com
LIGHTSPEED_PLATFORM_OIDC_CLIENT_ID=lightspeed
LIGHTSPEED_PLATFORM_OIDC_CLIENT_SECRET=<deployment secret>
LIGHTSPEED_PLATFORM_OIDC_USER_GROUP=lightspeed-users
LIGHTSPEED_PLATFORM_OIDC_ADMIN_GROUP=lightspeed-platform-admins
```

Use the public HTTPS origin as the base URL. Fetch `GET /api/login-config`
from that origin and use its `providerId` to register this redirect URL with
your provider:

```text
https://lightspeed.example.com/api/auth/callback/<providerId>
```

Use the exact entitlement values issued by your provider. The admin entitlement
alone is enough to sign in as a Platform Admin. Ordinary users need the user
entitlement. For custom discovery, claim names, or entitlements in JWT access
tokens, see the [Platform environment-variable
reference](../reference/environment-variables.md#platform-backend). For a local
walkthrough, see [Company sign-in with local
Keycloak](../development/local-development.md#company-sign-in-with-local-keycloak).

## Sign in and grant access

Visits without a current session start company sign-in automatically. To use
a sign-in button instead, set `LIGHTSPEED_PLATFORM_OIDC_AUTO_SIGN_IN=false`.
The login page remains at `/app/login`. Device authentication for
`lightspeed connect login` uses the same company sign-in and asks the person to
approve the CLI's device code.

Once a colleague signs in, an Organization Admin can add them to a shared
organization. The organization's default universe access and any individual
grants determine what they see. Company group policies can grant that access
at sign-in. For the manual procedure, see [Add people to a shared
organization](people-and-roles.md#add-people-to-a-shared-organization).

## Emergency administrator access

Open `/app/login` directly and choose **Admins** to use the emergency password
form. It is available to designated local Platform Admins. The initial local
administrator is designated during bootstrap. On an existing deployment,
`LIGHTSPEED_PLATFORM_ADMIN_EMAIL` can designate a local Platform Admin who
already has a password. Use a different email from the administrator's
company account. Platform Admins can create additional emergency accounts
under **Admin → Platform → Users**.

Set `LIGHTSPEED_PLATFORM_PASSWORD_SIGN_IN=off` to disable password access
entirely. The default with OIDC is `break-glass`, which allows the emergency
form. The default without OIDC is `local`.

## Session expiry and suspension

With company sign-in configured, browser, CLI, and emergency password sessions
have an absolute lifetime of eight hours by default. You can adjust it with
`LIGHTSPEED_PLATFORM_OIDC_SESSION_MAX_AGE_SECONDS`. When a company session
expires, the user signs in again. Their provider may complete that flow using
its own existing session.

Lightspeed refreshes company admission and group claims at sign-in. A provider
response that removes both entitlements revokes the user's Platform sessions.
If the response removes only the admin entitlement, the next request uses
ordinary permissions. If the provider refuses login without returning claims,
existing Lightspeed sessions expire on their original schedule. Company
personal API keys require the user to renew their admission by signing in
within the same configured window.

To block a person's access immediately, open **Admin → Platform → Users** and
suspend their account. Suspension blocks subsequent requests and fresh sign-ins.
Reinstatement requires a fresh login. To end browser or CLI sessions only,
choose **Sign out all sessions**. Revoke API keys separately if those
credentials should stop working. Running work must be stopped separately.
See [Remove or change
access](people-and-roles.md#remove-or-change-access).

## Verify the deployment

1. Sign in as an ordinary user and as a company administrator. Confirm each
   has the expected access, and that a user with neither entitlement is refused.
2. Add the ordinary user to a shared organization or map their company group.
   Check that they can open the expected universe and that their role persists
   across sign-in.
3. Remove entitlements and test again with an existing company browser session.
   Check how quickly your provider supplies the new claims or refuses login.
   Test local suspension and emergency access as well.

If the callback fails, check the exact redirect URL, issuer and client
credentials, then confirm the email and entitlement claim returned by the
provider. For access-token entitlements, check the audience as well. If sign-in
succeeds and a universe is missing, check membership in its organization and
the user's effective universe role. Platform Admins can review admission
changes under **Admin → Platform → Audit log**.
