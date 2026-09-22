# P176 — Identity foundation and universe authorization

**Status:** Implementation steps 1–4 complete, including Platform directory
integration, response-time revocation and significant access auditing. First slice of
[enterprise authorization and identity](later/pNNN-enterprise-authorization.md).
Core contracts, method classification, authenticated request contexts and scoped
service keys, ownership and action enforcement are implemented. Platform
identity integration now uses core records. Acceptance criteria remain deferred.

Lightspeed is still greenfield. Reshape contracts and replace implicit defaults
where needed; compatibility with the current authorization model is not a goal.

## Outcome

A person's universe permissions apply consistently through the Platform and
direct API access, with trustworthy attribution of actions. Local users and
groups are enough to deliver this foundation; enterprise directories connect to
the same identity lifecycle later.

The starting point was Platform-owned permission checks and runtime principals
used mainly for attribution. This slice makes runtime authorization authoritative
for universe access. Platform membership changes now use those same core records.

## Ownership

| Layer | Responsibility in this slice |
| --- | --- |
| Platform | Login credentials/sessions, external identity mapping, SSO/provisioning integration, user profiles, and administration UI. |
| Core authorization subsystem | Canonical principal IDs and effective status, groups/memberships, role assignments, service capabilities, permission evaluation, and access audit records. |
| Runtime gateway/shared services | Trusted request context and enforcement across every exposed access path. |
| Persistence/API | Runtime records through `store-pg`; public contracts and method classifications through `api`. |

Platform, CLI, and future provisioning integrations use the same core identity
and access contracts. There is no separate Platform-owned authorization directory
to project into runtime. External directories may own imported membership; their
connectors still need reliable synchronization into these effective local records.
The deterministic engine performs no identity or policy I/O. Core contracts and
role evaluation live in `access`; `PgAccessStore` owns transactional persistence.

## Scope

1. **Identity and administration.** Store stable user/service principals, local
   groups, memberships, and effective active/disabled status in core. Provide
   Platform administration and headless principal/group/role commands. CLI or
   installer bootstrap creates the first deployment administrator. Development
   `single` mode uses an explicit local principal with universe and deployment
   administration rights; authenticated mode never falls back to it. Remove
   `PrincipalKind::UniverseDefault`; missing actor context is an error.

2. **Roles and recovery.** Assign Viewer, Contributor, Operator, and Admin within
   universes; distinguish DeploymentAdmin and scoped service capabilities.
   Universe creation atomically assigns its creator as Admin. Guard deliberate
   removal of the last usable administrator, accounting for group membership,
   without preventing identity disablement. Deployment administrators can repair
   orphaned universe assignments through an audited operation. Authenticated
   self-queries expose only the caller's accessible universes/effective rights.

3. **Authenticated services and keys.** Replace bare `trusted-header` trust with
   authenticated service keys; retain development `single` and authenticated
   modes. Platform may assert an active user only with a scoped `assert_user`
   capability. Check the user's permissions without adding Platform's service
   privileges, and attribute both identities. Connectors and Configurator receive
   only the scope/capabilities they need. Credential leasing, inbound admission,
   and identity administration require explicit capabilities, never service kind.

4. **Ownership and attribution.** Record trusted `created_by` for sessions, bots,
   and profiles, and actor facts for creation, run admission, control, and config
   changes. Contributors control their own sessions. Operator/Admin roles allow
   stopping other users' sessions, while submission/steering requires ownership;
   they retain normal rights over their own work. Bot-owned sessions follow bot
   management; delegated children follow their authorized controller lineage.
   Provenance or a fork relationship alone does not grant control. Shared writers
   arrive with session access policies in the next slice. Actor facts do not
   become mutable policy in the engine; their event/envelope placement stays open.

5. **Complete enforcement.** Make `access` mandatory in API method declarations,
   covering scope plus authenticated/action/ownership/capability requirements.
   Export it in the manifest; an omitted classification is a compile error.
   Gateway checks and contextual shared-service checks cover direct/internal
   calls, lists, reads, streams, and mutations. A method matrix establishes
   classification coverage, not proof that ownership or each entry path is safe.
   Daemon endpoints retain explicit daemon authentication and environment-state
   checks rather than inheriting user/service API admission.

6. **Audit and UI.** Add a small groups/roles UI driven by core permissions.
   Persist audit records for identity, role, key, capability changes, denials,
   run admission and consequential configuration/control/deployment operations;
   keep routine successes quiet and retain actor attribution on other domain
   actions and traces. Audit survives session deletion and offboarding without storing
   credentials or session contents. Later sensitive reads and exceptional
   private-content access must support durable access auditing too.

Keys authenticate a principal within a universe or deployment scope; scope limits
the principal's current authority. Members may mint their own universe keys;
minting for a service principal requires universe Admin and authority to manage
that principal in scope. Universe Admin cannot mint keys for deployment/integration
principals. Deployment-scoped keys require DeploymentAdmin and authority over the
bound principal. Record key creator separately from bound principal. CLI key
creation requires an explicit principal. Key scope supplies no permissions itself.

Internal events carry `Internal { component, cause }` attribution and a reference
to the admitted configuration. Internal actors hold no roles and are not an
authorization bypass. For existing automation in this slice, checked configuration
admission is the temporary authority boundary; internal service paths must be
explicit. Standing execution authority and effect-time revocation follow later.

## Contract sketch

Illustrative shapes, not final wire signatures:

```text
Principal = id + kind(user | service) + effective_status
RoleAssignment = scope(deployment | universe) + subject(principal | group) + role
AuthenticationReference = credential_reference + authenticated_principal
RequestContext = rights(acting_principal, target_scope, roles, capabilities, policy_revision)
               + authentication_reference + credential_scope
ResourceOwnership = created_by + controller + owner + bot
Actor = Principal(id) | Internal(component, cause)

authorize(context, action, resource) -> decision + reason + policy_reference
```

The acting principal is the authenticated caller or a user asserted through an
authorized service. Identity/access administration is itself privileged. Internal
attribution is distinct from authenticated request context and future execution
authority. Credential references identify a key or explicit local-development
authentication, never secret values. The gateway resolves the context once per
request, including the acting principal's rights, and installs it as a fallible
task-local around dispatch; spawned tasks do not inherit it implicitly. Handlers
decide roles and capabilities from that context and read storage only for
ownership.

## Revocation boundary

No authorization caches: every request resolves its key, principal status,
assertion capability and effective rights from committed state (two or three
statements), so a committed local change is visible to every subsequent request.
External directory changes still require delivery and reconciliation; moving
local ownership into core does not remove that work.

Only a parked request can outlive a change. Transcript delivery uses bounded long
polls, and a waiting poll revalidates at most every 250 ms. Every identity, role,
capability and key change, and universe removal, advances the policy revision, so
the recheck is one read of that revision; only a changed revision re-resolves the
caller. A revoked reader ends without content (`unauthenticated` for a lost
credential, `forbidden` for lost rights). A request that is already reading is not
rechecked and already-sent responses cannot be recalled: the former closes only a
millisecond race and cost three full re-resolutions per read. There are no
persistent user-content streams; environment daemon connections retain their
separate authentication boundary.

This revokes access to the API and content. Stopping already admitted execution,
revoking standing bot authority, and cancelling external processes belong to the
execution-authority slice. Offboarding a configuring operator must not implicitly
revoke an independent service identity.

## Implementation order

1. [x] Rename deployment-facing `operator/*`, scope/types, and consumers to
   `deployment/*` in a separate mechanical commit, regenerating contracts. Keep
   Operator as the universe role; qualified binding names need no rename.
2. [x] Define core identity/access contracts, persistence, bootstrap, and the initial
   method/action mapping. Reserve credential grant terminology for `auth/grants/*`;
   use access policy/resource permission for authorization.
3. [x] Introduce service authentication, key scopes, explicit contexts, and ownership
   facts; enforce checks across gateway and shared-service entry points. Authentication,
   scoped keys, contexts, caller migration, ownership and action enforcement are complete.
4. [x] Connect Platform and CLI administration to core records, removing competing
   Platform-owned authorization state. **Directory integration complete:** canonical
   account mapping, authenticated user assertions, core membership/group/role APIs,
   administration UI/CLI and bootstrap. Response-time revocation for the current
   long-poll transport and the remaining request/deployment audit surfaces are complete.

## Implemented foundation

- Mechanical deployment API rename committed separately (`72d7b332`), including
  generated contracts, TypeScript consumers, connector discovery, and docs.
- `access` defines stable UUID principals, flat deployment groups, scoped roles,
  service capabilities, typed access changes, effective rights, and the initial
  role/action matrix. Session control still requires ownership for every role;
  Operator/Admin may stop other sessions.
- The identity baseline stores these records. Identity writes authorize and commit
  with revision/audit updates and group-aware last-admin guards. Disablement may
  orphan a universe; explicit recovery assigns an active principal without
  giving the recovering administrator membership. No authorization cache.
- Host CLI supports one-shot bootstrap, typed identity changes, effective-rights
  queries, and accessible-universe queries. CLI universe creation requires a
  named deployment administrator and atomically assigns them universe Admin.
- Every API declaration requires `access:` metadata; scope derives from it.
  Manifest, OpenRPC, API reference, and generated TypeScript carry the mapping.

The gateway now authenticates canonical principals through scoped keys, checks
current membership and service/deployment capabilities, and carries an explicit
request context. `single` uses a named local development identity; no implicit
principal remains. Bare trusted headers are retired. Assertions require scoped
`assert_user`, retain both identities, and never union their permissions.

The identity baseline defines canonical scoped API keys directly. There is no
legacy-key archive, replacement table or compatibility path. Issuance
checks the named creator's current authority, credential ceiling and principal
management scope transactionally; key creation/revocation is audited.
Platform and connectors send service credentials. Configurator setup provisions
a universe-managed service and key. Platform now asserts its mapped user and
uses core access records; the following cutover replaces its former directory.
A small authenticated identity-mutation endpoint supports service provisioning.

A separate ownership baseline defines immutable resource reservations; the audit
baseline stores significant action admission attribution. Creation records the trusted principal or internal
controller cause; retries cannot transfer ownership. Deleted content retains its
reservation. No legacy ownership inference or backfill is provided.

Every universe service method checks its declared action against current rights.
Viewer mutations fail; Contributor control resolves ownership; Operator/Admin may
stop personal sessions but cannot steer/delete them. Bot control follows management
rights, and delegated children follow separately admitted controller edges, never
metadata, history forks or origin alone. Profile upserts check creation or management
as appropriate. Cascade deletion checks every affected session. Trigger reads redact
webhook/pairing secrets for non-managers; read-only skill queries do not refresh state.
Bot activities carry authority tied to their Temporal controller; sub-agent admission
reserves the control edge before creation. Channel reply reads require the admitted
conversation binding and receive read-only authority for that session. Internal
actors do not inherit user roles.

Response-time revocation and request/deployment auditing are implemented below.
Content remains universe-visible;
private access and standing execution authority are follow-ups.
The host CLI's `--actor-principal` is trusted database administration, not a remote
identity assertion. See [core identity administration](../documentation/deployment/identity-and-access.md).

### Platform integration

- Each local or externally provisioned login stores one immutable canonical user
  mapping; email/name edits cannot rebind it. Credentials, external accounts and
  human profiles remain in Platform. Provisioning never matches identities by email
  or silently reactivates a disabled principal.
- Every interactive runtime call authenticates the service and asserts the user,
  including deployment operations, keys and Configurator installation. Missing
  request identity fails closed; concurrent users have isolated request contexts.
- Core self-access and scoped administrative directory queries complement typed
  access mutations. They enforce credential ceilings and current authentication.
  Universe Admins manage universe role assignments and universe-owned services;
  deployment groups and group memberships remain deployment administration.
- Platform removes Better Auth organizations, memberships and admin-role state.
  Users/Groups/Members administration and CLI commands write core records. The UI
  receives effective core roles; DeploymentAdmin never supplies universe membership.
  Adoption adds metadata only, while creation assigns the real acting user Admin.
  Deployment provider-binding inventory has an administrative endpoint independent
  of universe content membership.
- Platform migration 2 deliberately requires a fresh Platform database; it neither
  imports permissions nor invents canonical identities for legacy accounts.
  Bootstrap binds a pre-provisioned core administrator. The full development
  launcher provisions separate user and service identities. Platform startup
  waits for the configured runtime's HTTP health endpoint before bootstrap,
  including in the platform-only profile. This prevents fresh-database bootstrap
  racing Rust compilation; unavailable runtimes stop dependent startup.

Validation for steps 1–2 (2026-09-21):

- `cargo check --workspace --all-targets` passed.
- Core/API/store tests: 111 passed, including schema-artifact checks. Server
  library/CLI tests: 345 passed; one existing library test remained ignored.
- Strict Clippy passed for `access`, `api`, and `store-pg`, including all targets.
- `npm install`, contract/client regeneration, and `npm run check` passed.
- Release schema metadata, changed-document links, formatting, and whitespace
  checks passed.
- Live validation passed against disposable PostgreSQL 17.10: identity lifecycle,
  migration locking/checksums, and universe-scoped API-key management (three tests).
  A separate fresh database passed 34 server CLI checks covering explicit migration,
  bootstrap retries, creator ownership, groups, denied mutations, last-admin
  rollback, disablement, orphan recovery, and scoped capabilities. Existing databases
  and the local `.env` were not used; the disposable container was removed afterward.

Validation for request contexts and scoped authentication (2026-09-21):

- Workspace/all-target compilation passed. Affected Rust unit/CLI suites:
  567 passed, one existing ignored. Strict Clippy passed for access/auth/API/store.
- Four serialized live suites passed on disposable PostgreSQL 17.10, covering
  migrations, identity, scoped key management and authentication. Cases include
  assertions without privilege union, scope ceilings, disablement, membership/key
  revocation, provisioning retries, and disabled-bootstrap rejection.
- Eleven actual server CLI checks passed against a fresh disposable database.
- `npm install`, regeneration and full `npm run check` passed, including 529
  consumer tests and builds. Generated-file checks used a temporary Git index
  containing the regenerated artifacts; the user's index was unchanged.
- Documentation checks/build, release metadata, formatting and whitespace passed.
  Disposable databases were removed; existing services and provider credentials
  were not used for live validation. Temporal workflow live suites were not run.

Validation for ownership and action enforcement (2026-09-21):

- Affected Rust unit suites: 442 passed, one existing ignored. Workspace/all-target
  compilation and strict Clippy for access/store/server passed.
- Twenty live tests passed on disposable PostgreSQL and Temporal: four storage
  suites, authentication, authenticated HTTP/direct-service authorization, five
  bot scenarios, six sub-agent scenarios, and three channel scenarios. Models and
  connectors were fake/scripted; no external provider credentials were used.
- The HTTP matrix checks every mutating/service route against Viewer, cross-user
  control and retry denials, elevated stop-only rights, profile ownership/upserts,
  bot-secret redaction, direct-service enforcement and membership revocation.
  Storage tests cover concurrent ownership reservation and explicit controller chains.
- API/TypeScript regeneration, `npm install`, full `npm run check` (529 consumer
  tests and builds), documentation checks/build, release metadata and formatting
  passed. Generated checks used a temporary Git index; the real index was unchanged.
- Private-session/standing-execution semantics and response-time revocation were
  not claimed or tested by that ownership-only validation run.

Validation for Platform integration (2026-09-21):

- 444 affected Rust library/CLI tests passed, one existing ignored. Strict
  all-target Clippy passed for access/API/store/server.
- Full `npm run check` passed: 532 consumer tests, TypeScript checks, generated
  artifacts and live/demo builds. Separate request-context tests check parallel
  user isolation, mandatory attribution and endpoint confinement.
- The new cross-stack live test passed 62 HTTP checks using actual Better Auth
  login, Platform routes, authenticated runtime, PostgreSQL and Temporal with
  scripted worker adapters. It checks ownership attribution for two concurrent
  users, spoof rejection, core role/group revocation, disabled identities,
  password-session revocation, key ceilings, creator Admin, last-admin protection,
  content/admin separation and Configurator installation/retry. External-login
  adapter provisioning is also exercised; no external identity provider is contacted.
- Three core storage live suites and the authentication live suite passed.
  Platform migration checks passed fresh install, empty-ledger upgrade, and
  rejection/preservation of populated legacy authorization state.
- Documentation checks/build, release metadata, formatting and whitespace checks
  passed. Tests used disposable services and no model/provider credentials.
  Response-time reauthorization and the remaining access-audit surfaces are
  tracked separately below; UI action affordances are covered by the follow-up implementation.

## UI action affordances

- Added a bounded `access/read` preview backed by core action/ownership policy,
  including delegated controllers and optional cascade-deletion checks. Platform
  forwards the authenticated user; the UI does not infer ownership from metadata.
- Session input, steering, approvals, settings, stop/cancel, close and delete use
  separate action decisions. Bulk controls count only permitted targets. Bot and
  profile creation is available to Contributors; editing follows ownership or
  Operator/Admin rights. Resource configuration and use remain distinct.
- Permission hints are isolated by signed-in account, refreshed on mount, on focus
  and after mutations (no background polling), and fail closed on lookup errors. Runtime enforcement
  remains authoritative; these hints do not add private/shared session semantics.
- API-key controls expose members' own keys and eligible universe-managed service
  principals for admins instead of inviting arbitrary principal IDs.
- User creation/editing presents deployment administration as an explicit switch,
  separate from universe roles. The user list shows effective administrator access;
  editing distinguishes direct grants from access inherited through groups.
- Universe Members provides role editing for users and groups. A core atomic
  replacement preserves independent grants, rejects stale edits, and rolls back
  last-administrator demotions without partially adding or removing permissions.
  Validation passed: full `npm run check` (443 web tests), affected Rust library
  tests, two identity tests against disposable PostgreSQL, strict all-target
  Clippy for access/store/server, and documentation checks/build. The role editor
  covers successful user/group edits, rejected edits, and permission revocation.

Validation for UI action affordances (2026-09-21):

- Full `npm run check` passed: 576 consumer tests (439 web), TypeScript checks,
  regenerated contract/client checks, and live/demo production builds. Generated
  checks used a temporary Git index; the real index was unchanged.
- 88 API tests and 2 focused runtime preview tests passed. The expanded ownership
  live suite passed against disposable PostgreSQL, covering foreign ownership,
  bot/delegated controller lineage, missing resources, cascade permission checks
  and absence of hypothetical mutation audit writes. The container was removed.
- Rendered UI tests cover read-only sessions, stop without control, mixed bulk
  selections, bot invoke versus manage, profile ownership, resource configuration,
  own-key issuance, failed lookups and permission changes. MCP discovery requires
  resource configuration permission even inside a writable session/profile editor.
- Formatting, whitespace and documentation checks passed.

## Revocation and remaining audit

- Event long polls reauthorize in the shared service while waiting and after
  projection, including quiet polls and backward history pages. HTTP also checks
  buffered universe reads before delivery. This revokes content access without
  cancelling already admitted execution.
- The access-audit baseline directly defines committed permission changes in
  `access_audit_changes` and security decisions in `access_audit_events`.
  No transitional request/action tables or history-copy steps remain.
  Deployment methods revalidate current credentials and roles on direct calls
  as well as through HTTP. Contextual key/identity checks remain in stores.
- An explicit significant-action policy keeps run admission, control/deletion/
  approval actions, configuration changes, denials and consequential deployment
  admission/outcome. Routine reads/inventories, credential leasing (including bot
  polling), MCP discovery, session naming/context edits, profile editing and
  storage housekeeping/ingress successes are quiet. Model/tool steps remain in
  session history.
  A run gets one admission, a deployment mutation two correlated event rows, and
  a propagated denial one event. Committed permission changes add their own
  transactional row; repeated key revocation does not add a change row.
  Retention/export/pruning remain deferred. See the
  [audit policy](../documentation/deployment/authentication-and-tenancy.md#durable-access-audit)
  for the current scope and limitations.
- Audit retains authenticated and acting identities, safe target identifiers and
  stable error categories, never raw headers, bodies, secrets, metadata or content.
  Committed identity changes retain typed change metadata.
  Records survive target deletion/offboarding. A deployment operation must persist
  admission before effects; admission is not success, and missing completion means
  an unknown outcome. Audit-outcome failure cannot roll back prior effects.
- Universe-scoped user assertions cannot escape into other identity scopes or
  enumerate access outside their assertion scope.
- Focused live tests passed 20 deterministic revocation races through HTTP and
  direct services, plus administrative attribution, denial/redaction, purge survival,
  assertion scope and fail-closed audit admission checks. Existing authentication,
  ownership/authorization, schema-migration and Platform identity live suites also
  passed; the latter exercised 62 HTTP checks. All used disposable PostgreSQL/
  Temporal with no external provider credentials.
- Audit simplification validation: nine live tests passed across seven suites,
  including zero new audit events for 50 successful routine calls, exactly one
  run admission, one denial per revocation attempt, two correlated events per
  significant deployment operation and no-op key revocation. Test services were disposable; provider credentials were absent.
- Final validation: 360 affected Rust library tests passed (one existing ignored),
  strict all-target Clippy passed for access/store/server, and documentation,
  release metadata, formatting and whitespace checks passed.

## Simplification pass

A review found the enforcement and audit layering to be where nearly all the
complexity and cost sat: identity was re-resolved three to five times per request
(about 67 statements for an ordinary read through Platform, 111 for an event read,
22 every 250 ms per parked long poll). This pass keeps the model and removes the
layering. It supersedes the earlier statements in this document about
response-time rechecks of buffered reads, admission/completion audit pairs and
fail-closed audit admission.

- **Resolve once.** `effective_access` is one SQL statement (one snapshot, no
  explicit transaction). Authentication is a read-only key-and-principal lookup,
  an `assert_user` check when asserting, and that statement. `RequestContext`
  carries the resolved rights; shared-service and deployment admission decide from
  it without re-resolving, and only ownership reads storage. A context is one
  request's snapshot. The HTTP-boundary and post-projection rechecks are gone;
  parked long polls revalidate against the policy revision as described above.
  Universe removal now advances that revision too.
- **Key usage is coarse.** `last_used_at_ms` is refreshed at most once a minute,
  so Platform's single service key is no longer rewritten on every request.
- **Distinct refusals.** `unauthenticated` (no valid credential) and `forbidden`
  (an authenticated caller lacks permission) are separate API error kinds;
  `rejected` again means only a refusal for reasons of state. Platform maps them to
  502/403/409 and the Configurator to 401/403/409.
- **One audit record.** `audit: true|false` is a mandatory part of every method
  declaration beside `access`. The gateway writes one best-effort row per audited
  call after it completes, and one `denied` row per refused authenticated caller;
  unauthenticated callers are only logged and cannot make the deployment write.
  The per-attempt task-local, dedup set, four stages, admission/completion pairs
  and the hand-maintained significant-method list are removed. Targets are taken
  from a fixed list of identifier parameters, so a pasted key secret is never
  retained. Permission and key changes keep their transactional change log. The
  events table has plain columns and time, actor and universe indexes.
- **Flat ownership.** A reservation stores the owning principal and managing bot,
  copied from the admitted controller's row, so ownership is one lookup with no
  lineage walk; a controller without ownership of its own cannot delegate. The
  controller authority loses its read-only variant and its second lookup, and
  reaches beyond itself only for a bot's own sessions. The permission preview
  decides every action of a resource from one lookup.
- **Removed.** Unaudited key revoke/list store methods without callers, the
  uncalled single-instance gateway entry point, the duplicate service-capability
  check, the repeated bot-activity authority preamble, the inlined credential
  ceiling predicate (now `AccessScope::permits`), and `RequestContext`
  deserialization.
- **Fixed on the way.** HMAC-verified bot webhooks leased their signing secret
  through the caller-authorized method and failed without a caller; ingest now
  leases from the admitted trigger configuration, with a live regression test that
  runs outside any request context. An unauthorized assertion no longer attributes
  the claimed user in the denial record. Two live suites that called services
  without a caller were given explicit callers.

Not changed here, and still open from the review: Contributor-reachable credential
use (bot poll triggers, the Configurator's Operator key), the `single` default auth
mode, GitHub account linking, DeploymentAdmin minting keys for other people (kept:
headless deployments onboard people this way), release of ownership reservations,
and replacing the permission preview with role-derived affordances (it is now one
lookup per resource and no longer polled, so it was kept). The action vocabulary and the
`CredentialManagement`/`Identity` split were left alone because they are part of
the exported contract and the UI.

Validation for the simplification pass (2026-09-21):

- Workspace/all-target compilation, formatting and strict all-target Clippy for
  access/auth/API/store/server passed. Unit suites: access 11, API 89, auth 85,
  store 10, server library 344 (one existing ignored). API contract, TypeScript
  client and Configurator tools were regenerated; full `npm run check` passed
  (590 consumer tests, type checks, live/demo builds). Release metadata is
  unchanged at schema revision 11; `010` and `011` were edited in place, so this
  remains a reset boundary for development databases.
- Live suites ran serialized on disposable PostgreSQL 17.10, Temporal and MinIO
  containers on separate ports; the developer's stack, databases and object store
  were not used. Store: identity 2, ownership 1, keys 1, migrations, sessions and
  bots. Server: authentication 1, authorization 1, revocation/audit 3, tenancy 2,
  Platform identity 1 (62 HTTP checks through Better Auth), bots 7, channels 3,
  sub-agents 7, profiles 2, sessions 12, runs 9, MCP 5, workflow-tool plugins 14,
  environment providers 2. Provider-backed cases used the repository `.env` keys.
- The revocation suite parks 16 long polls (HTTP and direct; role, group
  membership, group role, user/service disablement, assertion, user/service key)
  and requires each to end within three seconds with the right refusal kind and,
  through HTTP, exactly one attributed denial. The audit suite checks that 20
  bad-credential calls, an anonymous call and an unknown method add no rows, that
  each audited deployment mutation adds exactly one attributed row, that a state
  refusal is `failed` and never `denied`, that an unauthorized assertion does not
  attribute the claimed user, that a pasted key secret is not retained, that 50
  routine calls add no rows, and that an audit-table outage neither blocks an
  operation nor loses its result.
- Measured on the routine-traffic test with statement logging: 47 authenticated
  requests resolved identity with one key read and one rights read each (plus one
  assertion check when a service asserts a user), six ownership lookups and two
  key-usage stamps in total.

## Greenfield migration baseline

The runtime schema is consolidated from fifteen migrations into eleven domain
files. `007_identity_access.sql` includes canonical scoped API keys;
`010_resource_ownership.sql` keeps immutable control facts separate;
`011_access_audit.sql` creates the two final audit tables. Auth principal constraints
and independent environment lifecycle are folded into their original definitions.
Obsolete table creation/replacement, renaming, history copying and their upgrade
fixture are removed. The embedded and release revisions are both 11.

This is a reset boundary for development databases, not an upgrade path. Recreate
the runtime schema and migration ledger; keep checksum, locking and startup
verification intact. The normal local reset command also resets Platform and the
Lightspeed MinIO prefix. No existing user database was reset for this change.

A before/after catalog comparison on disposable PostgreSQL matched all 38 tables,
358 columns, 355 constraints and 91 indexes, plus identity-sequence configuration
and the initial policy row. Physical column order, constraint/index names and the
migration ledger are excluded from that comparison. All 10 store library tests
and seven focused live tests passed (migration/ledger checks, identity, keys,
ownership, revocation and audit). Store Clippy with warnings denied, documentation
build/checks, release metadata and formatting checks passed. Live services used
disposable PostgreSQL/Temporal with no provider credentials and were removed.

## Boundary and follow-up

Existing session content remains universe-visible to authorized readers. This
slice makes no private-session or resource-isolation promise, and its UI must not
present one. The agreed separation of administration from private content is
implemented when private sessions become available.

The next slice, [session access and execution authority](p177-session-access-and-execution-authority.md),
combines private/shared session access with one complete authorized execution
path: one stable execution identity per session, bounded run authority, and
turn-boundary checks. It must cover associated transcripts, streams, files, and reachable
execution paths together. Other follow-ups include individual resource
restrictions, full bot/delegation lifecycle enforcement, SSO/provisioning, and
external credential delegation.

Current seams: [authentication and access](../documentation/deployment/authentication-and-tenancy.md),
[Platform gateway](../../platform/server/src/routes/gateway.ts),
[runtime gateway](../../crates/temporal-server/src/gateway/http.rs),
[request principal](../../crates/temporal-server/src/gateway/principal.rs).
