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
| Platform | Local login, deployment-wide users/service principals and groups, identity lifecycle, and administration UI. |
| Runtime authorization | Minimal identity projection, universe role bindings, permission evaluation, and access audit records. |
| Runtime gateway/shared services | Trusted request context and enforcement across every exposed access path. |
| Persistence/API | Runtime records through `store-pg`; public contracts and method classifications through `api`. |

Platform owns identity and group membership. Runtime owns universe role bindings;
Platform administers those through runtime contracts rather than keeping a second
authoritative role system. The deterministic engine performs no identity or
policy I/O. Exact authorization module/crate placement remains an implementation
decision.

## Scope

1. **Local identity and projection.** Give users and services stable principal
   IDs. Support local groups and active/disabled status. Supply a minimal,
   versioned projection to runtime, including membership changes and revocation.
   Directory synchronization can later use the same boundary.

2. **Universe role bindings.** Bind principals/groups to Viewer, Contributor,
   Operator, and Admin roles. Start with coarse action bundles following the
   parent proposal: read, use agent work, configure resources, and govern access.
   Ownership-dependent actions such as managing one's own work need an ownership
   check or remain unavailable until it exists; a coarse role must not grant
   control of everyone else's work. Individual resource grants are deferred.

3. **Trusted caller context.** Platform requests, API keys, and internal services
   carry explicit authenticated identities. Missing identity never silently
   becomes `universe_default`. A configured agent execution identity is distinct
   from the caller; this slice does not add user-selectable execution bindings.
   Keys authenticate their bound principal and do not grant independent authority
   after that principal loses access.

4. **Consistent enforcement.** Explicitly classify every public API method and
   reject unclassified operations. Runtime checks the applicable permission on
   every entry path, including reads, lists, streams, mutations, and configuration.
   Shared services enforce contextual checks where method classification is
   insufficient. The universe Operator role, deployment administration, and
   trusted integration capabilities are separate. Service principal kind alone
   must not authorize credential leasing or privileged service methods.

5. **Administration and audit.** Provide a small Platform interface for local
   groups and universe role assignments, using runtime permissions to present
   available actions. Attribute requests and administrative/permission changes
   to authenticated actors. Keep access audit records independent of session
   deletion, preserve historical attribution after offboarding, and exclude
   credentials and session contents from access logs.

## Contract sketch

Illustrative shapes, not final wire signatures:

```text
IdentityProjection = revision + principals(id, kind, status)
                   + groups + memberships
UniverseRoleBinding = universe + subject(principal | group) + role
RequestContext = authenticated_actor + authentication_reference
               + target_scope(deployment | universe)

apply_identity_projection(change) -> acknowledged_revision
authorize(context, action, resource) -> decision + reason + policy_reference
```

Derive caller context from trusted authentication, never ordinary request fields.
Projection updates and role administration are privileged operations themselves.
Keep authentication credentials, role bindings, and future execution authority
as separate concepts.

## Revocation boundary

Once an identity/group change or role removal is acknowledged, subsequent access
decisions must observe it, including API-key requests. Acknowledgement must cover
enforcement caches/replicas, not merely receipt of an update. Existing read
streams and long polls must recheck access before further delivery or stop within
an explicit bounded interval; they cannot retain authorization indefinitely.
The propagation mechanism and bound remain implementation decisions.

This revokes access to the API and content. Stopping already admitted execution,
revoking standing bot authority, and cancelling external processes belong to the
execution-authority slice. Offboarding a configuring operator must not implicitly
revoke an independent service identity.

## Implementation order

1. Define identity projection, role binding, and authorization contracts and their
   persistence; decide the initial method/action mapping.
2. Make caller propagation explicit and apply runtime checks across gateway and
   shared-service entry points, including deployment/service capabilities.
3. Connect Platform administration to runtime bindings and remove competing
   Platform-only authorization decisions.
4. Complete acknowledged revocation, stream handling, and durable access audit.

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
