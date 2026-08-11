# P118: Environment Domain, Bindings, And Durable Lifecycle

**Status**

- Proposed 2026-08-11.
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

- one physical provider is operator-scoped and deployment-seeded;
- universes consume it through revisioned policy bindings;
- environments have stable identity plus incarnation-scoped physical facts;
- create and close are asynchronous, idempotent intents;
- a Lightspeed reconciler resumes lifecycle work after restart; and
- a fake provider proves quota and cross-universe isolation without requiring
  provider persistence.

## Domain model

```text
EnvironmentProvider
  providerId
  controller connection
  implementation/protocol versions
  capabilities
  health/liveness

EnvironmentProviderBinding
  bindingId
  universeId
  providerId
  status
  allowedTemplateVersionIds
  maxEnvironments
  maxCpu
  maxMemoryBytes
  maxDiskBytes
  publicIngressAllowed
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
  imageFingerprint?
  admittedDaemonId?
  providerObservation
  gatewayObservation
```

`providerTargetId`, enrollment tickets, and daemon admissions are not stable
environment source identity. Rebuild and reenrollment may replace them while
the logical environment remains unchanged.

There is at most one binding for a `(universeId, providerId)` pair. Bindings
store immutable template-version IDs. Disabling a binding rejects new creates
while existing environments remain operable. Deletion is rejected while a
non-closed environment references the binding.

## APIs

Operator-scoped:

```text
operator/universes/provider-bindings/put
operator/universes/provider-bindings/delete
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

Binding `put` replaces the whole policy document and requires
`expectedRevision` when replacing it. Physical provider registration is not a
universe API. v1 seeds exactly one provider from deployment configuration.

`environments/create` accepts a caller-generated stable `requestId`,
`bindingId`, immutable `templateId`, and bounded resource overrides. The
unique key `(universeId, requestId)` maps retries to the same environment.
The API transaction validates policy, reserves quota, creates the environment
and incarnation, and returns the provisioning resource before provider I/O.

Provisioning, failed, ready, offline, and closing environments retain quota.
Only `Closed` releases it. This deliberately conservative rule avoids
capacity oversubscription while backend cleanup is uncertain.

## Lifecycle reconciliation

Lightspeed Postgres is authoritative for lifecycle intent and the current
incarnation. One reconciliation loop in `temporal-server` scans pending and
closing rows, calls the provider controller with stable IDs, and records
observations.

v1 runs one active reconciler. Controller calls are idempotent, so a crash
between provider I/O and observation persistence is recovered by repeating the
same request. Multi-replica operation leases may be added later if the runtime
actually needs them.

The provider/controller has no database. It reconstructs physical target state
from backend inventory and immutable target metadata. P118 must not introduce a
provider-side database, Redis, NATS, or another coordination service.

Readiness is derived from source-specific observations:

- provisioned environments require an available provider target and an
  admitted current data-plane incarnation;
- enrolled environments require an admitted current data-plane incarnation;
- stale observations never make a dead route ready.

P118 may represent the future gateway observation with a fake/test adapter.
P119 supplies the real data plane.

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

`createTarget` carries the stable request, environment, incarnation, binding,
template-version, bounded resource, and opaque bootstrap facts. It does not
accept arbitrary cloud-init, raw backend objects, host paths, privileged device
maps, or unrestricted provider JSON.

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

- [ ] Add typed binding, incarnation, provisioning-request, daemon, and
      connection identifiers.
- [ ] Replace required provider/target environment fields with stable source
      plus current-incarnation records.
- [ ] Move physical providers to deployment/operator scope.
- [ ] Add revisioned universe provider-binding storage and APIs.
- [ ] Add immutable template-version catalog views and bounded overrides.
- [ ] Add transactional request-id deduplication and quota reservation.
- [ ] Add asynchronous lifecycle states and independent provider/gateway
      observations.
- [ ] Implement the single-active Lightspeed lifecycle reconciler.
- [ ] Add binding-aware host controller operations and delete unrestricted
      arbitrary-provider creation vocabulary.
- [ ] Implement a stateless fake provider over in-memory backend truth.
- [ ] Regenerate contract artifacts and both TypeScript consumers.
- [ ] Update ls.bot's provider view to read universe bindings.
- [ ] Delete obsolete provider self-registration and compatibility code.

## Verification

- Unit-test identifier, source/incarnation, lifecycle, binding-revision,
  template, and quota invariants.
- Prove two calls with the same request ID return one environment and cause one
  physical target to be adopted.
- Prove a restart between create/close I/O and observation persistence
  converges through reconciliation.
- Prove two universes can share one provider while list/read/create/close,
  templates, quotas, and target inventory remain isolated.
- Prove binding disable/delete and conservative quota-release semantics.
- Run the API schema export and TypeScript client/configurator checks.

## Done

P118 is complete when the new domain and API have replaced the current
provider-as-singleton model, fake-provider lifecycle survives Lightspeed and
provider restarts without a provider database, and the existing active-session
environment tools continue to resolve the new universe resources correctly.

## Deferred

- Real daemon connections and direct enrollment: P119.
- Final removal of `attachTarget` and `AttachedHost`: P119.
- Incus targets and images: P120.
- Public ingress: P121.
- Multi-node provider scheduling: P122.
- Model-driven provisioning and finer universe roles.
