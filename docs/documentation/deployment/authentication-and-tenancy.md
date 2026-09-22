# Authentication and access

The runtime authenticates canonical principals from the [core identity
registry](identity-and-access.md). The Platform signs people in with Better Auth
and binds each account to an immutable canonical user UUID. Interactive calls
authenticate with the Platform service key and assert that user; the runtime
evaluates the user’s current permissions without adding service privileges.

## Gateway modes

| Mode | Request identity |
| --- | --- |
| `single` | Explicit local development service principal with deployment and configured-universe administration rights. Startup creates the configured universe and initializes this identity. Rejects bearer, universe and principal headers. Keep this development listener private. |
| `authenticated` | Requires `Authorization: Bearer lsk_…`. Reads the key, active canonical principal and current scoped rights on every request. |

`trusted-header` and `api-key` modes are retired. The greenfield baseline creates
canonical scoped credentials directly, without legacy keys or anonymous
principal records. Development databases from an earlier baseline require
recreation of the runtime schema and migration ledger before issuing new keys.

A universe key selects exactly its universe. An optional matching
`x-lightspeed-universe` is accepted; a different UUID is rejected. A deployment
key must select a universe with that header for universe/service calls. Neither
scope grants permissions. Deployment administration requires DeploymentAdmin;
service methods require their declared capability, never just service kind.
Key-management methods additionally check issuance authority or key ownership.

`x-lightspeed-principal: user:<canonical-uuid>` (or a bare canonical UUID) is an
assertion, not authentication. Only an authenticated service with `assert_user`
in the target scope may assert an active user. A deployment-scoped credential
may use deployment-wide `assert_user`. The request uses the user's permissions
alone, retaining both authenticated and acting identities in request context.
Missing context and duplicate identity headers fail closed.

**Current boundary:** the gateway and shared services enforce the universe role/action
matrix. Viewer is read-only. Contributors control their own sessions and manage
their own profiles/bots. Operator/Admin can manage bots and profiles, configure
resources, and stop other people's sessions, but cannot steer or delete their
personal sessions. Bot-controlled sessions follow bot-management rights; delegated
children follow explicitly admitted controller lineage. Metadata and provenance
do not grant control. Bot trigger secrets are visible only to managers.

Ownership reservations preserve the creator/controller across retries and deletion.
They are independent of execution credentials. Each reservation also records the
owning principal and managing bot of its control lineage, copied from the admitted
controller, so a permission check reads one row. There is no legacy ownership
backfill: content without trusted ownership cannot be claimed by retrying creation.

Each request is resolved once at the API boundary: the presented key and its
principal, an asserting service's `assert_user` capability, and the acting
principal's roles and capabilities in the addressed scope. Handlers decide from
that resolved context; nothing is cached across requests, so every new request
sees committed status, membership, capability and key changes.

Session content remains universe-visible. Only a parked request can outlive a
change: an event long poll revalidates while it waits (at most 250 ms between
polls). Every identity, role, capability and key change, and every universe
removal, advances one policy revision, so an unchanged revision proves the
resolved context still holds and the recheck is a single read; a changed revision
re-resolves the caller. A reader that loses its credential gets `unauthenticated`,
one that loses its rights gets `forbidden`, and the wait ends without content. A
request that was admitted and is already reading is not rechecked, and a response
handed to the transport cannot be recalled.

Live transcripts use these bounded long polls, not persistent user-content
sockets. Environment daemon connections retain their separate authentication
boundary. Private-session policies and standing execution authority remain
pending: access revocation does not stop admitted runs or external processes.

## Durable access audit

The baseline defines two access-audit tables. The purpose is
accountability for authority changes and significant actions, not a second copy
of the session event log.

| Table | What is written |
| --- | --- |
| `access_audit_changes` | One committed identity, group, membership, role, capability or API-key change, in the same transaction as its policy revision. No-op changes, including repeated key revocation, add no change row. |
| `access_audit_events` | One row per call of an audited API method, with its outcome, and one row per refusal of an authenticated caller. |

Every API method declares `audit: true` or `audit: false` next to its `access`
requirement, so a new method cannot be added without deciding. Audited methods
include run admission, cancellation, approvals, session
configuration/closure/deletion/retention, bot and trigger configuration, MCP and
integration configuration, credential import/revocation/binding, environment
administration, channel configuration, workspace deletion, and deployment
mutations (identity/key administration, universe creation/deletion,
provider/binding configuration and environment adoption). A run produces one
record; it does not produce an audit event for every model iteration or tool call.

Each audited call writes exactly one row at the API boundary after it completes:
`succeeded`, or `failed` with the error category. A refusal for reasons of state
(`rejected`, `conflict`, `not_found`) is a failed operation, never a denial. A
`denied` row is written whenever an authenticated caller is refused
(`forbidden`), on any method, including a caller whose authority is revoked while
its request is parked. A successful API permission change therefore adds one
event row and one committed change row.

Callers without a valid credential (`unauthenticated`: missing, malformed,
unknown or revoked keys, and keys of disabled principals) are logged but leave no
row, so an anonymous caller cannot make the deployment write. Successful reads,
inventories, self queries, permission previews, transcript polls, session
creation/renaming/context edits, profile editing, blob/snapshot writes, workspace
head updates, credential leasing, MCP discovery, and routine bot/channel ingress
add no rows. Internal work (bot activities, delegated sessions, reapers) is not
an API caller: it is attributed in domain events and logs, not in this table.
Normal MCP tool execution uses session history; a tool calling an audited
Lightspeed administration API is audited as that API operation.

Event rows hold the authenticated principal, the acting principal (an
unauthorized assertion never attributes the claimed user), a non-secret
credential reference (the key's display prefix), the method, the universe, the
policy revision the decision was made under, selected target identifiers, the
outcome and the error category. Target identifiers come from a fixed list of
parameter names; a malformed identifier or a key prefix that is not a display
prefix is dropped rather than stored. Events exclude bodies, credential values,
display names, endpoint URLs, metadata and session content. Committed change
records contain typed identity change metadata, such as group display names, but
no credential values or session content. Both tables survive target deletion and
offboarding, and events are indexed by time, acting principal and universe.

Event rows are best-effort by design: a failed write is logged as an error and
never changes the response, so an audit outage cannot block stopping work or
discard a committed result such as a newly minted key. Permission and key changes
do not depend on it: their change row commits in the same transaction as the
change, or not at all. Retention, pruning, export and a reader are deferred until
this auditing policy has been exercised; no automatic audit cleanup is
implemented.

## Issue a key for an API client

Run the server CLI from a trusted administrative environment with the runtime
database configured. It does not require Temporal or Platform:

```bash
lightspeed-server migrate
lightspeed-server identity bootstrap \
  --principal-id "<admin-uuid>" --display-name "Administrator"
lightspeed-server universe create --slug acme --creator-principal "<admin-uuid>"
lightspeed-server api-key create --universe-id "<universe-uuid>" \
  --principal "<principal-uuid>" --actor-principal "<issuer-uuid>" --name acme-client
lightspeed-server api-key create --deployment \
  --principal "<service-uuid>" --actor-principal "<admin-uuid>" --name platform
lightspeed-server api-key list
lightspeed-server api-key revoke "<key-prefix>" --actor-principal "<issuer-uuid>"
```

Create users/services and assign roles or capabilities through `identity apply`
or authenticated `deployment/identity/apply`. Both use the same audited core
mutation rules. Universe creation assigns the acting creator as universe Admin.

Members may issue their own universe keys. Universe Admin may issue keys for
services managed in that universe, but cannot impersonate other users or mint
keys for deployment/integration services. DeploymentAdmin may issue deployment
keys and manage principals across the deployment. Key metadata records the bound
principal and issuer separately; plaintext appears only at creation.

## Platform and connectors

Set `LIGHTSPEED_PLATFORM_API_KEY` to an explicitly provisioned Platform service
key and `LIGHTSPEED_API_URL` to the authenticated runtime `/rpc` endpoint. The
Platform sends this key only to the configured runtime URL; per-universe endpoint
overrides cannot receive a credential for another endpoint. Provision a
deployment-scoped service key with `assert_user` and `manage_identity`
capabilities. The service needs no universe roles or DeploymentAdmin role.
`manage_identity` is used only when provisioning an account through the login
adapter; interactive identity administration asserts the administrator as usual.

Bootstrap core first, then set `LIGHTSPEED_PLATFORM_ADMIN_PRINCIPAL_ID` to the
active core user holding DeploymentAdmin, alongside
`LIGHTSPEED_PLATFORM_ADMIN_EMAIL` and `LIGHTSPEED_PLATFORM_ADMIN_PASSWORD`.
Platform verifies that principal before creating the first login; these settings
apply only while its users table is empty and never reset passwords or core roles.
The full development launcher provisions a separate named user and service key.

Platform stores login credentials, external account mappings, user profiles and
universe display/routing metadata. It stores no roles, groups or memberships.
Admin → Users manages local accounts, direct deployment roles and effective
status; Admin → Groups manages deployment groups, membership and role assignments.
Universe Admins grant the four universe roles to accounts or groups under Members.
Inherited deployment roles remain visible and are changed through their group.
Adopting a universe adds display metadata only; it never grants content access.
Universe creation assigns its acting creator the core Admin role.

Core membership/status changes affect both browser requests and direct runtime
keys at the next admission. Password resets revoke Platform login sessions.
Account email/name edits preserve the canonical principal ID. Browser-supplied
principal headers and profile edits cannot change that mapping.

The greenfield Platform identity migration requires an empty Platform database;
it refuses to infer identities or copy legacy permissions. Re-provision accounts
and universe links explicitly after resetting a disposable pre-release Platform
database. Runtime records are not migrated or reset by this Platform migration.

Connectors use their own `LIGHTSPEED_CONNECTOR_API_KEY`. Assign deployment
`discover_channel_accounts`, and per-universe `lease_credentials` and
`admit_channel_inbound` capabilities as needed. Do not substitute a forged
service principal header. Connectors receive neither `assert_user` nor general
administration by default.

## Configurator and development

Configurator uses `authenticated` mode and forwards its bearer credential and
optional universe/user assertion to the runtime for validation. Its host/origin
checks complement authentication. The Platform setup creates a universe-managed
Configurator service with an Operator assignment and a universe-scoped key,
then stores that key in an outbound auth grant. The old loopback trusted-header
path is retired.

`./dev.sh full` defaults to authenticated mode. When no Platform service key is
configured, it explicitly initializes the local development principal and mints
a launcher key, passing the secret to child processes in memory. `runtime`
defaults to single mode. `identity development --universe-id <uuid>` is a
host-only development bootstrap, never an authenticated gateway fallback.

See [multitenancy](multi-tenancy.md), [self-hosting](self-hosting.md), and the
[environment reference](../reference/environment-variables.md) for deployment
and service configuration.
