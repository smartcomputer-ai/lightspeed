# P96: Environment API Review — Machines vs Bindings, Real Presence

**Status**
- Implemented 2026-07-09 as a greenfield breaking change, with the remaining
  workflow-supervision items called out below. The shipped cut separates
  provider presence, universe environment instances, session bindings, and
  environment-owned jobs across the domain, store, API, gateway, CLI, bridge,
  profiles, and worker wiring.
- **Greenfield: breaking changes are fine.** Store schemas
  (`006_environments.sql`) and wire shapes change in place; local stacks are
  reset; contract artifacts + TS client regenerated.
- Follows P95's boundary model (config = grants, resources = bindings) and
  applies the same whole-surface pass to the environment domain that config
  received: P75–P87 accreted capabilities without a layering review.

## Goal

Restore the same three-layer story the rest of the API now follows —
catalog/presence → universe resource → session binding — by splitting
**machine lifetime** from **session binding lifetime** (the factoring VFS
already has with workspaces vs mounts), making provider **presence real**,
and making durable jobs **environment-owned resources** with optional
run/session structured supervision layered on top.

## Previous state (pre-P96)

Verified against the implementation; the entanglements that motivate the
redesign:

1. **Machine creation is session-scoped.** `session/environments/create`
   provisions the machine (controller `create_target`) and binds it in one
   call; the created target is mirrored into the shared provider inventory
   (`environment_lifecycle.rs` `store_session_environment_binding` →
   `upsert_target`), but inventory rows are an unowned cache: no delete API,
   no GC, status touched only by heartbeats and close. Nothing ever cleans up
   machines — session close does not touch environments, and the only
   teardown is per-session `environments/close` defaulting `close_target:
   true`, which closes a machine **other sessions may be attached to** (no
   occupancy check; binding uniqueness is only `(session, env_id)`).
2. **Connection details are frozen into every binding.**
   `HostConnectionSpec` (endpoint, caps, cwd) is denormalized into each
   `SessionEnvironmentBindingRecord`; if a machine's endpoint changes, every
   binding is stale with no refresh path.
3. **Bindings are insert-only and statuses are write-once.**
   `create_binding` is `ON CONFLICT DO NOTHING`; `Attaching`/`Degraded` are
   assigned only at create/attach from the target status, `Detached` only on
   close; nothing ever transitions a live binding when a bridge dies. If a
   provider goes offline, bindings stay `Ready` and every data-plane call
   fails at call time.
4. **Presence is decorative.** Provider `status` goes `Online` at register
   and heartbeat, `Offline` only via explicit unregister; `Registering`,
   `Stale`, `Disabled` are dead variants never assigned; `lease_expires_ms`
   is stored, validated, and **never enforced** — `read_online_...` checks
   the status flag only. A crashed bridge stays `Online` forever, and the
   bridge's own shutdown path doesn't unregister.
5. **Jobs are session-namespaced but machine-resident.** Job handles are
   keyed `(session, env_id, job_id)` with host `namespace = session_id`;
   they snapshot `(provider_id, target_id)` and are reachable only while the
   current binding still points at that exact target — a re-attach under a
   new env_id orphans them. Model-started jobs do create P92 `EnvJob`
   promises with the right run/session cancellation semantics, but the source
   locator still routes through the mutable binding. Direct API starts have
   no promise at all. The base job contract therefore conflates execution
   location, access, and optional structured supervision.
6. **Capability conflation.** `SessionEnvironmentCapabilities` mixes machine
   facts (fs/process/job caps) with binding policy (`persistent` ≔ kind,
   `network` hardcoded `false`).
7. **Fleet sharing is implicit.** `EnvironmentPolicy` has a single variant
   (`Share`) whose environment half is a no-op; children see the parent's
   environments because session clone copies binding rows. Two sessions
   pointing at one machine is thus already normal — unmodeled, not
   forbidden.
8. The wire surface (16 of 82 methods): universe
   `environments/providers/register|heartbeat|unregister|list` +
   `providers/targets/list`; session `create|read|list|attach|activate|
   deactivate|close` + `credentials/bind|list|unbind` +
   `jobs/create|read|list|cancel`.

What is **right** and stays: bindings as session verbs gated by
`features.environments` (P95 §5); activation as pure engine routing
(`SetDefaultToolTarget env:{env_id}`); context projection
(`environment.catalog`/`environment.active`); the gateway-dials-provider
connection model (control plane + per-operation data plane); credential
bindings; DAG/lane job semantics executed host-side; profile environments as
one-shot setup steps (per P95 §5 refinement: environments fail the
*stability* test for config membership — this spec does not revisit that).

## Design

Three layers, each with its own lifetime and semantics:

The redesign uses three deliberately different identities. Do not reuse
`env_id` for more than one of them:

| Concept | Rust / store name | Wire name | Scope |
|---|---|---|---|
| Universe machine | `EnvironmentInstanceId` | `instanceId` | Universe-unique Lightspeed identity |
| Provider target | `HostTargetId` / `provider_target_id` | `providerTargetId` | Provider-native identity |
| Session binding alias | `EnvironmentId` / `env_id` | `envId` | Unique only within a session; forms `env:{envId}` |

The prose below calls the universe resource a *machine*; code and wire types
use *environment instance* so the model does not require a physical machine.

### 1. Provider presence (universe) — a lease, not a catalog

`environments/providers/register|heartbeat|unregister|list` keep their method
names, and the registry stays upsert/last-write-wins **deliberately**: it is
service discovery (a liveness lease over a controller connection), not
configuration. Heartbeat DTOs change as described below. The P95
put-with-revision treatment does not apply here; this distinction becomes
documented API semantics.

Make presence real:
- A provider is **live** iff `status == Online` *and* `lease_expires_ms` is
  in the future. `read_online_environment_provider` (attach/provision
  admission) checks the lease. No background reaper needed — liveness is
  evaluated at read time.
- Delete the dead `Registering`/`Stale`/`Disabled` status variants; the
  stored status is `Online | Offline`. The API may project/filter the derived
  state `Stale` when an online row's lease has expired. The bridge's shutdown
  path calls unregister.
- Liveness helpers take an explicit `now_ms` (backed by the gateway clock) so
  admission and list filtering use identical, deterministic semantics.

### 2. Machines (universe resource) — the missing "workspace" of this domain

Promote the target inventory to owned, universe-scoped
`EnvironmentInstanceRecord`s: `instance_id`, `provider_id`, the explicit
provider-native `provider_target_id`, `origin: Provided | Provisioned`,
machine facts (capabilities, mutable `connection: HostConnectionSpec`,
`default_cwd`, fs root metadata), lifecycle `status`, `observed_at_ms`, and
created/updated timestamps. The record is the **single source of connection
truth**; bindings stop copying it.

`(universe_id, provider_id, provider_target_id)` is unique. A provider
heartbeat or create response therefore updates an existing instance instead
of allocating a second Lightspeed identity for the same provider target.

- `Provided` machines are the provider's own inventory (a bridge's `local`),
  allocated on first observation and subsequently upserted by heartbeats.
- `Provisioned` machines are created explicitly via the new universe verb.
- Heartbeats carry a complete snapshot of full target descriptors, not only
  summaries. Each descriptor includes the target id, lifecycle state,
  capabilities, current data-plane connection, cwd, and metadata. The gateway
  assigns the observation timestamp. Reported targets update either origin;
  a previously `Provided` target omitted from a successful complete snapshot
  becomes `Unknown` rather than remaining permanently `Ready`. Omission does
  not delete a record or change a `Provisioned` record.
- Controller create/get responses and heartbeats all write through the
  same observation helper, with newer observations winning. This is the
  refresh path for rotated endpoints.
- Target observation DTOs are normalized to the same full descriptor shape;
  `get_target` therefore returns connection data as well as target metadata.
  The old controller `attach_target` capability is removed from the
  Lightspeed environment flow: a target must first be observed or provisioned,
  then session attach only creates a binding. A future provider-specific
  import operation belongs at the universe instance layer, not in a session
  verb.

New universe methods:
- `environments/create` — provision via a live provider (controller
  `create_target`); returns the machine record. Requires
  `capabilities.create_target`. **No session involved.**
- `environments/read`, `environments/list` (filter by provider) — replaces
  `environments/providers/targets/list`.
- `environments/close` — the only machine-teardown path. It is
  **transactionally occupancy-checked** and rejects while sessions hold
  attached bindings or nonterminal environment job groups remain, listing
  both. `begin_close(instance_id)` locks the instance row, performs the check,
  and transitions it to `Closing`; attach and job creation reject `Closing`
  instances. The gateway then calls controller `close_target` and finalizes
  the observed status. A definite provider rejection restores the prior
  state; an indeterminate transport failure leaves the instance `Unknown`
  for reconciliation.

There is no `force` option in the first cut. Force-close requires coordinated
detachment and engine deactivation across every occupying session; it should
arrive later as an explicit administrative workflow rather than bypassing the
occupancy check.

Universe instance and bare-job authorization is universe-scoped. Session
feature grants cannot authorize these methods because no session participates.
A composed CLI/profile provision-and-attach flow preflights the destination
session's provider allow-list before provisioning, but universe methods
themselves do not read session config.

No revision/put semantics: machines are stateful instances (like sessions),
not documents; their API is verbs.

### 3. Session bindings — pure references

`SessionEnvironmentBindingRecord` slims down to session-scoped facts:
`(session_id, env_id)` where `env_id` is the session-local binding name,
a **reference to `instance_id`**, `state: Attached | Detached`, fs routes,
cwd override, and timestamps. There is no durable `Attaching`/`Ready`/
`Degraded` state machine without a reconciler. Machine kind, lifecycle,
connection, and capabilities remain on the instance. Availability is derived
at read/use time from binding state + instance state + provider liveness;
operation failures remain call-scoped.

- `session/environments/attach` — binds an **existing instance by
  `instanceId`** (+ activate flag, cwd/fs-route options). Replaces today's
  provider+request attach; the provision-and-bind flow becomes
  `environments/create` followed by attach (the CLI keeps a one-shot
  `--create` composition; `session/environments/create` is removed from the
  wire).
- `session/environments/detach` — renames today's `close`, now touching
  **only the binding** (state → `Detached`, deactivate if active). Machine
  teardown is exclusively the universe `environments/close`.
- `activate`/`deactivate`, `credentials/*`: unchanged at the wire level.
- Bindings become **re-attachable**: replace insert-only `create_binding`
  with put-or-reattach on `(session, env_id)` (a detached binding can be
  re-pointed / re-attached without a new name).
- Re-attaching an alias to the same `instance_id` preserves its credential
  bindings. Re-pointing it to a different instance atomically clears all
  credentials for that `(session_id, env_id)`; credentials must never follow
  an alias silently onto a different machine.
- Session close performs binding bookkeeping (mark bindings `Detached`) with
  **no instance-lifecycle side effects** — closing a session never closes a
  machine. Structured-concurrency teardown may separately cancel jobs that a
  promise in that session supervises (§4).
- Multi-session attachment is now modeled, not accidental: fleet `Share`
  creates an explicit child binding to the same instance id instead of
  relying on clone-copied rows.

### 4. Jobs — environment-owned, optionally promise-supervised

Separate the bare environment contract from session structured concurrency:

```text
environment instance owns job
        optional promise supervises or observes job
                run/session owns promise scope
```

The job is intrinsically an environment resource. A promise is an optional
higher-level relationship; run or session scope is never a property of the
job itself.

#### Bare environment job contract

`EnvironmentJobRecord` is keyed `(instance_id, job_id)` with host `namespace =
instance_id`. It contains provider routing, an opaque `job_group_id`, name and
queue metadata, creation timestamp, request hash, and optional creating
session/run/turn provenance. Provenance is audit data, not ownership, and no
session or binding foreign key is required. The Temporal runtime derives its
workflow id from the group identity; Temporal vocabulary does not enter the
bare domain record or public handle.

Provider-facing job ids are instance-unique. Auto-generated ids include the
create request identity; caller-supplied ids are canonicalized into the same
instance-unique form before dependency resolution and provider dispatch. A
conflicting id with a different request hash is rejected.

The universe API exposes the complete bare contract:

- `environments/jobs/create` — starts one provider job group (one job, batch,
  or DAG) on `instanceId`; no session or promise is required. It returns after
  the workflow has registered the group and the provider has accepted the
  idempotent start, not after jobs finish.
- `environments/jobs/read|list|cancel` — address jobs through `instanceId` and
  remain usable independently of session bindings.

A bare job continues until it reaches a provider terminal state or is
explicitly cancelled. It is not implicitly cancelled because some unrelated
session or run ends. First-cut instance close is refused while its job group
is nonterminal. The provider remains authoritative for live execution state
and retained output; Lightspeed stores routing, idempotency, the latest
observation, and the monotonic fact that a job/group has become terminal.

#### One peer workflow per job group

Each `environments/jobs/create` request starts a peer
`EnvironmentJobWorkflow`, with a deterministic workflow id derived from
`(universe_id, instance_id, job_group_id)`, where the group id is allocated
idempotently from `request_id`. It is not a Temporal child of a session: bare
jobs have no session, and a supervised job may intentionally outlive its
creating run.

Use one workflow per create request rather than one workflow per individual
job. The host protocol already starts and reads a batch/DAG together, so this
preserves batched polling and dependency semantics while giving the group an
independent lifecycle. DAG/lane scheduling remains host-side; the workflow is
the durable control plane, not the executor.

```text
EnvironmentJobWorkflow
  state     instance id, request hash, job ids, latest observations,
            terminal marker
  activities
            batched read, selected cancel
  signals   cancel jobs, provider-changed nudge
  queries   group/job snapshot
```

The workflow polls with the existing P86 backoff, accepts provider/bridge nudge
signals, and records the terminal marker in the job index before completing
when every job is terminal. Long histories continue-as-new. Terminal
output/status reads still go to the provider; the workflow's observation is
coordination state, not a competing source of truth.

Implementation note: the first P96 cut registers and runs the peer workflow
after the gateway has reserved the group and the provider has accepted the
idempotent start. The workflow owns batched polling, cancellation activities,
terminal marking, queries, cancel/nudge signals, and continue-as-new. Moving
provider start itself behind a workflow activity, plus full subscription
fanout, is kept as follow-up so this branch can land the new contract without
also changing the host start handshake.

This deliberately revisits P86's rejection of a separate polling workflow.
There it would only have duplicated a session-owned wait. Here it represents
an independently addressable environment resource, removes polling from the
session workflow, and leaves room for future subscribers to share one provider
poll loop.

#### Optional promise supervision

There is no public session-specific job API. Public callers use
`environments/jobs/create|read|list|cancel` against stable
`(instanceId, jobId)` handles. Session/run supervision is a layer above that
bare contract: when the model starts a job through the session tool layer, the
runtime resolves the active binding to `instance_id`, applies binding cwd and
credential policy, starts the same job-group workflow, records provenance, and
creates one supervising promise per accepted job. A model tool call creates a
run-scoped promise; `detach(promise)` promotes it to session scope.

The promise source uses the stable environment locator rather than a mutable
binding:

```text
EnvJob {
  instance_id,
  job_id,
}
```

For the first cut, every environment-job promise is supervising: cancelling the
promise signals the job workflow to cancel the provider job. Observing an
existing bare job without cancellation authority is a future operation and
should be added only when there is a real subscriber contract to model.

New session-created promises are run-scoped as today. A terminal run cancels
its environment-job promises; `detach(promise)` promotes the promise to
session scope; normal session close refuses while that promise is pending;
force-close or session teardown cancels it and therefore the job. The job
workflow does not know run/session scope — it polls the environment-owned job
group and accepts cancel signals after the session promise layer applies its
ownership rules.

Clients that need status, output, listing, or cancellation use the universe
job methods directly. Binding churn therefore cannot prevent a promise cascade
from reaching its job, and the wire API does not need a parallel session job
surface.

Session tool starts use deterministic workflow/job/promise ids. Promise
creation is admitted through deterministic tool effects; it is never
workflow-resident state only. Bare starts do not create promises, so the repair
path never mistakes an intentionally unsupervised job for an orphan.

Implementation note: this branch uses a minimal supervising `EnvJob` promise
source and does not expose direct session job APIs. The pending/confirm
supervision handshake and subscription fanout are deferred follow-up work.

### 5. Config and capability cleanup

- `features.environments` grows an optional allow-list:
  `providers: Option<Vec<String>>` (absent = all), mirroring
  `fleet.profiles.allow`. Session attach checks it against the selected
  instance's provider. Composed provision-and-attach clients preflight it;
  raw universe provisioning and bare-job methods use universe authorization.
  (Target-level allow-lists are deferred until a use case.)
- Capabilities split along the machine/binding line: machine facts
  (fs/process/jobs caps, cwd, network — real value from the provider instead
  of hardcoded `false`) live on the instance record. Binding policy is only
  cwd/fs-route restrictions. `persistent` is removed: under this design every
  instance outlives a binding until explicitly closed, so persistence is
  lifecycle semantics, not a capability.
- `ProfileEnvironment` setup steps stay one-shot, reshaped to the new verbs:
  `environment: Existing { instance_id } | Provision { provider_id, request }`,
  plus `activate` — apply = (optionally create) + attach, best-effort,
  counted, exactly as today.

## Wire surface delta

Removed: `session/environments/create`,
`environments/providers/targets/list`, and the session job convenience methods
`session/environments/jobs/create|read|list|cancel|observe`.
Added: `environments/create|read|list|close` and
`environments/jobs/create|read|list|cancel`.
Renamed: `session/environments/close` → `session/environments/detach`.
Net: 82 → 84 methods.

## Decisions

| Decision | Recommendation | Alternative considered |
|---|---|---|
| Provider registry semantics | Presence lease (upsert, liveness = status ∧ lease) — documented as deliberately different from catalogs | Put-with-revision (rejected: it's service discovery, not config) |
| Identity | `instance_id` (universe), `provider_target_id` (provider), and `env_id` (session alias) are distinct typed ids | Reuse `env_id` (rejected: hides scope and makes reconciliation ambiguous) |
| Provider inventory | Heartbeat is a complete snapshot of full target descriptors; missing provided targets become `Unknown` | Summary-only append/upsert (rejected: cannot refresh connections or detect disappearance) |
| Provider target attach | Observe/provision an instance first; session attach never calls the provider controller | Keep controller `attach_target` in the session path (rejected: reintroduces machine creation/lookup into binding lifetime) |
| Machine ownership | First-class universe instance with verbs and unique `(provider, provider_target)` identity | Keep inventory-as-cache (rejected: no lifecycle, no teardown, no source of connection truth) |
| Provision entry point | Universe `environments/create`; session attach binds by id | Keep fused session create (rejected: conflates lifetimes; CLI keeps the one-shot composition) |
| Universe authorization | Universe auth for instance and bare-job methods; session provider allow-list for attach and session tool job starts | Read session grants in universe verbs (rejected: no session exists at that boundary) |
| Machine teardown | Atomic `begin_close`, reject attached bindings and nonterminal job groups, no first-cut force option | Gateway check then close (rejected: races new attach/job start); per-session close (rejected: kills shared machines) |
| Binding contents | Instance reference + alias/routes/cwd + `Attached | Detached`; connection/caps/kind resolved from instance | Denormalized facts or unsupported readiness states (rejected: stale/fake precision) |
| Binding credentials | Preserve on same-instance reattach; clear atomically when re-pointed | Let credentials follow the alias (rejected: can disclose secrets to a different machine) |
| Job ownership | Jobs are instance resources with optional creator provenance; a session/promise is not required | Session-owned jobs (rejected: execution-resource lifetime and structured supervision are different layers) |
| Job orchestration | One peer `EnvironmentJobWorkflow` per create request/job group | Session-owned polling (rejected: bare jobs have no session); workflow per individual job (rejected: loses batched host reads) |
| Structured supervision | Session start adds an environment-job promise by composition; run/session scope remains entirely in the promise layer | Encode run/session scope on the job (rejected: pollutes the bare environment contract) |
| Job observation | Deferred until there is an explicit attach/observe-existing-job contract | Add an unused observer promise relation now (rejected: extra state without behavior) |
| Lease enforcement | Read-time liveness check, no reaper | Background reaper marking `Stale` (rejected: extra machinery; derive staleness) |
| Binding availability | Derive from binding + instance + provider at use time; failures stay call-scoped | Durable degraded tracking (deferred; requires a controller/reconciler) |

## Contract invariants

These are cross-layer requirements, not gateway conventions:

1. One provider target maps to at most one environment instance within a
   universe.
2. Provider admission always evaluates the lease against the current gateway
   clock; stored `Online` alone is insufficient.
3. A session binding stores only its alias, instance reference, and
   session-scoped policy. Runtime connection and machine facts come from the
   latest instance record.
4. Attach, job-group creation, and `begin_close` serialize on the instance
   row. Once an instance is `Closing`, no binding can attach and no job group
   can start. Close rejects attached bindings and nonterminal job groups.
5. Detaching or closing a session never closes an instance. Closing an
   instance never happens through a session verb. Session teardown may still
   cancel jobs supervised by that session's promises.
6. Re-pointing an alias to a different instance clears its credential
   bindings in the same transaction.
7. A bare job has no required session, binding, run, or promise owner. Its
   stable identity and routing are `(instance_id, job_id)`.
8. Each create request has one peer job-group workflow. Provider execution
   state remains authoritative; the workflow owns polling, cancellation, and
   monotonic terminal coordination only.
9. Promise scope and job ownership remain separate. Cancelling an environment-job
   promise cancels the underlying job; bare jobs have no promise owner.
10. A session-started environment-job promise is active only once
    `Promise(Created)` is durable. A stronger pending/confirm supervision
    handshake is deferred.
11. Structured cancellation routes through the instance/job identity, never
    through a current session binding. Hard-terminated holder workflows are
    treated as dead owners by the reaper.

## Implementation slices

Engine activation commands and context keys are untouched. The promise source
shape changes from session/binding routing to stable instance/job routing.
Sequencing follows P95's pattern: domain crate first, then one cross-layer
alignment slice.

1. **Done: `crates/environments`** — instance record type + store trait
   (`EnvironmentInstanceStore`:
   `observe/read/list/begin_close/finalize_close`),
   distinct instance/target/alias id types, slimmed binding record (instance
   ref + `Attached | Detached`), explicit-time presence liveness helper,
   environment-owned job/group records, delete dead stored provider/binding
   status variants; memory store + tests.
2. **Done: `crates/store-pg`** — `006_environments.sql` edited in place
   (`environment_targets` → `environments` owned table; bindings drop
   connection/caps/kind columns, gain instance FK, put-or-reattach with
   credential clearing on re-point; unique provider-target identity; atomic
   occupancy-checked close transition). Re-key `environment_jobs` to
   `(instance_id, job_id)` with no session/binding FK; add a job-group
   discovery/idempotency index containing group id, request hash, and
   monotonic terminal marker so `begin_close` can reject active groups; impls
   + live tests.
3. **Done: `crates/api` + gateway** — new universe methods, session verb changes
   (attach-by-instance-id, detach), full heartbeat target descriptors,
   universe-vs-session authorization, close state machine, connection
   resolution at use time, bare universe job CRUD, session-close binding
   bookkeeping, public environment job CRUD, and
   `features.environments.providers` admission; projection unchanged in
   shape.
4. **Done: `crates/temporal-workflow` + worker structured concurrency** —
   add and register peer `EnvironmentJobWorkflow` under
   `workflows/environment_job.rs` with batched polling, cancel activities,
   nudge/cancel signals, queries, continue-as-new, and terminal index
   finalization. Move the session workflow under `workflows/session/`. Change
   `PromiseSource::EnvJob` to `{ instance_id, job_id }`; make the session tool
   job-create path compose the bare workflow with deterministic run-scoped
   promises that can be detached to session scope; route session tool
   list/read/cancel without a live binding. Follow-up remains for provider start
   as a workflow activity, subscription fanout, and the pending/confirm
   supervision handshake.
5. **Done: Fleet + profiles** — `Share` creates an explicit child binding by
   instance id (stop relying on clone-copied rows); `ProfileEnvironment`
   reshape; profile apply unchanged in spirit.
6. **Done: CLI + host-bridge + tests + docs** — CLI env/job subcommands (including
   provision-and-attach and bare-vs-supervised job creation), bridge
   unregister-on-shutdown, live suites ported, workflow tests, and the pre-existing
   host-bridge fs-routing doubled-path fix while fs routes are touched. Retire
   the environment-facing gateway controller `attach_target` path; regenerate
   the contract + TS client; update README/AGENTS notes.

## Deferred work

- Machine GC: `Provisioned` machines with zero bindings and no active job
  groups persist until explicit close. Idle reaping / TTL leases are deferred;
  `environments/list` keeps leaks visible in the first cut.
- Administrative force-close: deferred until there is a workflow that can
  detach bindings, deactivate engine routes, and cancel active job-group
  workflows across all occupants.
- Job adoption/ownership transfer: the first cut allows a supervising promise
  only when the session tool creates the job. Promoting an existing bare job to
  a supervisor, observing a job without cancellation authority, transferring
  supervision between sessions, and releasing a supervised job back to bare are
  deferred until their authorization and race semantics are concrete.
- Terminal job-group workflow history and job-index retention need an operator
  policy. The first cut keeps the lightweight job index and uses normal
  Temporal retention for completed workflow histories.
- Bridge auto-attach remains client-side polling (register → poll attach).
  A push "offer to session" flow is out of scope here.
- Job workflow provider-start ownership, subscription fanout, and supervision
  confirmation are deferred. The current branch starts providers through the
  gateway, then hands polling/cancel/terminal coordination to the peer workflow.
