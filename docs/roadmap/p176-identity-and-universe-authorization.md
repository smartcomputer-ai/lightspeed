# P176 — Identity foundation and universe authorization

**Status:** Steps 1–3 complete; step 4 pending. First slice of
[enterprise authorization and identity](later/pNNN-enterprise-authorization.md).
Core contracts, method classification, authenticated request contexts and scoped
service keys, ownership and action enforcement are implemented. Platform
identity integration remains pending. Acceptance criteria remain deferred.

Lightspeed is still greenfield. Reshape contracts and replace implicit defaults
where needed; compatibility with the current authorization model is not a goal.

## Outcome

A person's universe permissions apply consistently through the Platform and
direct API access, with trustworthy attribution of actions. Local users and
groups are enough to deliver this foundation; enterprise directories connect to
the same identity lifecycle later.

The starting point was Platform-owned permission checks and runtime principals
used mainly for attribution. This slice makes runtime authorization authoritative
for universe access. Platform membership changes remain separate until step 4
connects its user identities to core.

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
   and deployment operations; retain actor attribution on other domain actions
   and traces. Audit survives session deletion and offboarding without storing
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
RequestContext = acting_principal + authentication_reference + target_scope
Actor = Principal(id) | Internal(component, cause)

authorize(context, action, resource) -> decision + reason + policy_reference
```

The acting principal is the authenticated caller or a user asserted through an
authorized service. Identity/access administration is itself privileged. Internal
attribution is distinct from authenticated request context and future execution
authority. Credential references identify a key or explicit local-development
authentication, never secret values. The gateway installs a fallible task-local context around dispatch; spawned
tasks do not inherit it implicitly.

## Revocation boundary

Start without authorization caches: key, status, membership, and permission
checks read authoritative committed state. A committed local change is visible
to subsequent access decisions. External directory changes still require delivery
and reconciliation; moving local ownership into core does not remove that work.
Existing streams/long polls must recheck before delivery or stop within a declared
bound. Current transcript delivery uses bounded long polls; admission alone does
not reauthorize an in-flight response. Exact queries and the bound stay open.

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
4. [ ] Connect Platform and CLI administration to core records, removing competing
   Platform-owned authorization state. Complete revocation, streams, and audit.

## Implemented foundation

- Mechanical deployment API rename committed separately (`72d7b332`), including
  generated contracts, TypeScript consumers, connector discovery, and docs.
- `access` defines stable UUID principals, flat deployment groups, scoped roles,
  service capabilities, typed access changes, effective rights, and the initial
  role/action matrix. Session control still requires ownership for every role;
  Operator/Admin may stop other sessions.
- Schema revision 11 stores these records. Identity writes authorize and commit
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

Schema revision 12 replaces the old key table outright and requires canonical
credentials. There is no legacy-key archive or compatibility path. Issuance
checks the named creator's current authority, credential ceiling and principal
management scope transactionally; key creation/revocation is audited.
Platform and connectors now send service credentials. Configurator setup provisions
a universe-managed service and key. Platform still applies its existing login and
membership model; its canonical user mapping is not implemented by this change.
A small authenticated identity-mutation endpoint supports service provisioning.

Schema revision 13 adds immutable resource ownership reservations and separate
mutation-admission attribution. Creation records the trusted principal or internal
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

Step 4 still owns the Platform directory cutover, response-time/stream revocation,
and remaining audit surfaces. Platform requests still represent its configured
service until canonical user mapping is connected. Content remains universe-visible;
private access and standing execution authority are follow-ups.
The host CLI's `--actor-principal` is trusted database administration, not a remote
identity assertion. See [core identity administration](../documentation/deployment/identity-and-access.md).

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
  not claimed or tested by this slice.

## Boundary and follow-up

Existing session content remains universe-visible to authorized readers. This
slice makes no private-session or resource-isolation promise, and its UI must not
present one. The agreed separation of administration from private content is
implemented when private sessions become available.

The next slice combines private/shared session access with one complete authorized
execution path: stable personal/service bindings, bounded run authority, and
effect-time checks. It must cover associated transcripts, streams, files, and
reachable execution paths together. Other follow-ups include individual resource
restrictions, full bot/delegation lifecycle enforcement, SSO/provisioning, and
external credential delegation.

Current seams: [authentication and access](../documentation/deployment/authentication-and-tenancy.md),
[Platform gateway](../../platform/server/src/routes/gateway.ts),
[runtime gateway](../../crates/temporal-server/src/gateway/http.rs),
[request principal](../../crates/temporal-server/src/gateway/principal.rs).
