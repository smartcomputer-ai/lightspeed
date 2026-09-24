# Enterprise authorization and identity

**Status:** High-level ownership, authorization, sharing, privacy, and initial bot
lifecycle decisions are agreed. Detailed permissions and implementation remain
exploratory.

Lightspeed is still greenfield, with room to reshape contracts, data models, and
implementation boundaries. Existing defaults and implementation choices need not
constrain the design; prefer a coherent model over preserving them for compatibility.

## Direction

Give people understandable control over access to agent work and its execution
authority. Alice's personal investigation and a team's service-bound work can
coexist in one universe. SSO/provisioning connects identities to this model.

## Proposed concepts

| Concept | Responsibility |
| --- | --- |
| Principal | Deployment-wide user or service identity; roles and resource grants give it authority. |
| Group | Deployment-wide membership managed locally or through an enterprise directory. |
| Universe | Tenant/resource boundary with role assignments, policy, and defaults. |
| Access policy | Determines who may read, use, control, share, or administer a resource. Roles bundle these permissions. |
| Execution identity | The principal a session runs as, fixed at creation: the person who created it, or a keyless service principal the requester may run as. Distinct from its audience and individual requesters. |
| Execution authority | Bounded, revocable authorization for a run and its delegated work. |
| Approval | An additional condition on an authorized operation, with a separately entitled reviewer. |

Use qualified names for attachments, and role assignment for assigning roles.
Credential grants refer to `auth/grants/*`; authorization uses access policies
and resource permissions.

Every principal has a stable Lightspeed identity, either local or linked to an
enterprise directory. Directory linkage does not supply downstream credentials:
acting as an external user/service account requires an explicitly configured
connection and supported authentication/delegation mechanism. A dedicated service
account may be a directory user object; this does not make it personal authority.

Deployment-wide identity does not imply global permissions, visibility, or
administration. A service identity may be managed by one universe/team. An
installation hosting unrelated organizations may need separate identity realms.

## Decided: ownership and enforcement

| Layer | Owns |
| --- | --- |
| Platform | Authentication credentials/sessions, external identity mapping, SSO/provisioning integration, user profiles, administration and sharing UI. |
| Core authorization subsystem | Canonical principal IDs and effective status, groups/memberships, role assignments, service capabilities, resource access policies, execution authorizations, and their persistence/contracts. |
| Runtime gateway and adapters | Enforcement across reads, streams, commands, tools, credentials, and background work. |
| Deterministic engine | Recorded identity/authorization facts needed for execution and replay; no directory or policy I/O. |

Platform, headless administration, and provisioning integrations use the same core
identity/access contracts. Core owns the effective local directory; no separate
Platform authorization projection is required. External directories may remain
authoritative for imported membership, so reliable synchronization and offboarding
propagation from those sources remain necessary.

Services authenticate explicitly. Asserting a user requires a scoped capability;
operations then use that user's permissions without adding the asserting service's
privileges. Preserve both identities in attribution. API keys authenticate a bound
principal within a scope and do not confer independent authority. Internal event
provenance identifies a component and cause; it is not an authorization bypass.

## Decided: universe roles and resource defaults

| Role | Default responsibility |
| --- | --- |
| Viewer | Read universe-visible sessions and permitted resource information. |
| Contributor | Use available resources and interact with bots; create sessions, and profiles within granted authority, and manage their own work. |
| Operator | Make the universe usable: configure shared MCP connections and their secrets, model providers, workspaces, environments, profiles, and bots; provision through assigned environment providers. |
| Admin | Set up the universe, assign roles, decide who may run as which service principal, govern access and high-level policy, and authorize administrative operations such as future bulk deletion. |

Operators usually maintain shared bots; contributors usually interact with them.
This does not prohibit contributors from creating their own bots using permitted
resources. Profiles remain session templates and cannot confer resource access.

DeploymentAdmin and trusted service capabilities are distinct from universe roles.
Deployment administration uses `deployment/*`; Operator remains the universe role.
Headless bootstrap and audited recovery of orphaned universes must be supported.
Administrative role assignment does not automatically grant private-content access.

Model connections, MCP servers, environments, workspaces, secrets/integrations,
and bots are permissioned resources. They normally inherit universe role rights;
restricted resource policies replace the corresponding inherited allowance.
Model-provider configuration remains distinct from other integrations even when
both use the same underlying secret/credential-grant storage.

Using a configured connection includes brokered use of its bound credential;
raw secret access and configuration are separate permissions. Operators manage
shared resources within assigned scope; configuration changes cannot bypass
resource access policies. The role does not grant access to restricted personal
resources, authority to widen their audience, or permission to change universe
membership, who may run as a service principal, or its own access-management
limits.
Managing a shared template does not grant control of personal sessions created
from it. Owners retain the sharing rights below; administrative content access is
separately authorized.

## Decided: execution identity

There is no execution-binding object. A session runs as a principal, and that
principal's effective rights (roles, group memberships, resource grants) are
what its resource use is checked against. Everything an "execution binding"
would say decomposes into facts the authorization model already holds:

```text
runs as              = a principal recorded on the session at creation
who may run as it    = a `run_as` grant on that service principal (Admin, ManageAccess)
what it may use      = the principal's roles and `use` grants on resources
narrower scope       = a different, narrower service principal
```

Execution principals hold no credentials. A service principal that holds or
may hold an API key is a login identity and never a `run_as` target; a `run_as`
grant is refused for it structurally, not by convention. This keeps "run as"
from becoming "act as": key-level rights and capabilities never reach a session.
Creating a narrower persona therefore costs one row, not a new concept, and
"why may this session use X" is always one lookup: which principal, which
grants.

Start with one universe execution service per universe: a keyless service
principal holding the system Executor role (see and use, nothing else, so that
agent authority is not coupled to a role written for people), which every
Contributor and Operator may run as by default. Individual resource policies are exceptions, not mandatory setup.
Sharing group membership does not itself grant permission to act as another
principal. Further service principals are created when distinct authority
boundaries are needed (research versus release, development versus production
operations); who may run as each is a grant, replacing the role default.

Session, bot and collection creation record the concrete execution identity;
changing the universe default does not rebind existing work. Profiles confer no
authority, and a denied or unavailable identity must not silently fall back to
another. Personal execution (running as oneself) requires explicit admin
enablement for the universe and retains the owner-only control rules below.

Keep three permissions distinct:

- **Run as a service principal:** create sessions, bots and collections that
  execute under it. This intentionally delegates that principal's resource-use
  authority; callers need not duplicate those permissions on their personal
  principal. Session control permissions still apply.
- **Invoke a bot:** use its configured service capability without automatically
  gaining permission to configure the bot or create arbitrary sessions as its
  execution identity.
- **Grant resource use:** a resource's owner or an administrator grants `use` to
  a principal or group. Granting a service principal another resource also
  delegates its use to everyone who may run as it, so the grant must be
  authorized for that audience. Configuring a resource or holding personal use
  permission alone does not grant this authority.

## Decided: session access and personal authority

A session has three separate properties: its universe, its audience/controllers,
and its execution identity. Universe members can read universe-visible sessions.
Restricted sessions are readable only by their owner and explicitly granted
readers/groups, all of whom must also have current universe access. A private
session starts with only its owner in that audience.

```text
Read session = universe access AND (universe-visible OR explicit read access)
Run/control = universe access AND (Contributor or above on universe-visible
              team work, OR owner, OR explicit control grant)
              AND permission to run as its execution identity
```

Lightspeed is multiplayer: universe-visible team work is worked on together,
so every Contributor and above controls it. Grants matter on restricted work.

Personal/team experiences are policy presets over the same session model:

- **Personal:** runs as its owner, with a private initial audience; by
  default only that user may submit or steer work. Nobody else controls the
  session under the user's authority unless the owner explicitly adds them as a
  writer; configuration changes never grant it.
- **Team:** runs as a service principal and is universe-visible by default, so
  every Contributor and above can work in it. Restricted team work is limited
  to explicitly authorized writers/controllers. Requests remain attributable to
  individuals.

A session writer may share read access with existing universe users/groups or
make the session universe-visible. Readers cannot change sharing. Sharing never
grants control, credentials, or universe membership; granting writer rights is
a separate management action. Removing universe access also blocks previously
shared sessions.

Session permissions govern retained content and session-owned outputs, including
results acquired through restricted connections or service principals. An
authorized writer sharing a session authorizes disclosure of those results;
readers need no access to the original source or execution identity. References to other resources
do not grant access to them. Revoking source access prevents future use but does
not retract retained results. Source-imposed audience limits are deferred.

Administrators can configure and govern the universe and stop/suspend work without
permission to read private data/results or continue work under another user's
identity. Reading private content outside normal sharing requires a separate,
explicit, audited permission; the Admin role does not confer it. Sharing/role
management, diagnostic surfaces, and configuration changes that could expose
private inputs/results must respect this separation. Deployment operators with
host/database control are a separate infrastructure trust boundary.

Connection credentials are separate from execution identity: personal execution
may use an approved service connection. Using and administering a connection are
distinct permissions; downstream authorization must also permit the operation.

Initially, keep execution identity stable for the session. Changing identity
requires a new session and deliberate, authorized context transfer: old context
may contain information acquired under different authority. Each run still
checks current permissions.

## Decided: initial bot authority

Support personal bots through explicit standing authorization, initially for
owner-configured schedules. Runs use the owner's delegation and default to
private. Logout does not end authorization; disabling the owner or revoking the
delegation blocks further work, as does removing the owner's universe access,
subject to the execution limits below.

Shared bots and bots accepting other people's requests use service
identities and organizational authorization. Offboarding their creator/configuring
operator removes that person's access but does not revoke the bot's authority.
Revoking the `run_as` grant, the service principal's resource permissions, or
its credential grants blocks further affected work; creator attribution is distinct from the ongoing
source of authority.

A trigger's source never selects the execution identity. Instructions, profiles,
trigger configuration, manual event submission, and replay are control surfaces
subject to the same authority rules as session messages.

Personal event-driven automation is deferred. Supporting it later requires a
constrained trigger contract that distinguishes task inputs from another person's
control requests. Bot-created sessions inherit the bot's execution identity and
visibility defaults; activity/event views must respect restricted session content.

## Contract sketch

Illustrative operations, not proposed public API signatures:

```text
RequestContext = target_scope + acting_principal + authentication_reference
AuthenticationReference = credential_reference + authenticated_principal
UniverseExecutionPolicy = execution_principal + personal_execution_enabled
RunAsGrant = service principal + subject(principal | group) + granted_by
Session = universe + access policy + execution identity + capability limits

authorize(context, action, resource) -> decision + reason + policy reference

admit_run(context, session, requested_capabilities)
  -> execution authority

ExecutionAuthority = run_as + authorized_by + admitted scope
                   + delegation parent + validity/revocation reference

authorize_effect(authority, action, resource) -> decision
```

Derive run identity from the session's execution identity and actor identity
from trusted authentication or an authorized service assertion. Persist admitted scope and
provenance; live policy can narrow or revoke authority. The authorizing actor is
distinct from the personal/service authorization whose continued validity the run
depends on. Adapters check policy and record facts needed for replay.

Important boundaries:

- **Delegation:** ordinary children stay within the parent's delegable scope.
  Invoking a separately privileged service requires an explicit permission.
  Profiles request capabilities; they cannot confer authority.
- **Information:** permissions cover discovery, transcripts, streams, children,
  files, snapshots, and blob reads. Possessing a content hash is insufficient.
- **Credentials:** machine access and credential use are separately authorized.
  Machine-wide injection and shared OS access need an isolation boundary.
- **Revocation:** recheck before new effects, retries, queued execution, and
  approval release. Block further work and request cancellation when authority
  expires; stopping active processes or revoking issued credentials depends on
  the adapter. Completed effects cannot be undone by revocation.
- **Evidence:** attribute requests, permission changes, approvals, and outcomes
  to actors and execution identities, preserving attribution after offboarding.
  Administrative/access audit records need a lifecycle independent of session
  deletion, without retaining secret values. Keep two streams: transactional
  permission changes (`access_audit_changes`) and significant security decisions
  (`access_audit_events`). Record run admission, meaningful configuration/control
  actions and denials; keep routine reads, credential leasing and individual
  model/tool steps out of access audit. Consequential administrative operations
  retain admission/outcome correlation, without duplicate records from nested
  checks. Establish this policy before designing retention. Sensitive reads and
  exceptional private-content access will require auditable access records beyond
  domain events.

## Implementation sequence

1. [Identity foundation and universe authorization](../p176-identity-and-universe-authorization.md):
   core-owned local identity and roles, authenticated callers, consistent request
   authorization, ownership checks, access revocation, and attributable audit
   records.
2. [Session access and execution authority](../p177-session-access-and-execution-authority.md):
   private/shared sessions and their content together with one complete
   authorized execution path: a stable execution identity per session and
   bounded run authority.
3. [Resource access and session authority](../p178-resource-access-and-session-authority.md):
   the version 0.1 boundary. Policy rows and one `use` grant for workspaces,
   environments and MCP servers, the Executor role for agent identities,
   attachment admission against the execution identity, and per-turn
   revocation extended to attached resources. Keeps the default and personal
   execution choices; additional execution identities, general `run_as`
   grants and model connections as a kind remain later work.
4. Connect SSO and provisioning to the same identity and membership lifecycle;
   offboarding must also govern API keys and delegated work.

Product surfaces should make this understandable: an **Access** panel,
**Running as** information, and an explanation of effective permissions.

## Questions to resolve

- Which external credential/delegation mechanisms are needed first, and what
  narrower access-management scope should operators receive?
- For later personal event automation, how do we distinguish task inputs from
  another person's control requests?
- Are access policies and groups sufficient initially, or is a project-level
  collaboration scope needed? The second slice built collections, a minimal
  shared root, and the third removes them again: nothing in the product needed
  one yet, and a project scope will be designed when something does.
- How do external directory changes reach core, and when is offboarding
  considered complete?
- How is the separate private-content access permission assigned and exercised,
  including audited emergency access?
- What revocation latency and reauthorization behavior can we promise across
  tools, streams, jobs, and already running processes?

Current boundaries: [authentication](../../documentation/deployment/authentication-and-tenancy.md),
[tenancy](../../documentation/deployment/multi-tenancy.md),
[environment credentials](../../documentation/environments/credentials.md).
