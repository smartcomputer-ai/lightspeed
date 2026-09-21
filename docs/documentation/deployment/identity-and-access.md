# Core identity and access records

The core `access` crate defines deployment-wide user/service identities, groups,
memberships, scoped role assignments, and explicit service capabilities.
`store-pg::PgAccessStore` persists them. Platform login credentials and external
identity mappings remain separate from these effective authorization facts.

This foundation is available through host administration commands. **The runtime
HTTP gateway and Platform still use the authentication/access behavior described
in [Authentication and access](authentication-and-tenancy.md).** Creating these
records does not yet enable runtime role enforcement, personal session privacy,
or API-key offboarding. Method access metadata describes the requirements for
that integration; clients must not treat the metadata as evidence of enforcement.

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
ownership or change its slug. Existing Platform/RPC creation paths are unchanged
until they acquire trusted core principal context.

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
separate deployment role and supplies no implicit universe membership. The initial
role matrix distinguishes creating work, controlling owned work, operating shared
resources, and managing access. In particular, controlling a session requires
ownership even for Admin; Operator/Admin may stop another session. Ownership
includes authorized controller lineage and must be resolved by the runtime, not
from provenance or client-supplied claims.

Service kind grants no capability. Credential leasing, inbound admission, user
assertion, channel-account discovery, and identity provisioning have explicit
capability assignments. Capabilities apply in their declared scope. Only deployment
administration can assign capabilities in this foundation. The `manage_identity`
capability delegates directory management, including membership and offboarding;
this is powerful authority because membership can affect privileged group roles.
It does not permit direct role/capability assignment or universe recovery.

Identity changes serialize on a deployment policy row. Authorization, the write,
the last-admin guard, revision increment, and audit append commit together. Guarded
role/membership removal accounts for active principals reached through groups.
Identity disablement can still remove the last administrator. An active
DeploymentAdmin can explicitly recover an orphaned universe by assigning a named
active principal, without becoming a member themselves.

Reads use current committed facts with no authorization cache. Multi-query reads
use a consistent snapshot. Audit stores access-change metadata, never credentials
or session content, and survives universe deletion and identity disablement.
Universe-scoped assignments cascade away on universe deletion; canonical service
identity records retain their management provenance.

These guarantees cover the core store and its host administration entry points.
They do not yet cover gateway request admission, streams, running sessions, bot
standing authority, or external effects.
