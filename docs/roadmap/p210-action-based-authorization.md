# P210 — Action-based authorization and default universe access

**Status:** Implemented, 2026-11-17: action-based checks and default
universe access; unit and integration suites pass. Service users are decided
for now: one organization each (see [Service users](#service-users)).
Deployment-wide service users are on hold; the outline below is kept for when
they are needed. **Revisit its open details with the owner before building
it.**

[Platform organizations, authentication and authorization](p209-platform-authentication-and-authorization.md)
defines the organization hierarchy, identities, credentials and roles. This
document changes how those roles are evaluated and adds organization member
access.

## Problem

Authorization decisions compared role names throughout the Platform and the
web: `role === "owner"`, `roleAtLeast(role, "operator")`, `platformAdmin`.
Three problems followed.

- **Roles could not grow.** Giving someone one extra ability (a Viewer who may
  invoke bots, a deployment role that may only create organizations) had no
  representation. Every such need would add another special case.
- **The meaning of a role was spread out.** What an Operator may do was
  scattered across the gate, the routes, the web and the demo backend, each
  with its own copy.
- **Organization members saw nothing by default.** A Member saw only universes
  they were explicitly granted, which works against an organization being the
  natural unit of collaboration.

## Decision

Authorization asks whether a principal holds an **action** at a **scope**:
the deployment, an organization or a universe.

- A **role** is a fixed, named bundle of actions at one scope. All bundles are
  defined in one place, `platform/shared/src/access.ts`, and nowhere else.
- A principal can hold several roles at a scope at once. Its authority there
  is the **union** of their bundles. Grants only add; there are no deny
  rules.
- **Checks never compare role names.** Role names are what people assign and
  what screens display. The invariants about which roles are *assigned* (an
  organization keeps an explicit Admin, a universe keeps an explicit Owner)
  are data rules, not authorization checks, and keep naming roles.
- Administration flows downward. Deployment administration holds the
  Organization Admin bundle in every organization; organization
  administration holds the Owner bundle on every universe in it.
- The server computes the caller's actions and returns them on universe and
  organization views (`actions`, `organizationActions`). Clients decide on
  those; they never derive permissions from a role name.
- API-key restrictions still narrow every request independently.

### Universe actions

Core declares one action on every universe method; a generated table
(`backend/src/routes/method-actions.ts`) maps methods to it. The table is
typed against the core action list in `access.ts`, so a new core action fails
the Platform build until it is placed in a bundle. A test checks every
method's action first appears in the role core recommends for it.

| Role | Adds |
| --- | --- |
| Viewer | `read` |
| Contributor | `createSession`, `controlSession`, `stopSession`, `closeSession`, `shareSession`, `invokeBot`, `useResource` |
| Operator | `configureSession`, `deleteSession`, `closeSharedSessions`, `createProfile`, `manageProfile`, `createBot`, `manageBot`, `configureResource`, `createWorkspace` |
| Owner | `setSessionRetention`, `accessPrivateSessions`, `administer` |

Each role also holds everything the role above it in the table holds. The
three Platform actions express rules inside methods that previously named
roles:

- `accessPrivateSessions`: read and act on sessions their creators have not
  shared, and watch every session.
- `closeSharedSessions`: close a shared session, including one somebody else
  started. Closing an unshared session needs its creator or
  `accessPrivateSessions`; deleting one needs `accessPrivateSessions`.
- `administer`: the universe's grants, settings and lifecycle.

Without `configureSession`, a session starts from the universe defaults or a
saved profile, unshared, without overrides.

### Organization actions

| Role | Holds |
| --- | --- |
| Member | No organization action. Universe access comes from the default universe access and grants. |
| Creator | `createUniverse` |
| Admin | `createUniverse`, `administerUniverses`, `manageMembers`, `manageServiceUsers`, `managePolicy` |

`administerUniverses` holds the Owner bundle on every universe of the
organization, clears deletion protection, and appoints another principal as
a new universe's first Owner. Browsing an organization's principals needs
`manageMembers`, or `administer` on one of its universes.

### Deployment actions

Platform Admin is the one deployment role and holds every deployment action:
`administerOrganizations`, `createOrganization`, `setUniverseQuota`,
`manageUsers`, `readAudit`, `configureDeployment`, `manageUnclaimedUniverses`
and `purgeSessions`. Narrower deployment roles become possible by adding a
bundle; see [Later](#later).

The deployment switch that let any admitted user create shared organizations
is removed. Creating an organization is the `createOrganization` action.
Self-service creation also defeated the per-organization universe quota,
because any user could create unlimited organizations.

## Default universe access

A shared organization sets its **default universe access**: a universe role
that every member *person* holds on every universe in it. It can be any
universe role, `viewer` by default, or none (`null`), which limits members to
their grants. `owner` makes every member an Owner of every universe, including
`accessPrivateSessions` and `administer`; choosing it is the organization's
decision.

- It joins the union with the member's own grant and declared policy; a grant
  above it wins, a grant below it adds nothing.
- Only principals holding an organization role of their own receive it. A
  Platform Admin acting from outside the organization holds Owner through
  administration, not the default.
- **Service users never receive it.** They hold only their grants, so an
  integration's reach stays explicit.
- Personal organizations store none; their owner is their only member and
  administers them.
- The organization is the boundary. Work that some members must not see
  belongs in a separate organization. A per-universe opt-out is not part of
  this design.

Below Owner, members never read another person's unshared session through the
default; that remains `accessPrivateSessions`.

Storage: `organization.base_universe_role` (a universe role or null), default
`viewer`, null for personal organizations (Platform migration `0011`); the API
field is `baseUniverseRole`. Organization Admins change it with
`managePolicy`; the web shows it as **Default universe access** under
organization settings.

## Why not Better Auth's access control

Better Auth's access-control module has the same basic shape (statements of
resource to actions, roles as bundles). It cannot decide who holds which role
here. It knows only a deployment role on the user and a role on a membership
row, with no universe scope and no inheritance between scopes. Its members are
always users, while service users are a separate table. It sees only stored
roles, while declared policy, directory groups and default universe access are computed
per request. It has no resource-dependent rules such as shared versus
unshared sessions. Platform keeps Better Auth for identities and records and
evaluates authorization itself.

## Service users

Decided 2026-11-17: a service user belongs to exactly one organization, holds
one organization role there and universe grants in its universes, and its
name is unique in that organization. It never receives the default universe
access. This covers automation for now; a pipeline's first key is created by
an administrator. Its keys can be generated, declared with a supplied secret,
or given a new secret in place (P209).

### Deployment-wide service users (on hold)

**Not implemented and on hold. Revisit the details below with the owner
before starting.**

Service users stay separate from Better Auth users. A Better Auth user is a
human login (email, password or SSO, sessions, the Platform role, company
admission, a personal organization), and every one of those paths would need
a non-human exception.

The agreed outline is one service-user concept with an owner, rather than
separate organization and deployment kinds:

- A service user is a principal with **0..n organization memberships**, each
  with an organization role, universe grants through those memberships, and,
  where allowed, a deployment role. It never has a personal organization.
- It has exactly one **owner**: an organization or the deployment. The
  owner's administrators create it, mint and revoke its keys, and disable it.
  A key carries all of the service user's memberships, so without a single
  owner, the admins of one organization could act in another through it.
- Granting a service user membership trusts its owner, as granting a GCP
  service account a role in another project trusts that project.

| | Owned by an organization | Owned by the deployment |
| --- | --- | --- |
| Managed by | that organization's Admins | Platform Admins |
| Memberships | its home organization | any organization |
| Deployment role | never | allowed, such as a narrow provisioner |
| Typical use | one team's integration | cross-organization or deployment automation |

Organization-owned service users stay in their home organization at first:
letting organization B add organization A's service user would expose A's
service users to B's administrators.

Expected data changes: `service_users.organization_id` becomes the owner
(null for the deployment); the single `role` moves to a
`service_user_memberships` table (`service_user_id`, `organization_id`,
`role`), because Better Auth's `member` table holds only users; access
resolution reads those memberships the same way it reads people's.

Open questions:

- Key ownership for deployment-owned service users. Better Auth keys belong to
  a user or an organization; the likely answer is a reserved internal
  organization as the key owner, behind the existing server-controlled
  key-to-service-user association. Verify against the plugin first.
- Who may add a deployment-owned service user to an organization: Platform
  Admins only, or also that organization's Admins.
- Which narrow deployment roles to offer first, and whether any may be held by
  people.
- How disabling, deleting and auditing work when memberships span
  organizations.

## Automation and declared universes

How automation should authenticate:

- **Runtime token**: full runtime authority, outside Platform authorization.
  Keep it for infrastructure bootstrap. Universes it creates land among
  unclaimed universes until a Platform Admin adopts them into an organization.
- **Organization service user**: the default for automation that works in
  one organization, as Creator (owns what it creates) or Organization Admin.
- **Deployment-owned service user** (on hold, above): automation that creates
  organizations or spans them, holding a narrow deployment role instead of a
  person's Platform Admin key. Until then, a Platform Admin's key creates
  organizations.

Declared members and directory-group mappings now live in Platform and are
declared in organization documents (`membershipPolicy`) and universe
documents (`access`); see P209.

## Later

- Additional roles are bundle entries in `access.ts`: for example a deployment
  provisioner holding `createOrganization` and `administerOrganizations`.
- Custom, user-defined roles would store bundles as data; the checks already
  ask for actions and would not change.
- Deny rules are not planned.

## Validation and progress

- [x] Shared action catalog, bundles and union resolution; session lifecycle
  rules on actions.
- [x] Generated method-to-action table, typed against core's action list, with
  a consistency test against core's recommended roles.
- [x] Platform gate, routes, deployment pages and Better Auth admin endpoints
  decide on actions.
- [x] Universe and organization views return the caller's actions; the web
  and the demo backend decide on them.
- [x] Default universe access: storage, migration, API, settings UI and
  tests, including service users not receiving it.
- [x] Self-service organization creation switch removed.
- [x] Service users decided as one organization each; names unique there.
- [x] Product manual updated for organizations, effective access, session
  visibility, company sign-in, and API keys. The access section is visible
  by default in the sidebar. The docs build validated routes and examples
  were checked against CLI help and the generated contracts.
- [ ] Deployment-owned service users and a first narrow deployment role (on
  hold).
