# P181 — Single sign-on and directory membership

**Status:** Proposed, 2026-09-26. Platform only; core is unchanged. Builds
on [Platform: organizations, roles and unshared work](p180-platform-organizations-roles-and-unshared-work.md).
[Deprovisioning and directory updates](p182-deprovisioning-and-directory-updates.md)
follows it and keeps what this slice grants current. First item of
[enterprise authorization](later/pNNN-enterprise-authorization.md).

## Outcome

> People sign in with their company account. Which universes they join, and
> with which role, follows from their groups in the company directory. The
> company's own access-request process decides who is in those groups;
> Lightspeed has no access-request process of its own.

A deployment administrator writes a handful of rules once ("members of this
group are contributors in that universe"). From then on nobody adds members
by hand: a person who was granted the group signs in and finds their
universes.

## Why

Today people are created by a platform admin under Admin → Users with a
password, and added to universes on Members. That does not survive contact
with an enterprise:

- Enterprises sign in through their identity provider and will not issue
  application passwords.
- Access to an internal application is requested and approved in the
  company's own tooling (a ticket, an approval chain, an access review). The
  approval's effect is membership in a directory group; applications read
  that group, they do not keep their own list.
- Offboarding and access reviews work on those groups. An application whose
  members are kept by hand is an exception the security team has to track.

So the question "how does a person join a universe, and with what role" is
answered by the directory, and this slice is the translation.

## The setup this is designed for

A common enterprise arrangement, and the first one this must work with:

- An on-premises directory behind a federation service that speaks OpenID
  Connect. Other providers in scope: Entra ID, Okta, Keycloak, Google
  Workspace; anything that issues standard OIDC tokens.
- An application is registered with the provider through an IT request: a
  client id and secret, redirect URLs, and which claims the tokens carry.
- For each application, the company creates groups that stand for its access
  levels, and people request membership in them. The provider is configured
  to put the application's groups into a `groups` claim; unrelated groups are
  filtered out.
- Some providers put custom claims such as groups only into the access
  token, and return next to nothing from their userinfo endpoint.
- Some internal applications sit behind an authenticating reverse proxy that
  signs people in and forwards their identity in headers. Lightspeed does
  not: see decision 2.

## Decisions

### 1. One provider per deployment, configured by the deployment

The provider is deployment configuration, not data: issuer URL, client id,
client secret, requested scopes, and the name of the groups claim (`groups`
by default). Secrets stay in the environment like the other Platform
settings. Several providers, or one per organization with email-domain
routing, are later work; the first deployments belong to one company.

### 2. The Platform is the OIDC client, with no proxy in front

The Platform is registered with the provider as a confidential client and
runs the authorization code flow with PKCE, through Better Auth's generic
OAuth plugin (already in the installed `better-auth`). Better Auth's
separate SSO package is built for per-organization providers with domain
routing, which one provider per deployment does not need.

An authenticating reverse proxy in front of the Platform is not supported.
It covers only browsers, while API clients, the CLI and MCP clients reach
the Platform with bearer tokens; it would split session renewal between two
components (see P182); and trusting identity headers is safe only if nothing
can reach the Platform around the proxy.

Claims are read from the ID token, verified as part of the flow, not from
the userinfo endpoint, which some providers leave nearly empty. Where a
provider issues custom claims only in the access token, the deployment
either asks for them in the ID token (for some providers a scope, such as
AD FS's `allatclaims`) or reads them from the access token, verified against
the provider's published keys with the configured audience. Either way the
result is one identity:

```text
VerifiedIdentity { issuer, subject, email?, name?, username?, groups[] }
```

### 3. A person is the provider's subject

A person is identified by issuer and subject, never by email or username:
both change on marriage or reorganisation, and email is reused. Email, name
and preferred username are profile fields refreshed at every sign-in.
Account linking stays off: an SSO account never attaches to an existing
local account by email.

### 4. Password sign-in becomes break-glass

With a provider configured, password and GitHub sign-in are off, except for
platform admins created locally (the bootstrap admin), so a deployment whose
provider is down can still be administered. A deployment may switch password
sign-in off entirely.

### 5. Rules map groups to universes and roles

A rule is `group → universe, role`, or `group → platform admin`. A person's
membership in a universe is the highest role among the rules their groups
match; no match, no membership. Platform admin status follows the same way.

```text
research-contributors   → Research, contributor
research-leads          → Research, admin
support-agents          → Support, operator
lightspeed-admins       → platform admin
```

Rules live in the Platform database and are managed by platform admins
under Admin → Directory. Universe admins see which rules feed their
universe, read-only: pointing a group at a universe grants access to its
content, which is a deployment decision. Universes are still created by
platform admins; a group never creates one.

Group values are opaque, exact strings from the configured claim. Providers
send names or identifiers; the rule uses whatever the provider sends. To make
rules writable without guessing, Admin → Users shows the groups each person
presented at their last sign-in. Tokens are never stored or shown.

### 6. Directory memberships are recomputed at sign-in

Each membership records its source: `directory` or `manual`. At every
sign-in and renewal, the Platform recomputes the person's directory memberships from the rules and
their groups: adds, changes the role, or removes. Manual memberships are
never touched by this; they are the exceptions a universe admin adds by hand,
for someone the directory does not cover.

On Members, a directory membership shows the rule that grants it, and its
role and removal are disabled there: change it in the directory. A person
with both keeps the higher role.

Last-admin protection applies to manual changes only. The directory is the
authority for directory memberships; platform admins can always act on any
universe.

### 7. Signed in with no universe

A person whose groups match no rule is signed in and sees a page that says so
and how to request access. The deployment configures that text and link,
since access is requested in the company's own process, not in Lightspeed.

## Configuration

Illustrative names, in the style of the existing Platform settings:

```text
LIGHTSPEED_PLATFORM_OIDC_ISSUER          https://login.example.com
LIGHTSPEED_PLATFORM_OIDC_CLIENT_ID
LIGHTSPEED_PLATFORM_OIDC_CLIENT_SECRET
LIGHTSPEED_PLATFORM_OIDC_SCOPES          openid profile email (default)
LIGHTSPEED_PLATFORM_OIDC_GROUPS_CLAIM    groups (default)
LIGHTSPEED_PLATFORM_OIDC_CLAIMS_TOKEN    id (default) | access   (decision 2)
LIGHTSPEED_PLATFORM_OIDC_AUDIENCE        the access token's audience, when read
LIGHTSPEED_PLATFORM_PASSWORD_SIGN_IN     break-glass (default with OIDC) | off
LIGHTSPEED_PLATFORM_ACCESS_REQUEST_URL   shown with the no-universe page
```

## Persistence

Platform database, migrations edited in place:

- `directory_rules`: id, group, universe (null for platform admin), role,
  created by, created at; unique on group and universe.
- `member.source`: `manual` or `directory`.
- `user.directory_groups` and `user.directory_seen_at`: what the person
  presented at their last sign-in, for Admin → Users and for P182.
- Better Auth's `account` row holds issuer and subject, as it does for
  GitHub today.

## Implementation order

1. OIDC sign-in with the generic OAuth plugin; subject-keyed accounts;
   profile refresh; break-glass password sign-in.
2. Rules, `member.source`, recomputation at sign-in, platform admin from a
   group; Admin → Directory; the no-universe page.
3. Members shows directory memberships as such; Admin → Users shows
   presented groups.
4. `platform/README.md`, and deployment documentation for registering the
   Platform with a provider (redirect URL, claims in the ID or access token,
   groups filter).

## Validation

- Unit: rule evaluation picks the highest role and ignores unmatched groups;
  recomputation adds, changes and removes directory memberships and leaves
  manual ones alone; a person with no match has no universes; a changed email
  keeps the same person.
- Claims from the access token: a token with a bad signature, wrong issuer,
  wrong audience or past expiry is refused; a valid one yields its groups.
- Live: sign in through a local Keycloak with two users and three groups;
  memberships and roles follow the rules; moving a user between groups and
  signing in again moves them; password sign-in is refused for a directory
  user and accepted for the break-glass admin.

## Later

SAML; several providers, or one per organization with domain routing; an
authenticating proxy in front of the Platform, if a deployment requires one;
universe admins managing rules for their own universe; group-driven
universe creation; invitations for people outside the directory.

## Current seams

- [Better Auth setup](../../platform/server/src/auth.ts),
  [Platform settings](../../platform/server/src/env.ts),
  [universes routes](../../platform/server/src/routes/universes.ts),
  [bootstrap](../../platform/server/src/bootstrap.ts).
- [Platform schema](../../platform/db/src/schema/auth.ts),
  [Members page](../../platform/web/src/pages/MembersPage.tsx).
