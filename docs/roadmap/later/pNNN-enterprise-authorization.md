# Enterprise authorization and identity

**Status:** Later work. Version 0.1 access is
[core: universes, keys and actors](../p179-core-universes-keys-and-actors.md)
and [Platform: organizations, roles and unshared work](../p180-platform-organizations-roles-and-unshared-work.md).
The first attempt, a full authorization system inside core, was built and
cut back; [the retrospective](../archive/p176-p178-access-retrospective.md)
tells that story and lists what does not come back unasked.

The first SSO delivery is implemented and locally validated in the two slices
below; deployment-provider acceptance remains pending. The
remaining items wait for a deployment that needs them; they are not
prerequisites for company sign-in.

## Where 0.1 draws the line

- **Core** scopes every call to a universe, authenticates keys that carry
  method groups and may assert an actor, records who asked for each run,
  steer, cancellation and approval, and guards its own bots and sub-agents.
  It never resolves or decides from the actor.
- **The Platform** owns people: sign-in, organizations as universes, four
  member roles, the gate that decides method and target before a request
  reaches core, and the audit trail.
- **Sessions** start private to their creator and the universe's admins, and
  are shared with the universe one way. Bots are always shared. Nothing runs
  as a person.

Later work keeps that split: identity and people in the Platform, tenancy
and the runtime's own guards in core. A new rule goes into core only when
core alone can enforce it.

## First SSO delivery

Two Platform-only slices, delivered together:

- [Single sign-on and application access](../p181-single-sign-on-and-directory-membership.md):
  OIDC sign-in with the Platform as the client. The company grants ordinary
  Lightspeed access or platform-admin access, potentially through separate
  IT requests. Platform admins and universe admins manage local universe
  memberships and the four roles on Members. Explicit local emergency
  admins keep password login. No directory group is needed per universe.
- [Session expiry and access revocation](../p182-deprovisioning-and-directory-updates.md):
  an absolute session limit, provider reauthentication, immediate local
  suspension, and the boundary between stopping access and stopping work.

The first slice includes upgrading Better Auth to `1.7.6` and using its
Generic OAuth plugin with one configured OIDC provider, authorization code
flow, PKCE and nonce binding. Require a verified ID token and check the two
application entitlements before issuing a session. The SSO roadmap records
the package comparison and required integration hooks; `@better-auth/sso`
can be reconsidered for SAML or customer-managed provider connections.

The same OIDC flow supports a brief internal SSO redirect and external
credential/MFA prompts according to provider policy. The initial deployment
must verify how its provider handles disabled accounts and lost entitlements
before claiming an end-to-end revocation bound.

SSO renewal refreshes application admission and platform-admin status;
it preserves locally assigned universe memberships. Losing company
admission blocks Platform access even while those memberships remain
recorded. Admin → Users and Universe → Members provide administration;
there is no Directory page or group-mapping editor in the first delivery.

Core API keys continue to serve CLI, API and MCP clients with independent
authority. Offboarding a person does not revoke keys they retained; those
need separate revocation. Already admitted requests and runs may finish,
and shared bots continue. There is no automatic inactivity deactivation.

The first delivery also includes durable records for access changes,
membership edits, emergency sign-ins and Platform key operations, visible
under Users. Comprehensive gateway auditing, retention and export remain
separate work.

## Later work

**Identity and provisioning**

- Optional group-driven universe access, if a deployment needs it.
- SCIM provisioning and directory updates, when a deployment requires push.
- SAML, several providers, and authentication through trusted proxy headers.
- Personal API tokens issued by the Platform and proxied to core, and
  CLI/MCP access with a Platform user's permissions and revocation.

**Sharing and visibility**

- Unsharing, and sharing with named people or teams instead of the whole
  universe.
- Operators seeing, and stopping, private running work without reading it.
- Private bots, and conversations private to whoever invoked a bot.
- Separating administration from content: admins govern and stop work
  without reading it, with an explicit, audited exceptional read.

**Execution authority**

- Personal bots that run on their owner's standing authorization, and
  personal event-driven automation.
- Restricting a workspace, environment, MCP server or credential to some
  people, and sessions using only what their requester may use.
- Actor propagation to tools, so an MCP server such as the Configurator acts
  as the person who requested the run.
- A universe-wide stop that refuses new runs.

**Evidence**

- Audit retention and export, and a two-person rule for exceptional access.

## Questions to resolve

- Which external credential and delegation mechanisms are needed first?
- For personal event automation, how are task inputs told apart from
  another person's control requests?
- What revocation latency can we promise across tools, streams, jobs and
  running processes, now that nothing is rechecked mid-run?
- Is a project scope needed between a session and a universe? Collections
  were built once and removed because nothing used them.

Current boundaries: [access and security](../../documentation/access-and-security/overview.md),
[people and roles](../../documentation/access-and-security/people-and-roles.md),
[tenancy](../../documentation/access-and-security/tenant-isolation-and-data-protection.md),
[environment credentials](../../documentation/environments/credentials.md).
