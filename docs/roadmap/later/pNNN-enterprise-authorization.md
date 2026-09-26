# Enterprise authorization and identity

**Status:** Later work. Version 0.1 access is
[core: universes, keys and actors](../p179-core-universes-keys-and-actors.md)
and [Platform: organizations, roles and unshared work](../p180-platform-organizations-roles-and-unshared-work.md).
The first attempt, a full authorization system inside core, was built and
cut back; [the retrospective](../archive/p176-p178-access-retrospective.md)
tells that story and lists what does not come back unasked.

Lightspeed is greenfield. Nothing here is designed yet; each item waits for
a user who needs it.

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

## Later work

**Identity and provisioning** — next, as two slices:

- [Single sign-on and directory membership](../p181-single-sign-on-and-directory-membership.md):
  OIDC sign-in with the Platform as the client, and directory
  groups mapped to universes and roles.
- [Deprovisioning and directory updates](../p182-deprovisioning-and-directory-updates.md):
  bounded sessions, SCIM, and what deactivation does to a person's access
  and work.

After those: SAML, several providers, and human API keys issued by the
Platform and proxied to core.

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

Current boundaries: [authentication](../../documentation/deployment/authentication-and-tenancy.md),
[identity and access](../../documentation/deployment/identity-and-access.md),
[tenancy](../../documentation/deployment/multi-tenancy.md),
[environment credentials](../../documentation/environments/credentials.md).
