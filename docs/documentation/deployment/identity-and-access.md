# Core identity and access records

The core `access` crate defines deployment-wide user/service identities, groups,
memberships, scoped role assignments, and explicit capabilities for people and
services.
`store-pg::PgAccessStore` persists them. Platform login credentials and external
identity mappings remain separate from these effective authorization facts.

The runtime gateway uses these records for scoped key authentication, current
roles/actions, resource audiences, execution authority and user assertions. Platform
accounts map to these principals and its requests assert the signed-in user. See
[Authentication and access](authentication-and-tenancy.md) for the current boundary.

## Bootstrap and administer without the Platform

Run migrations explicitly, then create the first core deployment administrator:

```bash
lightspeed-server migrate
lightspeed-server identity bootstrap \
  --principal-id 11111111-1111-4111-8111-111111111111 \
  --display-name "Deployment administrator"
lightspeed-server universe create --slug finance \
  --creator-principal 11111111-1111-4111-8111-111111111111
```

These commands use the configured runtime PostgreSQL database directly. Bootstrap
needs no Platform, Temporal, provider credentials, or secrets master key. It
creates identity records, not login credentials or API keys. Retrying the same
bootstrap principal is a no-op while that principal remains an active deployment
administrator. A different principal, or retrying after disablement/removal of its
role, cannot reopen bootstrap. Retain another usable deployment administrator
before disabling the initial one; bootstrap is not an emergency recovery command.

Universe creation requires an active DeploymentAdmin and atomically assigns that
creator the universe Admin role. Retrying an existing universe does not assign
ownership or change its slug. Authenticated runtime universe creation uses the same creator assignment.

`identity apply` accepts one typed `AccessChange` JSON file. For example:

```json
{
  "operation": "create_principal",
  "id": "22222222-2222-4222-8222-222222222222",
  "kind": "user",
  "displayName": "Analyst",
  "managementScope": { "kind": "deployment" }
}
```

```bash
lightspeed-server identity apply \
  --actor-principal 11111111-1111-4111-8111-111111111111 \
  --file create-analyst.json
lightspeed-server identity effective \
  --principal-id 22222222-2222-4222-8222-222222222222 \
  --universe-id "<universe-uuid>"
lightspeed-server identity universes \
  --principal-id 22222222-2222-4222-8222-222222222222
```

Host database access is the trust boundary for these commands. The explicit actor
selects whose current authority and attribution to use; it is not a login or a
remote impersonation parameter. Effective access without `--universe-id` selects
deployment scope. The universe query returns only universes with a current direct
or group role, and returns none for a disabled principal.

The same change contract supports group creation/renaming, membership changes,
role/capability assignment and revocation, identity disablement, universe creation,
and orphaned-universe recovery. The canonical shapes are exported in the
[API schema](../../../crates/api/contract/api.schema.json) and TypeScript client.
Principal kind and management scope are immutable; identities are disabled,
not deleted. Groups are flat and deployment-wide. Credential grants remain the
separate `auth/grants/*` resource.

## Authority and transaction boundaries

Viewer, Contributor, Operator, and Admin are universe roles. DeploymentAdmin is a
separate deployment role and supplies no implicit universe membership. The enforced
role matrix distinguishes creating work, controlling owned work, operating shared
resources, and managing access. Controlling a standalone session requires
ownership or a write grant even for Admin; Operator/Admin may stop another
session without acquiring its audience. Bot managers control the bot's visible
conversations. The runtime resolves these rights from the resource and its root
policy, never from provenance or client-supplied claims.

Service kind grants no capability. Credential leasing, inbound admission, user
assertion, channel-account discovery, and identity provisioning have explicit
capability assignments. Capabilities apply in their declared scope, and their
holder kind is checked when assigned and used. Deployment administration assigns
service capabilities. A universe Admin may also assign or revoke
`read_private_content` for a person in that universe. This permits audited reads
outside the person's ordinary audience, without granting control or ownership.
The `manage_identity`
capability delegates directory management, including membership and offboarding;
this is powerful authority because membership can affect privileged group roles.
It does not permit direct role/capability assignment or universe recovery.

Identity changes serialize on a deployment policy row. Authorization, the write,
the last-admin guard, revision increment, and `access_audit_changes` append commit
together. No-op changes add no committed change record. See the
[access audit policy](authentication-and-tenancy.md#durable-access-audit) for
significant runtime events and deliberately quiet routine traffic. Guarded
role/membership removal accounts for active principals reached through groups.
Identity disablement can still remove the last administrator. An active
DeploymentAdmin can explicitly recover an orphaned universe by assigning a named
active principal, without becoming a member themselves.

Reads use current committed facts with no authorization cache. Multi-query reads
use a consistent snapshot. Audit stores access-change metadata, never credentials
or session content, and survives universe deletion and identity disablement.
Universe-scoped assignments cascade away on universe deletion; canonical service
identity records retain their management provenance.

These guarantees cover core storage, gateway admission, shared runtime services
and Platform administration. Parked transcript reads revalidate their caller.
Runs check their execution principal at admission and before each model call;
losing that authority fails the next check. Completed effects and external jobs
are not undone or terminated by those checks. The [access guide](authentication-and-tenancy.md#execution-authority)
explains the difference between personal work and work running as the universe
service.

## Authenticated request boundary

The gateway now uses these records for key authentication, membership admission,
service capabilities, and authenticated user assertions. See
[authentication and access](authentication-and-tenancy.md) for key issuance,
canonical keys, resource sharing, execution checks and the limits of revocation.

## UI action permissions

`access/read` previews the current caller's universe actions and actions on up to
100 requested sessions, bots, profiles or collections. The runtime resolves ownership and
controller lineage using the same policy as mutations; the client supplies no
owner or alternate actor. `sessionDeleteCascade` also checks deletion permission
for every descendant. Missing targets return no actions. Previews describe access,
not lifecycle readiness: a permitted deletion still requires closed sessions.

Platform uses these decisions to show creation, editing, invocation, stop and
delete controls separately. Viewers retain readable content; they cannot send,
steer, approve tools or mutate resources. Operator/Admin may stop another user's
session without receiving its input or settings controls. Permission previews are
account-scoped presentation hints; each operation remains independently authorized.

## Platform directory access

`deployment/identity/self` accepts a scope and returns only the acting user's
current rights and accessible universes, limited by the key scope. It accepts no
other user selector. `deployment/identity/directory` requires administration of
the requested scope (or explicit deployment directory provisioning authority).
Universe administrators can select deployment-wide principals/groups, but see
only that universe's assignments and memberships of its assigned groups.
Deployment administration can read the full directory; this grants no content access.

`deployment/identity/apply` authorizes each typed change in core. Universe Admins
may assign/revoke roles in their universe and create universe-managed services.
They may also grant or revoke the universe's `read_private_content` capability
for people. They cannot edit deployment groups, global identities or service
capabilities. Group changes can affect many universes and remain deployment
administration.
All query and mutation handlers check the credential ceiling and revalidate
captured authentication; missing context is never a service fallback.

The Platform CLI exposes `identity list` and `identity apply '<AccessChange JSON>'`
through these same records. Its member commands use Viewer, Contributor, Operator
and Admin assignments; deleting one assignment leaves independent direct/group
assignments intact.

In Platform, universe Admins can edit a user or group's role from Settings →
Members. A role edit atomically replaces the selected grant; independent direct
and group grants still apply. Core rejects stale assignments and changes that
would remove the last active administrator, leaving the original grant intact.
The same operation is available through `identity apply` as `replace_role` with
the existing `assignment` and replacement `role`, and produces one committed
access-change record.

A person's role editor also shows **Private-content access**. Its separate
control makes one capability change immediately; **Save role** changes only the
role. Groups and services have no private-content control. Removing a role does
not remove independent grants or capabilities, although a capability alone
cannot supply the universe membership required to read content.

Ordinary members use `access/subjects` to find sharing recipients. This bounded
search returns only names and identifiers of active principals and groups with
universe roles. It does not expose the administrative directory or grant the
ability to change membership.
