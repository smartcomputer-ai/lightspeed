# P176 — Identity foundation and universe authorization

**Status:** Proposed first slice of
[enterprise authorization and identity](later/pNNN-enterprise-authorization.md).
Scope and contract sketch only; detailed schemas, method permissions, and
acceptance criteria remain deferred.

Lightspeed is still greenfield. Reshape contracts and replace implicit defaults
where needed; compatibility with the current authorization model is not a goal.

## Outcome

A person's universe permissions apply consistently through the Platform and
direct API access, with trustworthy attribution of actions. Local users and
groups are enough to deliver this foundation; enterprise directories connect to
the same identity lifecycle later.

Today, Platform checks most permissions, forwarding the caller principal is
optional, and runtime principals mainly provide attribution. Runtime API keys
can remain usable after Platform membership is removed. This slice makes runtime
authorization authoritative for universe access.

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
The deterministic engine performs no identity or policy I/O. Exact module/crate
placement and tables remain implementation decisions.

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
authentication, never secret values. Explicit parameters versus fallible task-local
context remains open.

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

1. Rename deployment-facing `operator/*`, scope/types, and consumers to
   `deployment/*` in a separate mechanical commit, regenerating contracts. Keep
   Operator as the universe role; qualified binding names need no rename.
2. Define core identity/access contracts, persistence, bootstrap, and the initial
   method/action mapping. Reserve credential grant terminology for `auth/grants/*`;
   use access policy/resource permission for authorization.
3. Introduce service authentication, key scopes, explicit contexts, and ownership
   facts; enforce checks across gateway and shared-service entry points.
4. Connect Platform and CLI administration to core records, removing competing
   Platform-owned authorization state. Complete revocation, streams, and audit.

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
