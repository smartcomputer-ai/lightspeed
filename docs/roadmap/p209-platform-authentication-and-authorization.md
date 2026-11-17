# P209 — Platform accounts, authentication and authorization

**Status:** First-release account, grant and credential model implemented,
2026-11-17. CLI consolidation and browser-approved device sign-in are implemented;
unit, integration and live authentication checks pass, including real SDK model
calls through Platform. The earlier test hold has been lifted.
Existing-installation migration and authoritative background company refresh
remain follow-ups. Platform uses Better Auth organizations as accounts, each
containing multiple universes, and shares one authorization gate across browser
sessions, CLI sessions, and personal/application API keys.

[Runtime access simplification](p208-runtime-access-simplification.md) leaves
trusted component authentication in the runtime and puts caller authorization
in Platform. [The resource HTTP API](p208b-resource-http-api.md) supplies the
shared transport and generated operation catalog. This document defines the
account hierarchy, identities, credentials and authorization above it.

## Decision

A deployment contains accounts. An account contains universes. People and
applications receive roles at the account and universe levels. The same
Platform gate serves browser, CLI, SDK and integration requests.

- A Better Auth **organization represents an account**, not a universe.
- Every universe belongs to exactly one account. The account is its durable
  home; deleting or disabling its creator does not orphan it.
- Accounts can be personal or shared. They use the same underlying model.
- Platform Admin is rare, deployment-wide authority. Ordinary universe
  creation is an account capability, not a deployment-administration role.
- Account roles are **Admin, Creator, Member**. The name Creator replaces
  the working name Builder; it is not merely a `createdBy` attribution.
- Universe roles are **Owner, Operator, Contributor, Viewer**. Owner replaces
  the present universe Admin role. Multiple Owners are supported.
- No separate single-principal universe `ownerId` exists. `createdBy` is
  attribution, never a permanent authorization bypass.
- A credential authenticates an identity. Its restrictions can only narrow
  that identity's current authority.
- Runtime tokens remain separate full-authority infrastructure credentials.

## Accounts and deployment configuration

An account groups universes, membership, administration, applications and
resource limits. It supplies a future billing boundary without requiring a
billing feature in the first version. It is not Better Auth's OAuth `account`
record: the implementation uses its `organization` entity.

A public signup can automatically receive a personal account and begin
creating universes without a separate organization-creation wizard. The
person administers that account. Enterprise deployments may also enable
personal accounts for work, development and exploration.

A deployment can instead provision a single corporate account, several
accounts for business units or verticals, or a mixture of shared and personal
accounts. Accounts are siblings; nested organizations are not required.
Membership in one account does not grant access to another.

Deployment policy controls these independently:

- Signup and sign-in admission.
- Personal-account provisioning and who may create shared accounts.
- Account membership and account-role assignment, including company-group
  mappings.
- Universe-creation quotas and defaults or requirements for deletion
  protection.

A local development deployment can provision one shared account with Creator
access for its developers. A corporate deployment may give ordinary users
Member access and selected groups Creator or Account Admin access. Personal
accounts remain subject to deployment administration and corporate policy.
The first-release defaults are listed below; deployment configuration can
change them independently. There is no mandatory public-versus-enterprise
mode that fixes all of them.

Account membership alone does not reveal or grant access to every universe.
Creators and Members see universes to which they have access. Account Admins
administer and can access all universes in their account; Platform Admins can
access all accounts and universes. Resource privacy must not imply protection
from those administrative roles.

## Roles and universe lifecycle

### Deployment

**Platform Admin** administers the deployment and has access across accounts
and universes. This is a distinct platform function, assigned sparingly.
Account creation or universe creation does not grant it. Any narrowed
credential used by an administrator still applies its restrictions.

### Account

| Role | Authority |
| --- | --- |
| Admin | Administer account membership and applications, account policy and universes; recover universe administration. |
| Creator | Create universes in this account and become an Owner of each one created; no automatic access to other universes. |
| Member | Access assigned universes; cannot create universes merely by belonging to the account. |

Higher account authority includes creation. Creator is account-scoped: a
person or application may be a Creator in one account and a Member in another.
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

Universe grants require membership in the containing account. Removing that
membership disables access through any universe grants there. Universe Owners
manage universe grants within the account; that does not itself grant them
account-membership administration or access to a deployment-wide directory.
Company-group assignments must resolve to the same scoped role model.

### Creating and maintaining Owners

A create request selects an account. Platform checks account creation
permission, credential restrictions, account policy and quotas. It creates
the runtime resource and establishes the Platform linkage and an initial
Owner grant. The creating principal normally receives that grant. A
Platform Admin provisioning on behalf of someone else must establish an
explicit initial Owner too.

At least one explicit Owner is required on creation. Ordinary grant edits
cannot remove or demote the last Owner. Suspension or account-membership
revocation still takes effect immediately even if it disables the last
Owner; Account Admins and Platform Admins can recover administration.

Several principals may be Owners with equal authority. An account Member
may be made Owner of an existing universe without becoming a Creator.
Losing Creator status prevents new creation but does not revoke existing
Owner grants or their lifecycle authority. Losing an Owner grant removes
that authority even from the original creator.

Universes are not re-parented when a creator leaves. Moving universes between
accounts is deferred. Quotas belong to the account model; the earlier
per-principal ownership count and its proposed default of 25 are superseded.

### Deletion protection

Reuse the runtime `protected` flag designed in
[universe metadata, protection and expiry](p207-universe-metadata-protection-and-expiry.md).
The runtime mechanism and Platform administrative protection gate are implemented.

- The runtime refuses deletion while `protected` is true, including expiry.
  Even administrative deletion must first clear the flag explicitly.
- An Owner may delete an unprotected universe. Account Admins and Platform
  Admins may also do so, subject to credential restrictions.
- An Owner may enable protection but cannot clear it. Only an Account Admin
  for the containing account, or a Platform Admin, may clear it through
  Platform, subject to account policy.
- Accounts can default universes to protected or require protection. A
  mandatory policy must be changed before it can permit unprotection.
- Owners may manage expiry where protection and account policy permit it.
- These rules cover UI, CLI, SDK, configuration documents and every update
  path. `protected: false` in a document must not bypass authorization.

Document-created universes retain the default specified in P207: omitted
protection protects a newly created universe and leaves an existing universe's
setting alone. An explicit false is honored only when policy permits it.

## People, applications and credentials

A **principal** is the identity whose permissions are evaluated. Two cases
must be supported by the authorization design:

- **User:** a person authenticated through the existing Better Auth sign-in
  and session system. A CLI or script can act as that person.
- **Application:** independently managed automation associated with an
  account, with account/universe role assignments and an enabled state.
  Its administrator is not the identity it authenticates as.

Application is the product term for the non-human principal. It need not
have a human profile, email address, password login or another universe
ownership system. In the first release it is a small Platform record with an
account, name, enabled state and scoped grants. Its API keys use Better Auth's
organization-owned key configuration, with a server-controlled association
between key ID and application ID. It is not a fake human user or an OAuth
client registered solely to obtain a long-lived secret.

An application can be Operator in one universe, Viewer in several universes,
or Creator in its account and Owner of universes it creates. Application
suspension removes its access, but its resources remain in the account.
Application administration and role assignment cannot be inferred merely
from being the human who generated a credential.

### Better Auth reuse

The installed Better Auth version is 1.7.7. Reuse sign-in, sessions, SSO and
organizations. Use the API Key plugin for both personal and application API
keys; do not build a second key generator, hash store or verifier.

The library supports user- and organization-owned keys, not an arbitrary
principal foreign key. Integrate those supported ownership modes explicitly:

| Credential | Better Auth key owner | Platform authorization subject |
| --- | --- | --- |
| Personal API key | User | That user, with current roles and key restrictions. |
| Application API key | Organization representing the containing account | The associated application, with its current roles and key restrictions. |

Platform stores the application and a server-controlled key-to-application
association. Verification checks that the key's configuration and organization
match that application and account, and that both remain active. An unlinked
organization key is refused by the shared API. Neither key ownership nor the
human who created it supplies the application's permissions. Arbitrary key
metadata or caller-supplied identifiers cannot choose the principal.

Use separate plugin configurations for personal and application keys. Expose
key creation, association, updates and revocation through reviewed Platform
services. Do not expose unrestricted plugin management endpoints that bypass
application or credential-restriction policy. Interrupted creation must not
leave an unassociated key with usable Platform authority.

Better Auth handles credential generation, hash storage, verification and
expiry. Platform handles application identity, scoped roles, resource rules,
key-to-application association and any restrictions beyond the library's
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
allowed accounts/universes and API capabilities or operations. Neither token
issuance nor verification may manufacture additional authority.

Restriction semantics are part of the first release:

- Restrictions are ceilings, not independent role grants.
- Membership loss, suspension and role changes are evaluated on requests;
  permissions are not copied into a permanent credential snapshot.
- Explicit read-only or universe restrictions remain in force if the user's
  broader roles grow.
- Creation requires an allowed target account and creation capability. A
  token limited to existing universes does not implicitly allow creation.
  Account-scoped credentials can use newly created universes only where the
  principal's new grant and the credential's operation limits both permit it.
- Restricted credentials must not mint or modify broader credentials as a
  way to escape their restrictions.

Tokens have expiry, independent revocation and a recognizable identifier or
prefix for listings. Secrets are not returned in ordinary listings. Reuse
library storage and lifecycle capabilities; validate integration details for
the API Key plugin. The initial lifetimes and company-access freshness policy
are recorded below. Expiry of a browser session does not by itself invalidate
an otherwise valid API key.

There is no separate universe-key authority model. A credential can be
limited to one universe while still authenticating its user or application.
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
custom refresh token, or fake application user is introduced.

The plugin issues an opaque Better Auth session token. A server-controlled
session provenance field distinguishes CLI from browser sessions. Only CLI
bearer sessions enter the shared API; browser-only UI/auth routes reject them.
API keys remain separate and cannot manage credentials. Human CLI sessions can
manage credentials just like browser users. Fresh admission and access-version
checks apply at approval, redemption, and subsequent requests. Local CLI
sessions renew through Better Auth within the 30-day inactivity and 90-day
absolute limits; company sessions retain their configured absolute limit.
`connect logout` revokes only the current CLI session and removes its private file.

Connection login and logout live under `lightspeed connect`. Account/member,
application, and API-key administration live under `lightspeed platform`.
Universe grants and declared policy live under `lightspeed universe access`;
universe creation and archive/restore stay under `lightspeed universe`.
The generic `lightspeed api` command remains at the root. `--account`
overrides the selected account; saving a different account clears the previous
universe. The generic `api` command uses generated operation catalogs, with
explicit private-file or stdout handling for newly issued secrets. Platform
native DTOs remain TypeScript-owned. The obsolete `lightspeed-platform` workspace
is removed; deployment-user suspension and company identity administration remain
web features rather than exposing browser-private endpoints to CLI credentials.

Both SDKs compose runtime and Platform-native operation IDs into `client.call`.
Native helpers are generated under `client.platform` and take typed parameter
objects; account IDs remain explicit. Node's `createClientFromConnection` from
`@lightspeed-ai/sdk/node` and Python's asynchronous
`AsyncLightspeedClient.from_connection` reuse saved CLI connection credentials
and renewal. They expose the selected account as `accountId` / `account_id`,
reread credentials for each request, and do not perform automatic login or
rewrite externally managed secret files. The main TypeScript entry point
remains browser-safe.

An application's own end-user attribution is optional future behavior. If
introduced, it must be explicitly authorized, namespaced under the verified
application, and never replace the authenticated application identity used
for permission checks and auditing. This is not required for initial tokens.

## Platform gate and API boundary

The HTTP adapter foundation from P208b is implemented with browser member
policy; it is not completion of this identity and account design. `/api/v1`
serves shared resources and `/api/ui` serves remaining browser adapters. Both
must use the same gate and lifecycle services.

Each request resolves an authenticated subject and credential restrictions.
Platform checks active identity, account membership, account/universe scope,
operation admission, role, enabled features and the existing resource rules.
It resolves the runtime universe UUID and sets attribution itself. A client
cannot choose a trusted upstream or impersonate another principal by header.
Unknown operations are refused until reviewed.

| Operation | Platform admission |
| --- | --- |
| Create universe | Creator or Admin in the selected account; Platform Admin may provision into an account. |
| List universes | Return only accessible universes within the authorized account/token scope; administrative visibility follows its scope. |
| Manage universe membership and lifecycle | Universe Owner or scoped administrative authority, with protection enforced separately. |
| Clear deletion protection | Containing Account Admin or Platform Admin, subject to account policy. |
| Manage account membership/policy | Account Admin or Platform Admin. |
| Deployment infrastructure and environment-provider administration | Platform Admin; preserve existing infrastructure boundaries. |
| Machine-only runtime operations | Not exposed through the general Platform API. |

Creation, deletion and native lifecycle handling coordinate both databases
with retry/recovery semantics. Account selection for creation is Platform
context: design it in the native contract without adding account identity or
policy to runtime DTOs or duplicating runtime route declarations.

Account management, applications, grants and token management live under the
Platform-owned `/api/v1/platform` contract. Generate its types and descriptors
once and compose them with admitted runtime operations as specified in P208b.
Do not move Platform identity records into Rust merely to share a generator.
Discovery and CLI status must expose the authenticated identity and relevant
account context without disclosing credentials or unrestricted inventories.

## Clients, declarations and configuration

The `lightspeed` CLI supports Platform credentials and direct runtime
infrastructure credentials. Direct mode remains available where the
operator permits it, including local development. It has full runtime
authority and does not implement account authorization.

Account selection is explicit or resolved from an unambiguous saved context.
Runtime universe paths continue to identify a universe; Platform verifies its
containing account. SDK helpers and CLI commands share the same HTTP contract.
The separate `lightspeed-platform` CLI is consolidated into `lightspeed`.

Declared members and company-group mappings belong in Platform. Remove the
runtime's temporary storage of that access policy as declarations move to
Platform-native services. Preserve existing enterprise admission and group
mapping behavior during that migration.

Configuration application runs as the authenticated user or application,
with normal role and token checks. Account selection, initial Owner grants,
quotas and protection policy apply to declared universe creation too.
Creator authority permits creating universes and grants the creator initial
Owner access; it does not grant access to other existing universes. A key
limited to existing universe UUIDs cannot create universes, regardless of its
principal's account role. Account membership, grants and access policies stay
in their native Platform APIs rather than becoming universe document sections.

Environment-provider bindings are deployment infrastructure allocations.
Their mutation remains Platform Admin-only: account and universe roles do not
entitle a principal to arbitrary deployment providers, and this design does
not yet include account-level provider entitlements. Ordinary account
automation consumes providers allocated by a deployment administrator and
omits `environmentProviders` from its documents. A deployment administrator
can reconcile allocation in a separate universe document. Provider
registration itself remains outside declarative universe configuration.

Supplied-secret key declarations are deferred until their fit with the API
Key plugin is established; they are not a reason to maintain an independent
legacy key implementation. Generated API keys cover the first release.

The Configurator's intended access is an application with Operator rights in
its target universe and credentials limited to configuration operations. It
must not receive a broad runtime token as the replacement for a restricted
Platform credential. Connector retirement/reintroduction is separate from
this account model and must not silently determine application permissions.

## Storage and rollout

The former unique organization-to-universe mapping is replaced with one
account-to-many-universes. Better Auth organization membership stores the
account relationship; explicit universe grants store universe roles. Reuse
library identity records where appropriate, with application storage chosen
after the credential integration decision.

Platform stores account policy and linkage, universe account ID and
attribution, scoped grants, and credential restrictions not already modeled
by the chosen library. There is no single-principal universe owner column.
Runtime stores the resource protection/expiry facts, never account roles.

Existing installations will be migrated separately with an agent, as directed
by the user. Do not build a compatibility layer or automatic account-grouping
migration for them as part of this implementation. Required database schema
migrations still belong to the change. The separate operational migration
must preserve intended universe grants and privacy, and must not promote old
universe administrators into account-wide administrators by inference.

Implement in these slices:

1. **Account hierarchy and grants:** database mapping, configurable account
   provisioning, account roles, universe Owner grants, shared authorization
   context and required schema migrations. Existing-installation data mapping
   is handled separately.
2. **Creation and protected lifecycle:** account-selected creation, quotas,
   initial Owner assignment, runtime protection/expiry from P207 and Platform
   unprotection policy across all callers.
3. **Personal tokens:** Better Auth API Key integration, narrowing,
   revocation, management and CLI/SDK access as the user.
4. **Applications:** organization-owned Better Auth API keys associated with
   application identities, scoped grants, credential lifecycle and the
   restricted Configurator path.
5. **Native access declarations and client consolidation:** move remaining
   runtime policy fields to Platform and expose the generated native API.
6. **CLI browser sign-in:** use the same subject and authorization path.

## Validation requirements

These are the acceptance requirements. Executed checks and their scope are
recorded under Validation and progress; the previous test hold was lifted
on 2026-11-17.

- Account membership never implies access to every universe. Creator sees
  assigned universes and can create; Member cannot create without that role.
- A user/application may have different roles in different accounts and
  universes. UUIDs, handles, list queries and tokens cannot cross the boundary.
- Creation grants an initial Owner and enforces account quotas atomically
  under concurrency; retries do not duplicate resources or grants.
- Multiple Owners work; the original creator has no hidden authority.
  Last-Owner edits are guarded, but suspension still revokes access.
- Losing Creator status leaves existing Owner grants intact. Losing account
  membership removes effective access to all its universes.
- Owners can delete unprotected universes but cannot clear protection via
  any route, document or configuration update. Account-required protection
  cannot be disabled while that policy remains in force.
- Protected universes cannot expire or be deleted even on direct runtime
  access; authorized deletion first clears protection explicitly.
- Browser, personal-token and application requests share role/resource
  enforcement. Narrowed credentials cannot mint broader credentials.
- Role changes, local suspension, expiry and revocation take effect on the
  next admission; IdP changes obey the agreed freshness policy. Long-lived
  streams have bounded reauthorization behavior.
- Application credentials do not silently become the administrator's user
  credentials. Audit attribution distinguishes subject and credential.
- Existing enterprise memberships, directory mappings and resource privacy
  survive the account migration without administrative privilege expansion.

## First-release defaults

The user delegated selection of sensible defaults, 2026-11-16. These are
initial product defaults, configurable by the deployment rather than new
hard-coded role distinctions.

| Setting | Initial default |
| --- | --- |
| Public self-signup | Disabled unless explicitly enabled; preserve configured company admission. |
| Personal accounts | Provision one for each admitted user; deployments may disable this. The user is its Account Admin. |
| Shared-account creation | Platform Admins provision them initially; broader self-service can be enabled explicitly. |
| New shared-account members | Member; Creator and Account Admin require explicit grants or configured company-group mappings. |
| Local development | One shared development account; admitted developers are Creators. |
| Application management | Account Admins create/manage applications and their keys and grant their scoped roles. Personal-account users can do this as their own Account Admin. |
| Universe quota | 25 active universes per personal account; 100 per shared account, with deployment and account overrides. |
| Interactive/API-created protection | Off for personal accounts; on by default for shared accounts. Development accounts default off. Accounts may change the default or require protection. |
| Document-created protection | On when omitted; existing universes retain their current value when omitted, as specified in P207. |
| Personal and application API-key expiry | 365 days by default and as the default maximum; deployments can choose a different maximum. Shorter expiry is selectable; keys are revocable at any time. |
| API-key rotation | Issue another key for the same principal, move consumers, then revoke the old key. Overlap is explicit; do not silently invalidate a working key on new-key creation. |
| Browser and CLI login | A 30-day inactivity window with a 90-day absolute login lifetime and renewal during use. Provider/deployment policy may require earlier authentication; company sessions retain their bounded absolute lifetime. |
| Local access changes | Check local revocation, suspension and effective grants on admission; no artificial delay is required. |
| Company access refresh | Target a background refresh at least hourly, independent of API-key or login lifetime. See the integration requirement below. |

The shared-account protection default is not mandatory protection: an Account
Admin can configure a development account for disposable universes. Mandatory
protection remains a distinct account policy. Quotas count admitted creations
and resources still consuming their allocation; deletion does not release a
slot before the resource is actually removed. Applications consume their
containing account's quota, not a separate creator quota.

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
require another interactive login. Disabling an application is local Platform
state and does not depend on company identity refresh.

## Remaining follow-ups

The first release uses separate Better Auth API Key configurations for user
and organization ownership, with a protected key-to-application association.
Generic plugin management endpoints are hidden. Native account selection is
explicit, universe handles remain globally unique, and resource paths resolve
to UUIDs before credential restrictions are applied. Keys support exact
operation IDs, account/universe lists, read-only restrictions, and a universe
creation ceiling. Capability bundles can be added without changing this model.

CLI browser login and account-aware command consolidation are implemented.
The command hierarchy separates `connect` login/session management, `platform`
account/application/key administration, and `universe access` grants/policy.
Existing-installation data mapping is separate and will be performed with an agent. Background company identity refresh still
needs an authoritative source; the implementation retains the existing bounded
company admission window and does not promise hourly provider polling.
External application-user attribution, supplied-secret key declarations and
broader request auditing remain follow-up work.

## Outside the first version

- Nested accounts, cross-account universe transfer/sharing and arbitrary
  custom-role or policy-language editors.
- An authorization model in the runtime or a second gate for the browser.
- Single-principal universe ownership and privileged `createdBy` behavior.
- Automatic application impersonation of human users.
- OAuth client-credentials issuance as a prerequisite for integrations that
  can use API keys; applications initially use the API Key plugin.
- Compatibility/data-migration tooling for existing installations in this
  implementation; operational migration is handled separately.

## Validation and progress

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
