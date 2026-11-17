# P209 — Platform organizations, authentication and authorization

**Status:** Implemented and validated, 2026-11-17: organizations, grants,
personal and service-user API keys, browser-approved CLI sign-in, action-based
authorization ([P210](p210-action-based-authorization.md)), declarative
organizations and access, and deployment settings. Unit, integration and live
authentication checks pass, including real SDK model calls through Platform.
What is left is listed under [Remaining work](#remaining-work).

Platform uses Better Auth organization records, each containing multiple
universes, and shares one authorization gate across browser sessions, CLI
sessions, and personal and service-user API keys.

Revised 2026-11-17, in order:

- Authorization decides on actions, with roles as bundles, and organizations
  give members a default universe access (P210).
- Organization kinds and organization suspension are removed. Personal
  organizations admit only their owner. Universe limits and organization
  creation are Platform Admin decisions.
- Organizations and universe access are declared in configuration documents;
  service users and their keys can be declared with supplied secrets; any key
  can be given a new secret in place.
- Deployment-wide settings are configurable by Platform Admins, with the
  environment winning; organizations follow the deployment's universe limit
  unless given their own.
- Environment providers are available to every universe unless limited to
  bound universes.

[Runtime access simplification](p208-runtime-access-simplification.md) leaves
trusted component authentication in the runtime and puts caller authorization
in Platform. [The resource HTTP API](p208b-resource-http-api.md) supplies the
shared transport and generated operation catalog. This document defines the
organization hierarchy, identities, credentials and authorization above it.

## Decision

A deployment contains organizations. An organization contains universes. People and
service users receive roles at the organization and universe levels. The same
Platform gate serves browser, CLI, SDK and integration requests.

- A Better Auth **organization is the administrative boundary**, not a universe.
- Every universe belongs to exactly one organization. The organization is its durable
  home; deleting or disabling its creator does not orphan it.
- Organizations are personal or shared. A personal organization belongs to one
  user: it has no other members, membership policy or rename, so its universes
  cannot be shared with other people. A shared organization is everything else.
  Every organization has a deployment-unique **handle** (Better Auth's `slug`
  column; the API, documents and UI call it handle, like a universe's).
- Every check asks for an action; roles are fixed bundles of actions and a
  principal's authority is the union of the roles it holds (P210).
- Platform Admin is rare, deployment-wide authority. Ordinary universe
  creation is an organization capability, not a deployment-administration role.
- Organization roles are **Admin, Creator, Member**. The name Creator replaces
  the working name Builder; it is not merely a `createdBy` attribution.
- Universe roles are **Owner, Operator, Contributor, Viewer**. Owner replaces
  the present universe Admin role. Multiple Owners are supported.
- No separate single-principal universe `ownerId` exists. `createdBy` is
  attribution, never a permanent authorization bypass.
- A credential authenticates an identity. Its restrictions can only narrow
  that identity's current authority.
- Runtime tokens remain separate full-authority infrastructure credentials.

## Organizations and deployment configuration

An organization groups universes, membership, administration, service users and
resource limits. It supplies a future billing boundary without requiring a
billing feature in the first version. It is not Better Auth's OAuth `account`
record: the implementation uses its `organization` entity.

A public signup can automatically receive a personal organization and begin
creating universes without a separate organization-creation wizard. The
person administers that organization. Enterprise deployments may also enable
personal organizations for work, development and exploration.

A deployment can instead provision a single corporate organization, several
organizations for business units or verticals, or a mixture of shared and personal
organizations. Organizations are siblings; nested organizations are not required.
Membership in one organization does not grant access to another.

Deployment policy controls these independently:

- Signup and sign-in admission.
- Personal-organization provisioning. Shared organizations are created by
  Platform Admins.
- Organization membership and organization-role assignment, including company-group
  mappings.
- Universe limits, and defaults or requirements for deletion protection.

Deployment-wide policy that is not infrastructure is a **Platform setting**,
changed by Platform Admins under Admin → Platform → Settings: personal
organizations on or off, and the universe limits of personal and shared
organizations. Each setting has an environment variable. A set variable wins
and locks the setting; otherwise a value set in the UI applies, and otherwise
the built-in default. An organization follows the deployment limit for its
kind until a Platform Admin gives it its own, and can be returned to it.

A local development deployment can provision one shared organization with Creator
access for its developers. A corporate deployment may give ordinary users
Member access and selected groups Creator or Organization Admin access. Personal
organizations remain subject to deployment administration and corporate policy.
The first-release defaults are listed below; deployment configuration can
change them independently. There is no mandatory public-versus-enterprise
mode that fixes all of them.

Organization membership gives a member person the organization's default
universe access on every universe in it: Viewer by default, any universe role,
or none, which limits members to the universes they are granted (P210). Service users hold only
their grants. Organization Admins administer and can access all universes in
their organization; Platform Admins can access all organizations and universes.
Resource privacy must not imply protection from those administrative roles.

## Roles and universe lifecycle

### Deployment

**Platform Admin** administers the deployment and has access across organizations
and universes. This is a distinct platform function, assigned sparingly.
Organization creation or universe creation does not grant it. Any narrowed
credential used by an administrator still applies its restrictions.

### Organization

| Role | Authority |
| --- | --- |
| Admin | Administer organization membership and service users, organization policy and universes; recover universe administration. |
| Creator | Everything a Member has, plus creating universes in this organization and becoming an Owner of each one created. |
| Member | The organization's default universe access on every universe, plus assigned universe roles; cannot create universes merely by belonging to the organization. |

Higher organization authority includes creation. Creator is organization-scoped: a
person may be a Creator in one organization and a Member in another.
A service user has one organization and a role within it.
It does not require a deployment-wide Creator flag or entitlement group.

### Universe

| Role | Authority |
| --- | --- |
| Owner | Manage universe access and settings, appoint other Owners, and delete an unprotected universe. |
| Operator | Configure and operate the universe, without managing membership or deleting the universe. |
| Contributor | Perform ordinary work under the existing input and resource restrictions. |
| Viewer | Read resources permitted by the existing visibility rules. |

Existing session visibility, Contributor restrictions, transcription access
and other resource rules remain explicit policy. The role rename does not
turn an Operator or Viewer into a reader of all private sessions.

Universe grants require membership in the containing organization. Removing a
person from an organization deletes the universe roles they were given in its
universes. When a person leaves through a membership-policy change instead,
which has no removal to hook, their roles stay but give nothing: they are shown
as no longer a member, never count as an Owner, and apply again if the person
returns. A disabled service user's roles behave the same way. Universe Owners
manage universe grants within the organization; that does not itself grant them
organization-membership administration or access to a deployment-wide directory.
Company-group assignments must resolve to the same scoped role model.

### Creating and maintaining Owners

A create request selects an organization. Platform checks organization creation
permission, credential restrictions, organization policy and quotas. It creates
the runtime resource and establishes the Platform linkage and an initial
Owner grant. The creating principal normally receives that grant. A
Platform Admin provisioning on behalf of someone else must establish an
explicit initial Owner too.

At least one explicit Owner is required on creation. Ordinary grant edits
cannot remove or demote the last Owner who is still an organization member. Suspension or organization-membership
revocation still takes effect immediately even if it disables the last
Owner; Organization Admins and Platform Admins can recover administration.

Several principals may be Owners with equal authority. An organization Member
may be made Owner of an existing universe without becoming a Creator.
Losing Creator status prevents new creation but does not revoke existing
Owner grants or their lifecycle authority. Losing an Owner grant removes
that authority even from the original creator.

Universes are not re-parented when a creator leaves. Moving universes between
organizations is deferred. Quotas belong to the organization model; the earlier
per-principal ownership count and its proposed default of 25 are superseded.

### Deletion protection

Reuse the runtime `protected` flag designed in
[universe metadata, protection and expiry](p207-universe-metadata-protection-and-expiry.md).
The runtime mechanism and Platform administrative protection gate are implemented.

- The runtime refuses deletion while `protected` is true, including expiry.
  Even administrative deletion must first clear the flag explicitly.
- An Owner may delete an unprotected universe. Organization Admins and Platform
  Admins may also do so, subject to credential restrictions.
- An Owner may enable protection but cannot clear it. Only an Organization Admin
  for the containing organization, or a Platform Admin, may clear it through
  Platform, subject to organization policy.
- Organizations can default universes to protected or require protection. A
  mandatory policy must be changed before it can permit unprotection.
- Owners may manage expiry where protection and organization policy permit it.
- These rules cover UI, CLI, SDK, configuration documents and every update
  path. `protected: false` in a document must not bypass authorization.

Document-created universes retain the default specified in P207: omitted
protection protects a newly created universe and leaves an existing universe's
setting alone. An explicit false is honored only when policy permits it.

## People, service users and credentials

A **principal** is the identity whose permissions are evaluated. Two cases
must be supported by the authorization design:

- **User:** a person authenticated through the existing Better Auth sign-in
  and session system. A CLI or script can act as that person.
- **Service user:** independently managed automation associated with an
  organization, with organization/universe role assignments and an enabled state.
  Its administrator is not the identity it authenticates as.

Service user is the product term for the non-human principal. It need not
have a human profile, email address, password login or another universe
ownership system. In the first release it is a small Platform record with an
organization, name, enabled state and scoped grants. Its API keys use Better Auth's
organization-owned key configuration, with a server-controlled association
between key ID and service user ID. It is not a fake human user or an OAuth
client registered solely to obtain a long-lived secret.

A service user can be Operator in one universe, Viewer in several universes,
or Creator in its organization and Owner of universes it creates. Disabling a
service user removes its access, but its resources and grants remain in the
organization. Service-user administration and role assignment cannot be
inferred merely from being the human who generated a credential.

Decided 2026-11-17: a service user belongs to exactly one organization, and
its name is unique there. Deployment-wide service users are on hold;
organization service users cover automation for now, and a pipeline's first
key is created by an administrator. See P210 for the outline kept for later.

### Better Auth reuse

The installed Better Auth version is 1.7.7. Reuse sign-in, sessions, SSO and
organizations. Use the API Key plugin for both personal and service-user API
keys; do not build a second key generator, hash store or verifier.

The library supports user- and organization-owned keys, not an arbitrary
principal foreign key. Integrate those supported ownership modes explicitly:

| Credential | Better Auth key owner | Platform authorization subject |
| --- | --- | --- |
| Personal API key | User | That user, with current roles and key restrictions. |
| Service-user API key | Containing organization | The associated service user, with its current roles and key restrictions. |

Platform stores the service user and a server-controlled key-to-service-user
association. Verification checks that the key's configuration and organization
match that service user and organization, and that both remain active. An unlinked
organization key is refused by the shared API. Neither key ownership nor the
human who created it supplies the service user's permissions. Arbitrary key
metadata or caller-supplied identifiers cannot choose the principal.

Use separate plugin configurations for personal and service-user keys. Expose
key creation, association, updates and revocation through reviewed Platform
services. Do not expose unrestricted plugin management endpoints that bypass
service user or credential-restriction policy. Interrupted creation must not
leave an unassociated key with usable Platform authority.

Better Auth handles credential generation, hash storage, verification and
expiry. Platform handles service user identity, scoped roles, resource rules,
key-to-service-user association and any restrictions beyond the library's
permission metadata. Verify once and keep subject plus credential limits in
the request context; do not emulate an unrestricted browser session.

An API key is sent directly with API requests (the SDK can use the standard
Authorization Bearer header). It may be long-lived but still expires and is
revocable. It is not an OAuth access token that an integration must first
obtain from a token endpoint. OAuth client-credentials authentication is not
required in the first release; it may be added later for integrations that
want it. Browser-approved CLI sign-in uses the same user and authorization gate.

References checked 2026-11-16:

- [Better Auth organizations](https://better-auth.com/docs/plugins/organization).
- [API Key ownership, schema and configuration](https://better-auth.com/docs/plugins/api-key/reference).
- [API Key sessions and organization-owned keys](https://better-auth.com/docs/plugins/api-key/advanced).
- [Device authorization](https://better-auth.com/docs/plugins/device-authorization)
  for CLI sign-in. OAuth clients and token exchange are not a
  prerequisite for API-key authentication.

### Personal tokens and narrowing

A personal token acts with its user's current permissions. An unrestricted
token follows those permissions; a restricted token intersects them with its
allowed organizations/universes and API capabilities or operations. Neither token
issuance nor verification may manufacture additional authority.

Restriction semantics are part of the first release:

- Restrictions are ceilings, not independent role grants.
- Membership loss, suspension and role changes are evaluated on requests;
  permissions are not copied into a permanent credential snapshot.
- Explicit read-only or universe restrictions remain in force if the user's
  broader roles grow.
- Creation requires an allowed target organization and creation capability. A
  token limited to existing universes does not implicitly allow creation.
  Organization-scoped credentials can use newly created universes only where the
  principal's new grant and the credential's operation limits both permit it.
- Restricted credentials must not mint or modify broader credentials as a
  way to escape their restrictions.

Tokens have expiry, independent revocation and a recognizable identifier or
prefix for listings. Secrets are not returned in ordinary listings. A key
that should never expire must ask for it explicitly; a deployment maximum
refuses it.

Any personal or service-user key can be given a **new secret in place**: it
keeps its ID, name, restrictions and expiry, the new secret is shown once, and
the old one stops working at once. This serves a lost or leaked secret
without renaming anything that refers to the key. A gapless rotation still
uses a second key.

A service user's keys can also be **declared with a supplied secret**
(`platform/service-users/credentials/put`, by key name), so the secret flows
in from a secret store and never out of Lightspeed. Platform hashes the
supplied secret as Better Auth does and compares it server-side, so a dry run
reports whether it would change without revealing either side. A supplied
secret is `lsa_` and at least 64 characters; two keys cannot share one. Reuse
library storage and lifecycle capabilities; validate integration details for
the API Key plugin. The initial lifetimes and company-access freshness policy
are recorded below. Expiry of a browser session does not by itself invalidate
an otherwise valid API key.

There is no separate universe-key authority model. A credential can be
limited to one universe while still authenticating its user or service user.
The runtime's configured infrastructure tokens remain outside this caller
credential system and must not be handed to users, browsers or agents.

### CLI sign-in and external actors

The `lightspeed` CLI supports direct single-mode runtimes without credentials,
authenticated runtimes with infrastructure tokens, and Platform with API keys
or browser-approved user sessions. Named connections pin deployment URLs and
credentials together; explicit endpoint overrides do not reuse saved secrets.
Deployment origins and `/api/v1` base URLs are accepted, including base paths.

`connect login --url URL` uses Better Auth device authorization with the fixed
client ID `lightspeed-cli`. It prints a verification URL and code, optionally opens the
browser, and polls with the plugin's pending/slow-down/expiry rules. The browser
claims the code, shows the current identity, and requires explicit approval.
Password and company sign-in preserve the pending code. No separate OAuth server,
custom refresh token, or fake human user is introduced.

The plugin issues an opaque Better Auth session token. A server-controlled
session provenance field distinguishes CLI from browser sessions. Only CLI
bearer sessions enter the shared API; browser-only UI/auth routes reject them.
API keys remain separate and cannot manage credentials. Human CLI sessions can
manage credentials just like browser users. Fresh admission and access-version
checks apply at approval, redemption, and subsequent requests. Local CLI
sessions renew through Better Auth within the 30-day inactivity and 90-day
absolute limits; company sessions retain their configured absolute limit.
`connect logout` revokes only the current CLI session and removes its private file.

Connection login and logout live under `lightspeed connect`. Organization,
member, service user and API-key administration (create, revoke, replace,
generate a declared secret) live under `lightspeed platform`. Universe grants
live under `lightspeed universe access`; declared access policy is set with
documents. Universe creation and lifecycle stay under `lightspeed universe`.
The generic `lightspeed api` command remains at the root. `--organization`
overrides the selected organization; saving a different organization clears the previous
universe. The generic `api` command uses generated operation catalogs, with
explicit private-file or stdout handling for newly issued secrets. Platform
native DTOs remain TypeScript-owned. The obsolete `lightspeed-platform` workspace
is removed; deployment-user suspension and company identity administration remain
web features rather than exposing browser-private endpoints to CLI credentials.

Both SDKs compose runtime and Platform-native operation IDs into `client.call`.
Native helpers are generated under `client.platform` and take typed parameter
objects; organization IDs remain explicit. Node's `createClientFromConnection` from
`@lightspeed-ai/sdk/node` and Python's asynchronous
`AsyncLightspeedClient.from_connection` reuse saved CLI connection credentials
and renewal. They expose the selected organization as `organizationId` / `organization_id`,
reread credentials for each request, and do not perform automatic login or
rewrite externally managed secret files. The main TypeScript entry point
remains browser-safe.

A service user's own end-user attribution is optional future behavior. If
introduced, it must be explicitly authorized, namespaced under the verified
service user, and never replace the authenticated service user identity used
for permission checks and auditing. This is not required for initial tokens.

## Platform gate and API boundary

The HTTP adapter foundation from P208b is implemented with browser member
policy; it is not completion of this identity and organization design. `/api/v1`
serves shared resources and `/api/ui` serves remaining browser adapters. Both
must use the same gate and lifecycle services.

Each request resolves an authenticated subject and credential restrictions.
Platform checks active identity, organization membership, organization/universe scope,
operation admission, role, enabled features and the existing resource rules.
It resolves the runtime universe UUID and sets attribution itself. A client
cannot choose a trusted upstream or impersonate another principal by header.
Unknown operations are refused until reviewed.

| Operation | Platform admission |
| --- | --- |
| Create universe | Creator or Admin in the selected organization; Platform Admin may provision into an organization. |
| List universes | Return only accessible universes within the authorized organization/token scope; administrative visibility follows its scope. |
| Manage universe membership and lifecycle | Universe Owner or scoped administrative authority, with protection enforced separately. |
| Clear deletion protection | Containing Organization Admin or Platform Admin, subject to organization policy. |
| Manage organization membership/policy | Organization Admin or Platform Admin. |
| Deployment infrastructure and environment-provider administration | Platform Admin; preserve existing infrastructure boundaries. |
| Machine-only runtime operations | Not exposed through the general Platform API. |

Creation, deletion and native lifecycle handling coordinate both databases
with retry/recovery semantics. Organization selection for creation is Platform
context: design it in the native contract without adding organization identity or
policy to runtime DTOs or duplicating runtime route declarations.

Organization management, service users, grants and token management live under the
Platform-owned `/api/v1/platform` contract. Generate its types and descriptors
once and compose them with admitted runtime operations as specified in P208b.
Do not move Platform identity records into Rust merely to share a generator.
Discovery and CLI status must expose the authenticated identity and relevant
organization context without disclosing credentials or unrestricted inventories.

## Clients, declarations and configuration

The `lightspeed` CLI supports Platform credentials and direct runtime
infrastructure credentials. Direct mode remains available where the
operator permits it, including local development. It has full runtime
authority and does not implement organization authorization.

Organization selection is explicit or resolved from an unambiguous saved context.
Runtime universe paths continue to identify a universe; Platform verifies its
containing organization. SDK helpers and CLI commands share the same HTTP contract.
The separate `lightspeed-platform` CLI is consolidated into `lightspeed`.

Declared members and company-group mappings belong in Platform, and the
runtime no longer stores access policy.

Configuration application runs as the authenticated user or service user,
with normal role and token checks. Organization selection, initial Owner grants,
quotas and protection policy apply to declared universe creation too.
Creator authority permits creating universes and grants the creator initial
Owner access; it does not grant access to other existing universes. A key
limited to existing universe UUIDs cannot create universes, regardless of its
principal's organization role.

Platform configuration is declared in the same documents, through Platform
only (implemented 2026-11-17):

- `kind: organization`, found by handle: name, default universe access,
  protection policy, universe limit (Platform Admin), `membershipPolicy`, and
  service users by name with their declared keys.
- A universe document's `organization` (the handle of the organization that
  holds it; universes never move) and `access`: the universe's declared policy
  plus service-user grants by name.
- `apply` runs organization documents first and resolves organizations and
  service users by handle and name when each step runs, so one run creates an
  organization, its service users, a universe in it and their grants.
  Nothing is deleted. `config export --kind organization` writes the document;
  keys are never exported.

People are given roles one at a time in the web app; documents declare groups
and service users, whose names are the same in every deployment.

Environment providers are deployment infrastructure. A provider is available
to every universe unless a Platform Admin limits it to bound universes; only
Platform Admins register providers, change availability and bind universes.
Ordinary organization documents need no `environmentProviders` section. A
binding has no on/off state: unbinding cuts a universe off, including its
open environments on that provider, until it is bound again.

The Configurator's intended access is a service user with Operator rights in
its target universe and credentials limited to configuration operations. It
must not receive a broad runtime token as the replacement for a restricted
Platform credential. Connector retirement/reintroduction is separate from
this organization model and must not silently determine service user permissions.

## Storage and rollout

The former unique organization-to-universe mapping is replaced with one
organization-to-many-universes. Better Auth organization membership stores the
organization relationship; explicit universe grants store universe roles. Reuse
library identity records where appropriate, with service user storage chosen
after the credential integration decision.

Platform stores organization policy and linkage, universe organization ID and
attribution, scoped grants, and credential restrictions not already modeled
by the chosen library. There is no single-principal universe owner column.
Runtime stores the resource protection/expiry facts, never organization roles.

Existing installations will be migrated separately with an agent, as directed
by the user. Do not build a compatibility layer or automatic organization-grouping
migration for them as part of this implementation. Required database schema
migrations still belong to the change. The separate operational migration
must preserve intended universe grants and privacy, and must not promote old
universe administrators into organization-wide administrators by inference.

Implement in these slices:

1. **Organization hierarchy and grants:** database mapping, configurable organization
   provisioning, organization roles, universe Owner grants, shared authorization
   context and required schema migrations. Existing-installation data mapping
   is handled separately.
2. **Creation and protected lifecycle:** organization-selected creation, quotas,
   initial Owner assignment, runtime protection/expiry from P207 and Platform
   unprotection policy across all callers.
3. **Personal tokens:** Better Auth API Key integration, narrowing,
   revocation, management and CLI/SDK access as the user.
4. **Service users:** organization-owned Better Auth API keys associated with
   service user identities, scoped grants, credential lifecycle and the
   restricted Configurator path.
5. **Native access declarations and client consolidation:** move remaining
   runtime policy fields to Platform and expose the generated native API.
6. **CLI browser sign-in:** use the same subject and authorization path.

## Validation requirements

These are the acceptance requirements. Executed checks and their scope are
recorded under Validation and progress; the previous test hold was lifted
on 2026-11-17.

- Organization membership gives a person exactly the organization's default
  universe access (P210), which may be none; service users get only their
  grants. Creator can create; Member cannot create without that role.
- A user may have different roles in different organizations and
  universes. UUIDs, handles, list queries and tokens cannot cross the boundary.
- Creation grants an initial Owner and enforces organization quotas atomically
  under concurrency; retries do not duplicate resources or grants.
- Multiple Owners work; the original creator has no hidden authority.
  Last-Owner edits are guarded, but suspension still revokes access.
- Losing Creator status leaves existing Owner grants intact. Losing organization
  membership removes effective access to all its universes.
- Owners can delete unprotected universes but cannot clear protection via
  any route, document or configuration update. Organization-required protection
  cannot be disabled while that policy remains in force.
- Protected universes cannot expire or be deleted even on direct runtime
  access; authorized deletion first clears protection explicitly.
- Browser, personal-token and service user requests share role/resource
  enforcement. Narrowed credentials cannot mint broader credentials.
- Role changes, local suspension, expiry and revocation take effect on the
  next admission; IdP changes obey the agreed freshness policy. Long-lived
  streams have bounded reauthorization behavior.
- Service-user credentials do not silently become the administrator's user
  credentials. Audit attribution distinguishes subject and credential.
- Existing enterprise memberships, directory mappings and resource privacy
  survive the organization migration without administrative privilege expansion.

## First-release defaults

The user delegated selection of sensible defaults, 2026-11-16. These are
initial product defaults, configurable by the deployment rather than new
hard-coded role distinctions.

| Setting | Initial default |
| --- | --- |
| Public self-signup | Disabled unless explicitly enabled; preserve configured company admission. |
| Personal organizations | Provision one for each admitted user; deployments may disable this. The user is its Organization Admin. |
| Shared-organization creation | Platform Admins only. |
| Default universe access | Viewer on every universe of a shared organization; organizations may set any universe role or none. |
| New shared-organization members | Member; Creator and Organization Admin require explicit grants or configured company-group mappings. |
| Local development | One shared development organization; admitted developers are Creators. |
| Service-user management | Organization Admins create/manage service users and their keys and grant their scoped roles. A personal organization's owner is its Admin, but its administration pages are hidden from them (see below); its service users are reached through the API, the CLI or a Platform Admin. |
| Universe limit | 25 universes per personal organization and 100 per shared organization, as Platform settings. An organization follows these unless a Platform Admin gives it its own limit. |
| Interactive/API-created protection | Off for personal organizations; on by default for shared organizations. The seeded local development organization is off. Organizations may change the default or require protection. |
| Document-created protection | On when omitted; existing universes retain their current value when omitted, as specified in P207. |
| Personal and service-user API-key expiry | 365 days by default. No expiry is an explicit choice, refused when a deployment sets a maximum lifetime; there is no maximum by default. Keys are revocable at any time. |
| API-key rotation | For no gap, issue another key, move consumers, then revoke the old one. A new secret in place serves a lost or leaked secret and invalidates the old one at once. Creating a key never invalidates another. |
| Browser and CLI login | A 30-day inactivity window with a 90-day absolute login lifetime and renewal during use. Provider/deployment policy may require earlier authentication; company sessions retain their bounded absolute lifetime. |
| Local access changes | Check local revocation, suspension and effective grants on admission; no artificial delay is required. |
| Company access refresh | Target a background refresh at least hourly, independent of API-key or login lifetime. See the integration requirement below. |

The shared-organization protection default is not mandatory protection: an Organization
Admin can configure a development organization for disposable universes. Mandatory
protection remains a distinct organization policy. Quotas count admitted creations
and resources still consuming their allocation; deletion does not release a
slot before the resource is actually removed. Service users consume their
containing organization's quota, not a separate creator quota.

### Company identity freshness

Long credential lifetime and timely access revocation are separate concerns.
A one-year API key does not preserve stale roles for a year. Refresh company
admission and group assignments in the background, with an hourly freshness
target, while checking locally recorded changes on request admission.

The current integration stores admission claims at sign-in and deliberately
discards provider access/refresh tokens. It cannot meet the background target
just by increasing session duration or expiring a cache. Generic OIDC login
alone is not a directory-change feed.

The implementation must establish an actual supported freshness source for
configured company providers: directory synchronization/events, an appropriate
provider API, or refresh that yields authoritative current admission/group
information. Ordinary JWT validation alone is insufficient. If a source cannot
refresh, use a documented bounded reauthentication/staleness policy rather
than silently claim hourly revocation. Do not enable longer company-session
windows without that policy; retain the current bounded sign-in policy until
the freshness integration is ready. A successful routine refresh must not
require another interactive login. Disabling a service user is local Platform
state and does not depend on company identity refresh.

## Remaining work

Open, in rough order of value:

- **Organization templates.** Agreed outline, not built: a new universe starts
  as a *copy* of a template universe its organization names (a deployment-wide
  template serves personal organizations). Copies never link, so the runtime
  gains no hierarchy. Secrets move by a runtime copy operation that never
  returns them; an explicit "update from template" re-applies entries with
  `apply` semantics. Templates should refuse or loudly flag retrievable
  credentials, which any new universe's Owner could bind into an environment.
  Settle the copy operation's scope before building.
- **More Platform settings.** API key default and maximum expiry, browser
  session lifetimes and workspace upload limits follow the same
  environment-wins pattern as the universe limits.
- **Existing-installation data migration,** performed separately with an
  agent, as directed.
- **Authoritative company identity refresh.** The implementation keeps the
  bounded company admission window and does not promise hourly provider
  polling; it needs an actual freshness source.
- **Deployment-wide service users,** on hold (P210). Until then a pipeline's
  first key is created by a Platform Admin.
- **Request auditing** beyond the identity audit, and external
  application-user attribution for service users.
- **Personal organizations' service users** have no UI for their owner, since
  personal organizations are hidden from their owner's Admin. Add a place only
  if people need it.

Not planned without a new decision: deleting entries through `apply`
(no prune), bots in documents (on hold), a per-universe deny for providers
available to all universes.

## Outside the first version

- Nested organizations, cross-organization universe transfer/sharing and arbitrary
  custom-role or policy-language editors.
- An authorization model in the runtime or a second gate for the browser.
- Single-principal universe ownership and privileged `createdBy` behavior.
- Automatic service user impersonation of human users.
- OAuth client-credentials issuance as a prerequisite for integrations that
  can use API keys; service users initially use the API Key plugin.
- Compatibility/data-migration tooling for existing installations in this
  implementation; operational migration is handled separately.

## Organization-scoped administration

Product terminology is **Organization** and **Service user** throughout UI,
native API, CLI, SDKs and database identity records. Better Auth OAuth account
records and runtime provider/channel accounts remain distinct concepts.

**Universe switcher.** All accessible active universes, grouped by
organization; group headings appear only when universes span several
organizations, and search only past ten universes. **New universe** shows
while the caller can create somewhere and is enabled only while one of those
organizations is under its limit. The New universe dialog asks for a name; the
organization is a choice only when there are several, the handle follows the
name (changeable), and the creator becomes the Owner. A Platform Admin outside
the organization names a member as Owner.

**A universe's own settings.** Universe administration lives in the universe:
Settings → General (name, handle, features, appearance, protection, and a
Danger zone with archive, delete and, for Platform Admins, deleted sessions)
and Settings → Members (a summary of who gets in through the organization,
plus roles given here). The former Admin universe pages redirect there.

**Admin** uses `/admin/organizations/:organizationId/...` with Universes (an
inventory: icon, handle, role, status; rows open details with Open, Settings
and Members), Members (people and service users; a service user's details hold
its role and keys), API keys (every key that can reach the organization,
read-only except service-user revocation) and Settings (name, universe limit,
default universe access, protection). A personal organization is not listed
for its owner; Platform Admins still see it. Admin appears in the account
menu only for people with something to administer.

**Platform** pages form a separately labeled section: All Organizations,
Users, Channels, Audit log, Environment providers, Unclaimed Universes and
Settings. They do not inherit the selected organization's scope.

**Settings** (account menu) is one page: Account (profile, sign-in, API
keys, kept on the server) and Appearance (kept in this browser). Personal keys
default to a single organization, with an explicit broader choice.
Service-user keys stay limited to their one organization. Restrictions always
intersect current principal permissions.

## Validation and progress

- 2026-11-17 (later): the follow-through recorded in the status above.
  Organization documents, universe `organization`/`access`, declared
  service-user keys and in-place secret replacement; service-user names unique
  per organization (Platform migration 0013); organization handle; Platform
  settings with environment precedence and organization limit inheritance
  (migration 0012); universe roles removed with organization membership and
  inactive roles marked; `canCreateUniverse` on organization views; universe
  administration moved into the universe; Members, API keys and Settings
  pages; one Settings page for the account; environment provider availability
  (runtime migration 029). Full backend, web, CLI, runtime, store and API
  suites, Clippy, migration upgrade checks and documentation checks passed;
  the PostgreSQL live checks for providers passed. Temporal and Incus live
  suites were not rerun.

- 2026-11-17: replaced the Platform-wide universe inventory with **Unclaimed
  Universes**, for adoption of runtime-only universes. Adoption suggests the
  runtime display name or handle and preserves user edits. Claimed universes
  remain in their organization; deployment-admin reconciliation, handle sync,
  repair and deleted-session controls now live in that organization view. Handle
  synchronization is bounded to the selected organization. The full web suite
  passed (1,572 tests), followed by 16 backend and 47 demo checks for scoped
  repair and adoption. Type checking, builds, documentation validation and
  desktop/mobile browser inspection passed.

- 2026-11-17: renamed the product boundary to Organizations and automation
  identities to Service users across UI, native contracts, SDKs, CLI and database
  records. Admin now uses organization-scoped URLs and navigation with a separate
  Platform section. Working universes are searchable and grouped by organization;
  creation explicitly shows its destination. User keys visibly default to one
  organization, with an explicit broader-scope choice. Service-user keys stay
  bound to one organization. Existing provider and OAuth account concepts retain
  their names.
  Validation passed: 1,560 web tests plus focused post-review checks, 496 backend
  tests, 247 TypeScript SDK tests, 113 Python tests, CLI tests, exact workspace
  Clippy, type checks, production/demo builds, and reproducible generated
  contracts. PostgreSQL empty-install and upgrade checks passed. Live checks
  covered native APIs, service-user authority, CLI device approval, both SDKs,
  renewal/revocation and Configurator. Browser checks covered key creation and
  revocation, 390×500 scrolling, grouped search, Owner-only navigation and denied
  organization/platform URLs. The dev launcher's public origin and device proxy
  now support browser CLI approval without weakening same-origin validation.
  Retained runtime actor attribution is an explicit greenfield migration boundary,
  documented in the upgrade guide.

- 2026-11-17: unified administrative navigation under **Admin**, above the
  working universe. Accounts and application-key management are scoped to
  administered accounts; Universe Owners can administer their universes without
  deployment privileges. Deployment-wide pages remain Platform Admin-only.
  Universe grants and lifecycle/settings moved to `/admin/universes/:handle`,
  with the old Members and General settings routes removed. Accounts use an
  explicit inventory/detail flow rather than an implicit active account. Add,
  edit and revocation workflows use viewport-bounded scrolling dialogs; existing
  Admin creation dialogs were bounded as well. Application-key inventory is
  filtered by an explicitly selected account and optionally an application,
  reusing existing APIs. Personal keys stay with the user's profile.
  The full web suite passed (1,552 tests), followed by focused checks for final
  dialog refinements. Type checking, production and demo builds, and documentation
  validation passed. Live browser checks covered application/key creation,
  revocation, universe grant add/remove, account scoping and denied deployment
  access, plus dialog and page scrolling at 390×500. Temporary keys/grants were
  removed and the test application disabled after validation.

- 2026-11-17: reviewed the authentication UI against the account model.
  Personal keys live under Your profile, application keys under their named
  account application, and universe Access manages people/application grants.
  Outbound Credentials now sits under Setup. Added named account/universe key
  restrictions, accurate expiry/disabled states, one-time secret dismissal
  without query or mutation caching, account deep links, and working profile
  anchors for local and company-managed users. Account and universe role copy
  distinguishes explicit grants from administrative and configured-policy
  access. Platform admin now distinguishes users from accounts and exposes user
  IDs for membership management. All 1,528 web tests, web type checking, production build and documentation
  checks passed. Live browser checks cover personal/application key creation and
  revocation, application disabling, account links, non-admin controls, and desktop
  and mobile layouts. Disposable keys were revoked after validation.

- 2026-11-17: cleanup audit removed per-universe `gatewayUrl` from the active
  schema and public API; Platform schema revision 8 drops its unused column.
  Every universe uses the configured deployment runtime. Removed obsolete
  routing branches and tests, centralized runtime-client factories and HTTP
  error mapping, and replaced human-only caller naming with actor attribution.
  Configurator's existing application-key installation is exposed in the web
  and demo setup UI again, with account administration checks and credential
  rotation. Retired trusted-header configuration still fails closed at startup
  without an inert policy object threaded through the runtime. The standalone
  connector host remains a trusted runtime/Temporal bridge; only automatic
  development-launcher startup is disabled, which the current guides now state
  accurately. Fresh and upgrade migration checks passed, as did the complete
  backend suite and web suite. The live full-stack check installed and rotated
  the managed Configurator application credential and discovered tools through
  the runtime's stored brokered grant. Cross-SDK renewal, restrictions,
  revocation and a paid model run passed again after cleanup.

- 2026-11-17: completed broad and live authentication validation with the
  user's authorization. Backend 495, web 1,510, database 15, Configurator 25
  and connector 74 tests passed, alongside SDK suites and production builds.
  Real PostgreSQL fresh/upgrade migrations and the Keycloak code-exchange
  suite passed, including admission, renewal, demotion, entitlement removal,
  disabled users, expiry and emergency access. Added the reproducible
  `test:runtime` backend script and ran it against the full development stack:
  browser users, scoped personal and Platform-admin keys, account-bound
  Creator applications, disable/revocation, grant and membership changes,
  invalid-bearer precedence, CLI device login/API-key setup and declarative
  Creator reconciliation all passed. Configurator's binary adapter was
  exercised through both real HTTP hops. New cross-SDK live coverage verifies
  saved CLI credential renewal and persistence, native/runtime operations,
  binary streaming, SSE, read-only/key-management restrictions, cross-universe
  storage isolation even for an Owner of both universes, and revoked-key/session
  rejection. A separate real model run through Platform verified TypeScript
  submission and Python idempotent resumption. These checks supersede the
  historical validation holds below; existing-installation data migration and
  authoritative background company refresh remain follow-ups.

- 2026-11-16: initial proposal followed runtime access simplification. Its
  deployment-scoped Creator, unique universe owner and custom token-table
  assumptions are superseded by the agreed account model above.
- 2026-11-16: checked Better Auth capabilities and current repository state;
  recorded account hierarchy, Creator naming, multiple universe Owners,
  account-controlled protection, credential attenuation and explicit remaining
  integration decisions. Documentation only; no authentication/account code
  implemented and no tests run or resumed for this revision.

- 2026-11-16: selected the Better Auth API Key plugin for both personal and
  application keys, with organization-owned application keys explicitly
  associated with Platform application identities. Recorded long credential
  lifetimes, hourly company-access freshness target and configurable product
  defaults. Existing-installation migration and CLI work are deferred by user
  instruction. Documentation only; tests remain on hold.

- 2026-11-16: implemented accounts with one-to-many universes, personal and
  development provisioning, account roles, user/application universe grants,
  multiple Owners, quota reservations and administrative protection. Native
  access-policy declarations now belong to Platform; runtime declaration-role
  DTOs were removed. Platform migration 0005 and schema revision 6 add the
  account/grant/application/key associations without guessing existing data
  ownership.
- Added Better Auth API Key 1.7.7 with user and organization configurations.
  A protected association resolves applications; no fake users or sessions.
  Current principal authority intersects key restrictions on every admission.
  Browser-only key management, hashed secrets, expiry, revocation, bounded
  company admission and overlapping managed rotation are implemented. Watch
  connections reconnect after at most one minute for fresh admission.
- Added native account, application, grant and credential routes from shared
  Zod descriptors and exported JSON Schema/HTTP catalogs. Both SDKs now expose
  native operations through unified `call` and generated `client.platform`
  helpers. Web account management, key
  management, account-qualified creation, grants, protection and expiry use
  the same services. Admin adoption and repair retain explicit account/Owner
  assignment. Configurator uses a scoped application key and brokered runtime
  storage; its process has no runtime infrastructure credentials.
- Validation: workspace Rust compilation and TypeScript/Python type checks
  passed. Auth, grant, lifecycle, SDK and UI tests were authored or updated,
  but tests and Clippy were not run at the user's request. Database migrations
  were generated, not applied to existing installations. Broad behavioral
  validation remains required before treating this as a validated deployment.

- Final implementation review added bounded-query account/universe inventory,
  recovery of failed provisioning reservations without consuming quota forever,
  atomic protection on administrative runtime repair, serialized mandatory
  protection changes, and credential replacement that preserves a working
  Configurator installation on failure. SDK, Configurator, web and demo builds
  passed; behavioral validation and migration execution remain deferred.

- 2026-11-16: implemented unified CLI connections, deployment URL normalization,
  explicit account selection, native account/member/application/grant/policy/key
  commands, and account-qualified universe/config provisioning. Configuration
  plans expose provisioning UUIDs for explicit retries. Removed the obsolete
  TypeScript Platform CLI workspace and packaging references.
- Added Better Auth device authorization for the fixed first-party CLI client,
  explicit browser code approval, password/SSO return handling, CLI-only session
  provenance, renewal, identity reporting, and current-session revocation. Keys
  cannot manage credentials; human CLI sessions can. Approval and redemption
  recheck company admission/access versions. Migration 0006 adds device codes
  and session provenance; Platform schema revision is 7. Migrations are not
  applied by this work.
- Verification: CLI all-target compilation and help rendering, full TypeScript
  checks, Python generation/type checks, SDK/Configurator/web/demo builds,
  release metadata verification, and whitespace checks pass. Auth, device flow,
  CLI, lifecycle, UI, and release regression tests were authored or updated but
  not executed. Tests and Clippy remain on hold; end-to-end behavioral validation
  is still outstanding.

- 2026-11-16: regrouped CLI commands as `connect login/logout`,
  `platform account/application/api-key`, and `universe access`. The generic
  `api` command stays at the root. Updated current CLI examples and access
  guides. Tests remain on hold; this documentation revision does not claim
  additional behavioral validation.

- 2026-11-17: composed runtime and native Platform operations into one SDK
  `call` surface and generated `client.platform` helpers from the native
  contract. Added Node-only and Python saved-connection factories with explicit
  account context and CLI-session renewal. Documentation now uses these
  surfaces; behavioral tests remain on hold.

- SDK completion verification: both generators reproduce current artifacts;
  full TypeScript checks, strict Python typing, Python wheel/sdist and
  TypeScript package-content checks, SDK/Configurator/web/demo builds,
  documentation build, CLI all-target compilation, and release metadata checks
  pass. Added typed runtime/Platform discovery and contract collision checks.
  The runtime member gate explicitly refuses native Platform calls, including
  generated helpers. Development connection imports persist verified credential
  provenance for SDK reuse. Regression coverage is authored; tests and Clippy
  remain on hold, and no live authentication checks were run.

- 2026-11-17: both SDKs now have named helpers for the complete runtime and
  native Platform catalogs (148 runtime and 26 native operations). Runtime
  names share one inventory across languages, with snake_case names in Python;
  native names remain contract-owned under `client.platform`. This adds no
  permissions or proxy admission: the selected endpoint applies its existing
  account, universe and credential policy. Catalog parity, generation, type,
  packaging and build checks pass; behavioral tests remain on hold.

- 2026-11-17: reviewed declarative configuration against the account model.
  Updated the configuration-as-code guide for application/personal keys,
  browser-authorized CLI connections, account-qualified creation, explicit
  administrative initial Owners, UUID recovery, credential ceilings and
  protected-universe policy. Pipeline examples now use a Platform application
  key, and obsolete deployment-export syntax was removed. Provider allocation
  remains deployment administration; account-level provider delegation is not
  implemented. Configuration planning reports changed provider allocations as
  conflicts for non-admin principals before any document writes, while
  unchanged bindings remain acceptable. Server admission remains authoritative
  for credential ceilings and current access on every request. Tests remain on
  hold.

- 2026-11-17: the deployment-admin audit log now uses server-side cursor
  pagination instead of a fixed latest-100 window. The browser offers 25/50/100
  records, Previous/Next, and a refresh that returns to the newest records.
  Timestamp/ID ordering preserves page boundaries, including PostgreSQL
  sub-millisecond timestamps and new events arriving between page requests.
  The demo uses the same response and validation. Backend integration and UI
  tests cover boundaries, invalid cursors, permissions, and navigation recovery.
