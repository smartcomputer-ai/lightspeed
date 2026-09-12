# P125: Profile-Provisioned Environments

> Superseded: profile provisioning and session-bound environment cleanup were removed by [workflow-owned session preparation](../p172-workflow-owned-toolset-reconciliation.md). Environments now have independent lifecycles. The material below records the former design.

**Status**

- Proposed and implemented 2026-08-16 (all slices; live-validated against
  the fake provider with real Postgres/Temporal). Runtime build and hz01
  deployment of Slice 0 remain deployment work.
- Builds on [P85](p85-agent-profiles.md) (profiles),
  [P113](p113-explicit-vfs-and-environment-tool-domains.md) (environment tool
  domain), [P118](p118-environment-domain-and-lifecycle.md) (durable
  lifecycle), [P119](p119-environment-daemon-gateway-enrollment.md) (on-demand
  routing), and [P120](p120-incus-environment-provider.md) (Incus provider).
- Greenfield: renames the profile environment field instead of keeping the
  old shape beside the new one.
- Amends the "profiles do not provision or close environments" statements in
  [P108](p108-universe-environments.md) and `docs/spec/04-environments.md`.
  Both already reserve room for "a separate lifecycle policy" that closes a
  session-created environment; P125 is that policy.
- Extended 2026-09-04 with a narrow creation-time override: a start may
  activate a different existing environment or explicitly select none while
  retaining the named profile. Omission still uses the profile intent, and
  provisioning remains authored in the profile.

## Goal

Let a profile say not only *"activate this existing environment"* but also
*"provision a fresh environment for this session and activate it"*, so that
starting a session from a profile — from the CLI, Platform, Channels, Fleet
`agent_spawn`, or `session/managed/start` — is enough to obtain a private,
durable development VM without a separate `environments/create` call by the
caller.

The environment stays a universe-owned resource with the P118 lifecycle. The
session does not own it; the profile only decides how the session's active
environment comes into existence and, optionally, that closing the session
should also close it.

## Today

`ProfileDocument` is:

```text
config?                SessionConfig
instructions?          ProfileInstructions
activeEnvironmentId?   EnvironmentId    # must already exist and be reachable
```

`crates/api/src/profiles.rs`, `crates/profiles/src/lib.rs`, and the applier in
`crates/temporal-server/src/gateway/service/profiles.rs` implement exactly
that: at `session/start` (after the workflow is started) or `profiles/apply`,
the applier calls the same path as `session/environments/activate`, which
requires `features.environments`, an allowed provider, and a live end-to-end
reachability probe (`EnvironmentResolver::selectable`).

Consequences:

- Every consumer that wants "one sandbox per session" has to call
  `environments/create`, poll until `ready`, then `session/environments/activate`
  or `profiles/apply`. Foundry does this by hand
  (`platform/foundry/src/activities/lightspeed.ts` `resolveFoundryEnvironment`),
  Channels cannot do it at all, and Fleet `agent_spawn { base: profile }`
  cannot give a child its own machine.
- A profile can only name a concrete environment id, so a stored profile is
  either tied to one machine (all sessions share it) or carries no environment.
- The Platform profile editor is a free-text environment id field.

## Design decisions

### 1. The profile carries an environment *intent*, not just an id

Replace `activeEnvironmentId` with one tagged field:

```text
ProfileDocument {
  metadata?      Map<String, String>  # creation-time session defaults
  retention?     ProfileSessionRetention  # P154 creation default
  config?        SessionConfig
  instructions?  ProfileInstructions
  environment?   ProfileEnvironment
}

ProfileEnvironment (tag "type"):
  existing  { environmentId }
  provision { providerId, templateId,
              displayName?, metadata?,
              retention: closeWithSession | retain }   # default closeWithSession
```

- `existing` is today's behavior, unchanged.
- `provision` reuses the `environments/create` vocabulary (provider-owned
  immutable `templateId`, `displayName`, `metadata`) — no second dialect, per
  P85. Lightspeed derives the request id (decision 3).
- The provider is named by `providerId`, not `bindingId`. A binding is the
  universe-scoped, revisioned admission record for `(universe, provider)` —
  at most one per pair — so the two identify the same thing; the difference
  is portability. Profile documents are copied around (Channels config,
  inline Foundry profiles, exported JSON) and applied in several universes of
  one deployment; `providerId` is the same string in all of them and pairs
  naturally with the provider-wide template catalog, whereas `bindingId` is
  a routing artifact the author would have to look up per universe. The
  applier resolves the universe's binding for that provider at apply time and
  fails with a typed error when there is none or it is disabled.
- Absence still means "leave the session's active environment unchanged".
- Profiles remain references-only: no credential *values*, no provider
  config, no images. The provider still validates the template and applies
  its own policy; Lightspeed still persists no quotas.
- 2026-08-18 (P127 D5): `provision` gained an optional `credentials` list —
  `[{ envName, source }]` with the same source shape as
  `environments/credentials/bind` (grant / provider credential / secret
  *references*). The applier binds them right after `create_environment`
  and before activation, only when it actually provisioned the environment
  (a re-apply that finds the session's environment does not resync); they
  are ordinary environment bindings from then on. References are validated
  at profile put and at session start (`existing` environments do not take
  the list: they own their bindings).

Rejected alternatives:

- **Put provisioning in `SessionConfig`** (`features.environments.provision`).
  Config is a steady-state capability document replaced whole by
  `session/config/put`; provisioning is a one-shot creation act. Putting it in
  config would make every config put re-evaluate "should I create a VM?".
  Profiles are already the provisioning document ("compiles into
  operations", P85), so this is their natural home.
- **A second provisioning document on `session/start`.** Provisioning stays in
  the profile. The later lightweight override deliberately supports only
  `{type: existing, environmentId}` and `{type: none}` so an interactive user
  can choose a machine without copying the full profile inline.
- **A model-visible `environment_create` tool.** Explicitly deferred by
  P117/P118 behind a future default-off grant. Nothing here changes that:
  provisioning happens on the trusted `session/start`/`profiles/apply` path
  with a caller who could call `environments/create` anyway.

### 2. Provenance and optional close-with-session, not ownership

P117 keeps "a session owns an environment" as a non-goal. P125 keeps that.
The environment record gains one optional provenance fact:

```text
Environment
  ...
  originSession?  { sessionId, profileId?, closeWithSession: bool }
```

- Written once, at creation, by the profile applier. Never written by
  `environments/create`.
- The environment is otherwise ordinary: it appears in `environments/list`,
  can be activated by other sessions, closed by the universe, given ingress,
  bound credentials, and so on. `environments/list` gains an optional
  `originSessionId` filter.
- `closeWithSession: true` is a *close trigger*, not a lease: when the
  originating session reaches `Closed`, Lightspeed calls the ordinary
  idempotent close path for that environment. If the universe already closed
  or reused it, nothing special happens. This is the "separate lifecycle
  ownership policy" that P108 and `docs/spec/04-environments.md` leave open;
  active selection by any session stays independent of it.

Retention semantics — only the `provision` variant has a retention field;
an `existing` environment was not created for the session and is never
closed by it:

- `closeWithSession` (default): the environment was created for this
  session and goes away with it. This is what every sandbox-per-session
  consumer (Fleet children with `close_on_terminal`, managed sessions,
  Channels conversations, worker profiles) wants without further
  configuration, and it keeps a profile from silently accumulating VMs.
- `retain`: opt-in for profiles whose environment is meant to outlive the
  session (a durable dev box seeded by the first session). The universe then
  owns cleanup (`environments/list` filtered by session, or a later TTL
  policy — deferred as in P117).

Rejected alternative: a general "environment holders/leases" model where many
sessions hold an environment and it closes when the last one leaves. Nothing
needs it yet and it would create a second lifecycle authority next to the
universe. The single origin-session trigger covers the sandbox-per-session
case and stays trivially reconcilable.

### 3. Deterministic request identity: one profile-provisioned environment per session

The provision request id is derived, not supplied:

```text
requestId = "session:" + sessionId
```

- `session/start` retries with a client-supplied session id, gateway crashes
  between workflow start and provisioning, and repeated `profiles/apply` all
  converge on the same environment through the existing
  `(universeId, requestId)` unique key.
- A session therefore has at most one profile-provisioned environment. Applying
  a `provision` profile to a session that already provisioned one is a
  convergent no-op (activate it if it is not active). If that environment is
  `closed`/`failed`, apply fails with a typed error naming it; the caller
  either activates another environment or creates one through
  `environments/create`. A "generation" suffix to allow re-provisioning is
  deliberately left out until a real need appears.

### 4. Provisioning is asynchronous; activation admits a not-yet-ready environment

`environments/create` returns before provider I/O and a VM takes tens of
seconds to minutes to reach `ready`. `session/start` must stay an
acceptance/start boundary, so:

Gate at the tool call, not at session creation. People and systems will
send input the moment the session exists; blocking `session/start` until the
VM is ready would leave that input with nowhere to go for tens of seconds
to minutes and turn a start call into a minutes-long RPC. Accepting the
session and its runs immediately, and letting only the calls that actually
need the machine wait, makes "send input right away" work and leaves turns
that never touch the environment unaffected. (A run-level gate — no first LLM
turn until ready — is simpler to describe but delays turns that do not need
the environment and couples the session workflow to environment lifecycle;
rejected.)

- The applier records the environment in the registry, then activates it
  immediately. `selectable` is relaxed for the applier and for
  `session/environments/activate`: an environment in `provisioning` or
  `booting` is admitted **without** a reachability probe (it cannot be
  reachable yet and its status is authoritative intent). `ready`, `offline`,
  and `unknown` keep the live probe; `closing`, `closed`, and `failed` are
  rejected as today. The session's active environment id is thus set when
  `session/start` returns and `SessionView` reflects it.
- Environment tool calls do **not** wait inside their own activity. P114
  gives every tool class tight, invariant-checked deadlines (interactive
  90 s / 120 s, remote 120 s / 150 s, no heartbeats, bounded attempts); a
  minutes-long wait there would loosen every environment class. Instead the
  wait is a workflow step around the unchanged tool activity:
  1. `EnvironmentResolver::selectable` becomes status-aware and returns a
     typed `NotReady { status }` for `provisioning`/`booting` and a typed
     failure with the provider message for `failed` (today it ignores status
     and only probes).
  2. The single environment resolution chokepoint in the worker
     (`environment_manager_for_session`, entered only for `env.*`/job calls)
     maps `NotReady` to a distinguished per-call outcome. The call has not
     executed; fs/process/PTY/job/search tools and their activity options
     are untouched, and non-environment calls in the same batch proceed.
  3. The session workflow's per-call dispatch handles that outcome by running
     one new activity, `await_environment_ready(environmentId)` —
     heartbeated, retry-safe, idempotent, with its own long start-to-close
     (default 10 minutes) and heartbeat timeout — that polls the registry and
     probes the route, returning `Ready | Failed(message) | TimedOut`. On
     `Ready` the workflow re-dispatches the same call with its normal options;
     `Failed`/`TimedOut` become terminal tool failures through the existing
     P114 boundary-failure conversion. The fast path (environment already
     ready) never touches the extra activity.
  The engine is unchanged; the model simply sees its first environment
  command take longer, and the wait is durable and visible in Temporal
  history.
- `environment_read` and the model-visible environment view already expose
  status, so a model with selection tools can explain the wait.

Rejected alternatives: blocking `session/start` until ready (minutes-long RPC,
contradicts the start-boundary rule); deferring activation to the reconciler
once ready (session shows no active environment for a while, mixes the
lifecycle reconciler with session commands, and races with the first run).

### 5. Authorization is unchanged

- The effective session config must grant `features.environments` and, if
  `providers` is set, allow the binding's provider — the same checks
  activation performs today. A profile whose config omits the grant fails at
  apply time, exactly like `existing` does now.
- No new grant is introduced. `session/start`, `session/managed/start`, and
  `profiles/apply` are trusted universe-caller boundaries; `agent_spawn` may
  only use profiles the Fleet grant allows (`named_profile_allowed`), so a
  model can provision only through a profile a human authored *and* allowed
  for spawning. Provider capacity admission bounds the blast radius.
- Failure ordering at `session/start`: enabled-binding and provider-allowed
  checks run before the workflow is started so the common misconfigurations
  fail without creating a session; the environment itself is created after the
  workflow exists (an orphan session is cheap, an orphan VM is not).

## Slice 0: restore environment filesystem tools (independent bug fix)

Environment filesystem tools (`read_file`, `write_file`, `edit_file`,
`apply_patch`, search, glob, …) currently fail against every environment
while `exec_command` and `environment_read` work. Cause:
`RuntimeEnvironment::from_resource` in `crates/temporal-server/src/environment.rs`
still contains the pre-P119 safety gate

```rust
let _ = fs_context;
tool_context.filesystem = None;
```

introduced in `a5ee6121` together with the test
`environment_has_no_filesystem_before_p119_data_plane_admission`. P119
shipped; the gate was never removed. Upstream is already correct:
`RemoteEnvironmentConnection::into_contexts`
(`crates/tools/src/environment_protocol/remote.rs`) builds the remote
filesystem from the negotiated `EnvironmentCapabilities`, chooses
`FullReadWrite`/`FullReadOnly` from `filesystem_write`, attaches it to the
environment context, and sets the cwd. `process_executor()` is separately
capability-gated, which is why processes work.

Fix (ships first, independent of the rest of P125; needs a runtime build and
deployment, no provider or VM change):

- delete the gate and the now-redundant `fs_context` parameter of
  `from_resource`; the environment context keeps the filesystem
  `into_contexts` already attached;
- honor `filesystem_read == false` in `into_contexts` (no filesystem context
  rather than tools that fail remotely);
- replace the stale test with coverage for read/write, read-only, and
  no-filesystem capability negotiation, and add an environment-provider live
  assertion that `read_file`/`write_file` succeed against a provisioned
  environment.

## Wire changes

`crates/api`:

- `ProfileDocument.active_environment_id` → `ProfileDocument.environment:
  Option<ProfileEnvironment>`; new tagged `ProfileEnvironment` enum and
  `ProfileEnvironmentRetention` enum.
- `ProfileApplySummary.active_environment_changed` stays; add
  `environment_provisioned: bool`.
- `SessionStartParams.environment` and `ManagedSessionStartParams.environment`
  optionally override the resolved profile intent with an existing environment
  or no environment. Absence preserves the profile behavior.
- `EnvironmentView.origin_session: Option<EnvironmentOriginSessionView>`;
  `EnvironmentListParams.origin_session_id`.
- Regenerate `crates/api/contract/*`, TypeScript client, Configurator MCP.

`crates/environments` / `crates/store-pg`:

- `EnvironmentRecord.origin_session`, `CreateEnvironment.origin_session`,
  migration adding the columns, list filter by session, and a store query for
  open environments whose `close_with_session` origin session is closed.

`crates/profiles`: validate the new document shape (non-empty ids, metadata
rules shared with `environments/create`).

## Runtime changes (`crates/temporal-server`)

- Profile applier: `apply_profile_environment` handles `existing` (as today)
  and `provision` (resolve the enabled binding for `providerId` → derive
  request id → `create_environment_record` with `origin_session` →
  activate).
- `session/start`: pre-start binding/provider checks for `provision`
  profiles; provisioning after workflow start. Resolve the creation override
  first, so choosing an existing environment or none skips profile provisioning.
- `EnvironmentResolver::selectable`: status-aware admission (decision 4):
  `NotReady` for `provisioning`/`booting`, typed failure for `failed`,
  probe only for `ready`/`offline`/`unknown`. Today it ignores status
  entirely and gates on reachability alone.
- Worker: `environment_manager_for_session` maps `NotReady` to a per-call
  not-ready outcome; no change to any tool package or tool activity options.
- Workflow (`temporal-workflow`): new `await_environment_ready` activity
  with heartbeat and long bounded options; per-call dispatch re-dispatches an
  environment-dependent call once after `Ready`, converts `Failed`/`TimedOut`
  via the existing boundary-failure path. Sequential awaits only
  (workflow-waker constraint).
- Lifecycle reconciler (`environment_lifecycle.rs`): a close-with-session
  sweep — open environments with `closeWithSession` whose session projection
  is `Closed` → ordinary close. `session/close` and `session/delete` also
  request the close eagerly; the sweep is the restart-safe backstop and covers
  sessions closed from inside the workflow (`close_on_terminal`).

## Consumers

- **CLI**: `profiles import/check` accept the new shape; `chat --profile-json`
  works unchanged. The online check in `profile_cli.rs`
  (`validate_environments`) gains the `provision` case: an enabled binding
  exists for the provider and the template is listed for it.
- **Platform web**: the shared `profile-environment-editor.tsx` (used by
  `ProfilesPage` and the inline-profile editor in `SessionsPage`) becomes an
  existing-vs-provision chooser: environment select as today, or provider and
  template selects from `environments/provider-bindings/list` and
  `environments/templates/list` plus a retention toggle; its "profiles select
  an existing environment; provisioning is managed separately" copy goes
  away. `resource-features.ts` and `api.ts` read `environment` instead of
  `activeEnvironmentId`; `EnvironmentsPage` shows origin session and can
  filter by it.
  The basic new-session dialog also offers profile default, no environment,
  or an existing universe environment when the selected profile grants the
  environments feature; this does not convert the profile to inline JSON.
  Profile editing, customized session creation, live-session editing, and bot
  settings present the capability grant and the associated environment choice
  or provisioning intent in one Environment panel. This is a presentation
  grouping only: `config.features.environments`, profile `environment`, and a
  live session's active environment remain separate fields and operations.
- **Channels**: binding profiles can now provision one VM per conversation
  session with no Channels code change.
- **Fleet**: `agent_spawn { base: profile }` with a `provision` +
  `closeWithSession` profile gives each child its own sandbox that disappears
  when the child closes.
- **Foundry**: continues to resolve environments itself; `resolveManagerProfile`
  drops the profile's `environment` field (today `activeEnvironmentId`) and
  the corresponding test fixture is renamed. It may later adopt `provision`
  and delete `resolveFoundryEnvironment`. Not part of P125.

## Implementation

- [x] Slice 0: removed the pre-P119 filesystem gate in
      `RuntimeEnvironment::from_resource` (and its `fs_context` parameter);
      `into_contexts` attaches a filesystem only when `filesystem_read` was
      negotiated; capability-negotiation tests in `tools` and
      `temporal-server`. The remote read/write path is covered by
      `existing_file_tools_work_through_remote_context`; a session-level live
      assertion needs a real envd data plane (the fake provider has none) and
      belongs to the Incus smoke test. Runtime build and deployment pending.
- [x] `api`: `ProfileEnvironment`, `ProfileEnvironmentRetention`,
      `EnvironmentOriginSessionView`, `EnvironmentListParams.originSessionId`
      (not `sessionId`: the contract reserves that name for `session/*`),
      `ProfileApplySummary.environmentProvisioned`; contract exported.
- [x] `environments` + `store-pg`: `EnvironmentOriginSession`,
      `EnvironmentProvisionRequestId::for_session`, fields folded into the
      pre-release `005_environments` baseline, list filter, close-with-session query;
      the deployment reconciler scan now also selects universes holding such
      environments whose session is closed.
- [x] `profiles`: document validation for both variants.
- [x] `temporal-server`: applier `provision` path with derived request id and
      origin provenance, pre-start binding/grant checks, status-aware
      `selectable`/`activatable`, `NotReady` per-call outcome, hosted
      `await_environment_ready`, reconciler sweep, eager close on
      `session/close`/`session/delete`, public `reconcile_environments_once`
      for acceptance tests.
- [x] `temporal-workflow`: `ToolInvokeCallActivityResult`,
      `await_environment_ready` activity with heartbeated bounded options,
      per-call re-dispatch after readiness.
- [x] TypeScript client and Configurator MCP regenerated; Platform web
      editor (existing/provision chooser with provider, template, retention),
      `originSessionId` route passthrough, environments page provenance,
      stub gateway; CLI online `provision` validation; Foundry test fixture.
- [x] Platform environment setup consolidated under the Environment capability
      in profile, session-create, live-session, and bot editors without changing
      the wire document. Bot setup edits the complete session profile through
      one section and one save boundary.
- [x] Docs: `README.md`, `AGENTS.md`, P108 and `docs/spec/04-environments.md`
      amendments. The readiness wait is a workflow constant
      (`ENVIRONMENT_READY_WAIT`, 10 minutes), not a deployment variable.

## Verification

Executed 2026-08-16:

- `cargo test --workspace` (75 test binaries green), `cargo clippy
  --workspace --all-targets`, `npm run typecheck`, `npm run test`,
  `npm run build`, `npm run check:identity`.
- `store_pg_live::pg_live_universe_environments_are_independent_of_sessions`
  (origin-session round trip, filter, sweep query).
- `profiles_live::temporal_live_profile_provisions_environment_for_session`:
  unknown provider rejected before any session exists; `session/start`
  returns with the environment active while still `provisioning`; derived
  request id and origin provenance recorded; repeated start and
  `profiles/apply` create no second environment; the reconciler brings it to
  `ready`; `session/close` closes it; the sweep alone closes an environment
  whose session is already closed; `retain` leaves the environment `ready`
  after its session closes.
- `environment_provider_live`,
  `temporal_live_profiles_create_start_and_apply_idempotently`,
  `temporal_live_parallel_tool_batch_completes_per_call`, and
  `temporal_live_fleet_executor_spawns_profile_child` remain green.
- Migration `008` applied on top of an existing revision-7 database.

Unit coverage added: profile document validation for both variants;
request-id derivation (verbatim and digest form); `selectable` returns
`NotReady` for `provisioning`/`booting` without probing, typed failure for
`failed`, `Closed` for closing/closed, and `activatable` admits a booting
environment; `await_environment_ready` options are heartbeated and bounded
while the P114 tool-class options are unchanged; the hosted per-call path
reports `EnvironmentNotReady` for a provisioning environment while the
generic trait path degrades to a failed result; the readiness wait fails
fast on a `failed` environment and times out instead of returning a false
`Ready`; origin-session filter and sweep query in memory and Postgres.

Not yet covered end-to-end (needs a real envd data plane, which the fake
provider does not have): the first environment tool call actually waiting
through `await_environment_ready` and succeeding after re-dispatch, a
provider rejection surfacing as a typed tool error, and a Fleet-spawned
`provision` child. These belong to the Incus live smoke test once Slice 0 is
deployed.

## Resolved decisions

1. **Retention** applies only to `provision`; default `closeWithSession`,
   opt-in `retain`. `existing` environments are never closed by a session.
2. **Provider reference** by `providerId` + `templateId`, resolved to the
   universe's enabled binding at apply time (portable documents; bindings are
   routing artifacts).
3. **Readiness gate** per environment-dependent call, implemented as a
   workflow-level `await_environment_ready` step around the unchanged tool
   activity — never inside tool activities, at `session/start`, or at run
   start.
4. **One provisioned environment per session** as a hard rule; a
   re-provision generation can be added later without changing the profile
   shape.

## Deferred

- Environment pools / pre-warmed capacity behind the same `provision` intent.
- ~~Idle TTL and reaping policy for retained environments (P117 deferral).~~
  Done in [P126](p126-environment-power-and-idle-policy.md): staged
  `idlePolicy` (pause/suspend/stop/close) applied by the power reaper, and
  `provision.idlePolicy` on profiles.
- Model-driven provisioning tools and their grant (P117/P118 deferral).
- Profile layering (`extends`/`compose`, P85 deferral) — the new field is
  keyed and additive-friendly, so it composes when layering lands.
- Foundry migration to `provision` profiles.
