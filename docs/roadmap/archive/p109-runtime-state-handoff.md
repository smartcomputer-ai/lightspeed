# P109: Runtime State Handoff Without Hot-Path Replay

**Status**

- Completed 2026-07-30. Frequent runtime execution paths consume bounded
  workflow-owned facts and perform no session-log replay. Spawn, clone, fork,
  explicit inspection, bootstrap, and recovery remain intentional cold-path
  replay boundaries.
- Proposed 2026-07-30 after P107/P108 made session-owned VFS and environment
  facts explicit on runtime requests and exposed the remaining replay-based
  lookups in tool execution.
- Internal runtime change. No client API, gateway projection, or session event
  vocabulary change is required.
- Initial session bootstrap, continue-as-new rehydration, and recovery scans
  remain valid cold paths. P109 removes replay from ordinary run execution.
- Issue 1 implemented 2026-07-30: workflow-tool bindings and prior emission
  counts are carried per call, and ordinary non-`await` batches no longer
  replay solely to discover workflow-tool dispatch.
- Issue 2 implemented 2026-07-30: a CAS-only preparation activity extracts
  bounded `cancel`/`detach` ids, the workflow joins their Promise projections,
  and built-in and Fleet execution share one replay-free evaluator.
- Issue 3 implemented 2026-07-30: admitted environment-provider policy and
  active selection are supplied on tool batches; environment control and
  active process/filesystem setup no longer replay the owning session.
- Issue 4 implemented 2026-07-30: `job_submit` always targets the environment
  active for its original tool batch; that id and provider policy are pinned
  in a durable opaque execution-context ref, and workflow preparation no
  longer replays the holder session.
- Issue 5 target admission implemented 2026-07-30: `agent_send` and
  `agent_request` retain only direct-link authorization outside the target;
  target workflow admission owns lifecycle, idempotency, and message/run
  mechanics without caller-side replay or a Fleet-specific queue preflight.
  Fleet requests carry an empty per-run override document so the target's
  live session config remains authoritative. Ordinary `session/runs/start`
  now also builds only explicit overrides from its already-loaded state,
  removing its redundant config replay.
- Issue 6 completed 2026-07-30: admitted Fleet policy is carried on tool-batch
  requests, environment-job activities receive narrow blob/environment/
  credential dependencies instead of `PgStore`, remaining Fleet reductions
  are confined to child creation, and zero-read regression coverage protects
  the frequent paths.
- Relevant serial Temporal live coverage passed 2026-07-30: ordinary run
  admission, Promise await, parent delivery, clone/profile spawn, profile
  list/read, environment-job round trip, and both bound and start-on-call
  workflow-tool plugin dispatch.
- Provider-backed live coverage passed 2026-07-30: hosted OpenAI admission;
  OpenAI and Anthropic adapter generation, MCP execution, prompt projection,
  and skill selection/activation; Anthropic function-tool round trip; host
  credential injection; and Temporal MCP/session-link materialization.

## Problem

The Temporal session workflow already owns an up-to-date `CoreAgentState`.
Nevertheless, several activities and tool executors read every persisted
session entry and replay the log to rediscover facts that were present when the
workflow scheduled the activity.

The most expensive case is `SessionTools::workflow_tool_batch_runtime`: every
ordinary non-`await` tool batch replays the complete owning session merely to
recover immutable workflow-tool bindings and per-run emission counts. Other
same-session replays recover Promise control facts, environment provider
policy, or the active environment. Fleet repeats some of the Promise and
parent-policy lookups and also performs replay-based target preflights before
submitting work to another session.

This has four problems:

1. Work per tool batch grows with total session history. A long-lived session
   repeatedly pays for old events, making cumulative execution approach
   quadratic behavior.
2. The activity reads mutable shared storage instead of consuming the exact
   state that caused the workflow to schedule it.
3. Activity retry behavior can depend on a later store observation even though
   Temporal records and retries the original activity input.
4. Runtime code acquires `SessionStore` access only to reconstruct state that
   the deterministic owner already had, obscuring ownership boundaries.

P107 established the intended pattern for workspace links, and P108 already
carries `active_environment_id` on `ToolInvocationBatchRequest`: derive bounded
facts from `CoreAgentState`, put them on the activity request, and let the
activity read only external resources needed to perform the side effect.

## Decision

Ordinary runtime work must not replay its owning session log.

When the session workflow schedules an activity, it supplies every
session-owned fact required by that activity. Inputs must be bounded,
purpose-specific projections rather than `CoreAgentState`, raw session entries,
or an opaque serialized state snapshot.

The boundary is:

```text
CoreAgentState                         (authoritative event-sourced state)
  -> purpose-specific runtime facts   (deterministically derived)
  -> Temporal activity input          (recorded exact scheduling input)
  -> live external resource reads     (catalogs, CAS, providers, child workflow)
  -> activity result / tool effects
  -> engine admission against current CoreAgentState
```

Activities may continue to read:

- CAS blobs referenced by the request;
- VFS workspace and snapshot catalog resources;
- universe environment/provider resources;
- auth grants and secrets needed for an invocation;
- session graph/link records; and
- explicitly requested history or presentation data.

They must not use `session_id` to reconstruct the owning session's config,
Promises, workflow-tool bindings, active environment, tool policy, or run
state.

## Shared Internal Request Shape

Extend the engine/runtime tool invocation DTOs with a versioned, internal
runtime context. One possible shape is:

```text
ToolInvocationBatchRequest
  session_id
  run_id / turn_id / batch_id
  workspace_links
  active_environment_id
  environment_policy?
  fleet_policy?
  promise_controls             // only ids referenced by this batch
  calls[]

ToolInvocationRequest
  call_id / tool_name / arguments_ref / execution_target
  workflow_tool?

WorkflowToolCallRuntime
  binding
  prior_emission_count

PromiseControlRuntime
  promise_id
  ownership
  scope
  status

EnvironmentToolRuntime
  allowed_provider_ids?   // absent means every provider

FleetToolRuntime
  config                  // admitted Fleet feature policy
```

The exact Rust grouping may differ, but the constraints are fixed:

- derive the values in `engine::next_tool_batch_request` from the same
  `CoreAgentState` used to plan the pending batch;
- attach facts only for installed/requested tool families where practical;
- carry an admitted `WorkflowToolBinding`, not a second executor-owned
  interpretation of it;
- do not carry provider observations, credentials, workspace contents, or
  other live external state;
- do not put these fields into public `api` wire DTOs or durable session
  events; and
- keep result admission authoritative. Runtime facts help execute the call but
  do not bypass engine validation of returned effects.

The request is a transient deterministic handoff. It is recorded in Temporal
activity history, so retries receive the same session-owned facts.

Some built-in control arguments are CAS-backed, so the engine initially knows
the argument ref but not the ids inside it. Do not solve that by copying the
entire potentially long-lived Promise component onto every request. Use one of
these bounded approaches:

1. parse the small built-in control vocabulary into reducer facts when tool
   calls are materialized; or
2. run a CAS-only argument-facts preparation activity for the affected calls,
   then let the workflow join its returned ids with current `CoreAgentState`
   before scheduling the main tool activity.

The first implementation should prefer the second approach unless the parsed
facts are also needed for deterministic engine branching. It adds a small
activity only to control batches, respects the existing 32-id tool-schema
limits, and does not give that activity session-store access. Either approach
must reject argument/fact mismatches rather than silently fall back to replay.

## 1. Workflow-Tool Dispatch And Emission Limits

**Implemented 2026-07-30.** `ToolInvocationRequest.workflow_tool` carries a
versioned `WorkflowToolCallRuntime`; executor dispatch and sibling cap
accounting use those supplied facts with no session-log fallback.

### Current problem

`SessionTools::invoke_batch` calls `workflow_tool_batch_runtime` before almost
every ordinary batch. That helper replays the complete session to:

- map tool names to `WorkflowToolBinding`; and
- count prior emissions for each binding in the active run.

Bindings are already in `CoreAgentState.workflow_tools.bindings`, and emission
counts are already derivable from `CoreAgentState.workflow_tools`.

### Change

While constructing each `ToolInvocationRequest`, look up whether its tool name
belongs to a workflow-tool binding. If it does, attach:

- the exact admitted binding; and
- `emission_count(run_id, binding.tool_id)` as observed before this batch.

`SessionTools` dispatches a workflow tool only when this runtime field is
present. It verifies that the call name and binding definition agree, applies
the existing cap to the supplied prior count, and creates the normal trusted
workflow-tool effect. Calls without the field follow built-in, remote MCP, or
other generic runtime dispatch.

For multiple calls to the same workflow tool in one parallel batch, enforce
the cap deterministically using `prior_emission_count + successful earlier
calls in stable batch order`, preserving the current behavior.

Delete `workflow_tool_batch_runtime` and remove its session-store dependency.
No executor-side fallback replay is allowed when the runtime field is absent;
absence means the call is not a workflow-tool call for that planned batch.

### Tests

- managed and system workflow-tool calls dispatch from supplied bindings;
- binding fingerprint/name mismatches fail the call;
- emission caps account for prior calls plus siblings in the same batch;
- ordinary file/web/MCP-only batches do not load session entries; and
- an activity retry uses the same binding/count input even if unrelated store
  state changes.

## 2. Promise `await`, `cancel`, And `detach`

**Implemented 2026-07-30.** Control arguments are prepared through a CAS-only
activity, joined against workflow-owned state, and carried per call. Both
executor paths reject missing or mismatched facts and have no replay fallback.

### Current problem

`await` is already shaped correctly: the executor parses an `AwaitSpec`, and
the engine validates requested Promise ids and scope against its current state
when admitting the deferred batch.

`cancel` and `detach` instead replay the owning session in both `SessionTools`
and Fleet to produce model-visible statuses and trusted effects. The replay is
unnecessary because the workflow has the relevant Promise component.

### Change

For a batch containing `cancel` or `detach`, first obtain the bounded requested
ids through the argument-facts boundary above, then carry a Promise control
projection for exactly those ids from the scheduling state. Each found record
needs only:

- id;
- model/runtime ownership;
- run/session scope; and
- pending/resolved/failed/cancelled status.

Do not include Promise payloads, error bodies, sources, provider detail, or the
rest of `CoreAgentState`. The activity parses requested ids from the CAS-backed
tool arguments and evaluates the existing user-visible rules against the
supplied projection:

- unknown ids fail;
- runtime-owned Promises cannot be model-controlled;
- cancelling terminal Promises reports their terminal status;
- detaching a session-scoped Promise reports `already_detached`;
- detaching a pending Promise owned by another run fails; and
- valid pending operations emit the existing cancel/detach effects.

The engine continues to validate those effects when it admits the tool result.
This second check protects invariants and determines the durable event order;
the runtime projection is not a new authority.

Use one shared Promise-control helper for the built-in and Fleet executor
paths. Remove `cancel_promises_from_session`, `detach_promises_from_session`,
and the duplicated Fleet log replay.

Preserve requested ids that are absent from state so the executor can report
them as `unknown`; do not drop them during the join. The existing tool schemas
bound each control call to 32 ids, so P109 does not need to impose a new total
limit on the session's Promise component.

### Tests

- every ownership/scope/status branch matches current output and effects;
- cancel/detach batches perform zero `SessionStore::read_after` calls;
- a Promise resolution queued while the activity is running is ordered by the
  workflow/engine when results are admitted, not by an executor store read;
- Fleet and non-Fleet deployments produce identical Promise behavior; and
- `await` remains engine-validated and gains no replay-based preflight.

## 3. Environment Discovery And Selection Tools

**Implemented 2026-07-30.** A versioned batch-level environment policy carries
the admitted provider filter alongside the existing active environment id.
Ordinary `SessionTools` no longer owns a `SessionStore`; resolver observations
remain live external reads.

### Current problem

`ToolInvocationBatchRequest` already carries `active_environment_id`, but
`SessionTools::environment_policy` replays the session again to recover both
that id and `features.environments.providers` for every environment control
call. The same helper is also reached while resolving an active environment
for ordinary process/filesystem batches, making this a general active-session
hot path rather than only a discovery-tool cost.

### Change

Add the admitted environment provider filter to the batch runtime context.
Derive it from `SessionConfig.features.environments` alongside the existing
active id. Presence of the installed selection tools already proves the
feature/sub-grant; the activity must not reload config to prove it again.

Environment tools then:

1. use the supplied provider filter and active id as session policy/state;
2. query `EnvironmentResolver` for live universe resources and observations;
3. return current list/read results; and
4. produce the existing activation/deactivation effects.

Live environment existence, status, capabilities, connection data, and
credentials remain resolver reads. They are not copied into deterministic
state or activity input.

Delete `SessionTools::environment_policy`. Environment selection tools must be
usable without giving ordinary `SessionTools` a `SessionStore`.

### Tests

- absent and explicit provider filters are enforced from request facts;
- the active marker comes from the supplied active id;
- activation/deactivation retain the mixed-batch restriction from P108;
- provider status changes remain visible through live resolver reads; and
- environment selection calls perform zero session-log reads.

## 4. Environment-Job Workflow Starts

**Implemented 2026-07-30.** `job_submit` is active-environment-only. The model
cannot override its target; the runtime pins the original batch's active id
and admitted provider policy in a CAS-backed execution context carried by the
durable workflow-tool invocation.

### Current problem

The environment-job workflow-tool preparation activity accepted an optional
environment id. When it was omitted, the activity replayed the holder session
to find `EnvironmentState.active_environment_id`.

That selection existed when the original tool call was planned and must not be
reinterpreted later when the start-on-call workflow happens to prepare its
provider request.

### Change

At the original tool-batch boundary, require
`ToolInvocationBatchRequest.active_environment_id` and fail the original call
clearly when it is absent. `JobSubmitArgs` and the canonical model-facing schema
do not expose an environment override.

Keep the model-authored `arguments_ref` unchanged so generic workflow-tool
argument validation retains its exact-reference invariant. Write a separate,
versioned CAS document containing the active environment id and admitted
provider filter, and attach its ref as
`WorkflowToolInvocation.execution_context_ref`. This field is opaque to the
engine and interpreted only by the receiving system workflow.

`environment_job_prepare_workflow_tool` requires that execution context and
never loads the holder session. It reads the pinned environment resource live,
rechecks the pinned provider policy and job capability, resolves credentials,
and prepares the provider workflow request.

The resolved environment is immutable for that invocation. Changing active
selection after the tool call must not redirect the pending or retried job
start.

### Tests

- the call uses the active id observed by the original batch;
- model-authored environment overrides are rejected and provider policy remains
  enforced;
- changing active selection before workflow preparation does not retarget the
  job;
- activity retry keeps the same environment identity; and
- preparation performs zero session-log reads.

## 5. Fleet Parent State And Cross-Session Operations

Fleet has two different state-access cases and must not conflate them.

### Owning parent state

**Implemented 2026-07-30.** Promise control and admitted Fleet policy come
from the same `CoreAgentState` that scheduled the batch. `fleet_policy` is a
bounded transient projection on `ToolInvocationBatchRequest`; spawn policy and
profile list/read consume it without reconstructing the parent. Cancel and
detach consume their per-call Promise-control projections.

Spawn, clone, and fork are intentionally cold paths: they create durable
sessions, links, workflows, and sometimes workspaces. They may reconstruct the
source state needed by `core_agent_clone_opening_events`, and VFS isolation may
reconstruct the just-created child. This avoids a clone-seed query/export
protocol whose only benefit would be eliminating replay from an already rare
creation operation. The replay calls remain local to child-creation helpers;
ordinary Fleet messaging, profile inspection, and Promise operations cannot
reach them.

### Target session admission

**Implemented 2026-07-30.** `agent_send` and `agent_request` authorize only a
direct session link in the catalog, then submit the command to the target
workflow's existing admission funnel and wait on its workflow-local status.
They do not load the target session record, replay target events, or call
`start_session` before every delivery.

The target engine owns open/closed state, compaction exclusion, input
validation, stable-submission idempotency, mailbox wake-up, and run allocation.
The former Fleet-specific 64-run queue preflight and `QueueFull` result were
removed. If product backpressure is needed later, define one deterministic
engine/session limit rather than a racy Fleet-only observation.

Successful tool output is compact and unconditional: `agent_send` returns the
target and submission id; `agent_request` additionally returns the admitted
run id and holder Promise. Missing parents, unlinked targets, closed targets,
and admission rejections are ordinary tool failures rather than successful
status documents. Both operations may address any directly linked session,
including a parent.

Named-session clone sources use the same explicit cold-path reconstruction as
self-clones. This is not a fallback for messaging or target admission.

### Explicit inspection

`agent_read` and transcript/event reads are explicitly model-requested
inspection operations. They may continue to use bounded API/event projections
in P109. General optimization of gateway `session/read` and historical reads
is a separate read-model project.

Session link/catalog reads also remain valid: they read graph resources, not
reconstructible owning-session state.

### Tests

- Fleet policy and profile tools use supplied parent facts with no parent
  replay;
- self-clone and VFS isolation preserve P107/P108 config and active selection
  across their allowed creation-time replay;
- send/request are admitted by the target workflow without target replay;
- named-source clone reconstructs state only inside the explicit creation
  path;
- explicit `agent_read`/event-read behavior remains unchanged; and
- counting and store-free tests prove no hidden event-page read in ordinary
  send/request/profile/cancel/detach paths; spawn is explicitly excluded.

## 6. Runtime Ownership And Dependency Cleanup

**Implemented 2026-07-30.** After the handoffs above:

- ordinary `SessionTools` must not own a `SessionStore` merely for state
  reconstruction;
- `runtime_projection_refresh` continues to receive workspace declarations,
  prompt/skill roots, and active projection refs explicitly;
- environment-job preparation receives only blob storage, environment catalog,
  and credential resolution dependencies; its type cannot access session
  history;
- Fleet keeps session graph/storage dependencies needed to create links,
  clones, and forks, while replay calls are local to child creation; and
- hot-path modules do not import or call `read_all_session_entries` or
  `replay_core_agent_state`.

Prefer narrow traits for live resources and storage mutations. Removing broad
store access makes regressions harder than relying only on comments.

A counting `SessionStore` records event-page reads for Fleet send/request and
profile policy tests. SessionTools concurrency/environment/job tests construct
their executors without any session store, which makes the zero-read guarantee
structural; the job retry test also proves retries reuse pinned runtime input.

## Allowed Replay Boundaries

P109 deliberately permits complete session reduction at these boundaries:

- initial Temporal session-workflow bootstrap;
- continue-as-new bootstrap/rehydration;
- reaper, repair, audit, or disaster-recovery flows whose purpose is to
  reconstruct sessions after runtime loss;
- explicit gateway/API session projections until a separate read model is
  introduced; and
- explicitly requested bounded historical/event inspection; and
- Fleet spawn, clone, fork, and child-creation resource isolation.

Temporal's own deterministic workflow-history replay is not a Lightspeed
session-log read. Recorded activity results must not cause the storage activity
to execute again during Temporal replay.

The allowlist is intentional. New exceptions require an architecture decision;
"the activity only has a session id" is not sufficient justification.

## Retry, Ordering, And Security Rules

- Frequent-path activity inputs capture the session-owned facts at scheduling
  time. A retry uses those facts even if config, selection, or external
  catalogs later change. Creation-time clone reconstruction is the explicit
  exception and observes the source used for that creation attempt.
- External resource resolution remains live unless another roadmap explicitly
  pins it. For example, an environment identity is fixed while its provider
  availability is resolved live.
- Tool effects remain untrusted until engine admission validates them against
  current deterministic state.
- Runtime projections must contain admitted facts, never model-asserted policy.
- Do not replace replay with an unversioned process-local cache. A cache may
  accelerate external reads but cannot become session-state authority.
- Do not add a current-session side table for each feature. If a future generic
  materialized state read model is introduced, the session log remains
  authoritative and P109 activity requests remain self-contained.

## Temporal Compatibility

P109 is being implemented before hosted Temporal histories need compatibility
with the old activity-input shape, so the request changes are direct and carry
no patch branch or legacy replay fallback. Once deployment compatibility is
required, future activity-input changes must use the SDK's patch/version
mechanism; Serde defaults alone do not preserve command matching during
workflow replay.

The change does not require a session feature-version bump because no durable
session behavior or client config vocabulary changes. Internal request structs
may carry an explicit runtime-context version to reject unsupported live
activity payloads clearly.

## Non-Goals

P109 does not:

- optimize `session/read`, config puts, profile application, or other gateway
  handlers that currently replay for API projection/admission;
- replace initial bootstrap, continue-as-new rehydration, or recovery replay;
- introduce a generic persisted `CoreAgentState` snapshot or session read
  model;
- move live VFS, environment, credential, or provider observations into the
  deterministic engine;
- change public tool schemas or client-facing session APIs apart from removing
  the `job_submit` environment override; or
- address the separate P107 audit item where mutable-workspace skill/prompt
  scanning should read the pre-run pinned head rather than reopen a live head.

## Implementation Order

1. **Complete:** add runtime-fact projections and populate them in
   `next_tool_batch_request`, with engine unit tests.
2. **Complete:** remove workflow-tool batch replay and validate the zero-read
   ordinary tool path.
3. **Complete:** unify Promise control over supplied facts and delete both
   built-in and Fleet parent replay.
4. **Complete:** environment-job starts pin the original active identity and
   provider policy in an opaque execution context; preparation does not replay.
5. **Complete:** target send/request admission is replay-free and
   target-authoritative; admitted Fleet policy is carried on the batch while
   clone reconstruction remains an allowed creation boundary.
6. **Complete:** remove broad store dependencies/imports and add zero-read
   structural and counting-store regression tests.

## Done When

- [x] Ordinary non-`await` tool batches do not read or replay their owning
      session log.
- [x] Workflow-tool dispatch and emission caps use binding/count facts carried
      from `CoreAgentState`.
- [x] Promise cancel/detach use one shared supplied projection; built-in and
      Fleet paths perform no parent replay.
- [x] Environment discovery/selection uses carried provider policy and active
      identity plus live `EnvironmentResolver` reads.
- [x] Environment-job preparation never replays the holder session and cannot
      be retargeted by a later active-environment change.
- [x] Fleet parent policy and send/request preflights do not replay session
      histories; clone/fork/VFS child creation is an explicit cold boundary.
- [x] Explicit cross-session inspection remains bounded and is clearly
      separated from hidden runtime preflight.
- [x] `SessionTools`, environment-job preparation, and ordinary Fleet hot paths
      have no `read_all_session_entries` / `replay_core_agent_state` fallback.
- [x] Counting-store and store-free-construction tests prove zero event-page
      reads across the covered hot paths, including activity retry tests.
- [x] Existing bootstrap, continue-as-new, reaper/recovery, gateway projection,
      and explicit history-read behavior remains intact.
- [x] Engine, temporal-workflow, temporal-runtime, tools, and Fleet suites pass;
      relevant opt-in Temporal live paths pass serially against the local
      Temporal/Postgres/host-bridge stack.
