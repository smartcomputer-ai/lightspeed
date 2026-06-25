# P86: Durable Environment Jobs

**Status**
- Proposed 2026-06-25.
- G1-G5 implemented 2026-06-25 with the revised v1 shape:
  session-scoped provider jobs, optional per-job `queue_key`, explicit
  dependencies, no `deck_id`/group id, and no model-provided idempotency key.
  G5 adds Temporal-owned parked `job_wait` polling, absolute workflow
  deadlines, a short provider-read check activity, wake-hint signal plumbing,
  and continue-as-new blocking while environment job waits are active.
- Builds on **P75-P81 (Environments)** for the `fs`/`env` namespace split,
  active environment projection, provider registry, host protocol, and
  `host-bridge`.
- Builds on **P84 (Fleet Wait, Subscriptions, And Send)** for the generic
  deferred-tool-batch primitive (`ToolBatchOutcome::Deferred`, parked
  `ActiveToolBatch`, and `ResumeToolBatch`) and the Temporal workflow pattern
  for cheap hours-to-days waits.
- Owns the roadmap item: *"run coding agent (CC or Codex) on sandbox"* at the
  primitive level only. Product wrappers for Codex, Claude Code, repository
  checkout, PR creation, and result interpretation are deliberately deferred.

## Goal

Let a Lightspeed agent start, inspect, wait for, and cancel long-running work on
an active VM/sandbox/attached host without holding a host exec tool call open.

The primitive must cover both:

1. **Plain long host work** — repository checkout, dependency install, file
   download, large test run, build, migration, artifact generation.
2. **Guest-resident agent work** — launching Codex, Claude Code, OpenCode, or a
   custom coding agent as a process inside the environment.

The first implementation should expose only a small generic job surface. Anything
specific to "run Codex and make a PR" should be expressible as ordinary job
commands first, then later wrapped by skills or higher-level tools if product
pressure justifies it.

## Problem

`run_process` is the wrong unit for one-hour work.

The existing process tool starts a process through the active environment,
optionally waits a short interval, and returns a bounded output snapshot plus a
process handle. That is correct for shell commands and interactive snippets. It
is not correct for:

- checking out a large repository;
- running a coding agent for an hour;
- staging a sequence of VM tasks where later tasks must start only after earlier
  tasks finish;
- waiting cheaply while the parent Lightspeed session is parked or idle;
- surviving worker restarts without losing the join point.

Increasing `timeout_ms` would push the wrong abstraction harder: a Temporal tool
activity would be held open, output would be awkwardly bounded, cancellation and
retry semantics would be unclear, and the session would not have a durable job
handle it can inspect later.

P84 solved the analogous problem for Lightspeed subagents: a parent waits on a
run handle, and the workflow parks/resumes the tool batch. P86 applies the same
shape to work running inside an environment.

## Design Decision

Introduce **Environment Jobs**: durable, provider-backed work units attached to a
session environment.

An environment job is not a Lightspeed run and not a Fleet child. It is runtime
state for work executing inside a concrete `env:<id>` target.

```text
canonical job handle = { session_id, env_id, job_id }
input job handle      = { session_id?, env_id?, job_id }
```

Tool inputs may omit `session_id` and `env_id` for the common case:

- omitted `session_id` means the calling session;
- omitted `env_id` means the current active environment for that session.

Tool outputs should always return the fully resolved canonical handle. That lets
later calls remain exact even if the active environment changes.

The model-visible surface is five tools:

```text
job_start   start one or more jobs on an environment
job_list    read the latest known jobs for a session
job_read    read job status, output tail, and artifacts
job_wait    join one or more jobs; may park the current tool batch
job_cancel  cancel/terminate one or more jobs
```

`job_start` defaults to the active `env` execution target, but may
also accept an explicit `env_id` to start work on any environment bound to the
calling session. The other tools operate on job handles whose `env_id` may be
explicit or defaulted, and should continue to work even after the environment is
no longer active, as long as the session environment binding still exists and the
provider is reachable.

## Relationship To Existing Tools

Environment jobs extend the **environment** tool family. They do not change the
filesystem tool model.

Current split:

```text
fs tools                  -> fs:session
  read_file
  write_file
  edit_file
  apply_patch
  grep / glob / list_dir

environment process tools -> env:<active-env-id>
  run_process
  write_process_stdin

environment job tools     -> start/list/read/wait/cancel durable work on envs
  job_start
  job_list
  job_read
  job_wait
  job_cancel
```

Implementation-wise, the jobs code should live beside process execution under
`crates/tools/src/environment/`, for example:

```text
environment::process       short process capability
environment::jobs          durable job capability
environment::tools         model-visible process/job tools
```

The likely capability boundary is:

```rust
pub struct EnvironmentToolContext {
    pub process: Option<Arc<dyn ProcessExecutor>>,
    pub jobs: Option<Arc<dyn JobExecutor>>,
    ...
}
```

`job_start` follows the same defaulting model as `run_process`: when
no `env_id` is provided, the core-stamped active `env:<id>` target selects the
session environment binding/provider. When `env_id` is provided, the runtime
validates that the named environment is bound to the calling session and routes
the start there, even if it is not currently active.

`job_read`, `job_wait`, and `job_cancel`
operate on job handles. They belong to the environment package, but they should
not require the currently active environment to still be selected when `env_id`
is explicit. If `env_id` is omitted, they default to the current active
environment. In v1, they resolve the handle through the runtime job handle
registry, then call the provider/target recorded for that handle. This keeps
long-running work inspectable even if the agent later switches or deactivates
the active environment, provided the caller uses the canonical handle returned
by `job_start`.

## Tool Surface

### `job_start`

Starts one or more jobs on an environment and returns durable handles.

Input shape:

```text
env_id?                environment to start on; defaults to active environment
jobs[]                 one or more job specs
```

Job spec:

```text
name?                  model-chosen local name, unique within this start call
job_id?                optional explicit durable job id
argv                   process argv
cwd?                   process working directory
env?                   environment overrides
stdin?                 inline stdin text
timeout_ms?            provider-side timeout
depends_on?            [ JobDependency ]              explicit dependency edges
dependency_policy?     all_succeeded | all_terminal   default all_succeeded
queue_key?             optional per-session serialization key
```

Dependency reference:

```text
{ job_id }              already-known job in this session environment
{ name }                another job in the same start request
```

Output shape:

```text
env_id                 resolved environment id
jobs[]:
  name?
  job_id
  handle               { session_id, env_id, job_id }
  status                queued | running | succeeded | failed | cancelled | ...
  dependencies[]        resolved job ids
  queue_key?
```

The model does not provide provider idempotency keys, arbitrary metadata, or
group ids in v1. The runtime derives omitted `job_id`s and a provider
`request_id` from the session/run/turn/tool-call identity, then records only the
accepted handles. That makes tool activity retries idempotent without requiring
the model to invent stable retry keys.

#### Scheduling

Jobs are eligible to run when their dependencies are satisfied, their optional
`queue_key` is free, and provider capacity is available. Jobs without
dependencies or queue conflicts may run in parallel.

`queue_key` is a per-session serialization primitive. It is not a batch mode:
any job with the same queue key waits until the previous non-terminal job with
that key finishes. Different queue keys may run in parallel.

`depends_on` is the general DAG mechanism. It lets the agent define a multi-step
job plan:

```text
jobs:
  - name: checkout
    argv: ["git", "clone", "...", "/workspace/repo"]
  - name: install
    argv: ["npm", "install"]
    cwd: "/workspace/repo"
    depends_on: [{ name: checkout }]
  - name: tests
    argv: ["npm", "test"]
    cwd: "/workspace/repo"
    depends_on: [{ name: install }]
  - name: agent
    argv: ["codex", "exec", "--cwd", "/workspace/repo", "..."]
    depends_on: [{ name: tests }]
```

The provider runs submitted jobs as a DAG plus queue constraints:

- a job with no unsatisfied dependencies is eligible;
- a job with `dependency_policy = all_succeeded` starts only if all dependencies
  reached `succeeded`; a failed/cancelled dependency marks it
  `dependency_failed`;
- a job with `dependency_policy = all_terminal` starts after dependencies are
  terminal regardless of success, useful for cleanup/reporting jobs;
- a job with `queue_key` starts only when no earlier non-terminal job in that
  same session namespace and queue key is active;
- dependency cycles, duplicate local names, and references to unknown jobs are
  rejected before any job is started.

`job_id` may be supplied when the caller wants stable names. If omitted, the
runtime derives it from:

```text
session_id + env_id + run_id + turn_id + tool_batch_id + tool_call_id + job index
```

That makes tool activity retries idempotent.

### `job_list`

Reads the latest known jobs for a session.

Input:

```text
session_id?            defaults to the calling session
env_id?                optional environment filter
limit?                 latest N jobs; no cursor in v1
```

The provider namespace is the session id. There is no separate model-visible
namespace partition in v1. `job_list` is latest-N only, intended for recovery
after compaction, retries, and debugging before selecting exact handles for
`job_read`, `job_wait`, or `job_cancel`.

Output:

```text
jobs[]:
  handle
  status?
  created_at_ms?
  queued_at_ms?
  started_at_ms?
  finished_at_ms?
  exit_code?
  failure?
  dependencies[]
  error?
```

`job_list` should not include output tails by default. Models should use
`job_read` when they need bounded stdout/stderr chunks or artifacts.

### `job_read`

Reads job state without waiting.

Input:

```text
jobs[]                 [ { session_id?, env_id?, job_id } ]
output_bytes?          max output bytes to include
after_seq?             optional stream cursor
include_artifacts?     bool
```

`session_id` defaults to the calling session. `env_id` defaults to the calling
session's active environment. v1 only allows reading jobs owned by the calling
session; cross-session access should go through Fleet or a later capability
policy.

Output:

```text
jobs[]:
  handle
  status
  created_at_ms
  queued_at_ms?
  started_at_ms?
  finished_at_ms?
  exit_code?
  failure?
  dependencies[]
  stdout_tail?
  stderr_tail?
  output_next_seq?
  artifacts[]
```

Large logs and generated files must stay in the environment filesystem or CAS.
The tool returns bounded output chunks, cursors, and artifact references/paths.

### `job_wait`

Joins one or more jobs.

Input:

```text
jobs[]                 [ { session_id?, env_id?, job_id } ]
mode                   all | any                      default all
terminal_policy        any_terminal | all_succeeded   default any_terminal
timeout_ms?            optional timeout
output_bytes?          output bytes to include on resolution
```

Semantics:

- `mode = all`: resolve when all handles satisfy `terminal_policy`.
- `mode = any`: resolve when the first handle satisfies `terminal_policy`.
- `timeout_ms` resolves with `outcome = timeout` and partial job states.
- dependency-failed jobs are terminal job states, not transport errors.
- unknown/unreadable jobs are per-handle `error` entries.

If the wait is already satisfied at preflight, the tool returns inline. If at
least one job is still running/queued and the join is not satisfied, the tool may
return `ToolBatchOutcome::Deferred` with directive kind:

```text
lightspeed.environment.job_wait
```

The engine remains generic. It stores the opaque resume directive and knows only
that the tool batch is parked. Temporal workflow code owns timers, polling,
provider callbacks, and `ResumeToolBatch`.

### `job_cancel`

Cancels one or more jobs.

Input:

```text
jobs[]                 [ { session_id?, env_id?, job_id } ]
scope                  job | dependents                default job
force?                 provider-specific hard kill
```

`scope = job` cancels only the named jobs. `dependents` also cancels queued jobs
that depend on them.

Cancellation is best-effort for already-running OS processes. The result is a
job status transition, not necessarily immediate process death.

## Runtime Model

### Job Handle Registry

Add a first-class Lightspeed-side **job handle registry**, following the same
shape as the existing MCP/profile/environment registries: provider-independent
records and a store trait in a registry crate, with in-memory and Postgres
adapters outside the deterministic engine.

The registry belongs at the environment boundary for v1. The smallest
implementation can add `JobHandleStore` and DTOs to `environment-registry`,
beside `SessionEnvironmentBindingStore`, because a job handle is meaningful only
with the session environment/provider/target tuple that accepted it. If the
surface later grows beyond environment-scoped handles, it can split into a
dedicated `job-registry` crate without changing the provider job protocol.

The registry is not a job-state table. It records the fact that Lightspeed
created or observed a durable handle and how to route that handle back to the
environment provider:

```text
JobHandleRecord
  session_id
  env_id
  provider_id
  target_id
  namespace        // equals session_id in v1
  job_id
  name?
  queue_key?
  created_by_run_id?
  created_by_turn_id?
  created_by_tool_call_id?
  created_at_ms
  start_request_hash
```

This record says only: "Lightspeed accepted this provider-backed handle for this
session environment." It should not contain:

- status;
- stdout/stderr cursors or tails;
- exit code;
- failure text;
- dependency progress;
- provider-observed timestamps;
- artifact lists;
- copied argv/env/stdin beyond a hash.

The environment provider is the only source for those facts. If the provider is
unreachable, job state is `unknown` from Lightspeed's perspective.

The handle registry is still useful for Lightspeed-level concerns:

- ownership: this session is allowed to reference this handle;
- routing: which provider/target accepted the handle, even if the session's
  current `env_id` binding is later updated, detached, or reattached elsewhere;
- idempotency/audit: this deterministic `job_id` was issued with this start
  request hash;
- listing handles known to Lightspeed, without claiming their live state.

Recommended store surface:

```text
JobHandleStore
  create_job_handles(records[]) -> records[]
  read_job_handle(session_id, env_id, job_id) -> record
  list_job_handles(session_id, env_id?, limit?) -> records[]
  delete_job_handle(session_id, env_id, job_id) -> record       optional/deferred
```

This is an internal registry API. Model-visible `job_start`, and any future
public `session/jobs/create` method, must mean "ask the provider to start or
accept the job, then register the accepted handle." They must not merely create
a local registry row.

`create_job_handles` should be idempotent for the same
`{session_id, env_id, job_id, start_request_hash}` and should reject the same
handle with a different hash. That gives Lightspeed a local conflict check
without becoming responsible for execution state.

`job_start` should use two layers of idempotency:

1. Derive or accept stable `job_id`s and compute `start_request_hash` from the
   material job start input.
2. Call provider `job/start` with those ids. The provider is the execution
   authority and must make same-id/same-spec retries idempotent.
3. After provider acceptance, create/upsert the handle records. If the runtime
   crashes after provider acceptance but before the registry write, retrying
   `job_start` calls the provider with the same ids, receives the existing
   accepted handles, and then records them locally.

Registering after provider acceptance avoids durable local rows for jobs the
provider never accepted. It is still safe because provider-side idempotency is
the correctness boundary for duplicate execution.

`job_read`, `job_wait`, and `job_cancel` first read the handle record and use its
`provider_id`/`target_id` for routing. If the record is missing, the normal tool
result is an unknown-handle error for that session, not an inferred provider
state. Operator/debug APIs may later offer direct provider reads, but the
model-visible surface should stay registry-backed for ownership and routing.

### Provider Job Statuses

```text
accepted
queued
running
succeeded
failed
cancel_requested
cancelled
timed_out
dependency_failed
interrupted          provider/bridge restarted and cannot reattach
lost                 provider cannot find a previously accepted job
```

Terminal statuses:

```text
succeeded | failed | cancelled | timed_out | dependency_failed | interrupted | lost
```

### Authority Split

The crucial rule:

```text
Provider / bridge / sandbox runner = execution authority
Lightspeed store                   = job handle registry only
Temporal workflow                  = parked wait state and timers
```

The provider owns:

- dependency scheduling;
- queue-key ordering;
- process start/exit/cancel;
- retained stdout/stderr;
- whether a job survived a bridge/provider restart;
- whether a VM reset wiped the job.

Lightspeed must not pretend to know more than the provider. If the provider is
unreachable, job state is unknown. If the provider reports that a job was lost or
interrupted, that is a provider result returned by `job_read`/`job_wait`, not a
local database inference. A stale handle row is never proof that work is still
running, queued, terminal, or even present.

The Lightspeed job handle registry is useful only because the agent and API need
stable, session-owned handles, exact provider/target routing, listing, and
retry/audit records. It is not needed to know job progress, and it must not be
used that way.

### Provider responsibility

The environment provider is the authority for actual execution order. Lightspeed
submits session-namespace job specs to the provider; the provider starts
eligible jobs according to dependencies, per-job queue keys, and capacity.

For `host-bridge`, this means adding a job manager beside the current process
manager:

- client-chosen job ids;
- idempotent `job/start`;
- dependency graph validation;
- queue-key FIFO scheduling;
- process group start/terminate;
- retained stdout/stderr chunks with monotonic sequence numbers;
- optional job working directory such as `.lightspeed/jobs/{job_id}`;
- result artifact discovery;
- startup recovery that marks unrecoverable running jobs `interrupted`.

Providers with stronger persistence can keep jobs running across bridge/worker
restart. Providers that cannot reattach must say so clearly through
`interrupted`, not silently forget the job. If a VM is wiped or reset and the
provider cannot find the job at all, `job/read` returns a typed missing/lost
state so Lightspeed can reconcile its ledger.

## Host Protocol Additions

Keep this in `host-protocol`; do not rename it to `environment-protocol`.
`environment` is the Lightspeed/session abstraction (`env:<id>`, binding,
projection, tools). `host-protocol` is the substrate wire protocol for a concrete
target that exposes filesystem/process/job capabilities. A host-protocol
implementation may run inside the guest OS, outside the VM as a controller, or
as a provider adapter. The name is still correct because the protocol speaks to
the execution host/target, not to the model-facing environment abstraction.

Jobs should be added as a new **data-plane** method family, not as a separate
protocol. Controller-plane methods still create/attach/list/close targets and
return a `HostConnectionSpec`. Once a session has a data-plane connection for a
target, durable work on that target is started/read/cancelled through `job/*`,
beside `fs/*` and `process/*`.

Recommended crate shape:

```text
crates/host-protocol/src/data/jobs.rs
crates/host-client/src/data.rs        typed job methods beside process/fs
crates/host-bridge/src/jobs.rs        bridge JobManager beside ProcessManager
```

Host-protocol job ids are target-local within a provider namespace. They should
not contain Lightspeed `session_id` or `env_id`; those belong to the Lightspeed
job handle registry. The runtime supplies `namespace = session_id`. The registry
maps `{ session_id, env_id, job_id }` to the provider/target and namespace that
accepted the target-local `job_id`.

Extend `HostCapabilities` with job capabilities:

```text
job_start
job_list
job_read
job_cancel
job_wait_hint          optional; provider can long-poll or callback
job_dependencies
job_queue_keys
```

These capabilities must be mirrored through the existing environment capability
path:

```text
HostCapabilities
  -> EnvironmentTargetRecord.capabilities
  -> SessionEnvironmentCapabilities
  -> EnvironmentRecord.capabilities
  -> active environment prompt/tool availability
```

Existing providers remain valid: new booleans default to `false`, and the worker
must only advertise model-visible `job_*` tools when the active environment
supports them.

Add data-plane methods:

```text
job/start
job/list
job/read
job/cancel
```

`job/start` accepts one or more job specs for a session namespace and returns job
summaries. It must be idempotent for the same `{ namespace, job_id, spec }`.
If the same `{ namespace, job_id }` is reused with different material input, it
rejects with conflict.

Recommended v1 payload:

```text
StartJobsParams
  namespace              runtime-supplied session id
  request_id             runtime-supplied retry/debug id
  jobs[]:
    job_id               runtime-derived unless explicitly supplied
    name?
    argv
    cwd?
    env?
    stdin?
    timeout_ms?
    depends_on?
    dependency_policy?
    queue_key?

ListJobsParams
  namespace              runtime-supplied session id
  limit?                 latest N jobs; no cursor in v1

ReadJobsParams
  namespace              runtime-supplied session id
  jobs[]
  after_seq?
  max_bytes?
  include_artifacts?
  wait_ms?

CancelJobsParams
  namespace              runtime-supplied session id
  jobs[]
  scope                  job | dependents
  force?
```

`job/read` is the source of truth for live status and retained output. It should
support reading many jobs at once. Lightspeed should not cache provider status;
each read/wait preflight asks the provider when it is reachable. If the provider
is not reachable, the result is an `unknown`/unreachable tool result, not a
locally inferred state.

`job/cancel` requests cancellation and returns updated summaries.

A future `job/subscribe` or provider-to-gateway callback can reduce polling
latency, but v1 should not require it.

This means P86 must add real host-protocol code, not only Lightspeed tools:

- protocol DTOs, method constants, serde fixtures, and compatibility tests;
- `host-client` typed `start_jobs`, `read_jobs`, and `cancel_jobs` calls;
- `host-bridge` request handlers and a `JobManager`;
- tool/runtime adapters that implement a new environment `JobExecutor` by
  calling the host data-plane methods.

The bridge `JobManager` can reuse the process-spawning internals, but it cannot
be just `process/start` exposed under another name. Jobs need session namespace/job
idempotency, dependency scheduling, queue keys, retained output/status after
the starting tool call returns, and restart recovery that reports
`interrupted`/`lost` clearly. It must reserve or idempotency-check job ids before
spawning OS children, so a conflicting retry cannot accidentally start work.

## Waiting In Temporal

P86 should reuse P84's parked-batch machinery, but the wake source is different.
Fleet waits can subscribe to another session workflow's terminal run event.
Environment jobs run inside a VM/provider, so the universal v1 correctness path
is **workflow-owned polling with durable timers**.

### Wait Ownership

The owning `AgentSessionWorkflow` should own the parked `job_wait`, not a
separate polling workflow in v1.

Reasoning:

- the parked tool batch belongs to the session workflow, and only that workflow
  can safely emit `ResumeToolBatch` into the session log;
- Temporal timers already give the session workflow cheap 12-hour sleeps without
  occupying a worker or activity slot;
- a separate poller workflow would still have to signal the session workflow to
  resume the batch, adding lifecycle, cancellation, idempotency, and ownership
  surfaces without removing the need for session-side wait state;
- the specific job logic can be kept out of `engine` and isolated in the
  Temporal runtime as a directive handler, beside the existing Fleet wait
  handler.

The main workflow loop should stay generic: it keeps parked deferred waits,
selects on the nearest workflow wake time plus admissions/signals, and dispatches
due waits to directive-specific handlers. A reasonable implementation shape is:

```text
ActiveDeferredWait
  fleet_run_wait(...)          existing P84 shape
  environment_job_wait(...)    new P86 shape
```

The deterministic `engine` still only sees a parked tool batch and a later
`ResumeToolBatch`.

### Active Job Wait Record

When a `job_wait` defers, the workflow records durable wait metadata:

```text
ActiveEnvironmentJobWait
  batch_id
  run_id
  turn_id
  call_id
  handles[]                 canonical { session_id, env_id, job_id }
  mode                      all | any
  terminal_policy           any_terminal | all_succeeded
  output_bytes?
  include_artifacts?
  deadline_ms?              workflow-computed absolute timeout deadline
  next_check_at_ms          workflow-computed absolute poll deadline
  poll_attempt
```

This is wait state, not environment job state. It should not store provider
statuses, output tails, exit codes, artifact lists, or provider timestamps.
Every check asks the provider through `job/read`; a timeout result performs a
fresh final read when reachable.

Flow:

```text
model calls job_wait
  -> tool activity resolves handles through JobHandleStore
  -> tool activity preflights job states through provider job/read
  -> if already satisfied: return inline result
  -> else: ToolBatchOutcome::Deferred { resume_directive }
  -> AgentSessionWorkflow records ActiveEnvironmentJobWait
  -> workflow sleeps until the earliest next_check_at_ms/deadline_ms or signal
  -> due wait starts a short check_environment_job_wait activity
  -> activity resolves handles and calls provider job/read
  -> if satisfied/timeout/error: build tool result and queue ResumeToolBatch
  -> otherwise workflow records a new absolute next_check_at_ms and sleeps again
```

The check activity must be short-lived. It performs reads and returns. It never
waits for the VM job to finish.

### Polling And Timeouts

`timeout_ms` is optional with no default. If absent, the wait is indefinite, but
unlike Fleet `agent_wait`, an indefinite environment job wait still needs
periodic checks unless the provider offers a reliable callback/subscription.

The workflow computes absolute times using workflow time:

- `deadline_ms = workflow_now + timeout_ms`, when a timeout is present;
- `next_check_at_ms = workflow_now + next_poll_delay_ms`.

On replay or worker restart, the workflow re-derives remaining sleep durations
from those absolute timestamps. The timeout does not reset, and unrelated
admissions/signals do not drift the deadline outward.

Polling intervals should be bounded, backoff-friendly, and configurable. The
provider may return a `next_check_after_ms` hint, but the workflow clamps it to
runtime limits. Suggested defaults:

```text
first minute: 2-5s
after that: 15-30s
long waits: configurable max, e.g. 60-300s
```

For a 12-hour job, the session workflow is not blocked. It is asleep in Temporal,
wakes only on poll timers or signals, runs a short provider-read activity, and
then sleeps again. Completion is reported when either a poll observes a
satisfied wait or a provider callback wakes the workflow to poll immediately.
The result lands as the original `job_wait` tool result in the same parked turn;
the normal agent loop then continues and can summarize/report back to the user.

Timeout behavior:

- if `deadline_ms` fires before the join policy is satisfied, the workflow asks
  the provider for one final snapshot and resolves `outcome = timeout`;
- if the provider is reachable, the timeout result includes current per-job
  status/output tails subject to the request limits;
- if the provider is unreachable at timeout, the per-job state is reported as
  unknown/unreachable rather than inferred from local state.

### Provider Wake Hints

Provider/gateway callbacks can reduce latency but should not be required for v1:

```text
provider job changed / heartbeat changed-jobs / future job subscribe
  -> gateway resolves owning session from JobHandleStore
  -> gateway signals AgentSessionWorkflow.environment_job_changed
  -> workflow marks matching ActiveEnvironmentJobWait due now
  -> workflow calls check_environment_job_wait activity
```

The signal should be treated as a wake hint, not as authoritative state. It may
carry `{ session_id, env_id, job_id }`, but the workflow still calls `job/read`
before resolving anything. Duplicate callbacks are harmless. A missed callback
only adds up to one poll interval of latency.

### Why Not A Separate Poller Workflow

A separate `EnvironmentJobWatcherWorkflow` is a reasonable future optimization
if scale requires batching many waits by provider/target. It is not the v1
correctness path.

Use a separate watcher later only if one of these becomes true:

- many thousands of active waits make per-session polling too expensive;
- provider APIs strongly prefer batched long-poll/subscription leases per target;
- active job waits need to survive session workflow continue-as-new instead of
  blocking it;
- operator/product needs require shared progress fan-out to multiple sessions.

Even then, the session workflow should remain the resume authority: the watcher
would signal "check this wait now" or deliver a provider snapshot, and the
session workflow would still turn that into `ResumeToolBatch` idempotently.

Continue-as-new must be blocked while active environment job waits are parked,
just as P84 blocks while active run waits/subscriptions exist. Carrying active
job waits through continue-as-new can be added later if history pressure demands
it.

## Interaction With Existing Concepts

### Fleet

Fleet remains for supervising other Lightspeed sessions.

Environment jobs are for supervising work inside a VM. A Fleet child may start
environment jobs, and a parent may `agent_wait` on that child. But a plain
checkout/download/Codex process should not have to become a Lightspeed child
session just to be waitable.

### `run_process`

Keep `run_process` as the short command/interactivity primitive.

Use environment jobs when the model wants durable handles, dependencies,
multi-step job plans, long waits, or later inspection.

### Environment activation

`job_start` defaults to the active environment selected by core tool
routing, but an explicit `env_id` may target any environment bound to the
session. `job_read`, `job_wait`, and
`job_cancel` accept handles with optional `env_id`; omitted `env_id`
means the current active environment, while explicit `env_id` does not require
that environment to be active at call time.

### Schedules and triggers

Environment jobs are not a scheduler. They execute work now, subject to
dependencies and capacity. P101-style timers/schedules may later start jobs as
their firing action, but recurring definitions and missed-run policy stay in the
trigger system.

## Security And Secrets

Secret injection into the VM is deferred. P86 assumes the environment already
has the credentials it needs.

Job handle records must still avoid accidental leakage:

- do not store raw secret env values in durable job handle records;
- do not store copied argv/stdin/env in the Lightspeed handle record;
- do not echo full commands into model-visible output when they contain obvious
  secret-shaped values;
- keep provider credentials and connection specs out of the session log and
  model context;
- expose only bounded output tails by default.

## Non-Goals

- No Codex/Claude/OpenCode-specific tool in P86.
- No repository checkout or PR wrapper in P86.
- No GitHub OAuth or provider-secret injection.
- No multi-tenant policy model beyond current session ownership checks.
- No guarantee that arbitrary OS processes survive VM reboot.
- No cross-session job access in v1.
- No full log streaming into the model context.
- No generic workflow-as-tool surface.

## Implementation Plan

### G1. Contracts And Job Handle Registry

- Add environment job DTOs and validation to `environment-registry`.
- Add a `JobHandleStore` trait to `environment-registry` with in-memory and
  Postgres implementations for handle ownership/routing/idempotency only.
- Add a Postgres table keyed by `(universe_id, session_id, env_id, job_id)` and
  indexed by `(session_id, env_id)` for listing.
- Do not store provider job status, output cursors, exit codes, dependency state,
  or artifacts in Lightspeed.
- Add terminal-state helpers for provider DTOs, not for local persisted state.

### G2. Host Protocol Job Plane

- Add job capability flags to `HostCapabilities`.
- Add `job/start`, `job/read`, and `job/cancel` payloads and serde tests.
- Extend `host-client` with typed job methods.

### G3. `host-bridge` Job Manager

- Implement client-chosen job ids and retained output.
- Implement dependency DAG validation and scheduling.
- Implement queue-key serialization.
- Implement cancellation and timeout.
- Implement bridge restart recovery with `interrupted` for unrecoverable jobs.
- Add tests for parallel jobs, queue keys, explicit dependencies, cancellation,
  timeout, and idempotent retry.

### G4. Tool Surface

- Add `job_start/list/read/wait/cancel` tool contracts in `crates/tools`.
- Add toolset config gate for environment jobs.
- Start defaults through the active `env` target but supports explicit `env_id`;
  read/wait/cancel use job handles with optional `env_id` defaulting.
- Add tool executor logic in `temporal-server` that can return deferred outcomes
  for lone `job_wait` batches.

### G5. Workflow Wait Integration

- Implemented 2026-06-25. The workflow owns active environment job waits,
  records only durable wait metadata, wakes on absolute poll/deadline times,
  calls `check_environment_job_wait`, and resumes the original parked tool
  batch when the wait is ready or timed out. Provider wake hints are accepted
  through `environment_job_changed` and force a fresh read; they are not treated
  as authoritative job state. `AgentSessionWorkflow` now lives under
  `crates/temporal-workflow/src/workflow/` with focused modules for bootstrap,
  admissions, driving, wait-loop selection, Fleet waits, environment job waits,
  activity calls, session state, errors, clock helpers, and tests.
- Add a `lightspeed.environment.job_wait` resume directive.
- Split the workflow implementation into focused modules under
  `crates/temporal-workflow/src/workflow/`; the main workflow loop owns generic
  orchestration and directive-specific wait machinery lives in dedicated files.
- Add workflow-local `ActiveEnvironmentJobWait` state with absolute
  `deadline_ms?` and `next_check_at_ms`.
- Add a short `check_environment_job_wait` activity that resolves handles,
  calls provider `job/read`, and returns ready/not-ready/timeout snapshots.
- Extend the workflow loop to select over admissions, existing Fleet wait
  deadlines, environment job wait poll deadlines, and timeout deadlines.
- Add optional `environment_job_changed` wake-hint signal from the gateway.
- Resume parked batches with bounded job summaries and output tails.
- Block continue-as-new while environment job waits are active.

### G6. API And Projection

- Add optional public API methods for operator/UI inspection and external
  clients.
- Add `session/jobs/create` if non-model API clients need to start environment
  jobs; it must share `job_start` semantics.
- Add `session/jobs/list` to return registry handle records only, without
  pretending to know live status. Model-visible `job_list` may also ask the
  provider for current summaries for the latest listed handles.
- Add `session/jobs/read` to resolve registry handles and ask the provider for
  live status/output.
- Add `session/jobs/cancel` to resolve registry handles and ask the provider to
  cancel.
- Keep model-visible tools and public API DTOs aligned where possible.
- Project active environment capabilities so the model can know job support is
  available.

### G7. Live Coverage

- Add ignored live tests with `host-bridge`:
  - start a long-ish job, wait until terminal without holding a process call;
  - start queue-keyed jobs and verify execution order;
  - start parallel jobs and verify overlap;
  - start a dependency DAG and wait only on the final job;
  - cancel a running job;
  - retry `job_start` and verify no duplicate execution.

## Open Questions

- Should `job_start` accept same-call `name` references in v1, or
  should v1 require explicit `job_id` for every dependency and add local names in
  G2?
- Should provider callbacks go through provider heartbeat payloads or a separate
  internal gateway endpoint?
- How much output should be mirrored to CAS automatically versus kept only in the
  environment filesystem?
- Should a failed dependency produce `dependency_failed` immediately, or should a
  queued dependent remain inspectable until the user cancels it? The
  recommended v1 behavior is immediate `dependency_failed`.

## Done When

- An agent can start a checkout job and a dependent coding-agent process job on
  an active environment.
- The jobs run in provider-managed order without a long-running tool activity.
- The agent can wait on the final job and have the parked tool batch resume when
  it completes.
- The agent can inspect output tails and artifacts after completion.
- Idempotent retry does not duplicate jobs.
- Serial and parallel execution are both covered by tests.
