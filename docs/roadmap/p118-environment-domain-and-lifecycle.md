# P118: Environment Domain, Bindings, And Durable Lifecycle

**Status**

- Proposed 2026-08-11.
- Implemented 2026-08-12.
- First delivery milestone of
  [P117](p117-environment-compute-plan.md).
- Greenfield replacement of the current provider-as-singleton wire and storage
  shapes. Preserve the current universe-owned environment, active-session
  selection, explicit environment tool domain, credential, and provider-owned
  job boundaries.

## Goal

Establish the durable control-plane model for environment compute before
introducing the new daemon gateway or a real Incus backend.

After P118:

- physical providers are operator-registered and deployment-scoped;
- universes consume them through revisioned routing/admission bindings;
- environments have stable identity plus a minimal incarnation-to-provider-target
  reference;
- create and close are asynchronous, idempotent intents;
- a Lightspeed reconciler resumes lifecycle work after restart; and
- a fake provider proves provider-owned policy and cross-universe isolation
  without requiring provider persistence.

## Domain model

```text
EnvironmentProvider
  providerId
  controller connection
  operator metadata

EnvironmentProviderBinding
  bindingId
  universeId
  providerId
  status
  revision

Environment
  environmentId
  universeId
  source
  desiredLifecycle
  currentIncarnationId
  createdAtMs
  updatedAtMs

EnvironmentSource
  Provisioned { providerId, bindingId }
  Enrolled

EnvironmentIncarnation
  incarnationId
  environmentId
  provisionRequestId?
  providerTargetId?
  templateVersionId?
```

`providerTargetId` and direct-enrollment tickets/daemon identities are not
stable environment source identity. Rebuild and reenrollment may replace them
while the logical environment remains unchanged. Provider-managed
environments do not require direct daemon-enrollment records.

There is at most one binding for a `(universeId, providerId)` pair. A binding
is a stable Lightspeed routing and admission relationship, not a mirror of the
provider's template, quota, capacity, or ingress policy. Disabling a binding
rejects new creates while existing environments remain operable. Deletion is
rejected while a non-closed environment references the binding.

The provider owns the template catalog, binding entitlements, resource sizing,
aggregate quota, physical capacity, and public-ingress policy. The initial
provider may derive allocations from backend inventory and immutable target
metadata without a database. A provider that later needs durable coordination
may add it behind the controller contract without changing Lightspeed's domain
model.

## APIs

Operator-scoped:

```text
operator/environment-providers/put
operator/environment-providers/list
operator/environment-providers/read
operator/environment-providers/delete
operator/environment-providers/bindings/put
operator/environment-providers/bindings/delete
```

Universe-scoped:

```text
environments/provider-bindings/list
environments/provider-bindings/read
environments/templates/list
environments/templates/read
environments/create
environments/read
environments/list
environments/close
```

Binding `put` replaces the whole binding document and requires
`expectedRevision` when replacing it. Physical provider registration is not a
universe API. Multiple providers, including third-party services, are
registered by a Lightspeed operator by supplying their controller connection.
A provider never has to self-register and does not need access to Lightspeed's
operator API; Lightspeed only needs to be able to reach it. Provider
authentication will use an operator-managed secret reference when that
controller protocol is introduced; credentials must not be placed in metadata.

`environments/create` accepts a caller-generated stable `requestId`,
`bindingId`, and provider-owned immutable `templateId`. The unique key
`(universeId, requestId)` maps retries to the same environment. The API
transaction validates that the binding is enabled, creates the environment and
incarnation, and returns the provisioning resource before provider I/O.

Lightspeed does not persist or enforce environment-count, CPU, memory, disk,
template-entitlement, or ingress quotas. Provider creation atomically applies
the provider's current binding policy and physical-capacity rules. A rejection
is recorded asynchronously as a failed environment. P118 does not introduce
generic resource overrides; a later protocol version may add versioned,
provider-validated template parameters when a concrete need exists.

## Lifecycle reconciliation

Lightspeed Postgres is authoritative for lifecycle intent and the current
incarnation. One reconciliation loop in `temporal-server` scans pending and
closing rows, calls the provider controller with stable IDs, and records the
provider target ID plus the logical lifecycle status.

v1 runs one active reconciler. Controller calls are idempotent, so a crash
between provider I/O and observation persistence is recovered by repeating the
same request. Multi-replica operation leases may be added later if the runtime
actually needs them.

The provider/controller has no database. It reconstructs physical target state
from backend inventory and immutable target metadata. P118 must not introduce a
provider-side database, Redis, NATS, or another coordination service.

P118 stops a successfully-created environment at `waitingForDaemon`: it knows
which provider target it represents, but intentionally stores no data-plane
route, direct daemon enrollment, host capabilities, or liveness observation.
P119 owns those facts and is the milestone that may make an environment
`ready`.

## Host controller contract

Replace arbitrary target creation with binding-aware operations:

```text
controller/initialize
controller/listTemplates
controller/createTarget
controller/getTarget
controller/listTargets
controller/closeTarget
```

`controller/initialize` is a transient handshake performed immediately after
opening a controller connection. It verifies the protocol version and reports
implementation information and supported optional operations for that
connection. Lightspeed does not persist this response, use it as a heartbeat,
or maintain provider status, last-seen, or lease-expiry state in Postgres.

`createTarget` carries the stable request, environment, incarnation, binding,
provider-owned template version, and opaque bootstrap facts. The provider
authoritatively validates binding entitlement, quota, resource allocation, and
capacity as part of idempotent creation. The request does not accept arbitrary
cloud-init, raw backend objects, host paths, privileged device maps, or
unrestricted provider JSON.

`createTarget` and `closeTarget` are idempotent. Close verifies immutable
ownership metadata before deleting a backend object. Target listing and lookup
are filtered by authenticated binding context.

## Session and authorization boundary

Keep the P108/P113 session model:

- deterministic state stores only the active environment ID;
- environment file/process/job calls resolve that environment live;
- no environment inventory or connection data enters session state; and
- VFS remains a separate tool and filesystem domain.

P118 does not add model-driven provisioning. Authenticated universe API callers
may create environments from admitted bindings. A future model provisioning
surface requires its own default-off feature grant.

v1 universe credentials authorize the whole universe. Finer universe
administrator roles are deferred rather than implied by the API vocabulary.

## Implementation

- [x] Add typed binding, incarnation, provisioning-request, and provider-target
      identifiers.
- [x] Replace required provider/target environment fields with stable source
      plus current-incarnation records.
- [x] Move physical providers to deployment/operator scope.
- [x] Add operator-managed multi-provider put/list/read/delete APIs.
- [x] Add revisioned universe provider-binding storage and APIs.
- [x] Add provider-owned immutable template-version catalog views.
- [x] Add transactional request-id deduplication without a Lightspeed resource
      reservation ledger.
- [x] Add asynchronous lifecycle states without persisting provider or gateway
      observations.
- [x] Implement the single-active Lightspeed lifecycle reconciler.
- [x] Perform transient controller initialization on every new controller
      connection; do not persist capabilities, implementation, heartbeat,
      status, last-seen, or lease state.
- [x] Add binding-aware host controller operations and delete unrestricted
      arbitrary-provider creation vocabulary.
- [x] Implement a stateless fake provider over in-memory backend truth.
- [x] Regenerate contract artifacts and both TypeScript consumers.
- [x] Update profile validation's provider view to read universe bindings.
- [x] Delete obsolete provider self-registration and compatibility code.

## Verification

- Unit-test identifier, source/incarnation, lifecycle, binding-revision, and
  template invariants.
- Prove two calls with the same request ID return one environment and cause one
  physical target to be adopted.
- Prove a restart between create/close I/O and observation persistence
  converges through reconciliation.
- Prove two universes can share one provider while list/read/create/close,
  provider-filtered templates, provider policy, and target inventory remain
  isolated.
- Prove binding disable/delete semantics.
- Run the API schema export and TypeScript client/configurator checks.

## Done

P118 is complete when the new domain and API have replaced the current
provider-as-singleton model, fake-provider lifecycle survives Lightspeed and
provider restarts without a provider database, and active-session environment
tools remain explicitly unavailable until P119 supplies a live data-plane
route.

## Deferred

- Real daemon connections and direct enrollment: P119.
- Final removal of `attachTarget` and `AttachedHost`: P119.
- Incus targets and images: P120.
- Public ingress: P121.
- Multi-node provider scheduling: P122.
- Model-driven provisioning and finer universe roles.
