# P114: Per-Call Tool Activities And Host-Side Search

**Status**

- Delivery steps 1–3 implemented (2026-08-05): per-call engine completion via
  `CoreAgentDrive::resume_tool_call` with the cross-call environment-selection
  invariant; `ToolExecutionSpec` on the admitted `ToolSpec` with bounded
  per-class Temporal options (`tool_call_activity_options`,
  `tool_batch_activity_options` — no tool activity retries unbounded); the
  `tool_invoke_call` activity with a worker-enforced operation deadline;
  workflow-boundary conversion of activity failures into terminal failed call
  results; and the deployment-owned `run_process` timeout ceiling
  (`ToolLimits::max_process_timeout_ms`, asserted equal to
  `PROCESS_TIMEOUT_CEILING` by a temporal-runtime test). Await batches and
  workflow-tool batches still execute as one bounded batch-unit activity, as
  designed. One deliberate reshape: the environment-selection batch rule now
  fails only the participating calls, not unrelated siblings.
- Review hardening (2026-08-05): per-call results are validated against the
  scheduled call id; cancelled activities record terminal cancelled results;
  parallel-safe groups execute as a topped-up window bounded by
  `MAX_CONCURRENT_TOOL_CALLS_PER_BATCH`; and boundary-failure materialization
  uses bounded attempts with a well-known fallback blob, so the failure path
  itself can never retry unbounded.
- Live-validated 2026-08-05 against the local stack (temporal_live 18/18,
  workflow_tool_plugins_live 14/14, environment_provider_live 5/5,
  preprocess_live 2/2), including the new
  `temporal_live_parallel_tool_batch_completes_per_call` regression test: a
  three-call parallel-safe batch scheduled as concurrent per-call activities
  while `session/read` polling issues mid-batch status queries.
- Step 4 implemented (2026-08-05): `environment-protocol` gained the
  `filesystem_search` capability and the bounded `fs/searchText` operation
  (root, regex, include glob, case sensitivity, max depth, and mandatory
  match/file/byte/time limits; the response reports scan statistics and a
  typed stop reason). The bridge implements it in-process with the ripgrep
  engine crates (`grep-searcher`, `grep-regex`, `ignore`) — no external `rg`
  binary — under its filesystem-root confinement, advertising the capability
  at the data-plane handshake. `RemoteEnvironmentFileSystem` prefers the native
  operation for environment grep; every other backend falls through to the
  now-bounded generic traversal (`ToolLimits::max_search_{matches,files,
  bytes,duration_ms}`: 1000 matches / 5000 files / 64 MiB / 30 s defaults —
  callers may lower the match limit but can raise nothing). `GrepResult`
  carries the typed stop reason and the model-visible output names the
  exhausted budget with a narrowing hint. Covered by bridge search unit
  tests, bounded-fallback and native-preference grep tests, and an
  end-to-end grep-over-WebSocket assertion in the bridge server test.
- Step 5 implemented (2026-08-05): `fs/globFiles` (capability
  `filesystem_glob`) executes bounded recursive enumeration at the host with
  the generic tool's pattern semantics and mandatory match/entry/time limits;
  the glob tool prefers it and its generic fallback now shares the bounded
  traversal (previously still unbounded for glob). `fs/readFile` gained
  optional `offset`/`maxBytes` with `fileSize`/`truncated` on the response
  (capability `filesystem_ranged_read`), and `FileSystem::read_file_range`
  lets the read and grep tools bound every transfer: an oversized file is
  truncated at the source and rejected with its true size instead of shipping
  up to the 512 MiB worker cap before the check, and the grep fallback caps
  each per-file transfer by the remaining byte budget. Covered by bridge
  glob/ranged-read unit tests, tool fallback and native-preference tests, and
  glob-over-WebSocket in the bridge server test.
- Step 6 (downstream idempotency for side-effect retries) is deliberately
  parked (2026-08-05). One attempt plus a terminal boundary failure is a safe,
  model-recoverable outcome, and durable jobs already cover long-running work
  across worker restarts. Revisit when deploy-time terminal tool failures
  become noisy in real sessions or when invisible worker deploys become a
  product requirement — not before, since the hard part is `run_process`
  start-or-attach semantics with output replay, which should not be built
  without a demanding workload. P114 is otherwise complete.
- Client reporting follow-up implemented (2026-08-05): public tool call and
  batch views preserve cancellation as `cancelled`, and the CLI renders it as
  cancelled rather than failed. Exact activity dispatch state remains deferred.
- Greenfield runtime change; compatibility with the batch activity shape is not
  required.
- Addresses the production failure recorded in
  [Remote Filesystem Grep Can Trap a Session in an Infinite Retry Loop](later/pNNN-remote-filesystem-grep-retry-loop.md).

## Problem

The hosted runtime executes a complete tool batch in one Temporal activity.
Calls run sequentially inside that activity, share one 360-second
start-to-close timeout, and inherit Temporal's unlimited retry policy. A slow
call therefore delays its siblings, a timeout discards completed sibling
results, and Temporal retries the whole batch. This is unsafe for mixed or
non-idempotent batches and can prevent a session from ever advancing.

Remote recursive filesystem tools amplify the problem. `grep` and `glob`
enumerate through the generic `FileSystem` interface, turning one logical
search into thousands of serialized environment-protocol directory and file calls.

## Decision 1: One Activity Per Tool Call

Keep provider-observed batch identity and batch completion semantics, but
schedule each executable tool call as its own Temporal activity. Batch-level
validation runs before any call is scheduled. Each call result is accepted and
appended independently; the batch becomes complete only when every call is
terminal.

```text
tool batch
  -> validate batch constraints
  -> call A activity -> durable call result
  -> call B activity -> durable call result
  -> call C activity -> durable call result
  -> batch terminal when A, B, and C are terminal
```

Per-call completion is an engine contract, not a runtime convenience. The
drive machine accepts each terminal call result as its own durable event and
completes the batch when the last call turns terminal, replacing the single
monolithic batch-outcome resume. A failed or timed-out call therefore never
re-runs a completed sibling. The runtime may still execute a particular batch
as one unit where that is genuinely simpler — the concurrency primitives are
the expected case — but that is an execution optimization behind the same
progressive-completion contract.

Provider context materialization preserves original call order even when calls
finish in another order. Execution concurrency follows the admitted tool
parallelism policy. Batch-specific rules for `await`, environment selection,
Fleet, and workflow tools remain explicit preflight/orchestration rules; they
must not force ordinary filesystem and process calls back into one activity.
Per-call activities also give each call its own host client rather than one
serialized connection; a batch against a remote environment may open several
concurrent bridge connections, bounded by the same parallelism policy.

The activity input carries the stable session, run, turn, batch, and call
identity plus the bounded runtime facts needed for that call. This identity is
also the future idempotency key for side-effecting host operations.

## Decision 2: Tool-Specific Activity Policies

Replace the shared activity options with a small set of runtime-owned tool
execution classes. The policy is selected from the admitted logical binding,
not from model-controlled input. Initial classes are:

- **interactive filesystem** — approximately 90 seconds of operation time.
  The class follows the logical tool domain, not the transport: environment
  filesystem calls against a remote host use this same class, since a single
  bounded operation fits comfortably once recursive scans are bounded or
  pushed host-side;
- **process** — a fixed class deadline of the deployment-owned process
  timeout ceiling plus bounded transport/completion grace. The per-request
  validated timeout (model-supplied `timeout_ms` clamped to the same ceiling)
  is enforced worker-side by the process executor; the class deadline is only
  the backstop against a hung transport. The scheduling workflow cannot
  derive per-request deadlines because tool arguments live behind CAS refs it
  never reads;
- **remote interactive** — bounded network, MCP, and environment-control
  calls; and
- **long-running** — return a process handle, Promise, or job instead of
  occupying a long-lived activity.

Each policy defines an operation soft deadline, Temporal start-to-close,
schedule-to-close, retry safety, and parallelism. The invariant is:

```text
operation soft deadline < start-to-close <= schedule-to-close
```

For classes with a retry allowance, schedule-to-close must additionally cover
the allowed attempts plus queue time; setting it equal to start-to-close
silently disables retries.

An operation deadline or resource limit returns an ordinary terminal tool
failure and is never retried; activities must surface these as typed
non-retryable failures so the retry policy can distinguish them from
infrastructure errors. A Temporal activity failure is converted at the
workflow boundary into a terminal call result rather than failing the session
workflow. Cancellation remains cancellation; a canceled call records a
terminal canceled result so a partially completed batch still terminates
deterministically.

Read-only calls may receive a small bounded retry allowance for infrastructure
failure. Mutations and process starts use one attempt until the downstream
operation deduplicates the stable call identity. No tool activity has an
unlimited retry policy.

Long builds and tests remain supported. `run_process` may use an activity
deadline derived from its admitted timeout, but the preferred long-running
shape is to yield a durable process handle and poll or await it.

## Decision 3: Execute Expensive Search At The Host

Extend `environment-protocol` with an optional filesystem text-search capability and
operation. `RemoteEnvironmentFileSystem` prefers it for environment grep; hosts
without the capability use the bounded generic fallback.

The host search request includes the root, regular expression, include filter,
case sensitivity, maximum depth, match limit, scan limits, and timeout. The
bridge performs the traversal and matching in-process with the ripgrep engine
crates (`grep-searcher`, `grep-regex`, `ignore`) rather than shelling out to
an `rg` binary: hosts need no external tool, there is no argv surface, and the
regex dialect stays identical to the worker-side fallback so a pattern cannot
succeed on one path and fail on the other. The response contains only bounded
matches and scan statistics.

The response reports whether and why it stopped, for example match, file,
byte, or time limit. Recursive glob/find is the next candidate for a host-side
operation. Ranged or capped host reads should follow so an oversized file is
not transferred in full before the worker rejects it; today the worker's
default read cap is 512 MiB, and the entire file crosses the network before
that check runs.

## Narrow Resource Bounds

P114 does not introduce a universal execution-budget framework. Activity
deadlines bound runtime occupancy; expensive search operations additionally
receive only the limits needed to bound their amplification:

- entries or files visited;
- cumulative bytes searched;
- matches returned; and
- elapsed search time.

Deployment configuration owns the maxima. A caller may request a lower limit
but cannot raise it. The same bounds apply in the host implementation and the
generic fallback. Generalize these counters into a shared execution-budget
abstraction only after additional operations demonstrate the same need.

## Delivery Order

1. Replace the monolithic batch-outcome resume with per-call engine
   completion events, and schedule one activity per executable call. — Done.
2. Introduce tool execution classes and bounded Temporal policies, including
   the deployment-owned process timeout ceiling. — Done.
3. Convert activity deadline/failure into terminal call results. — Done.
4. Add bounded host-side text search and the generic bounded fallback. — Done.
5. Add host-side recursive glob and capped/ranged reads where justified. —
   Done.
6. Add downstream idempotency for side-effecting calls before enabling their
   retries.

## Acceptance

- A slow or failed call cannot restart completed sibling calls; their results
  are already durable engine events.
- Every tool call has a total deadline and bounded Temporal attempts.
- Filesystem and process calls may use different timeout classes.
- A deadline produces a visible terminal tool result, not a failed or
  indefinitely running session workflow.
- A remote broad search performs bounded work at the host when supported.
- A regression test with tiny time and scan limits reaches a terminal call and
  run outcome without repeating a non-idempotent sibling.

## Non-Goals

- A universal resource-accounting framework for all tools.
- Dedicated scan-progress or attempt-count observability; each call's own
  Temporal activity state is the intended visibility for now.
- Making every tool call parallel.
- Retrying side effects without downstream idempotency.
- Raising the existing batch timeout as a substitute for decomposition.

## Appendix: Client Reporting Follow-Up

Implemented 2026-08-05: cancellation is preserved through the public API
instead of projected as a failure. `Cancelled` was added to `ToolItemStatus`,
`ToolCallStatus::Cancelled` is mapped to it in `api-projection`, and the API
contract and TypeScript consumers were regenerated. A canceled call is
terminal, but clients should render it neutrally rather than as a failed call.

Do not add exact queued-versus-executing activity reporting as part of P114.
`ToolCallStarted` currently represents a call entering the invocable batch,
not necessarily the moment its per-call Temporal activity is dispatched.
Clients should therefore describe any nonterminal call generically as “in
progress.” If exact dispatch state becomes product-relevant later, introduce a
separate durable reporting event with explicit replay and recovery semantics.
