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

## Audience and control

A universe role establishes what kinds of work a person may do. Each session,
bot and collection also has an owner and an audience. These answer a more
specific question: which of this universe's work may this person read or control?
A Viewer can read work in their audience. A Contributor can create work and
control sessions they own or have a write grant on. An Operator's ability to
configure shared resources does not grant control of another person's standalone
session.

The audience has two visibility settings. `universe` lets every current universe
member read. `restricted` limits ordinary reads to the owner and people or groups
with a `read` or `write` grant. A grant does not supply universe membership: its
subject must still hold a role there. Restricted work outside a caller's audience
is omitted from lists and returns `not_found` when read directly. Admins follow
the same reading rule, with the explicit [private-content capability](#private-content-access)
as a separately assigned exception.

A bot's conversations and a session's delegated children inherit the same
policy, so a new conversation does not need a separate round of sharing.
The API also supports collections, which give several sessions and bots one
audience and execution identity. Collections are deferred in the web app;
create and share sessions or bots directly there. Through the API, resources
can join a collection only at creation, and a collection can be deleted only
once it is empty.

The resource whose policy governs this audience is called its *root*. A
standalone session or bot is its own root; a member of a collection uses the
collection's root. The **Access** dialog shows the owner, visibility, grants and
execution identity. For bot conversations and delegated sessions, it links to
the governing bot or session and explains that changes apply to everything
sharing that policy. Writers can add readers and change visibility. Only the owner can change writer grants. The web app offers
read-only sharing for personal work.

Ownership, reading and operational control remain separate. Operators and Admins
may stop sessions, including restricted ones, without receiving their contents.
Admins may delete sessions and empty collections under their lifecycle rules.
Managers of a visible bot can control its conversations. These governance rights
do not turn an administrator into the owner of the underlying work.

The runtime resolves each resource's recorded identity, root policy and caller
grant together. Client metadata cannot claim ownership or controller authority.
A missing root policy denies access. `access/policy/read` returns the governing
policy; `access/policy/put` replaces its visibility and grants. Use
`expectedRevision` from the read to protect edits against concurrent changes.
The web dialog does this and asks you to reload after a conflict.

### Create and share work in the web app

1. Open **New session** or **New bot**. Choose **Running as** and **Who can read**.
   New work defaults to the universe service and universe visibility; choosing
   **Me** defaults its audience to restricted.
2. Open **Access** on the resulting session or bot. Search for people or groups
   in the universe, add their grants, then save. A control grant lets another
   person start runs under the work's existing execution identity. Bot
   conversations and delegated sessions inherit access and execution.
3. For work running as the universe service, its owner can transfer the entire
   root through **Transfer ownership**. The former owner retains only access
   supplied by visibility, explicit grants or their role. Personal work cannot
   change owners.

The sharing search exposes names and identifiers, with at most 100 matches per
query. It is available to ordinary universe readers through `access/subjects`;
the administrative identity directory remains restricted to administrators.

## Execution authority

The person asking for a run and the principal carrying out the work can be
different. **Running as** makes that distinction visible. A shared control grant
allows someone to ask for work under the root's execution identity; it does not
replace that identity with the requester.

By default, work runs as a dedicated universe service. The runtime creates this
keyless service principal when it is first needed and assigns it Contributor in
that universe. Its authority survives a change of owner, which allows shared
work to continue when a person leaves. The service is disabled when the universe
is deleted. It is not a general credential that clients can borrow.

A universe Admin can enable personal execution under **Settings → General →
Execution**. A person can then choose **Me** when creating a root. That work runs
as its owner, and sharing control does not change who it runs as. The execution
choice is fixed at creation. Collection members, bot conversations and delegated
children inherit their root's choice. Turning off personal execution prevents
new personal roots; existing ones retain their identity.

At run admission, and before each model call, the runtime checks that the
execution principal is active and still has `UseResource` in the universe.
If the admission check fails, the run is refused. If authority is lost during a
run, the next model call fails with `authority_revoked`; the session remains
open. For example, reducing a personal owner's role to Viewer removes the
ability to use resources even though the person may still read the universe.
Disabling an owner does not stop work that runs as the universe service.

These checks happen at defined boundaries. They do not interrupt an already
admitted model call or undo a tool's completed effects, and they do not terminate
external jobs automatically. Offboarding and shutdown procedures still need to
account for those processes. Runtime controllers also remain bounded to their
own work: sharing an execution service does not give one session control of a
sibling session.

## Content reads and attachments

Large messages and files live in content-addressed storage. Knowing a content
hash identifies bytes; it does not grant permission to read them. A caller uses
`blobs/read` or `blobs/has` with a `resource` naming a readable session, bot or
collection. The blob must also be admitted content of that resource. Without a
resource, these methods expose the caller's own uploads and built-in engine
blobs. Uploading bytes that are already stored grants that uploader access to
those bytes without making other content readable.

Admission checks apply when a caller supplies references in run input, context
edits and snapshot manifests. References in content fields must be permitted at
that boundary. A digest written into ordinary text does not become admitted
content merely because it resembles a reference. The web app supplies the
session context when fetching full message text or media.

Workspaces and execution environments still have universe-wide visibility.
Attaching one to a restricted session does not restrict its audience. If the
investigation writes a report into a shared workspace, other universe members
can read that file through the workspace. The attachment editor states this
because it is a choice about where the work's output will be visible.

`vfs/workspaces/files/read` resolves a file path in the workspace's current head.
When editing, `vfs/snapshots/commit` can name `sourceWorkspaceId` to retain files
already in that readable head; other references still need admission. Workspace
creation and updates check file references even when the manifest was uploaded
as raw bytes. Snapshot-manifest reads remain universe-scoped, so restricting a
session does not make its workspace manifests private.

## Revocation while reading

Each request resolves the presented key, its principal, any asserted user, and
the acting principal's current scoped rights at the API boundary. Those facts
are not cached across requests. A committed change therefore applies to the
next request, including requests made through Platform.

A parked transcript long poll revalidates while it waits, at most 250 ms between
checks. Identity, membership, role, capability and key changes advance a policy
revision; an unchanged revision makes the check a single read. When the revision
changes, the runtime resolves the caller again. Resource sharing is checked too.
A revoked credential produces `unauthenticated`; lost authority can produce
`forbidden` or hide the resource with `not_found`. The wait ends without content.

A response already handed to the transport cannot be recalled, and downloaded
content is not erased by revocation. Live transcripts use these bounded long
polls rather than persistent user-content sockets. Environment daemon
connections have their own authentication lifecycle.

## Private-content access

Sometimes a person needs to inspect restricted work that has not been shared
with them. `read_private_content` is an explicit, universe-scoped capability for
that purpose. It can be assigned to a user principal, never a group or service.
The person must still be active and have a role that permits universe reads.
Neither Admin nor DeploymentAdmin implies this capability.

A universe Admin can grant or remove it in **Settings → Members → Edit role →
Private-content access**. The control applies immediately and independently of
the role edit. The same change is available through `deployment/identity/apply`
or the administrative CLI as `assign_capability` or `revoke_capability`, with the
universe scope, the person's canonical `principalId`, and capability
`read_private_content`. Deployment administrators can administer the assignment
too. Each change uses the ordinary attributed, audited identity-change path.

When a read depends on this capability, the runtime records an access audit event
with `privileged = true`. A read the person could already make through ownership,
visibility or a share remains an ordinary read. The successful HTTP response
carries `x-lightspeed-privileged-read: true` only in the first case. Platform
preserves that indication, and the web app labels affected views **Privileged
read**. On a list, the marker means the returned page includes content read this
way; it does not mean every item required the capability.

The capability grants reading, not control, sharing or ownership. Any stop or
delete authority comes from the person's role and the ordinary operation rules.
Removing the capability restores normal audience checks on subsequent reads.
The audit event's write is best-effort, as described below; the UI marker reports
the authorization decision, not a guarantee that the audit database write
succeeded.

## Durable access audit

The baseline defines two access-audit tables. The purpose is
accountability for authority changes and significant actions, not a second copy
of the session event log.

| Table | What is written |
| --- | --- |
| `access_audit_changes` | One committed identity, group, membership, role, capability or API-key change, in the same transaction as its policy revision. No-op changes, including repeated key revocation, add no change row. |
| `access_audit_events` | One row per audited API call, authenticated authorization refusal, or read that relied on private-content access, with its outcome. |

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
row, so an anonymous caller cannot make the deployment write. Ordinary successful
reads, inventories, self queries, permission previews, transcript polls, session
creation/renaming/context edits, profile editing, blob/snapshot writes, workspace
head updates, credential leasing, MCP discovery, and routine bot/channel ingress
add no rows. Reads that rely on `read_private_content` are the exception: they
write one event with `privileged = true`, including list pages and transcript
polls whose authorization needed it. Internal work (bot activities, delegated sessions, reapers) is not
an API caller: it is attributed in domain events and logs, not in this table.
Normal MCP tool execution uses session history; a tool calling an audited
Lightspeed administration API is audited as that API operation.

Event rows hold the authenticated principal, the acting principal (an
unauthorized assertion never attributes the claimed user), a non-secret
credential reference (the key's display prefix), the method, the universe, the
policy revision the decision was made under, selected target identifiers, the
outcome, the error category and whether a read relied on privileged access. Target identifiers come from a fixed list of
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
