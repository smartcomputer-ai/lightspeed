# P126: Environment Power States And Idle Policy

**Status**

- Proposed and implemented 2026-08-17 (all slices; live-validated against
  the fake provider with real Postgres/Temporal; Incus freeze/unfreeze/stop
  mapping implemented but not yet exercised against a live Incus).
- Builds on [P118](p118-environment-domain-and-lifecycle.md) (durable
  lifecycle, stateless provider, one reconciler),
  [P119](p119-environment-daemon-gateway-enrollment.md) (on-demand routing
  through the provider), [P120](p120-incus-environment-provider.md) (Incus
  provider), [P122](p122-incus-multi-node-pool.md) (pool capacity), and
  [P125](p125-profile-provisioned-environments.md) (per-session provisioning,
  `await_environment_ready` re-dispatch, origin-session close trigger).
- Picks up the "idle TTL and reaping policy" item deferred by
  [P117](p117-environment-compute-plan.md) and P125.
- Greenfield: no compatibility shims; new enum values and columns are added
  directly.

## Goal

Let an environment that nobody is using stop consuming compute without being
closed, and come back transparently the moment a session touches it again.

Concretely:

- The provider protocol can express *power intent* (`running`, `paused`,
  `suspended`, `stopped`) and observe the matching states, and a provider
  declares which of those it supports.
- Lightspeed records a per-environment **desired power state** next to the
  observed status; the existing lifecycle reconciler converges the two.
- Any environment-dependent use of a paused/suspended/stopped provisioned
  environment wakes it through the P125 `NotReady` → `await_environment_ready`
  → re-dispatch path. No tool package, session, or client learns anything new.
- Who decides to power down is policy that lives in Lightspeed, never in the
  provider: an explicit API and an idle reaper driven by an `idlePolicy` on
  the environment (settable at creation, from a `provision` profile, or later
  through the API).
- Idle detection reads activity from the environment daemon on demand. It adds
  no per-call write to Postgres and no new fact that has to be kept in sync
  between Lightspeed and the provider.

Non-goals: environment pools / pre-warmed golden snapshots (still deferred),
capacity accounting in Lightspeed (a provider concern), wake-on-ingress
(noted as a follow-up), and any model-visible power tools.

## Today

`ProviderTargetStatus` is `Creating | Starting | Ready | Stopped | Closing |
Closed | Failed | Unknown` (`crates/environment-protocol/src/control/targets.rs`).
Controller methods are `createTarget`, `adoptTarget`, `getTarget`,
`listTargets`, `closeTarget`, and the ingress pair. `Stopped` is only ever an
*observed* state: the Incus provider maps instance status `Stopped` to it and
stops an instance internally before deleting it, but nothing can request a
stop, and there is no notion of pause.

On the Lightspeed side `EnvironmentStatus::Offline` mirrors `Stopped`
(`environment_lifecycle.rs`), and `EnvironmentResolver::selectable` treats
`Offline` like `Ready`: it probes the daemon and, if unreachable, returns
`EnvironmentUnavailable` — a hard tool failure, not a `NotReady`. Nothing
tracks when an environment was last used, and nothing powers anything down;
the only lifecycle exits are `environments/close` and the P125
close-with-session trigger.

Incus supports what we need natively: `PUT /1.0/instances/{name}/state`
accepts `freeze` / `unfreeze` (VMs pause the QEMU process; status `Frozen`)
in addition to `start` / `stop`, and VMs additionally support a *stateful*
stop/start (RAM written to disk; requires `migration.stateful=true` and
`size.state`). Firecracker, the intended next provider for short-lived
sandboxes, has `Paused`/`Resumed` VM state and snapshot create/load, which is
its natural idle mechanism.

## Design decisions

### 1. One power verb in the protocol, four states, per-provider capability

The controller plane gains a single method:

```text
controller/setTargetPower
  request_id, environment_id, incarnation_id, binding, target_id
  power: running | paused | suspended | stopped
  → { target: ProviderTargetSummary }
```

`ProviderTargetStatus` gains `Paused` and `Suspended`. Semantics:

| State       | Meaning                                              | Incus                              | Firecracker                                   |
|-------------|------------------------------------------------------|------------------------------------|-----------------------------------------------|
| `paused`    | VMM alive, vCPUs stopped, RAM pinned, instant resume | `freeze` / `unfreeze` (`Frozen`)   | `PATCH /vm state=Paused/Resumed`              |
| `suspended` | RAM saved to disk, VMM gone, resume restores state   | stateful `stop` / `start` (later)  | `PUT /snapshot/create` → kill; load in new VMM |
| `stopped`   | powered off, disk kept, resume is a fresh boot       | `stop` / `start` (`Stopped`)       | kill VMM, keep rootfs; boot from kernel+rootfs |
| `running`   | as today (`Ready` once the daemon is reachable)      | `start`                            | resume / load / boot                          |

`ProviderTargetSummary` gains `power_states: Vec<PowerState>` — the states
a target can be moved *to* (a controller-plane fact, so it lives on the
target summary rather than on the daemon-negotiated `EnvironmentCapabilities`).
`ControllerCapabilities` gains `set_target_power`. Incus advertises
`[running, paused, stopped]` in v1 and adds `suspended` if stateful stop is
wired up; Firecracker will advertise all four; an OCI/gVisor-style provider
might advertise `[running, paused]` (cgroup freezer) or only `[running]`.
Lightspeed records the advertised set on the incarnation with every target
observation and validates every power request against it. `Suspended` is in
the vocabulary from day one even though Incus does not need it, so the
Firecracker provider does not have to widen the protocol.

`setTargetPower` follows the same contract as `createTarget`/`closeTarget`:
idempotent by inventory (an already-running target answers `Ready`; a
suspended target with only snapshot files on disk gets restored), safe to
repeat after a crash, and answered with the observed summary. Resuming may
replace the underlying host process (Firecracker restores into a new VMM;
Incus stateful start is a new QEMU); Lightspeed holds no process handle or
endpoint per environment (routing is by universe/environment route key through
the provider, `environment_gateway.rs`), so that is invisible above the
provider. The Incus provider needs the same "accepted asynchronous operation"
smoothing it already has for start (`status_after_accepted_start`).

The provider stays stateless and policy-free. It never decides to power down;
it never stores desired state; it reconstructs the physical state from
inventory (Incus API, or a per-target directory tree plus live jailer
processes for Firecracker).

Rejected: feature-specific verbs (`pauseTarget`, `resumeTarget`, `stopTarget`,
…). One converge-to-state verb matches the create/close style, gives providers
one place to map, and keeps the enum the single point of extension.

### 2. Desired power state on the record; the existing reconciler converges it

`EnvironmentRecord` gains `desired_power: PowerState` (default `running`).
`EnvironmentStatus` gains `Paused` and `Suspended`; `Offline` keeps meaning
"observed stopped".

The lifecycle reconciler in `temporal-runtime` already scans pending and
closing rows and converges them through the controller. It gains one more
rule: for a provisioned environment whose observed status is not `Closing` /
`Closed` / `Failed` and whose observed power state differs from
`desired_power`, call `setTargetPower` and record the observation. Postgres
remains the single lifecycle authority (P118). External environments (no
provider) have no power state; the API rejects power changes for them.

This adds one intent column. It adds no new *observed* fact beyond the two
status values, and nothing that has to be kept in sync: activity and job
liveness are read from the daemon on demand (decision 5).

### 3. Wake-on-use reuses P125 readiness

`EnvironmentResolver::selectable`/`activatable` change one branch: for a
provisioned environment whose provider advertises the transition, observed
`Paused` / `Suspended` / `Offline` (and `desired_power != running`) →
set `desired_power = running` (a small conditional write) and return
`NotReady`. Today's "probe anyway" behaviour is kept for external environments
and for `Unknown`.

Everything downstream already exists: the worker reports
`EnvironmentNotReady`, the workflow runs `await_environment_ready`, the
reconciler resumes the target, the daemon becomes reachable, the call is
re-dispatched. `session/environments/activate`, the selection tools, and
`profiles/apply` admit a powered-down environment exactly as they admit a
`booting` one. Resume latency (Incus unfreeze: milliseconds; Firecracker
snapshot load: milliseconds; Incus cold start: seconds) is well inside the
existing readiness wait.

The reaper never powers down an environment whose daemon reports running
jobs, so a wake never has to recover an interrupted job. A paused environment
with an in-flight *process* (P114 tool call) is not prevented — the call
surfaces a transport error, which is acceptable and rare because the reaper's
idle threshold is far above any single call.

### 4. Who decides: two policy sources, both in Lightspeed

1. **Explicit API** — `environments/power/put { environmentId, power }` for
   operators and Platform. Sets `desired_power`; validated against the
   provider's observed `power_states` (rejected before the first observation
   and for external environments); the response is the updated
   `EnvironmentView` (which now carries `desiredPower`, `idlePolicy`, and
   `incarnation.powerStates`). `environments/idle-policy/put { environmentId,
   idlePolicy? }` replaces or clears the policy separately, so "clear" is
   unambiguous.
2. **Idle reaper** — an optional `idlePolicy` on the environment record:

   ```text
   idlePolicy? {
     pauseAfterMs?    u64
     suspendAfterMs?  u64
     stopAfterMs?     u64
     closeAfterMs?    u64
   }
   ```

   Thresholds are measured from the daemon's reported idle duration and must
   be non-decreasing in the order shown; each stage requires the provider to
   advertise that state (`close` always exists). The policy is set at creation
   (`environments/create.idlePolicy`, and `ProfileEnvironment::Provision.
   idlePolicy` for provisioned environments) and updated through
   `environments/power/put`. Templates may carry a default policy in metadata
   that `environments/create` copies when the caller passes none.

   A power reaper loop in `temporal-runtime` (sibling of the P92
   `PromiseReaper`, single active instance like the reconciler; every 60 s)
   selects `Ready` provisioned environments with a policy, asks each daemon
   for its idle report, and sets `desired_power` (or requests close) when a
   threshold is crossed. Environments whose daemon is unreachable are left
   alone. The policy is set at creation (`environments/create.idlePolicy`,
   `ProfileEnvironment::Provision.idlePolicy`) or later through the API.

   A session-suspension trigger was considered and dropped: P92 suspension is
   run-level (`await`), and a session that has gone quiet is exactly what the
   idle reaper measures, so a second trigger would only duplicate it. Because paused environments still pin RAM on both
   Incus and Firecracker, a two-tier policy — pause soon, suspend/stop later,
   close eventually — is the expected shape. Deciding *how many* paused or
   suspended targets a node can hold stays with the provider (P122 pool
   admission); Lightspeed only expresses intent.

The reaper acts on the *daemon's* idle report and *its own* wall clock; it
never trusts a wall-clock timestamp from inside the guest.

### 5. Activity is a daemon fact, read on demand

`lightspeed-envd` keeps an in-memory monotonic activity clock, bumped on every
data-plane request (`fs/*`, `process/*`, `job/*`) and while any process or
job is running. A new data-plane method reports it:

```text
env/idle → { idleForMs: u64, runningProcesses: u32, runningJobs: u32 }
```

`idleForMs` is a duration, not a timestamp: after freeze, stateful stop, or
snapshot restore the guest wall clock is stale until it resyncs, and a
monotonic duration stays correct under both providers. Daemon restart resets
the clock to boot, which is the intended semantics.

Consequences: no per-call write to Postgres, no `lastUsedAt` column, no
liveness cache to invalidate. The reaper's poll is bounded (only environments
with a policy, only when `Ready`), and it uses the same on-demand data-plane
route as the resolver, so nothing new is exposed. Use by anything else that
talks to the daemon (Foundry, an operator shell) counts as activity, which is
what an operator would expect before their VM is frozen.

Rejected: a throttled `lastUsedAt` touch in the resolver (still a per-call
read-compare in the hot path and blind to jobs); deriving idleness from
session state (no session→environment index; misses external use).

### 6. Per-environment suspend snapshots are not template snapshots

Firecracker's "restore one golden snapshot many times" is the pre-warmed pool
feature (deferred) and carries clone-hygiene concerns (RNG state, MAC/IP,
conntrack). A per-environment `suspended` snapshot is restored at most once
into the same identity, so none of that applies. The protocol does not need to
distinguish the two; the provider keeps them separate internally.

## Wire changes

`crates/environment-protocol`:

- `control/targets.rs`: `PowerState` enum; `ProviderTargetStatus::{Paused,
  Suspended}` (+ `power_state()`); `ProviderTargetSummary.power_states`;
  `SetTargetPowerParams` / `SetTargetPowerResponse`.
- `control/handshake.rs`: `ControllerCapabilities.set_target_power`.
- `control/methods.rs`: `SET_TARGET_POWER_METHOD = "controller/setTargetPower"`.
- `data/idle.rs`: `ENV_IDLE_METHOD = "env/idle"`, `IdleParams` /
  `IdleResponse { idleForMs, runningProcesses, runningJobs }`.
- `environment-client`: `set_target_power` and `idle` methods.

`crates/api`:

- `EnvironmentLifecycleStatusView::{Paused, Suspended}`;
  `EnvironmentView.{desiredPower, idlePolicy}`;
  `EnvironmentIncarnationView.powerStates`; `EnvironmentPowerStateView`;
  `EnvironmentIdlePolicyView`.
- `environments/create.idlePolicy`; new methods `environments/power/put`
  (`{ environmentId, power }`) and `environments/idle-policy/put`
  (`{ environmentId, idlePolicy? }`).
- `ProfileEnvironment::Provision.idlePolicy`.
- Contract, TypeScript client, and Configurator MCP regenerated (90 methods).

`crates/environments` / `crates/store-pg`:

- `PowerState` (re-export), `EnvironmentIdlePolicy` (+ `IdleAction`,
  `due_action`), `EnvironmentRecord.{desired_power, idle_policy,
  power_diverges()}`, `EnvironmentIncarnationRecord.power_states`,
  `EnvironmentStatus::{Paused, Suspended}` (+ `power_state()`,
  `is_powered_down()`), `SetEnvironmentPower`, `SetEnvironmentIdlePolicy`;
  power columns folded into the pre-release `005_environments` baseline; store ops `set_environment_power`,
  `set_environment_idle_policy`, `list_environments_with_idle_policy`;
  `list_environments_needing_reconcile` and the deployment universe scan
  now include power divergence; `list_universes_with_idle_policies`.

`crates/profiles`: validate `idlePolicy` shape (positive, monotone
thresholds); provider capability checks happen at reap time where the
observed states are known.

## Runtime changes

`crates/environment-daemon`: monotonic `ActivityClock`, `env/idle` handler
counting live processes and jobs (handshake and idle requests are not
activity; live work keeps the clock fresh so the countdown starts when the
last process/job ends).

`crates/environment-provider-incus`: `IncusBackend::set_power` and
`controller/setTargetPower` via `/instances/{name}/state` (`freeze`,
`unfreeze`, `stop`, `start`; a stopped target asked for `paused` converges
through running first), `Frozen`/`Freezing` → `Paused` in status mapping
(adopted instances that are externally frozen stop showing as `Unknown`),
`power_states` on every summary, accepted-operation smoothing for
freeze/resume/stop, `set_target_power` capability. Stateful stop
(`suspended`) is not advertised. P122 pool accounting for paused RAM is not
changed (paused VMs still count as running instances there).

`crates/temporal-runtime`:

- Reconciler: `reconcile_environment_power` (decision 2) with one
  `record_target_observation` path shared with create/adopt; observed status
  mapping for `Paused`/`Suspended`; power states recorded on every
  observation.
- `EnvironmentResolver::selectable`: powered-down provisioned environment
  with `running` in `power_states` → set desired `running` + `NotReady`
  (decision 3); providers without power control keep the reachability probe.
- `environment_read` adds a `status_message` when wake-on-use applies,
  explaining that environment tools automatically wake it and wait until
  ready. The raw status is preserved; `environment_list` stays compact
  without the message. Focused unit coverage checks both tool outputs.
- Gateway: `environments/power/put`, `environments/idle-policy/put`,
  `idlePolicy` on create and on the profile applier, view mapping; ingress
  may be configured on paused/suspended environments too.
- `environment_power.rs`: `reconcile_idle_power_once` / public
  `reap_idle_environments_once`, `decide_idle_action`, and the daemon idle
  read through the ordinary gateway route; `run_power_reaper` in
  `universe.rs` and the local-mode loop in `http.rs`, spawned from `main.rs`
  next to the lifecycle reconciler (60 s cadence).

`crates/temporal-workflow`: no changes; the existing `await_environment_ready`
path covers wake.

Consumers: TypeScript client and Configurator regenerated; Platform server
routes `PUT /environments/:id/power` and `/idle-policy`; environments page
shows power/idle policy details and offers Resume/Pause/Suspend/Stop for the
states the provider reported; profile editor gains idle-policy stages under
`provision`; stub gateway simulates power convergence and wake-on-activate;
CLI `env power` and `env idle-policy` subcommands.

## Implementation

- [x] Slice 1 — protocol + daemon + client: `PowerState`, statuses,
      `setTargetPower`, `power_states`, `env/idle`; fake provider supports
      all four states.
- [x] Slice 2 — Incus provider: freeze/unfreeze/stop/start mapping, `Frozen`
      status, capabilities, unit coverage; a live run against local Incus is
      still pending (no Incus on the development host).
- [x] Slice 3 — domain + store + reconciler: record fields, migration `008` (extended),
      convergence rule, status mapping, `environments/power/put`,
      `environments/idle-policy/put`, `idlePolicy` on create/profile, views,
      contract export.
- [x] Slice 4 — wake-on-use: resolver change with unit coverage; the hosted
      acceptance test drives activation of a paused environment (wake +
      reconcile to ready). A tool call actually waiting through
      `await_environment_ready` after a pause needs a real envd data plane
      (fake provider has none) — same gap as P125, belongs to the Incus smoke
      test.
- [x] Slice 5 — policy: power reaper with daemon idle reports; profile
      `idlePolicy`. (`onSuspend` dropped, see decision 4.)
- [x] Slice 6 — consumers and docs (`README.md`, `AGENTS.md`,
      `docs/spec/04-environments.md`).

## Verification

Executed 2026-08-17:

- `cargo test --workspace`, `cargo clippy --workspace --all-targets`,
  `cargo fmt --all`, `npm run check`.
- Unit: protocol serde round trips (`power_vocabulary_round_trips_and_maps_steady_states`);
  idle policy validation and `due_action` provider filtering; memory-store
  power/idle ops and reconcile/reaper candidate lists; resolver
  `powered_down_environment_with_power_control_wakes_on_use`; reaper
  `idle_decision_respects_work_pending_power_and_provider_support`; fake
  provider pause/resume/closed rejection; Incus accepted-operation smoothing
  and advertised states; daemon
  `idle_report_counts_running_work_and_resets_on_activity`; profile document
  idle-policy validation; CLI parsing; API dispatch of both new methods.
- `store_pg_live::pg_live_universe_environments_are_independent_of_sessions`
  (power intent, power states, idle policy round trip; reconcile and reaper
  candidate queries) on a database re-migrated to revision 8 (extended 008).
- `environment_provider_live::temporal_live_environment_power_intent_converges_and_wakes_on_use`:
  create with idle policy; power change rejected before first observation;
  ready with four reported states; malformed idle policy rejected, clear and
  restore; reaper sees one candidate and one unreachable daemon; pause intent
  → reconciler → `paused`, filterable; activating for a session wakes it
  (desired `running`) and the reconciler brings it back to `ready`;
  suspended → offline → ready round trip; external environments rejected for
  power and idle policy; closed environments rejected.
- P125 live tests (`temporal_live_profile_provisions_environment_for_session`,
  `temporal_live_profiles_create_start_and_apply_idempotently`) and
  `environment_provider_live` remain green.

Not yet covered end-to-end: Incus freeze/unfreeze/stop against a real
cluster; a tool call re-dispatched after waking a paused environment; the
reaper actually pausing an idle environment through a live envd. These
belong to the Incus smoke test.

## Open questions

1. Wake-on-ingress (P121): a relay hit on a paused environment could set
   `desired_power = running` and hold the connection. Deferred; noted here
   so the ingress relay does not grow an incompatible answer.
2. Whether the Platform should let a universe set a default `idlePolicy` per
   binding or template. Deferred until there is a second provider.
3. P122 pool accounting: paused VMs pin RAM; whether the Incus provider
   should count them differently from running ones for admission.

## Deferred

- Environment pools / golden snapshots behind `provision`.
- Wake-on-ingress.
- Cross-host restore of suspended targets (provider concern; requires shared
  snapshot storage).
- Model-visible power tools.
