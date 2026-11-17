# People, organizations, and roles

A person signs in to the Platform. An **organization** groups universes, people, and
service users. Each universe belongs to exactly one organization. Better Auth
provides the organization and membership records. A Better Auth user remains
a person; its OAuth connection records are unrelated to organizations.

Organizations can be personal, shared, or development organizations. Personal organizations
are provisioned for admitted users by default, including company users; a
deployment can disable this. Shared organizations can represent a company or a
business unit. Organizations are siblings, without nesting. Organization Admins can see
and administer all universes in their organization, including private sessions.

## Organization and deployment roles

| Role | Authority |
| --- | --- |
| **Platform Admin** | Rare deployment-wide administration, including all organizations and universes. |
| **Organization Admin** | Organization policy, members, service users, and every universe in that organization. |
| **Creator** | Create universes within the organization's quota and policy. Receives Owner on universes they create, without gaining access to other universes. |
| **Member** | Access only to universes explicitly assigned to them or granted through organization policy. |

Organization membership is required for an ordinary user's universe grant to take
effect. Losing organization membership disables those grants; it does not destroy
universes. Removing Creator permission leaves existing Owner grants intact.
`createdBy` is attribution, not continuing ownership or authority.

## Universe roles

The four roles are cumulative. Session visibility remains a separate check.

| Role | Authority |
| --- | --- |
| **Viewer** | Read visible sessions and universe resources. Their own sessions remain visible after a downgrade. |
| **Contributor** | Create sessions using defaults or a saved profile; continue sessions, steer or stop work, decide approvals, invoke bots, and update workspace contents. |
| **Operator** | Configure profiles, bots, environments, MCP servers, credentials, channels, and session settings. Create workspaces and customize run setup. |
| **Owner** | Manage settings, universe access, and other Owners. Read, control, and share every session. Delete an unprotected universe. |

Contributors cannot author inline setup, override profile setup, create
integrated sessions, or edit profiles. Session configuration, custom
instructions, metadata, environment changes, and per-message model or
reasoning overrides require Operator or Owner, even in the caller's own
session. An Operator cannot read another person's private session merely
because they configure resources.

Contributors and Operators can start and control runs in shared sessions.
Sharing requires the creator with Contributor access, or an Owner.
Contributors can close their own private sessions; Operators can close their
own or shared sessions and soft-delete shared closed sessions. Owners can
close and soft-delete any session, subject to lifecycle requirements. Only
Platform Admins permanently purge soft-deleted session history.

A universe can have multiple Owners. Creation establishes an explicit initial
Owner. Ordinary grant changes cannot remove the last Owner; add another
first. Suspension still takes effect immediately, even for a sole Owner, and
Organization Admins can restore access. Owners grant access to existing organization
members and service users. Organization membership management requires an Organization
Admin.

## Provision people and organizations

Company sign-in admits users according to the configured provider claims.
Users are identified by exact issuer and subject, never automatically linked
to local users by matching email. Company identity and group claims refresh
at sign-in. See [Company sign-in](single-sign-on.md).

Without company sign-in, public signup is disabled. A Platform Admin creates
users under **Admin → Users**. The first administrator comes from
[the self-hosting setup](../deployment/self-hosting.md#configure-the-two-applications).
Users manage their profile, sign-in settings, and personal keys under **Your profile**.
The read-only **User ID** there can be shared with an organization admin when joining
an organization. Platform Admins can also copy it from **Admin → Users → Edit user**.
Company-managed users change their identity and password with their
company identity provider.

In **Admin**, choose an organization in the sidebar. Organization Admins manage
**Universes**, **People**, **Service users**, **API keys**, and **Settings** there.
People lists existing users: add them by user ID and assign Member, Creator, or
Admin. New shared members default to Member. Shared organization creation is
restricted to Platform Admins by default; deployments may allow admitted users
to create their own. There is no email invitation or SCIM workflow yet.

**New universe** selects an organization where the caller is a Creator or Admin.
Creating from an organization's Admin page fixes that destination. The initial
quotas are 25 universes for personal organizations and 100 for shared ones,
configurable by deployment and organization policy. Shared universes are
protected by default; personal and development universes are not. Organization
policy may require protection. Handles remain unique across the deployment.

The working universe switcher groups accessible active universes by organization
and supports search. A personal organization is labeled **Personal**. Switching
directly between universes does not require first switching organizations.
Organization membership alone does not make every universe visible: explicit
grants, declared policy, or administrator authority still determine access.

Under **Admin → organization → Universes → select universe → Access**, assign
existing organization users or service users one of the four universe roles.
Changes apply to the next admitted request. Requests already admitted and work
already running can continue; cancel that work separately if necessary.

The **Admin** area sits above working universes. Universe Owners can select the
organization containing their universe but only administer their owned
universes. They do not gain organization-wide People, Service users, API keys,
or Settings pages. Organization Admins administer all universes in their
organization. **Platform** is a separate sidebar section for organization
inventory, sign-in users, deployment-wide universe operations, infrastructure,
connectors, and audit. Its pages show **Platform administration** and are
unaffected by the organization selector.

Organization context is explicit in each Admin URL; it does not change another
tab's context. Creation and membership changes use explicit add/edit dialogs
with Save and Cancel. Dialogs remain scrollable on small screens. Organization
and universe access checks still run on the server.

## Service users and API keys

Service users are nonhuman identities belonging to one organization, with their
own organization role and universe grants. They have no fake email address or
human session. Organization Admins manage them under **Admin → organization →
Service users** and their credentials under **API keys**. Disabling a service
user blocks its keys without deleting its universes.

A user key represents the signed-in person; a service-user key represents the
service user. User keys live under **Your profile → Your API keys**. The web form for a new user
key defaults to one organization; selecting **All my accessible organizations**
explicitly permits broader use. That choice still grants only the user's
current permissions. Service-user keys always belong to their identity's one
organization and may be narrowed further.

Every request intersects that principal's current authority with the key's
allowed organizations, universe UUIDs, operations, and optional read-only
restriction. An existing-universe restriction does not permit creation.
Credential management requires a human browser or CLI session. See
[API keys and runtime tokens](api-keys-and-service-access.md).

## Protection and expiry

A protected universe cannot be deleted, including by an administrator or an
automatic expiry sweep. Owners can enable protection. Only an Organization Admin
or Platform Admin can clear it, and mandatory organization policy must first be
changed. Clearing protection and deleting are separate operations.

An unprotected universe may have an expiry timestamp. Expiry and protection
are mutually exclusive. The runtime rechecks the current expiry under the
same lock used for protection and deletion before admitting automatic
deletion. Metadata is retained in the deletion record.

Configuration documents protect newly created universes when protection is
omitted; they preserve an existing universe's protection when omitted.
Runtime configuration carries runtime settings and metadata only. Organization
membership declarations and universe access policies belong to the Platform.

## Session expiry and suspension

Local browser sessions have a 30-day inactivity lifetime and a 90-day absolute
limit by default. Company sign-in retains its bounded admission policy:
eight hours by default, without sliding renewal. A directory refresh source
has not yet been implemented, so company API keys also require current
company admission. A recent browser session is not required for local
user or service-user keys.

Suspension blocks subsequent requests and preserves resources and historical
attribution. Session revocation signs out browser sessions; revoke keys
separately when those credentials should stop working. Reinstatement does
not restore an expired or revoked credential. Runtime infrastructure tokens
remain separate full-authority credentials.
